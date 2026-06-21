//! One-shot helpers that adapt the existing vault to schema changes.
//!
//! Houses the `ingested` backfill, which walks the vault and sets the
//! `ingested:` frontmatter field on every `origin: assisted` note to a
//! homogeneous, sortable, second-precision local timestamp (ISO-8601 with
//! offset, e.g. `2026-06-05T08:27:25-07:00`):
//!
//! * If the note's `trace:` matches a `succeeded` row in `receipts.db`, the
//!   precise `received_at` capture time is used (and OVERWRITES any prior
//!   date-only `ingested:` value - an upgrade in place).
//! * Otherwise a date is promoted to local midnight (`<date>T00:00:00<offset>`):
//!   an existing date-only `ingested:` is homogenized in place, or - if absent
//!   entirely - the content `date:` is used. Notes whose `ingested:` is already
//!   a datetime are left untouched.
//!
//! Writing every value as an offset datetime keeps the `ingested` column a
//! single type so `borg-ledger.base` sorts lexically = chronologically without
//! relying on Bases' mixed date/datetime inference.

use crate::config::Config;
use crate::pipeline::atomic::{apply_ingested_date, apply_trace_expires, write_atomic};
use crate::receipts;
use crate::retention;
use chrono::{NaiveDate, TimeZone};
use chrono_tz::Tz;
use eyre::{Context, Result};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

/// Outcome of a backfill run. `dry_run = true` populates `would_backfill`;
/// `dry_run = false` populates `backfilled`. Splitting the two disambiguates
/// "what would happen" from "what did happen" without forcing sb to
/// cross-reference the input opts. `precise` counts how many of the applied
/// notes got a receipts-derived timestamp (vs the date-midnight fallback).
#[derive(Debug, Default)]
pub struct BackfillReport {
    pub scanned: usize,
    pub would_backfill: usize,
    pub backfilled: usize,
    pub precise: usize,
    pub skipped_origin: usize,
    pub skipped_already_had: usize,
    pub skipped_recent_mtime: usize,
    pub skipped_no_date: usize,
}

/// Per-path classification produced by the parallel scan phase. The sequential phase that
/// follows simply tallies the counts and (for `Apply`) writes through `write_atomic`. Keeping
/// the write phase sequential side-steps the parent-directory `fsync` contention that
/// `write_atomic` would cause if 16 threads concurrently wrote into the same `inbox/<date>/`
/// folder.
enum BackfillDecision {
    SkippedAuthored,
    SkippedAlreadyPresent,
    SkippedRecentlyModified,
    SkippedNoDate,
    ReadFailed,
    MtimeError,
    /// Note is eligible for backfill. `ingested` is `Some` when the `ingested:`
    /// field needs to be written/homogenized; `trace_expires` is `Some` when the
    /// note carries a `trace:` and its `trace-expires:` is missing or stale.
    /// At least one is `Some`. `precise` is true when the ingested value came
    /// from a receipts match.
    Apply {
        path: PathBuf,
        content: String,
        ingested: Option<String>,
        trace_expires: Option<String>,
        precise: bool,
    },
}

fn collect_md_files(root: &Path, skip_folders: &[String]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, root, skip_folders, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(current: &Path, root: &Path, skip_folders: &[String], out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(current).with_context(|| format!("read_dir {}", current.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string();
            if skip_folders.iter().any(|s| rel.starts_with(s)) {
                continue;
            }
            walk(&path, root, skip_folders, out)?;
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    Ok(())
}

fn extract_frontmatter_field(content: &str, field: &str) -> Option<String> {
    let (fm, _body) = vault::frontmatter::split_raw(content)?;
    for line in fm.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&format!("{field}:")) {
            let val = rest.trim().trim_matches('"');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Convert a receipts `received_at` (RFC-3339, typically UTC `...Z`) into a
/// local ISO-8601 timestamp with offset in `tz`. Returns `None` on parse
/// failure so a single malformed row never aborts the whole backfill.
pub(crate) fn local_from_utc(received_at: &str, tz: Tz) -> Option<String> {
    let dt = chrono::DateTime::parse_from_rfc3339(received_at.trim()).ok()?;
    Some(dt.with_timezone(&tz).format("%Y-%m-%dT%H:%M:%S%:z").to_string())
}

/// Promote a date-only `date:` value (`2026-05-11`) to a homogenized local
/// midnight ISO-8601 timestamp with offset, so the `ingested` column never
/// mixes bare dates with datetimes.
pub(crate) fn local_date_midnight(date: &str, tz: Tz) -> Option<String> {
    let nd = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    let ndt = nd.and_hms_opt(0, 0, 0)?;
    let local = tz.from_local_datetime(&ndt).earliest()?;
    Some(local.format("%Y-%m-%dT%H:%M:%S%:z").to_string())
}

/// Skip notes whose mtime is within `min_age` of now - guards against
/// rewriting a file currently being written by another borg process.
fn is_recently_modified(path: &Path, min_age: Duration) -> Result<bool> {
    let meta = std::fs::metadata(path).with_context(|| format!("metadata {}", path.display()))?;
    let mtime = meta.modified().context("modified time")?;
    let age = SystemTime::now().duration_since(mtime).unwrap_or(Duration::ZERO);
    Ok(age < min_age)
}

/// Classify a single note file. Pure CPU + read-only I/O; safe to run from a rayon worker.
///
/// `receipts` maps `trace_id -> precise local ISO timestamp`. A receipt match
/// always wins (upgrading a date-only value); absent a match, the note's
/// `date:` is promoted to local midnight and written only when `ingested:` is
/// missing. Either way, if the existing value already equals the target the
/// note is skipped so re-runs are idempotent.
fn classify_for_backfill(
    path: &Path,
    min_age: Duration,
    receipts: &HashMap<String, String>,
    tz: Tz,
    retention_days: u32,
) -> BackfillDecision {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("backfill-ingested: cannot read {}: {e}", path.display());
            return BackfillDecision::ReadFailed;
        }
    };

    // A note is borg-ingested (and visible in the `source != null` views) if it
    // carries a `source:` URL, OR is tagged `origin: assisted`. The `origin`
    // label is historically unreliable - some genuinely-ingested notes are
    // mislabeled `origin: authored` - so `source:` presence is the primary
    // signal and catches those that the bare origin gate would skip.
    let origin = extract_frontmatter_field(&content, "origin");
    let has_source = extract_frontmatter_field(&content, "source").is_some();
    if origin.as_deref() != Some(vault::schema::Origin::Assisted.as_str()) && !has_source {
        return BackfillDecision::SkippedAuthored;
    }

    let existing_ingested = extract_frontmatter_field(&content, "ingested");
    let trace = extract_frontmatter_field(&content, "trace").filter(|t| !t.is_empty());
    let existing_expires = extract_frontmatter_field(&content, "trace-expires");

    // --- Target `ingested` (the existing homogenization, decoupled from any
    // early skip so we can still stamp trace-expires on already-homogeneous
    // notes). `None` means "no rewrite of ingested" - either it's already a
    // datetime, or there is no date basis to promote.
    let precise_ts = trace.as_deref().and_then(|t| receipts.get(t)).cloned();
    let (target_ingested, is_precise): (Option<String>, bool) = match precise_ts {
        Some(p) => (Some(p), true),
        None => {
            let basis: Option<String> = match existing_ingested.as_deref() {
                // Already a homogeneous datetime: leave ingested as-is.
                Some(v) if v.contains('T') => None,
                // Date-only ingested: homogenize that value in place.
                Some(v) => Some(v.to_string()),
                // No ingested: promote the content `date:`.
                None => extract_frontmatter_field(&content, "date"),
            };
            (basis.and_then(|b| local_date_midnight(&b, tz)), false)
        }
    };
    let write_ingested = match &target_ingested {
        Some(v) => existing_ingested.as_deref() != Some(v.as_str()),
        None => false,
    };

    // --- Target `trace-expires` (only when the note has a `trace:`). Compute
    // it from the EFFECTIVE ingested date - the value ingested will hold after
    // this run - parsing whichever ingested form is present. Receipts being
    // unavailable does NOT skip: the midnight fallback still yields an ingested
    // date, so trace-expires is best-effort (precise=false) rather than absent
    // (which the design reserves for "no trace data").
    let effective_ingested = target_ingested.clone().or_else(|| existing_ingested.clone());
    let target_expires: Option<String> = match (&trace, effective_ingested.as_deref()) {
        (Some(_), Some(ing)) => {
            retention::parse_ingested_date(ing).map(|d| retention::trace_expires_for(d, retention_days))
        }
        _ => None,
    };
    let write_expires = match &target_expires {
        Some(v) => existing_expires.as_deref() != Some(v.as_str()),
        None => false,
    };

    if !write_ingested && !write_expires {
        // Nothing to do. Preserve the "no date basis" signal for notes that
        // genuinely carry no ingestable date; everything else is "already
        // settled" (idempotent re-run).
        if existing_ingested.is_none() && target_ingested.is_none() && existing_expires.is_none() {
            log::debug!("backfill-ingested: skipping {} (no date: field)", path.display());
            return BackfillDecision::SkippedNoDate;
        }
        return BackfillDecision::SkippedAlreadyPresent;
    }

    match is_recently_modified(path, min_age) {
        Ok(true) => {
            log::debug!(
                "backfill-ingested: skipping {} (mtime within {:?})",
                path.display(),
                min_age
            );
            BackfillDecision::SkippedRecentlyModified
        }
        Ok(false) => BackfillDecision::Apply {
            path: path.to_path_buf(),
            content,
            ingested: if write_ingested { target_ingested } else { None },
            trace_expires: if write_expires { target_expires } else { None },
            precise: is_precise && write_ingested,
        },
        Err(e) => {
            log::warn!("backfill-ingested: mtime check failed for {}: {e}", path.display());
            BackfillDecision::MtimeError
        }
    }
}

/// Run the backfill end-to-end against an explicit vault root + injected receipts map.
///
/// Pure helper, no global Config dependency: the `receipts` map and `tz` are passed in so
/// tests exercise the precise-upgrade and date-fallback paths without a real SQLite DB.
pub(crate) fn backfill_on(
    vault_root: &Path,
    skip_folders: &[String],
    receipts: &HashMap<String, String>,
    tz: Tz,
    retention_days: u32,
    dry_run: bool,
) -> Result<BackfillReport> {
    log::debug!(
        "backfill::backfill_on: vault={} receipts={} retention_days={retention_days} dry_run={dry_run}",
        vault_root.display(),
        receipts.len()
    );
    let md_files = collect_md_files(vault_root, skip_folders).context("Failed to walk vault")?;
    log::info!("backfill-ingested: scanning {} note files", md_files.len());

    // Parallel classification phase: pure CPU + read-only I/O per note.
    // Counters are aggregated lock-free via AtomicUsize; the only items that flow back to the
    // sequential write phase are the `Apply` decisions.
    let min_age = Duration::from_secs(60);
    let skipped_origin = AtomicUsize::new(0);
    let skipped_already_had = AtomicUsize::new(0);
    let skipped_recent_mtime = AtomicUsize::new(0);
    let skipped_no_date = AtomicUsize::new(0);

    let to_apply: Vec<BackfillDecision> = md_files
        .par_iter()
        .map(|path| classify_for_backfill(path, min_age, receipts, tz, retention_days))
        .filter_map(|decision| match decision {
            BackfillDecision::SkippedAuthored => {
                skipped_origin.fetch_add(1, Ordering::Relaxed);
                None
            }
            BackfillDecision::SkippedAlreadyPresent => {
                skipped_already_had.fetch_add(1, Ordering::Relaxed);
                None
            }
            BackfillDecision::SkippedRecentlyModified => {
                skipped_recent_mtime.fetch_add(1, Ordering::Relaxed);
                None
            }
            BackfillDecision::SkippedNoDate => {
                skipped_no_date.fetch_add(1, Ordering::Relaxed);
                None
            }
            BackfillDecision::ReadFailed | BackfillDecision::MtimeError => None,
            apply @ BackfillDecision::Apply { .. } => Some(apply),
        })
        .collect();

    let mut would_backfill: usize = 0;
    let mut backfilled: usize = 0;
    let mut precise: usize = 0;
    for decision in to_apply {
        let BackfillDecision::Apply {
            path,
            content,
            ingested,
            trace_expires,
            precise: is_precise,
        } = decision
        else {
            unreachable!("filter_map already kept only Apply variants");
        };

        if is_precise {
            precise += 1;
        }

        if dry_run {
            log::info!(
                "backfill-ingested: WOULD set ingested={ingested:?} trace-expires={trace_expires:?} on {}",
                path.display()
            );
            would_backfill += 1;
            continue;
        }

        let mut updated = content.clone();
        if let Some(ref iv) = ingested {
            updated = apply_ingested_date(&updated, iv);
        }
        if let Some(ref ev) = trace_expires {
            updated = apply_trace_expires(&updated, ev);
        }
        if updated == content {
            log::warn!(
                "backfill-ingested: field application produced no change for {}",
                path.display()
            );
            continue;
        }
        write_atomic(&path, updated.as_bytes()).with_context(|| format!("write {}", path.display()))?;
        log::info!(
            "backfill-ingested: set ingested={ingested:?} trace-expires={trace_expires:?} on {}",
            path.display()
        );
        backfilled += 1;
    }

    Ok(BackfillReport {
        scanned: md_files.len(),
        would_backfill,
        backfilled,
        precise,
        skipped_origin: skipped_origin.into_inner(),
        skipped_already_had: skipped_already_had.into_inner(),
        skipped_recent_mtime: skipped_recent_mtime.into_inner(),
        skipped_no_date: skipped_no_date.into_inner(),
    })
}

/// Build the `trace_id -> precise local ISO timestamp` map from `receipts.db`.
/// Only `succeeded` rows (the ones that produced a note) are considered.
fn load_received_at_map(tz: Tz) -> Result<HashMap<String, String>> {
    let conn = receipts::open_default().context("open receipts.db for backfill")?;
    let mut stmt = conn
        .prepare("SELECT trace_id, received_at FROM receipts WHERE status = 'succeeded'")
        .context("prepare receipts query")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .context("query receipts rows")?;
    let mut map = HashMap::new();
    for row in rows {
        let (trace, received_at) = row.context("read receipts row")?;
        if let Some(local) = local_from_utc(&received_at, tz) {
            map.insert(trace, local);
        }
    }
    Ok(map)
}

pub fn ingested(config: &Config, dry_run: bool) -> Result<BackfillReport> {
    let vault_root = config.vault_root()?;
    let tz: Tz = config.frontmatter.timezone_tz();
    // Receipts only go back to the staged-pipeline era; notes older than that
    // (or ingested before receipts existed) simply fall back to date-midnight.
    let receipts = load_received_at_map(tz).unwrap_or_else(|e| {
        log::warn!("backfill-ingested: receipts.db unavailable ({e:#}); using date fallback only");
        HashMap::new()
    });
    log::info!(
        "backfill-ingested: {} succeeded receipts available for precise timestamps",
        receipts.len()
    );
    backfill_on(
        &vault_root,
        &config.migration.skip_folders,
        &receipts,
        tz,
        config.staging.retention_days,
        dry_run,
    )
}

#[cfg(test)]
mod tests;
