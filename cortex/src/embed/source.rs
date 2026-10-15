//! Where a staging-sourced (Video/Youtube/Article) note's verbatim transcript
//! is read from: borg's durable source archive first, then borg's swept
//! staging root (2026-10-14-note-sources-intent-tools-channels), plus the
//! examined-sentinel revival that lets an archive backfill bring back notes
//! skipped for a missing source.

use std::path::Path;
use std::str::FromStr;

use eyre::Result;
use vault::distilled::{Distilled, FILENAME as DISTILLED_FILENAME};
use vault::schema::NoteType;
use vault::search::{EmbeddingKind, SearchIndex};

use crate::config::Config;

/// Where a Video/Youtube/Article transcript can live, in read order: borg's
/// durable source archive, then borg's swept staging root. Both are read
/// strictly read-only; borg is the sole writer of each.
#[derive(Debug, Clone, Copy)]
pub struct SourceRoots<'a> {
    pub archive: &'a Path,
    pub staging: &'a Path,
}

impl<'a> SourceRoots<'a> {
    pub fn of(config: &'a Config) -> Self {
        Self {
            archive: &config.archive_root,
            staging: &config.staging_root,
        }
    }
}

/// Resolve a Video/Youtube/Article note's verbatim transcript: the archived
/// `<archive>/<trace>/distilled.yml` first, then the staged
/// `<staging>/<trace>/distilled.yml` (2026-10-14-note-sources-intent-tools-channels).
/// The archive outlives the staging sweep, so a trace whose staging dir is
/// gone still embeds. `None` (and the caller's examined sentinel) only when
/// neither yields transcript text.
pub(super) fn read_source_transcript(roots: SourceRoots<'_>, trace: &str, note_path: &str) -> Option<String> {
    log::debug!(
        "cortex::embed::read_source_transcript: note={note_path} trace={trace} archive={} staging={}",
        roots.archive.display(),
        roots.staging.display(),
    );
    if trace.is_empty() {
        log::warn!("cortex::embed: {note_path} has no trace; cannot resolve its source transcript");
        return None;
    }
    let archived = vault::trace::dir(roots.archive, trace).map(|dir| dir.join(DISTILLED_FILENAME));
    if let Some(path) = archived.filter(|p| p.is_file())
        && let Some(t) = read_distilled_transcript(&path, note_path)
    {
        log::debug!("cortex::embed: {note_path} transcript resolved from the archive");
        return Some(t);
    }
    read_staged_transcript(roots.staging, trace, note_path)
}

/// Resolve a Video/Youtube/Article note's verbatim transcript from borg's
/// staged `distilled.yml` at `<staging_root>/<trace>/distilled.yml`, reading
/// the `transcript` field. Cortex reads borg-owned staging strictly read-only
/// (precedented: oracle reads borg's receipts DB read-only); borg remains the
/// sole staging writer. The fallback behind the archive
/// ([`read_source_transcript`]).
///
/// Returns `None` — and the caller records the examined sentinel — on every
/// failure mode, all fail-closed (never a silent empty embed, never an error
/// that spins the loop):
/// - empty `trace` (note predates the trace column, or was never staged)
/// - the staged file is missing or expired (swept by retention)
/// - the staged file is unreadable or malformed YAML
/// - `transcript` is absent/empty (e.g. the article source-quality gate cleared it)
///
/// A sentinel skip holds until the note's indexed `modified_at` advances, the
/// model version bumps, or an archive copy appears after the sentinel
/// ([`revive_archived_sentinels`]).
fn read_staged_transcript(staging_root: &Path, trace: &str, note_path: &str) -> Option<String> {
    log::debug!(
        "cortex::embed::read_staged_transcript: note={note_path} trace={trace} staging_root={}",
        staging_root.display(),
    );
    if trace.is_empty() {
        log::warn!("cortex::embed: {note_path} has no trace; cannot resolve staged transcript");
        return None;
    }
    read_distilled_transcript(&staging_root.join(trace).join(DISTILLED_FILENAME), note_path)
}

/// The `transcript` field of one `distilled.yml`, or `None` with a WARN when
/// the file is unreadable, malformed, or carries no transcript text.
fn read_distilled_transcript(path: &Path, note_path: &str) -> Option<String> {
    let yaml = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            // Missing (never staged / swept by retention) or otherwise unreadable.
            log::warn!(
                "cortex::embed: {note_path} distilled.yml unreadable at {}: {e}",
                path.display(),
            );
            return None;
        }
    };
    let distilled: Distilled = match serde_yaml::from_str(&yaml) {
        Ok(d) => d,
        Err(e) => {
            // Fail closed on malformed YAML: skip, never a partial embed.
            log::warn!(
                "cortex::embed: {note_path} distilled.yml at {} is malformed: {e}",
                path.display(),
            );
            return None;
        }
    };
    match distilled.transcript {
        Some(t) if !t.trim().is_empty() => Some(t),
        _ => {
            log::warn!(
                "cortex::embed: {note_path} distilled.yml at {} has no transcript text",
                path.display(),
            );
            None
        }
    }
}

/// Mtime (unix seconds, the unit of `notes.modified_at`) of a trace's archived
/// `distilled.yml`, or `None` when it is absent or unreadable.
fn archive_mtime_secs(archive_root: &Path, trace: &str) -> Option<i64> {
    let path = vault::trace::dir(archive_root, trace)?.join(DISTILLED_FILENAME);
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    i64::try_from(secs).ok()
}

/// The examined-sentinel watermark for a staging-sourced note found to have no
/// transcript: the note's indexed `modified_at`, raised to its archive's mtime
/// when the archive exists. Raising it is what keeps
/// [`revive_archived_sentinels`] from reviving the same note every tick when
/// its archive exists but carries no transcript (an article whose transcript
/// the source gate cleared): only an archive written AFTER this watermark
/// revives it. The stale query still excludes the note, since the watermark is
/// at or past its `modified_at`.
pub(super) fn sentinel_watermark(note_modified_at: i64, archive_root: &Path, trace: &str) -> i64 {
    match archive_mtime_secs(archive_root, trace) {
        Some(mtime) => note_modified_at.max(mtime),
        None => note_modified_at,
    }
}

/// Clear the transcript-chunk examined sentinel of every staging-sourced note
/// whose archived `distilled.yml` is newer than its watermark, so the next
/// stale read selects it again. This is how an archive backfill revives notes
/// skipped for a missing source. Returns how many were revived.
pub(super) fn revive_archived_sentinels(
    index: &mut SearchIndex,
    model_version: &str,
    archive_root: &Path,
) -> Result<usize> {
    let held = index.sentinelled_notes(EmbeddingKind::TranscriptChunk, model_version)?;
    let revive: Vec<String> = held
        .into_iter()
        .filter(|n| {
            NoteType::from_str(&n.note_type)
                .map(|nt| nt.transcript_from_staging())
                .unwrap_or(false)
        })
        .filter(|n| archive_mtime_secs(archive_root, &n.trace).is_some_and(|mtime| mtime > n.examined_at))
        .map(|n| n.note_path)
        .collect();
    if !revive.is_empty() {
        log::info!(
            "cortex::embed: reviving {} transcript sentinel(s) whose source archive appeared since they were skipped",
            revive.len()
        );
        index.clear_embedding_examined_batch(EmbeddingKind::TranscriptChunk, model_version, &revive)?;
    }
    Ok(revive.len())
}
