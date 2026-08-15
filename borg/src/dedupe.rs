//! `sb borg dedupe-sessions`: retire the surplus harvest-session-note forks
//! left behind by every `sb borg replay` before the trace-keyed-replace fix
//! (design doc `docs/design/2026-08-15-harvest-note-identity-trace-keyed-replace.md`,
//! Phase 6). Dry-run by default; `--apply` writes; `--purge` is a separate,
//! opt-in archival pass over already-tombstoned notes.
//!
//! **Not** the association merge executor (`cortex::association::decide`):
//! that groups by pairwise similarity, which is the wrong instrument for
//! notes that are identical BY CONSTRUCTION (15 re-renderings of the same
//! transcript). This module groups by identity - `trace:` frontmatter, full
//! stop - and picks a survivor by a fixed rule, never a similarity score.
//!
//! Losers become tombstones, never deletions: `slug:` is stripped,
//! `superseded-by: <survivor-stem>` is inserted, and the body becomes a
//! `Merged into [[survivor-stem]].` redirect - the exact shape
//! `cortex::association::tombstone_content` already ships (reused as a
//! CONTRACT, not a shared function: borg does not depend on cortex, per the
//! workspace's one-way capture/governance layering, so the shape is
//! reproduced here rather than imported). Because the filename never
//! changes, every inbound `[[wikilink]]` - piped, path-qualified, or
//! embedded (the single regex below matches all three; a `.base` view is a
//! property-filter query, not a literal wikilink, so it needs no rewrite
//! either) - keeps resolving through the tombstone's redirect. There is
//! deliberately no link-rewrite pass; see the design doc's Alternative 3.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

use eyre::{Context, Result};
use regex::Regex;
use rusqlite::Connection;

use vault::config::ScanConfig;
use vault::note::{self, Note};
use vault::schema::NoteType;

use crate::config::{Config, StagingConfig};
use crate::receipts::{self, Receipt};
use crate::stages::artifact::{ArtifactStore, FsArtifactStore};

/// New (Phase 1) frontmatter key holding the SHA-256 of the input transcript.
const HARVEST_BODY_HASH_KEY: &str = "harvest-body-hash";
/// Cortex's soft-retire marker (`cortex::association::tombstone_content`) -
/// a bare filename stem naming the live survivor. Reused here as a contract:
/// this module writes and reads the identical key/shape.
const SUPERSEDED_BY_KEY: &str = "superseded-by";
/// Borg's content-slug key. Stripped on tombstone so a retired note never
/// re-groups (mirrors `cortex::association::tombstone_content`).
const SLUG_KEY: &str = "slug";
/// Cortex's needs-review flag - one of the three degradation signals the
/// survivor rule checks.
const NEEDS_REVIEW_KEY: &str = "cortex-needs-review";
/// Distiller success flag - the survivor rule's first, coarsest gate.
const DISTILLED_KEY: &str = "distilled";
/// The literal fallback-summary markers a degraded distill pass emits
/// (`distillers::validate::fallback_distilled`: `format!("[{reason}]\n\n...")`
/// with `reason` in `{"missing-summary", "yaml-parse-error"}`). These land as
/// the first line of the rendered `## Summary` section, so a body substring
/// check is the correct seam - there is no separate frontmatter flag for
/// "this distill degraded", only these two literal tags plus
/// `cortex-needs-review` (an independent, cortex-side signal).
const DEGRADATION_MARKERS: &[&str] = &["[missing-summary]", "[yaml-parse-error]"];

/// Wikilink target extractor. Matches `[[target]]` and `[[target|alias]]`; an
/// `![[target]]` embed also matches because the `!` sits outside the capture
/// group. Deliberately re-derived here rather than imported from
/// `cortex::links` (borg does not depend on cortex).
static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]").expect("valid wikilink regex"));

/// `sb borg dedupe-sessions` flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct DedupeOpts {
    /// Write the tombstones (and, with `purge`, archive eligible ones).
    /// Without this, nothing is written - the report describes what WOULD
    /// happen.
    pub apply: bool,
    /// Additionally rkvr-archive any superseded-by tombstone (freshly planned
    /// this run, or left over from an earlier run) that has zero live inbound
    /// wikilinks. Independent of `apply`: `purge` alone previews/archives
    /// tombstones that already exist without planning new ones.
    pub purge: bool,
}

/// One trace's duplicate cohort: the chosen survivor plus every note this
/// run tombstones (or would, in a dry run). Paths are vault-relative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupeGroup {
    pub trace: String,
    pub survivor: PathBuf,
    pub tombstoned: Vec<PathBuf>,
}

/// Result of the `harvest-body-hash:` backfill pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Vault-relative paths that gained (or would gain) `harvest-body-hash:`.
    pub backfilled: Vec<PathBuf>,
    /// Vault-relative paths carrying `trace:` but no surviving staging to
    /// recover the hash from - reported, per the design's "the rest are
    /// reported, not silently skipped", never dropped from the output.
    pub uncovered: Vec<PathBuf>,
}

/// Result of the `--purge` pass. `None` on a `DedupeReport` when `--purge`
/// was not requested.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgeReport {
    /// Vault-relative tombstones archived (or that would be archived).
    pub archived: Vec<PathBuf>,
    /// Vault-relative tombstones refused because at least one live inbound
    /// wikilink remains, paired with the (sorted) vault-relative paths of the
    /// notes that link to it.
    pub refused: Vec<(PathBuf, Vec<PathBuf>)>,
}

/// Full outcome of one `sb borg dedupe-sessions` invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DedupeReport {
    /// True when this was an `--apply` run (writes happened); false for a
    /// dry-run preview.
    pub applied: bool,
    pub groups: Vec<DedupeGroup>,
    pub backfill: BackfillReport,
    pub purge: Option<PurgeReport>,
}

/// Composition root: resolves the vault root and opens the default receipts
/// DB, then delegates to [`run_with`] (the conn-injectable, test-friendly
/// core - mirrors `triage::audit_health_stats` / `audit_health_stats_conn`).
pub fn run(config: &Config, opts: &DedupeOpts) -> Result<DedupeReport> {
    let vault_root = config.vault_root().context("dedupe-sessions: resolve vault root")?;
    if !vault_root.exists() {
        eyre::bail!("dedupe-sessions: vault root does not exist: {}", vault_root.display());
    }
    let conn = receipts::open_default().context("dedupe-sessions: open receipts DB")?;
    run_with(&vault_root, &conn, &config.staging, opts)
}

/// The testable core. Scans the vault once, plans the trace groups, applies
/// them if requested, then runs the (always-planned, apply-gated) backfill
/// and the (opt-in) purge pass.
pub fn run_with(
    vault_root: &Path,
    conn: &Connection,
    staging: &StagingConfig,
    opts: &DedupeOpts,
) -> Result<DedupeReport> {
    log::debug!(
        "dedupe::run_with: vault_root={} apply={} purge={}",
        vault_root.display(),
        opts.apply,
        opts.purge
    );
    let notes = note::scan_vault(vault_root, &ScanConfig::default())
        .with_context(|| format!("dedupe-sessions: scan_vault {}", vault_root.display()))?;

    let groups = plan_groups(vault_root, conn, &notes)?;

    if opts.apply {
        for group in &groups {
            apply_group(vault_root, group)?;
        }
    }

    let tombstoned_this_run: HashSet<&PathBuf> = groups.iter().flat_map(|g| g.tombstoned.iter()).collect();
    let backfill = plan_backfill(vault_root, staging, &notes, &tombstoned_this_run, opts.apply)?;

    let purge = if opts.purge {
        Some(run_purge(vault_root, &notes, &groups, opts.apply)?)
    } else {
        None
    };

    log::info!(
        "dedupe::run_with: groups={} backfilled={} uncovered={} purge={}",
        groups.len(),
        backfill.backfilled.len(),
        backfill.uncovered.len(),
        purge.is_some()
    );
    Ok(DedupeReport {
        applied: opts.apply,
        groups,
        backfill,
        purge,
    })
}

fn is_harvest_session(note: &Note) -> bool {
    note.frontmatter.note_type.as_deref() == Some(NoteType::Session.as_str())
}

fn is_tombstone(note: &Note) -> bool {
    note.frontmatter.extra.contains_key(SUPERSEDED_BY_KEY)
}

fn body_hash_present(note: &Note) -> bool {
    note.frontmatter.extra.contains_key(HARVEST_BODY_HASH_KEY)
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Group harvest session notes by `trace:`, choose a survivor per group, and
/// name the rest as the tombstone set. Never groups by similarity or by
/// filename/slug.
///
/// A safety check beyond the doc's literal "group by trace:, full stop": a
/// trace bucket is split by `source:` before being accepted as a duplicate
/// cohort. A 32-bit trace value is vanishingly unlikely but not impossible to
/// collide across two UNRELATED sessions (the whole reason every other
/// resolution path in this design carries a three-term guard); silently
/// tombstoning a real, distinct session because it landed in the same trace
/// bucket as an unrelated one would be exactly the destructive mistake this
/// design exists to prevent. A same-trace, different-source split is WARNed
/// and each source's own sub-cohort is evaluated independently (a sub-cohort
/// of size 1 is not a duplicate group).
fn plan_groups(vault_root: &Path, conn: &Connection, notes: &[Note]) -> Result<Vec<DedupeGroup>> {
    let mut by_trace: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, n) in notes.iter().enumerate() {
        if !is_harvest_session(n) || is_tombstone(n) {
            continue;
        }
        let Some(trace) = n.frontmatter.trace.as_deref().filter(|t| !t.is_empty()) else {
            continue;
        };
        by_trace.entry(trace.to_string()).or_default().push(i);
    }

    let mut groups = Vec::new();
    for (trace, idxs) in by_trace {
        if idxs.len() < 2 {
            continue;
        }
        for cohort in split_by_source(notes, &idxs, &trace) {
            if cohort.len() < 2 {
                continue;
            }
            let receipt = receipts::get(conn, &trace)
                .with_context(|| format!("dedupe-sessions: receipts lookup for trace {trace}"))?;
            let survivor_idx = *cohort
                .iter()
                .max_by(|&&a, &&b| {
                    survivor_key(vault_root, receipt.as_ref(), &notes[a]).cmp(&survivor_key(
                        vault_root,
                        receipt.as_ref(),
                        &notes[b],
                    ))
                })
                .expect("cohort has >= 2 members, checked above");
            let survivor = notes[survivor_idx].path.clone();
            let mut tombstoned: Vec<PathBuf> = cohort
                .iter()
                .filter(|&&i| i != survivor_idx)
                .map(|&i| notes[i].path.clone())
                .collect();
            tombstoned.sort();
            groups.push(DedupeGroup {
                trace: trace.clone(),
                survivor,
                tombstoned,
            });
        }
    }
    groups.sort_by(|a, b| (&a.trace, &a.survivor).cmp(&(&b.trace, &b.survivor)));
    Ok(groups)
}

/// Split a trace bucket into per-`source:` sub-cohorts. WARNs once per trace
/// when more than one distinct source is present (a real collision, or a
/// note with no `source:` at all sitting alongside ones that have it).
fn split_by_source(notes: &[Note], idxs: &[usize], trace: &str) -> Vec<Vec<usize>> {
    let mut by_source: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &i in idxs {
        let key = notes[i].frontmatter.source.clone().unwrap_or_default();
        by_source.entry(key).or_default().push(i);
    }
    if by_source.len() > 1 {
        log::warn!(
            "dedupe-sessions: trace {trace} has {} distinct source: values ({} note(s) total) - \
             treating as a trace collision, evaluating each source's cohort independently rather \
             than tombstoning across sessions",
            by_source.len(),
            idxs.len()
        );
    }
    by_source.into_values().collect()
}

/// Survivor sort key (ascending; the group's MAX wins): `(is_clean,
/// effective_timestamp, path)`. `is_clean` dominates (a degraded note never
/// survives over a clean one); among clean notes the greatest timestamp wins;
/// the path is the final, always-available tie-break.
fn survivor_key(vault_root: &Path, receipt: Option<&Receipt>, note: &Note) -> (bool, Option<i64>, String) {
    let abs = vault_root.join(&note.path);
    (
        is_clean(note),
        effective_timestamp(vault_root, &abs, receipt),
        note.path.to_string_lossy().into_owned(),
    )
}

/// `distilled: true` AND neither degradation signal (`cortex-needs-review:
/// true`, nor a literal `[missing-summary]`/`[yaml-parse-error]` marker in the
/// rendered `## Summary`). All three landed-note-level checks, never
/// `ingested:` - the design doc rejects "earliest ingested" explicitly (it is
/// a date, not a timestamp, and it provably picks a degraded note in the real
/// `hv-e5d240` cohort).
fn is_clean(note: &Note) -> bool {
    let distilled = note.frontmatter.extra.get(DISTILLED_KEY) == Some(&serde_yaml::Value::Bool(true));
    let needs_review = note.frontmatter.extra.get(NEEDS_REVIEW_KEY) == Some(&serde_yaml::Value::Bool(true));
    let degraded = DEGRADATION_MARKERS.iter().any(|m| note.body.contains(m));
    distilled && !needs_review && !degraded
}

/// The greatest receipts `terminal_at` if `abs_path` is the CURRENT
/// `note_path` the trace's receipts row names (a shared-trace group has only
/// ONE receipts row, so `terminal_at` only ever attaches to whichever fork
/// receipts currently points at - every sibling fork falls through to its own
/// mtime), else the file's mtime, else `None` (both reads failed - the path
/// tie-break in [`survivor_key`] still applies).
fn effective_timestamp(vault_root: &Path, abs_path: &Path, receipt: Option<&Receipt>) -> Option<i64> {
    if let Some(r) = receipt
        && let Some(recorded) = r.note_path.as_deref()
        && let Some(terminal_at) = r.terminal_at.as_deref()
        && normalize_receipt_path(vault_root, recorded) == abs_path
        && let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(terminal_at, "%Y-%m-%dT%H:%M:%SZ")
    {
        return Some(parsed.and_utc().timestamp());
    }
    std::fs::metadata(abs_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

fn normalize_receipt_path(vault_root: &Path, recorded: &str) -> PathBuf {
    let p = Path::new(recorded);
    if p.is_absolute() { p.to_path_buf() } else { vault_root.join(p) }
}

/// Tombstone every loser in `group`. Re-reads each file fresh off disk
/// immediately before writing (mirrors
/// `cortex::association::execute_merge`'s apply order) rather than reusing
/// the earlier `scan_vault` snapshot.
fn apply_group(vault_root: &Path, group: &DedupeGroup) -> Result<()> {
    let survivor_stem = stem_of(&group.survivor);
    for loser in &group.tombstoned {
        let abs = vault_root.join(loser);
        let raw = std::fs::read_to_string(&abs).with_context(|| format!("dedupe-sessions: read {}", abs.display()))?;
        let (mut fm, _body) = vault::frontmatter::parse_frontmatter(&raw)?;
        fm.extra.remove(SLUG_KEY);
        fm.extra.insert(
            SUPERSEDED_BY_KEY.to_string(),
            serde_yaml::Value::String(survivor_stem.clone()),
        );
        let yaml = fm
            .to_yaml()
            .context("dedupe-sessions: serialize tombstone frontmatter")?;
        let redirect_body = format!("Merged into [[{survivor_stem}]].\n");
        let content = format!("---\n{yaml}---\n\n{redirect_body}");
        vault::note::write_atomic(&abs, content.as_bytes())
            .with_context(|| format!("dedupe-sessions: tombstone write {}", abs.display()))?;
        log::info!(
            "dedupe-sessions: tombstoned {} -> superseded-by {survivor_stem}",
            loser.display()
        );
    }
    Ok(())
}

/// Backfill `harvest-body-hash:` from staged `body.txt` onto every harvest
/// session note that has a `trace:`, is not (and will not become, this run) a
/// tombstone, and does not already carry the key. Every candidate lands in
/// exactly one of `backfilled` / `uncovered` - never silently dropped.
fn plan_backfill(
    vault_root: &Path,
    staging: &StagingConfig,
    notes: &[Note],
    tombstoned_this_run: &HashSet<&PathBuf>,
    apply: bool,
) -> Result<BackfillReport> {
    let store = FsArtifactStore::from_config(staging);
    let mut report = BackfillReport::default();

    for n in notes {
        if !is_harvest_session(n) || is_tombstone(n) || tombstoned_this_run.contains(&n.path) {
            continue;
        }
        let Some(trace) = n.frontmatter.trace.as_deref().filter(|t| !t.is_empty()) else {
            continue;
        };
        if body_hash_present(n) {
            continue;
        }

        match backfill_one(vault_root, &store, n, trace, apply) {
            Ok(true) => report.backfilled.push(n.path.clone()),
            Ok(false) => report.uncovered.push(n.path.clone()),
            Err(e) => {
                log::warn!(
                    "dedupe-sessions: backfill failed for {} (trace {trace}): {e:#}",
                    n.path.display()
                );
                report.uncovered.push(n.path.clone());
            }
        }
    }
    report.backfilled.sort();
    report.uncovered.sort();
    Ok(report)
}

/// `Ok(true)` = backfilled (or would be, on a dry run); `Ok(false)` = no
/// staging survives retention, reported as uncovered; `Err` = a real I/O/
/// encoding failure (also reported as uncovered by the caller, with the error
/// logged).
fn backfill_one(vault_root: &Path, store: &FsArtifactStore, note: &Note, trace: &str, apply: bool) -> Result<bool> {
    if !store
        .has_trace(trace)
        .with_context(|| format!("dedupe-sessions: probe staging for trace {trace}"))?
    {
        return Ok(false);
    }
    let bytes = store
        .read_body(trace)
        .with_context(|| format!("dedupe-sessions: read staged body for trace {trace}"))?;
    let text = String::from_utf8(bytes)
        .with_context(|| format!("dedupe-sessions: staged body for trace {trace} is not utf-8"))?;
    let hash = crate::harvest::watermark::body_hash(&text);
    if apply {
        write_backfilled_hash(vault_root, note, &hash)?;
    }
    Ok(true)
}

fn write_backfilled_hash(vault_root: &Path, note: &Note, hash: &str) -> Result<()> {
    let abs = vault_root.join(&note.path);
    let raw = std::fs::read_to_string(&abs).with_context(|| format!("dedupe-sessions: read {}", abs.display()))?;
    let (mut fm, body) = vault::frontmatter::parse_frontmatter(&raw)?;
    fm.extra.insert(
        HARVEST_BODY_HASH_KEY.to_string(),
        serde_yaml::Value::String(hash.to_string()),
    );
    let yaml = fm
        .to_yaml()
        .context("dedupe-sessions: serialize backfilled frontmatter")?;
    let content = format!("---\n{yaml}---\n\n{body}");
    vault::note::write_atomic(&abs, content.as_bytes())
        .with_context(|| format!("dedupe-sessions: backfill write {}", abs.display()))
}

/// Archive (via `rkvr::remove`) every candidate tombstone with zero live
/// inbound wikilinks; refuse (report, never write) the rest.
///
/// Candidates are the union of (a) every already-on-disk Session note
/// carrying `superseded-by:` and (b) this run's freshly-planned tombstones
/// (which, on a dry run, are not yet reflected in `notes` at all) - so
/// `--purge` alone still finds tombstones a PRIOR `--apply` run left behind.
fn run_purge(vault_root: &Path, notes: &[Note], groups: &[DedupeGroup], apply: bool) -> Result<PurgeReport> {
    let mut candidates: Vec<PathBuf> = notes
        .iter()
        .filter(|n| is_harvest_session(n) && is_tombstone(n))
        .map(|n| n.path.clone())
        .collect();
    for g in groups {
        for t in &g.tombstoned {
            if !candidates.contains(t) {
                candidates.push(t.clone());
            }
        }
    }
    candidates.sort();
    candidates.dedup();

    let index = build_link_index(notes);

    let mut report = PurgeReport::default();
    let mut to_archive_abs: Vec<PathBuf> = Vec::new();
    for tombstone in candidates {
        let stem = stem_of(&tombstone);
        let inbound = inbound_links(&index, &tombstone, &stem);
        if inbound.is_empty() {
            to_archive_abs.push(vault_root.join(&tombstone));
            report.archived.push(tombstone);
        } else {
            report.refused.push((tombstone, inbound));
        }
    }
    report.archived.sort();
    report.refused.sort_by(|a, b| a.0.cmp(&b.0));

    if apply && !to_archive_abs.is_empty() {
        crate::rkvr::remove(&to_archive_abs)?;
    }

    Ok(report)
}

/// (source note path) -> every wikilink target it contains, lowercased and
/// forward-slash-normalized. Built once per purge pass over the WHOLE vault
/// (a maintenance command, not a hot path).
fn build_link_index(notes: &[Note]) -> Vec<(PathBuf, Vec<String>)> {
    notes
        .iter()
        .map(|n| (n.path.clone(), extract_targets(&strip_fenced_code_blocks(&n.body))))
        .collect()
}

fn extract_targets(body: &str) -> Vec<String> {
    WIKILINK_RE
        .captures_iter(body)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().trim().to_lowercase().replace('\\', "/"))
        .collect()
}

/// Strip fenced code blocks so a literal `[[...]]` inside a quoted diff/
/// transcript is never mistaken for a real wikilink. Line-based, mirrors
/// `cortex::links::strip_fenced_code_blocks` (re-derived, not imported - see
/// the module doc).
fn strip_fenced_code_blocks(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push('\n');
            continue;
        }
        if in_fence {
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Every OTHER note's path whose body links to `tombstone` (by stem or by
/// full vault-relative path, case-insensitively) - sorted. A tombstone's own
/// outbound redirect (`Merged into [[survivor]].`) is never counted as
/// inbound to ITSELF.
fn inbound_links(index: &[(PathBuf, Vec<String>)], tombstone: &Path, stem: &str) -> Vec<PathBuf> {
    let stem_lower = stem.to_lowercase();
    let path_lower = tombstone
        .with_extension("")
        .to_string_lossy()
        .to_lowercase()
        .replace('\\', "/");
    let mut sources: Vec<PathBuf> = index
        .iter()
        .filter(|(src, _)| src != tombstone)
        .filter(|(_, targets)| targets.iter().any(|t| *t == stem_lower || *t == path_lower))
        .map(|(src, _)| src.clone())
        .collect();
    sources.sort();
    sources
}

#[cfg(test)]
mod tests;
