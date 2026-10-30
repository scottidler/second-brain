//! Where a staging-sourced (Video/Youtube/Article) note's verbatim transcript
//! is read from: borg's durable source archive first, then borg's swept
//! staging root (2026-10-14-note-sources-intent-tools-channels), plus the
//! examined-sentinel revival that lets an archive backfill bring back notes
//! skipped for a missing source.

use std::path::Path;
use std::str::FromStr;

use eyre::{Result, eyre};
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
/// gone still embeds.
///
/// `Ok(None)` means the system can no longer produce a transcript (no trace,
/// both copies missing, or no transcript text): the caller sentinels the note
/// and deletes its old rows. `Err` means a copy exists but cannot be read or
/// parsed and no other copy yielded text: the caller keeps the old rows and
/// retries next tick, because a broken file is not proof the source is gone.
pub(super) fn read_source_transcript(roots: SourceRoots<'_>, trace: &str, note_path: &str) -> Result<Option<String>> {
    log::debug!(
        "cortex::embed::read_source_transcript: note={note_path} trace={trace} archive={} staging={}",
        roots.archive.display(),
        roots.staging.display(),
    );
    if trace.is_empty() {
        log::warn!("cortex::embed: {note_path} has no trace; cannot resolve its source transcript");
        return Ok(None);
    }
    let archived = match vault::trace::dir(roots.archive, trace) {
        Some(dir) => read_distilled_transcript(&dir.join(DISTILLED_FILENAME), note_path),
        None => Ok(None),
    };
    if let Ok(Some(t)) = archived {
        log::debug!("cortex::embed: {note_path} transcript resolved from the archive");
        return Ok(Some(t));
    }
    let staged = read_staged_transcript(roots.staging, trace, note_path)?;
    match (staged, archived) {
        (Some(t), Err(e)) => {
            log::warn!("cortex::embed: {note_path} archive copy unusable, staged copy used instead: {e}");
            Ok(Some(t))
        }
        (Some(t), Ok(_)) => Ok(Some(t)),
        (None, Err(e)) => Err(e),
        (None, Ok(_)) => Ok(None),
    }
}

/// Resolve a Video/Youtube/Article note's verbatim transcript from borg's
/// staged `distilled.yml` at `<staging_root>/<trace>/distilled.yml`, reading
/// the `transcript` field. Cortex reads borg-owned staging strictly read-only
/// (precedented: oracle reads borg's receipts DB read-only); borg remains the
/// sole staging writer. The fallback behind the archive
/// ([`read_source_transcript`]).
///
/// `Ok(None)` — and the caller's examined sentinel plus row delete — when:
/// - `trace` is empty (note predates the trace column, or was never staged)
/// - the staged file is missing or expired (swept by retention)
/// - `transcript` is absent/empty (e.g. the article source-quality gate cleared it)
///
/// `Err` when the staged file exists but is unreadable or malformed YAML.
///
/// A sentinel skip holds until the note's indexed `modified_at` advances, the
/// model version bumps, or an archive copy appears after the sentinel
/// ([`revive_archived_sentinels`]).
fn read_staged_transcript(staging_root: &Path, trace: &str, note_path: &str) -> Result<Option<String>> {
    log::debug!(
        "cortex::embed::read_staged_transcript: note={note_path} trace={trace} staging_root={}",
        staging_root.display(),
    );
    if trace.is_empty() {
        log::warn!("cortex::embed: {note_path} has no trace; cannot resolve staged transcript");
        return Ok(None);
    }
    read_distilled_transcript(&staging_root.join(trace).join(DISTILLED_FILENAME), note_path)
}

/// The `transcript` field of one `distilled.yml`. `Ok(None)` with a WARN when
/// the file does not exist or carries no transcript text; `Err` when it exists
/// but cannot be read or does not parse as `Distilled`.
fn read_distilled_transcript(path: &Path, note_path: &str) -> Result<Option<String>> {
    let yaml = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::warn!("cortex::embed: {note_path} has no distilled.yml at {}", path.display());
            return Ok(None);
        }
        Err(e) => {
            return Err(eyre!("{note_path} distilled.yml unreadable at {}: {e}", path.display()));
        }
    };
    let distilled: Distilled = serde_yaml::from_str(&yaml)
        .map_err(|e| eyre!("{note_path} distilled.yml at {} is malformed: {e}", path.display()))?;
    match distilled.transcript {
        Some(t) if !t.trim().is_empty() => Ok(Some(t)),
        _ => {
            log::warn!(
                "cortex::embed: {note_path} distilled.yml at {} has no transcript text",
                path.display(),
            );
            Ok(None)
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
