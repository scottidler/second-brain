//! One-shot backfill sweep: strip the `## Transcript`-to-EOF section from
//! Video/Article notes ingested on or after [`CUTOFF_RFC3339`].
//!
//! Phase 6 of `docs/design/2026-07-07-distillation-output-restore.md`. Housed
//! under `bin/` (NOT a permanent `sb` subcommand), beside `bin/migrate-receipts`
//! -- one-shot surgery does not earn a forever spot on the CLI surface.
//!
//! The date scope IS the safety guard, not a heading heuristic: every note
//! produced by the legacy `cortex summarize --backfill` path carries a
//! pre-cutoff `ingested` (backfill rewrites the body but never `ingested`),
//! so the April baseline and every other legacy-body note is out of scope by
//! construction. A note missing `ingested` entirely, or carrying an
//! unparsable value, is refused rather than guessed at (fail closed).

#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]
// Core logic returns typed data; the bin's `main.rs` owns stdout.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use std::path::{Path, PathBuf};

use vault::config::ScanConfig;
use vault::note::{self, Note};
use vault::schema::NoteType;

/// Backfill date scope (Resolved Decisions: "backfill scope is date-scoped").
/// Notes ingested before this instant are protected legacy bodies; the sweep
/// refuses them by construction rather than sniffing the body for a
/// demoted-heading heuristic (panel finding: the heuristic both misses and
/// false-refuses real article transcripts that legitimately contain demoted
/// headings).
pub const CUTOFF_RFC3339: &str = "2026-06-28T00:00:00Z";

/// The section this sweep removes. Render always emits it last for
/// Video/Article/Youtube publishes minted before Phase 3 of this design
/// landed -- there is no "preserve the footer" case in the distilled shape,
/// so strip-to-EOF is exact.
const TRANSCRIPT_HEADING: &str = "## Transcript";

/// Why a candidate note was left untouched, or that it was stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    Stripped,
    Refused(String),
}

impl Disposition {
    pub fn is_stripped(&self) -> bool {
        matches!(self, Disposition::Stripped)
    }
}

/// One examined note and what happened to it.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub path: PathBuf,
    pub disposition: Disposition,
}

/// Result of a full sweep: every Video/Article/Youtube note the sweep
/// examined, in path order. Non-video/article kinds never appear here -- they
/// are ignored, not refused.
#[derive(Debug, Default)]
pub struct Report {
    pub outcomes: Vec<Outcome>,
}

impl Report {
    pub fn stripped(&self) -> usize {
        self.outcomes.iter().filter(|o| o.disposition.is_stripped()).count()
    }

    pub fn refused(&self) -> usize {
        self.outcomes.len() - self.stripped()
    }
}

fn cutoff() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(CUTOFF_RFC3339)
        .expect("CUTOFF_RFC3339 is a valid RFC3339 literal")
        .with_timezone(&Utc)
}

/// Video+Article scope: [`NoteType::transcript_from_staging`] is the exact
/// set (Youtube/Video/Article) whose transcript moved out of the note body in
/// this design -- the same seam Phase 5's embed re-point keys on. Imported,
/// never hardcoded, so this sweep can't drift from the schema.
fn in_scope_kind(note: &Note) -> bool {
    note.frontmatter
        .note_type
        .as_deref()
        .and_then(|s| s.parse::<NoteType>().ok())
        .is_some_and(|nt| nt.transcript_from_staging())
}

/// Locate the byte offset where a `## Transcript` heading LINE begins, if
/// present. Walks `split_inclusive('\n')` and accumulates exact segment
/// lengths rather than computing an offset by character count -- every
/// returned offset is therefore a str-safe char boundary by construction
/// (transcripts routinely contain multibyte text: em-dashes, accented names,
/// non-Latin scripts).
fn find_transcript_start(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim() == TRANSCRIPT_HEADING {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// Decide the fate of one in-scope note. Pure: no I/O, no writes.
fn classify(note: &Note, cutoff: DateTime<Utc>) -> Disposition {
    log::trace!("strip_transcripts::classify: path={}", note.path.display());
    let Some(ingested) = note.frontmatter.ingested.as_deref() else {
        return Disposition::Refused("missing ingested".to_string());
    };
    let parsed = match DateTime::parse_from_rfc3339(ingested.trim()) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(e) => return Disposition::Refused(format!("unparsable ingested {ingested:?}: {e}")),
    };
    if parsed < cutoff {
        return Disposition::Refused(format!("pre-cutoff ingested={ingested} (legacy body protected)"));
    }
    if find_transcript_start(&note.raw).is_none() {
        return Disposition::Refused("no ## Transcript section found".to_string());
    }
    Disposition::Stripped
}

/// Refuse to proceed against a dirty vault worktree. Git is the rollback for
/// this sweep only when the tree starts clean.
pub fn ensure_clean_worktree(vault_root: &Path) -> Result<()> {
    log::debug!(
        "strip_transcripts::ensure_clean_worktree: vault_root={}",
        vault_root.display()
    );
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(vault_root)
        .arg("status")
        .arg("--porcelain")
        .output()
        .with_context(|| format!("failed to run `git status --porcelain` in {}", vault_root.display()))?;
    if !output.status.success() {
        eyre::bail!(
            "git status failed in {} (exit {}): {}",
            vault_root.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if !output.stdout.is_empty() {
        eyre::bail!(
            "refusing to run: vault worktree at {} is dirty -- git is the rollback only when the tree \
             starts clean. Commit or stash pending changes first:\n{}",
            vault_root.display(),
            String::from_utf8_lossy(&output.stdout)
        );
    }
    log::debug!("strip_transcripts::ensure_clean_worktree: clean");
    Ok(())
}

/// Run the sweep: scan the vault, classify every Video/Article/Youtube note,
/// atomically strip the ones in scope, and return the full manifest. Caller
/// (`main`) is responsible for the dirty-worktree guard via
/// [`ensure_clean_worktree`] -- kept separate so tests can exercise
/// classification against a plain temp dir with no git repo at all.
pub fn run(vault_root: &Path) -> Result<Report> {
    log::debug!("strip_transcripts::run: vault_root={}", vault_root.display());
    let notes = note::scan_vault(vault_root, &ScanConfig::default())?;
    let cutoff = cutoff();
    let mut outcomes = Vec::with_capacity(notes.len());
    for candidate in notes.iter().filter(|n| in_scope_kind(n)) {
        let disposition = classify(candidate, cutoff);
        let disposition = if disposition.is_stripped() {
            match find_transcript_start(&candidate.raw) {
                Some(idx) => {
                    let new_raw = &candidate.raw[..idx];
                    let absolute = vault_root.join(&candidate.path);
                    note::write_atomic(&absolute, new_raw.as_bytes())
                        .with_context(|| format!("write_atomic failed for {}", absolute.display()))?;
                    log::info!("strip_transcripts::run: stripped {}", candidate.path.display());
                    disposition
                }
                None => {
                    // classify() already proved this Some(); unreachable in
                    // practice, but fail closed rather than panic if it ever
                    // diverges from find_transcript_start.
                    Disposition::Refused("transcript heading vanished between classify and strip".to_string())
                }
            }
        } else {
            disposition
        };
        outcomes.push(Outcome {
            path: candidate.path.clone(),
            disposition,
        });
    }
    outcomes.sort_by(|a, b| a.path.cmp(&b.path));
    log::debug!(
        "strip_transcripts::run: examined={} stripped={} refused={}",
        outcomes.len(),
        outcomes.iter().filter(|o| o.disposition.is_stripped()).count(),
        outcomes.iter().filter(|o| !o.disposition.is_stripped()).count()
    );
    Ok(Report { outcomes })
}

#[cfg(test)]
mod tests;
