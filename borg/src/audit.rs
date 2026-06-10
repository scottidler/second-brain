use crate::config::Config;
use crate::ledger;
use crate::migrate::reclassify_type;
use crate::quality;
use clap::ValueEnum;
use eyre::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The kind of an audit finding. Single source of truth for the finding
/// classes — `AuditFinding` (data-carrying) and the CLI parser both project
/// off this enum. Wire names are kebab-case (`mistype`, `orphan-replace`,
/// `github-creator-missing`, etc.); the CLI parses any case via `ignore_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum FindingKind {
    /// `type:` frontmatter doesn't match the URL's classifier output.
    /// Fix: edit the frontmatter `type:` field.
    Mistype,
    /// `🔄` ledger row exists but no corresponding `✅` row followed.
    /// Fix: drop the `🔄` row from the ledger.
    OrphanReplace,
    /// Note title matches the blocked-content heuristic (e.g. Cloudflare
    /// interstitial). Fix: `rkvr rmrf` the note + drop its `✅` row.
    Blocked,
    /// Note title is the literal source URL (title extraction failed).
    /// Fix: `rkvr rmrf` the note + drop its `✅` row.
    RawTitle,
    /// Multiple notes share the same `source:` value. Fix: keep newest
    /// by mtime, move the rest to `system/quarantine/<source-key>/`.
    Duplicate,
    /// A note whose `source:` is a github.com repo root has an empty
    /// `creator:`. Fix: set `creator:` to the repo owner parsed from the URL
    /// (no network). Never overwrites a non-empty creator.
    GithubCreatorMissing,
}

/// A single audit finding. Variant names mirror `FindingKind` 1:1 — the
/// data payload lives on the variants because Rust can't have a single
/// enum that's both a unit-variant `ValueEnum` (for CLI parsing) and a
/// data-carrying variant enum. Use `kind()` to project to `FindingKind`.
#[derive(Debug)]
pub enum AuditFinding {
    Mistype {
        source: String,
        current_type: String,
        expected_type: String,
        note_path: Option<PathBuf>,
    },
    OrphanReplace {
        source: String,
        replaced_date: String,
    },
    Blocked {
        source: String,
        title: String,
        note_path: Option<PathBuf>,
    },
    RawTitle {
        source: String,
        title: String,
        note_path: Option<PathBuf>,
    },
    Duplicate {
        source: String,
        note_paths: Vec<PathBuf>,
    },
    GithubCreatorMissing {
        source: String,
        owner: String,
        note_path: PathBuf,
    },
}

impl AuditFinding {
    pub fn kind(&self) -> FindingKind {
        match self {
            AuditFinding::Mistype { .. } => FindingKind::Mistype,
            AuditFinding::OrphanReplace { .. } => FindingKind::OrphanReplace,
            AuditFinding::Blocked { .. } => FindingKind::Blocked,
            AuditFinding::RawTitle { .. } => FindingKind::RawTitle,
            AuditFinding::Duplicate { .. } => FindingKind::Duplicate,
            AuditFinding::GithubCreatorMissing { .. } => FindingKind::GithubCreatorMissing,
        }
    }
}

impl std::fmt::Display for AuditFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditFinding::Mistype {
                source,
                current_type,
                expected_type,
                ..
            } => write!(
                f,
                "[MISTYPE] {source} -> type should be: {expected_type} (currently: {current_type})"
            ),
            AuditFinding::Blocked { source, title, .. } => {
                write!(f, "[BLOCKED] {source} -> title: \"{title}\"")
            }
            AuditFinding::RawTitle { source, title, .. } => {
                write!(f, "[RAW-TITLE] {source} -> title is raw URL: \"{title}\"")
            }
            AuditFinding::Duplicate { source, note_paths } => {
                write!(f, "[DUPLICATE] {source} -> {} notes found", note_paths.len())
            }
            AuditFinding::OrphanReplace { source, replaced_date } => {
                write!(
                    f,
                    "[ORPHAN-REPLACE] {source} -> marked replaced on {replaced_date} but no replacement ✅ exists"
                )
            }
            AuditFinding::GithubCreatorMissing { source, owner, .. } => {
                write!(f, "[GITHUB-CREATOR-MISSING] {source} -> creator should be: {owner}")
            }
        }
    }
}

/// Outcome of `borg::audit::run`. Carries the structured findings plus
/// the count of fixes that were applied (only nonzero when `--fix` was
/// passed). The pre-fix display lines and per-fix progress lines are no
/// longer buffered in the report; the `--fix` path emits each fix-event
/// through the `progress` callback so sb prints live as disk I/O
/// happens (per architect Alternative 3 table, audit --fix is one of
/// the genuinely sequential I/O cases that needs callback UX).
#[derive(Debug, Default)]
pub struct AuditReport {
    pub ledger_path: PathBuf,
    pub vault_root: PathBuf,
    /// Total entries scanned; 0 when no ledger existed at start.
    pub entries_scanned: usize,
    /// `true` when the ledger file did not exist; `findings` and
    /// `entries_scanned` are empty in this case.
    pub no_ledger: bool,
    pub findings: Vec<AuditFinding>,
    /// Number of fixes applied (only nonzero when `--fix` was passed).
    pub fixed_count: usize,
}

/// Live-progress event emitted by `audit::run` during the `--fix` phase.
/// sb's caller renders each variant to the human-readable line the lib
/// used to print directly.
#[derive(Debug)]
pub enum AuditEvent {
    FixStart {
        count: usize,
    },
    Fixed {
        rel_path: PathBuf,
        expected_type: String,
    },
    FixError {
        path: PathBuf,
        error: String,
    },
    NothingFixable,
    /// One `🔄` orphan row dropped from the markdown ledger.
    RowDropped {
        source: String,
        date: String,
    },
    /// One note removed via `rkvr rmrf` for blocked / raw-title cleanup.
    NoteRemoved {
        rel_path: PathBuf,
        source: String,
    },
    /// One duplicate set quarantined: `kept` stays in place, `quarantined`
    /// were moved into `system/quarantine/<source-key>/`.
    Quarantined {
        source: String,
        kept: PathBuf,
        quarantined: Vec<PathBuf>,
    },
    /// `rkvr` was needed but missing or failed.
    RkvrUnavailable {
        path: PathBuf,
        error: String,
    },
    /// One github note's empty `creator:` backfilled with the repo owner.
    CreatorSet {
        rel_path: PathBuf,
        creator: String,
    },
}

/// Delete a vault file, preferring `rkvr rmrf` (recoverable via `rkvr rcvr`)
/// and falling back to non-recoverable removal with a WARN when rkvr is not on
/// PATH. rkvr is preferred, not required. See [`crate::rkvr`].
fn rkvr_remove(path: &Path) -> Result<()> {
    crate::rkvr::remove(&[path])
}

/// Sanitize a source identifier (URL or arbitrary string) into a path-safe
/// quarantine key. Non-alphanumeric characters become dashes; runs of dashes
/// collapse; output is capped at 80 chars so quarantine paths stay reasonable.
fn quarantine_key(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut last_was_dash = false;
    for c in source.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !out.is_empty() {
            out.push('-');
            last_was_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.truncate(80);
    if out.is_empty() { "unknown".to_string() } else { out }
}

/// Convenience: scan + optionally apply fixes in one call. sb does NOT
/// use this entry point (it needs to print the summary BEFORE the fix
/// loop streams events); use `scan` + `apply_fixes` for that.
pub fn run(config: &Config, fix: Option<&[FindingKind]>) -> Result<AuditReport> {
    let mut report = scan(config)?;
    if let Some(kinds) = fix
        && !report.no_ledger
    {
        report.fixed_count = apply_fixes(&report, kinds, |_| {});
    }
    Ok(report)
}

/// Scan the vault for audit findings. Returns the typed report with
/// `fixed_count = 0`; no disk writes occur. Run `apply_fixes` afterward
/// (with the same report) to perform the --fix work.
pub fn scan(config: &Config) -> Result<AuditReport> {
    let ledger_path = ledger::ledger_path()?;
    let vault_root = config.vault_root()?;

    if !ledger_path.exists() {
        return Ok(AuditReport {
            ledger_path,
            vault_root,
            entries_scanned: 0,
            no_ledger: true,
            findings: Vec::new(),
            fixed_count: 0,
        });
    }

    let entries = ledger::parse_completed_entries(&ledger_path)?;
    let entries_scanned = entries.len();

    // Build a map of source URL -> note paths in vault
    let note_index = build_note_index(&vault_root, &config.migration.skip_folders)?;

    let mut findings: Vec<AuditFinding> = Vec::new();

    // Check each completed ledger entry
    for entry in &entries {
        // Skip image entries (not URLs)
        if entry.source.starts_with("[image:") {
            continue;
        }

        // 1. Type misclassification
        let expected_type = reclassify_type(&entry.source);
        if let Some(paths) = note_index.get(&entry.source) {
            for path in paths {
                if let Some(current_type) = read_note_type(path)
                    && current_type != expected_type
                {
                    findings.push(AuditFinding::Mistype {
                        source: entry.source.clone(),
                        current_type,
                        expected_type: expected_type.to_string(),
                        note_path: Some(path.clone()),
                    });
                }
            }
        }

        // 2. Blocked content / Raw URL titles. The ledger no longer carries
        //    the human title, so read it from the note's frontmatter via the
        //    note_index. Notes missing from the vault simply skip this check.
        let note_path = note_index.get(&entry.source).and_then(|p| p.first()).cloned();
        if let Some(ref path) = note_path
            && let Some(note_title) = read_note_title(path)
            && let Some(reason) = quality::detect_blocked_content("", &note_title)
        {
            if reason.contains("raw URL") {
                findings.push(AuditFinding::RawTitle {
                    source: entry.source.clone(),
                    title: note_title,
                    note_path,
                });
            } else {
                findings.push(AuditFinding::Blocked {
                    source: entry.source.clone(),
                    title: note_title,
                    note_path,
                });
            }
        }
    }

    // 3. Orphaned replacements (🔄 entries with no corresponding ✅)
    {
        let content = std::fs::read_to_string(&ledger_path).context("Failed to read Borg Ledger for orphan check")?;
        let mut replaced_sources: Vec<(String, String)> = Vec::new(); // (source, date)
        let mut completed_sources: std::collections::HashSet<String> = std::collections::HashSet::new();

        for line in content.lines() {
            if !line.starts_with('|') || line.starts_with("| Date") || line.starts_with("|--") {
                continue;
            }
            let cols: Vec<&str> = line.split('|').collect();
            if cols.len() < 8 {
                continue;
            }
            let status = cols[4].trim();
            let source = if cols.len() >= 11 { cols[7].trim() } else { cols[6].trim() };

            if status == "✅" {
                completed_sources.insert(source.to_string());
            } else if status == "🔄" {
                replaced_sources.push((source.to_string(), cols[1].trim().to_string()));
            }
        }

        for (source, date) in &replaced_sources {
            if !completed_sources.contains(source) {
                findings.push(AuditFinding::OrphanReplace {
                    source: source.clone(),
                    replaced_date: date.clone(),
                });
            }
        }
    }

    // 4. Duplicate notes (multiple notes with same source URL)
    for (source, paths) in &note_index {
        if paths.len() > 1 {
            findings.push(AuditFinding::Duplicate {
                source: source.clone(),
                note_paths: paths.clone(),
            });
        }
    }

    // 5. GitHub repo-root notes missing a creator. The owner is in the source
    //    URL, so this is a pure local backfill (no network). Only repo roots
    //    (`parse_repo_url` -> Some) qualify; deep paths / pseudo-owners return
    //    None and are skipped. Notes with a non-empty `creator:` are never
    //    flagged, so a hand-set creator is never clobbered.
    for (source, paths) in &note_index {
        let Some((owner, _)) = crate::github::parse_repo_url(source) else {
            continue;
        };
        for path in paths {
            let has_creator = std::fs::read_to_string(path)
                .ok()
                .and_then(|c| extract_frontmatter_field(&c, "creator"))
                .is_some();
            if !has_creator {
                findings.push(AuditFinding::GithubCreatorMissing {
                    source: source.clone(),
                    owner: owner.clone(),
                    note_path: path.clone(),
                });
            }
        }
    }

    Ok(AuditReport {
        ledger_path,
        vault_root,
        entries_scanned,
        no_ledger: false,
        findings,
        fixed_count: 0,
    })
}

/// Apply fixes for the findings in `report`, filtered to the requested
/// `kinds`. An empty `kinds` slice means "all fixable kinds". Streams
/// `AuditEvent`s through the callback as each fix lands (per architect
/// Alternative 3, audit --fix is sequential I/O where live UX matters).
/// Returns the number of fixes successfully applied; sb writes that back
/// into `report.fixed_count`.
pub fn apply_fixes(report: &AuditReport, kinds: &[FindingKind], mut progress: impl FnMut(&AuditEvent)) -> usize {
    log::debug!(
        "audit::apply_fixes: kinds={:?} findings={} vault_root={}",
        kinds,
        report.findings.len(),
        report.vault_root.display(),
    );

    let want = |finding: &AuditFinding| -> bool { kinds.is_empty() || kinds.contains(&finding.kind()) };

    let fixable: Vec<&AuditFinding> = report.findings.iter().filter(|f| want(f)).collect();

    if fixable.is_empty() {
        progress(&AuditEvent::NothingFixable);
        return 0;
    }

    progress(&AuditEvent::FixStart { count: fixable.len() });
    let mut fixed = 0usize;
    for finding in &fixable {
        match finding {
            AuditFinding::Mistype {
                expected_type,
                note_path: Some(path),
                ..
            } => {
                fixed += apply_fix_mistype(report, path, expected_type, &mut progress);
            }
            AuditFinding::OrphanReplace { source, replaced_date } => {
                fixed += apply_fix_orphan_replace(report, source, replaced_date, &mut progress);
            }
            AuditFinding::Blocked {
                source,
                note_path: Some(path),
                ..
            }
            | AuditFinding::RawTitle {
                source,
                note_path: Some(path),
                ..
            } => {
                fixed += apply_fix_delete_and_drop(report, source, path, &mut progress);
            }
            AuditFinding::Duplicate { source, note_paths } => {
                fixed += apply_fix_duplicate(report, source, note_paths, &mut progress);
            }
            AuditFinding::GithubCreatorMissing { owner, note_path, .. } => {
                fixed += apply_fix_github_creator(report, note_path, owner, &mut progress);
            }
            _ => {}
        }
    }
    fixed
}

/// Backfill an empty `creator:` with the github repo owner. Re-reads the note
/// and sets `creator:` only when it is still empty (defensive: never clobber a
/// non-empty value written between scan and fix). The line goes after `type:`
/// to match the render's frontmatter ordering (appended if `type:` is absent);
/// an existing empty `creator:` line is replaced in place, never duplicated.
fn apply_fix_github_creator(
    report: &AuditReport,
    path: &Path,
    owner: &str,
    progress: &mut impl FnMut(&AuditEvent),
) -> usize {
    match set_creator_if_empty(path, owner) {
        Ok(true) => {
            let rel = path.strip_prefix(&report.vault_root).unwrap_or(path).to_path_buf();
            progress(&AuditEvent::CreatorSet {
                rel_path: rel,
                creator: owner.to_string(),
            });
            1
        }
        Ok(false) => 0,
        Err(e) => {
            progress(&AuditEvent::FixError {
                path: path.to_path_buf(),
                error: format!("{e:#}"),
            });
            0
        }
    }
}

/// Set `creator: "<owner>"` in a note's frontmatter, only when no non-empty
/// `creator:` is already present. Returns `Ok(true)` when the file was written,
/// `Ok(false)` when an existing creator meant no change was needed.
fn set_creator_if_empty(path: &Path, owner: &str) -> Result<bool> {
    let content = std::fs::read_to_string(path).context("Failed to read note")?;
    if extract_frontmatter_field(&content, "creator").is_some() {
        return Ok(false);
    }
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        eyre::bail!("No frontmatter found");
    }
    let after_first = &trimmed[3..];
    let end_pos = after_first
        .find("\n---")
        .ok_or_else(|| eyre::eyre!("Unclosed frontmatter"))?;
    let fm = &after_first[..end_pos];
    let body = &after_first[end_pos..];

    let creator_line = format!("creator: \"{}\"", owner.replace('"', "\\\""));
    let mut new_fm_lines: Vec<String> = Vec::new();
    let mut placed = false;
    for line in fm.lines() {
        // Drop any existing `creator:` line (we only reach here when it was
        // empty - `extract_frontmatter_field` treats `creator: ""` as absent -
        // so replacing rather than appending guarantees exactly one creator
        // line, mirroring how `fix_note_type` replaces in place).
        if line.trim().starts_with("creator:") {
            if !placed {
                new_fm_lines.push(creator_line.clone());
                placed = true;
            }
            continue;
        }
        new_fm_lines.push(line.to_string());
        if !placed && line.trim().starts_with("type:") {
            new_fm_lines.push(creator_line.clone());
            placed = true;
        }
    }
    if !placed {
        new_fm_lines.push(creator_line);
    }

    let new_content = format!("---\n{}{}", new_fm_lines.join("\n"), body);
    std::fs::write(path, new_content).context("Failed to write fixed note")?;
    Ok(true)
}

fn apply_fix_mistype(
    report: &AuditReport,
    path: &Path,
    expected_type: &str,
    progress: &mut impl FnMut(&AuditEvent),
) -> usize {
    match fix_note_type(path, expected_type) {
        Ok(()) => {
            let rel = path.strip_prefix(&report.vault_root).unwrap_or(path).to_path_buf();
            progress(&AuditEvent::Fixed {
                rel_path: rel,
                expected_type: expected_type.to_string(),
            });
            1
        }
        Err(e) => {
            progress(&AuditEvent::FixError {
                path: path.to_path_buf(),
                error: format!("{e:#}"),
            });
            0
        }
    }
}

fn apply_fix_orphan_replace(
    report: &AuditReport,
    source: &str,
    replaced_date: &str,
    progress: &mut impl FnMut(&AuditEvent),
) -> usize {
    match drop_ledger_row(&report.ledger_path, source, "\u{1F504}", Some(replaced_date)) {
        Ok(true) => {
            progress(&AuditEvent::RowDropped {
                source: source.to_string(),
                date: replaced_date.to_string(),
            });
            1
        }
        Ok(false) => 0,
        Err(e) => {
            progress(&AuditEvent::FixError {
                path: report.ledger_path.clone(),
                error: format!("{e:#}"),
            });
            0
        }
    }
}

fn apply_fix_delete_and_drop(
    report: &AuditReport,
    source: &str,
    path: &Path,
    progress: &mut impl FnMut(&AuditEvent),
) -> usize {
    if let Err(e) = rkvr_remove(path) {
        progress(&AuditEvent::RkvrUnavailable {
            path: path.to_path_buf(),
            error: format!("{e:#}"),
        });
        return 0;
    }
    let rel = path.strip_prefix(&report.vault_root).unwrap_or(path).to_path_buf();
    progress(&AuditEvent::NoteRemoved {
        rel_path: rel,
        source: source.to_string(),
    });

    match drop_ledger_row(&report.ledger_path, source, "\u{2705}", None) {
        Ok(_) => 1,
        Err(e) => {
            progress(&AuditEvent::FixError {
                path: report.ledger_path.clone(),
                error: format!("{e:#}"),
            });
            // The note is gone but the ledger drop failed; don't double-count.
            0
        }
    }
}

fn apply_fix_duplicate(
    report: &AuditReport,
    source: &str,
    note_paths: &[PathBuf],
    progress: &mut impl FnMut(&AuditEvent),
) -> usize {
    if note_paths.len() < 2 {
        return 0;
    }
    let mut with_mtime: Vec<(PathBuf, SystemTime)> = note_paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok().map(|t| (p.clone(), t)))
        .collect();
    if with_mtime.len() < 2 {
        // Need at least two readable mtimes to pick a winner; punt.
        return 0;
    }
    with_mtime.sort_by_key(|b| std::cmp::Reverse(b.1));
    let (keep, _) = with_mtime.first().cloned().expect("len >= 2 verified above");
    let losers: Vec<PathBuf> = with_mtime.into_iter().skip(1).map(|(p, _)| p).collect();

    let quarantine_root = report
        .vault_root
        .join("system")
        .join("quarantine")
        .join(quarantine_key(source));

    let mut moved: Vec<PathBuf> = Vec::with_capacity(losers.len());
    for loser in &losers {
        let rel = loser.strip_prefix(&report.vault_root).unwrap_or(loser);
        let dest = quarantine_root.join(rel);
        if let Some(parent) = dest.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            progress(&AuditEvent::FixError {
                path: dest.clone(),
                error: format!("create_dir_all failed: {e:#}"),
            });
            continue;
        }
        match std::fs::rename(loser, &dest) {
            Ok(()) => moved.push(loser.clone()),
            Err(e) => progress(&AuditEvent::FixError {
                path: loser.clone(),
                error: format!("rename to {} failed: {e:#}", dest.display()),
            }),
        }
    }

    if moved.is_empty() {
        return 0;
    }
    progress(&AuditEvent::Quarantined {
        source: source.to_string(),
        kept: keep,
        quarantined: moved,
    });
    1
}

/// Rewrite the markdown ledger, dropping rows whose status matches `status_glyph`
/// and whose source column matches `source`. If `date_filter` is `Some`, only
/// drop rows whose date column matches as well (used for orphan-replace to
/// avoid removing unrelated rows that happen to share a source). Returns
/// whether any rows were dropped.
fn drop_ledger_row(ledger_path: &Path, source: &str, status_glyph: &str, date_filter: Option<&str>) -> Result<bool> {
    use fs2::FileExt;
    use std::fs::OpenOptions;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(ledger_path)
        .context("Failed to open Borg Ledger for write")?;
    file.lock_exclusive()
        .context("Failed to acquire exclusive lock on Borg Ledger")?;

    let content = std::fs::read_to_string(ledger_path).context("Failed to read Borg Ledger")?;
    let mut out_lines: Vec<&str> = Vec::with_capacity(content.lines().count());
    let mut dropped = false;
    for line in content.lines() {
        if !line.starts_with('|') || line.starts_with("| Date") || line.starts_with("|--") {
            out_lines.push(line);
            continue;
        }
        let cols: Vec<&str> = line.split('|').collect();
        if cols.len() < 8 {
            out_lines.push(line);
            continue;
        }
        let status = cols[4].trim();
        let row_source = if cols.len() >= 11 { cols[7].trim() } else { cols[6].trim() };
        let row_date = cols[1].trim();
        let date_ok = match date_filter {
            Some(d) => row_date == d,
            None => true,
        };
        if status == status_glyph && row_source == source && date_ok {
            dropped = true;
            continue;
        }
        out_lines.push(line);
    }

    if dropped {
        let new_content = format!("{}\n", out_lines.join("\n"));
        std::fs::write(ledger_path, new_content).context("Failed to write Borg Ledger")?;
    }
    file.unlock().ok();
    Ok(dropped)
}

/// Build an index mapping source URL -> list of note file paths in the vault.
///
/// Parallel over the `.md` file list via rayon. `par_iter().filter_map().collect::<Vec<_>>()`
/// preserves the input-slice order (which `collect_md_files` already sorts), so when we fold
/// the `(source, path)` pairs into the final `HashMap<String, Vec<PathBuf>>` sequentially on
/// the main thread the per-source `Vec<PathBuf>` ordering is identical to the previous
/// sequential implementation.
fn build_note_index(vault_root: &Path, skip_folders: &[String]) -> Result<HashMap<String, Vec<PathBuf>>> {
    use rayon::prelude::*;

    log::debug!(
        "audit::build_note_index: vault_root={} skip_folders={:?}",
        vault_root.display(),
        skip_folders
    );
    let md_files = collect_md_files(vault_root, skip_folders)?;

    let pairs: Vec<(String, PathBuf)> = md_files
        .par_iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(path).ok()?;
            let source = extract_frontmatter_field(&content, "source")?;
            Some((source, path.clone()))
        })
        .collect();

    let mut index: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for (source, path) in pairs {
        index.entry(source).or_default().push(path);
    }
    log::debug!("audit::build_note_index: indexed {} source url(s)", index.len());

    Ok(index)
}

/// Read the `type:` field from a note's frontmatter.
fn read_note_type(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    extract_frontmatter_field(&content, "type")
}

/// Read the `title:` field from a note's frontmatter.
fn read_note_title(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    extract_frontmatter_field(&content, "title")
}

/// Extract a simple string field from YAML frontmatter.
fn extract_frontmatter_field(content: &str, field: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_first = &trimmed[3..];
    let end_pos = after_first.find("\n---")?;
    let fm = &after_first[..end_pos];

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

/// Fix the `type:` field in a note's frontmatter.
fn fix_note_type(path: &Path, new_type: &str) -> Result<()> {
    let content = std::fs::read_to_string(path).context("Failed to read note")?;
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        eyre::bail!("No frontmatter found");
    }
    let after_first = &trimmed[3..];
    let end_pos = after_first
        .find("\n---")
        .ok_or_else(|| eyre::eyre!("Unclosed frontmatter"))?;

    let fm = &after_first[..end_pos];
    let body = &after_first[end_pos..];

    // Replace the type field in frontmatter
    let mut new_fm_lines: Vec<String> = Vec::new();
    let mut found_type = false;
    for line in fm.lines() {
        if line.trim().starts_with("type:") {
            new_fm_lines.push(format!("type: {new_type}"));
            found_type = true;
        } else {
            new_fm_lines.push(line.to_string());
        }
    }
    if !found_type {
        // Add type field if not present
        new_fm_lines.push(format!("type: {new_type}"));
    }

    let new_content = format!("---\n{}{}", new_fm_lines.join("\n"), body);
    std::fs::write(path, new_content).context("Failed to write fixed note")?;
    Ok(())
}

fn collect_md_files(root: &Path, skip_folders: &[String]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_md_recursive(root, root, skip_folders, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_md_recursive(current: &Path, root: &Path, skip_folders: &[String], files: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(current).context(format!("Failed to read dir: {}", current.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string();
            if skip_folders.iter().any(|s| rel.starts_with(s)) {
                continue;
            }
            collect_md_recursive(&path, root, skip_folders, files)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
