//! Archive backfill (`sb borg retention archive`): copy every staged trace's
//! source artifacts into the durable archive when the trace has no archive yet.
//!
//! The write goes through [`SourceArchive::write`], the same code the live
//! publish seam uses, so the backfill cannot drift from the per-kind rules in
//! `archive.rs`. A trace that already has an archive (`is_durable`) is skipped,
//! never rewritten. Existing video transcripts are archived as staged,
//! duplicated VTT text included: the dedupe fixes new ingests only.

use eyre::{Context, Result};
use std::path::Path;
use vault::distilled::Distilled;

use super::{DISTILLED_FILENAME, SOURCE_FILENAME, SourceArchive, is_durable};
use crate::config::Config;

const REPO_EXTRACTOR: &str = "distill-repo-v1";
const THREAD_EXTRACTOR: &str = "distill-thread-v1";
const SESSION_EXTRACTOR: &str = "distill-session-v1";
const STAGED_ENVELOPE: &str = "fetched.html";

#[derive(Debug, Clone, Default)]
pub struct BackfillReport {
    /// Staged trace dirs holding a `distilled.yml`.
    pub scanned: usize,
    /// Traces archived (`apply`) or that would be (dry run).
    pub to_copy: Vec<String>,
    pub already_archived: usize,
    /// Session traces: clyde keeps the durable copy.
    pub skipped_session: usize,
    /// `(trace, reason)` for traces that could not be read or written.
    pub failed: Vec<(String, String)>,
}

/// Archive every staged trace that has no archive yet. `apply=false` writes
/// nothing and reports what would be copied.
pub fn run(config: &Config, apply: bool) -> Result<BackfillReport> {
    log::debug!(
        "archive::backfill::run: staging={} archive={} apply={apply}",
        config.staging.root.display(),
        config.archive.root.display()
    );
    let staging = &config.staging.root;
    let mut report = BackfillReport::default();
    if !staging.is_dir() {
        return Ok(report);
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(staging).with_context(|| format!("read_dir {}", staging.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    names.sort();
    for trace in names {
        let dir = staging.join(&trace);
        if !dir.join(DISTILLED_FILENAME).is_file() {
            continue;
        }
        report.scanned += 1;
        if is_durable(&config.archive.root, &trace) {
            report.already_archived += 1;
            continue;
        }
        match archive_one(&config.archive.root, &trace, &dir, apply) {
            Ok(Outcome::Copied) => report.to_copy.push(trace),
            Ok(Outcome::Session) => report.skipped_session += 1,
            Err(err) => {
                log::warn!("[{trace}] archive backfill failed: {err:#}");
                report.failed.push((trace, format!("{err:#}")));
            }
        }
    }
    log::info!(
        "archive backfill: scanned={} to_copy={} already={} session={} failed={} apply={apply}",
        report.scanned,
        report.to_copy.len(),
        report.already_archived,
        report.skipped_session,
        report.failed.len()
    );
    Ok(report)
}

enum Outcome {
    Copied,
    Session,
}

fn archive_one(archive_root: &Path, trace: &str, staged: &Path, apply: bool) -> Result<Outcome> {
    let yaml = std::fs::read_to_string(staged.join(DISTILLED_FILENAME)).context("read staged distilled.yml")?;
    let distilled: Distilled = serde_yaml::from_str(&yaml).context("parse staged distilled.yml")?;
    let extractor = distilled.meta.extractor.as_str();
    if extractor == SESSION_EXTRACTOR {
        return Ok(Outcome::Session);
    }
    let source_md = staged_source_md(extractor, staged)?;
    let archive = SourceArchive {
        root: archive_root,
        trace_id: trace,
        distilled: &distilled,
        source_md: source_md.as_deref(),
    };
    if apply {
        archive.write()?;
    } else {
        super::trace_dir(archive_root, trace)?;
    }
    Ok(Outcome::Copied)
}

/// The source the distiller saw for the kinds whose `distilled.yml` does not
/// carry it. Repo: staged `transcript.md`, else the README decoded from the
/// Stage-0 envelope. Thread: staged `transcript.md`. Others: `None`.
fn staged_source_md(extractor: &str, staged: &Path) -> Result<Option<String>> {
    let staged_md = staged.join(SOURCE_FILENAME);
    match extractor {
        REPO_EXTRACTOR | THREAD_EXTRACTOR => {
            if staged_md.is_file() {
                return Ok(Some(
                    std::fs::read_to_string(&staged_md).context("read staged transcript.md")?,
                ));
            }
            if extractor == REPO_EXTRACTOR {
                let envelope = staged.join(STAGED_ENVELOPE);
                if envelope.is_file() {
                    let bytes = std::fs::read(&envelope).context("read staged fetched.html")?;
                    return Ok(crate::github::readme_from_stage0(&bytes).ok());
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests;
