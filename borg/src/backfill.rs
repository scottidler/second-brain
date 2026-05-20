//! One-shot helpers that adapt the existing vault to schema changes.
//!
//! Currently houses `backfill_ingested`, which walks the vault, finds every
//! `origin: assisted` note that lacks an `ingested:` frontmatter field, and
//! sets `ingested: <date:>` so the dashboard's `WHERE ingested = ...`
//! queries can find every legacy note from day 1.

use crate::config::Config;
use crate::pipeline::atomic::{apply_ingested_date, write_atomic};
use eyre::{Context, Result};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

/// Outcome of a backfill run. `dry_run = true` populates `would_backfill`;
/// `dry_run = false` populates `backfilled`. Splitting the two disambiguates
/// "what would happen" from "what did happen" without forcing sb to
/// cross-reference the input opts.
#[derive(Debug, Default)]
pub struct BackfillReport {
    pub scanned: usize,
    pub would_backfill: usize,
    pub backfilled: usize,
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
    /// Note is eligible for backfill. `date` is the value to splice into the `ingested:` field.
    Apply {
        path: PathBuf,
        content: String,
        date: String,
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
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after = &trimmed[3..];
    let end = after.find("\n---")?;
    let fm = &after[..end];
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

/// Skip notes whose mtime is within `min_age` of now - guards against
/// rewriting a file currently being written by another borg process.
fn is_recently_modified(path: &Path, min_age: Duration) -> Result<bool> {
    let meta = std::fs::metadata(path).with_context(|| format!("metadata {}", path.display()))?;
    let mtime = meta.modified().context("modified time")?;
    let age = SystemTime::now().duration_since(mtime).unwrap_or(Duration::ZERO);
    Ok(age < min_age)
}

/// Classify a single note file. Pure CPU + read-only I/O; safe to run from a rayon worker.
fn classify_for_backfill(path: &Path, min_age: Duration) -> BackfillDecision {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("backfill-ingested: cannot read {}: {e}", path.display());
            return BackfillDecision::ReadFailed;
        }
    };

    let origin = extract_frontmatter_field(&content, "origin");
    if origin.as_deref() != Some("assisted") {
        return BackfillDecision::SkippedAuthored;
    }
    if extract_frontmatter_field(&content, "ingested").is_some() {
        return BackfillDecision::SkippedAlreadyPresent;
    }
    let Some(date) = extract_frontmatter_field(&content, "date") else {
        log::debug!("backfill-ingested: skipping {} (no date: field)", path.display());
        return BackfillDecision::SkippedNoDate;
    };

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
            date,
        },
        Err(e) => {
            log::warn!("backfill-ingested: mtime check failed for {}: {e}", path.display());
            BackfillDecision::MtimeError
        }
    }
}

/// Run the backfill end-to-end on an explicit vault root and return the resulting report.
///
/// Pure helper, no global Config dependency. Used both by the public `run_backfill_ingested`
/// entry point (which then prints the report) and by the counter-correctness test.
pub(crate) fn backfill_on(vault_root: &Path, skip_folders: &[String], dry_run: bool) -> Result<BackfillReport> {
    log::debug!(
        "backfill::run_backfill_on: vault={} dry_run={dry_run}",
        vault_root.display()
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
        .map(|path| classify_for_backfill(path, min_age))
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
    for decision in to_apply {
        let BackfillDecision::Apply { path, content, date } = decision else {
            unreachable!("filter_map already kept only Apply variants");
        };

        if dry_run {
            log::info!("backfill-ingested: WOULD set ingested={date} on {}", path.display());
            would_backfill += 1;
            continue;
        }

        let updated = apply_ingested_date(&content, &date);
        if updated == content {
            log::warn!(
                "backfill-ingested: apply_ingested_date produced no change for {}",
                path.display()
            );
            continue;
        }
        write_atomic(&path, updated.as_bytes()).with_context(|| format!("write {}", path.display()))?;
        log::info!("backfill-ingested: set ingested={date} on {}", path.display());
        backfilled += 1;
    }

    Ok(BackfillReport {
        scanned: md_files.len(),
        would_backfill,
        backfilled,
        skipped_origin: skipped_origin.into_inner(),
        skipped_already_had: skipped_already_had.into_inner(),
        skipped_recent_mtime: skipped_recent_mtime.into_inner(),
        skipped_no_date: skipped_no_date.into_inner(),
    })
}

pub fn ingested(config: &Config, dry_run: bool) -> Result<BackfillReport> {
    let vault_root = config.vault_root()?;
    backfill_on(&vault_root, &config.migration.skip_folders, dry_run)
}

#[cfg(test)]
mod tests;
