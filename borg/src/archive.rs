//! Durable per-trace source archive (2026-10-14-note-sources-intent-tools-channels,
//! Part 1).
//!
//! Staging is swept on a 14/21-day window and the sweep deletes the whole
//! trace dir, so the verbatim source a note was distilled from expires. The
//! archive is the copy that does not: `<root>/<trace>/distilled.yml` plus the
//! source the distiller saw when `distilled.yml` does not already carry it.
//! Per kind (the doc's Resolved Decision on archive contents):
//!
//! - video, article: `distilled.yml` alone; its `transcript` IS the source.
//! - repo: `distilled.yml` + `transcript.md`, the rendered README transcript.
//! - thread: `distilled.yml` + `transcript.md`, the thread markdown.
//! - session: never archived; clyde keeps a durable copy of every session.
//!
//! borg is the sole writer, and it writes BEFORE the vault note,
//! independent of `staging.enabled`. A failed write is fatal to the trace
//! (`PublishError::ArchiveWrite` -> `publish-failed`); staged writes keep
//! their WARN-and-continue behavior. Nothing here prunes: a reingest gets a
//! new trace id and so a new archive dir.

use std::path::{Path, PathBuf};
use vault::distilled::Distilled;

/// The structured payload, named identically to the staged copy so one
/// reader parses either.
pub const DISTILLED_FILENAME: &str = crate::stages::distill::DISTILLED_FILENAME;

/// The source text the distiller saw, when `distilled.yml` does not carry it.
/// Named like staging's Stage-1 `transcript.md`, so a staged copy and an
/// archived copy of one trace hold the same file under the same name.
pub const SOURCE_FILENAME: &str = "transcript.md";

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// A trace id that is not one plain path component (empty, `.`, `..`, or
    /// containing a separator) would write outside `<root>/<trace>/`.
    #[error("trace id {0:?} is not a single plain path component")]
    InvalidTraceId(String),
    #[error("serialize {DISTILLED_FILENAME} for trace {trace_id}: {source}")]
    Serialize {
        trace_id: String,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("archive write {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// One trace's archive, ready to write. `source_md` is the source text for the
/// kinds whose `distilled.yml` does not carry it (repo README, thread
/// markdown); `None` for video and article.
#[derive(Debug)]
pub struct SourceArchive<'a> {
    pub root: &'a Path,
    pub trace_id: &'a str,
    pub distilled: &'a Distilled,
    pub source_md: Option<&'a str>,
}

impl SourceArchive<'_> {
    /// Write the archive dir and return it. `transcript.md` lands first and
    /// `distilled.yml` last, each temp-then-rename, so a present
    /// `distilled.yml` means a complete archive ([`is_durable`]).
    pub fn write(&self) -> Result<PathBuf, ArchiveError> {
        log::debug!(
            "archive::write: trace={} root={} has_source_md={}",
            self.trace_id,
            self.root.display(),
            self.source_md.is_some()
        );
        let dir = trace_dir(self.root, self.trace_id)?;
        std::fs::create_dir_all(&dir).map_err(|source| ArchiveError::Io {
            path: dir.clone(),
            source,
        })?;
        if let Some(source_md) = self.source_md {
            write_atomic(&dir.join(SOURCE_FILENAME), source_md.as_bytes())?;
        }
        let yaml = serde_yaml::to_string(self.distilled).map_err(|source| ArchiveError::Serialize {
            trace_id: self.trace_id.to_string(),
            source,
        })?;
        write_atomic(&dir.join(DISTILLED_FILENAME), yaml.as_bytes())?;
        log::info!(
            "[{}] archived source to {} (distilled_bytes={} source_md_bytes={})",
            self.trace_id,
            dir.display(),
            yaml.len(),
            self.source_md.map_or(0, str::len)
        );
        Ok(dir)
    }
}

/// `<root>/<trace_id>`, or `InvalidTraceId` when `trace_id` is not exactly one
/// plain path component (`vault::trace::dir`). Trace ids reach here from the
/// `/trace/{id}` route too, so a `..` must never resolve.
pub fn trace_dir(root: &Path, trace_id: &str) -> Result<PathBuf, ArchiveError> {
    vault::trace::dir(root, trace_id).ok_or_else(|| ArchiveError::InvalidTraceId(trace_id.to_string()))
}

/// Whether `trace_id` has a complete archive under `root`: its
/// `distilled.yml` exists. An invalid trace id is never durable.
pub fn is_durable(root: &Path, trace_id: &str) -> bool {
    let durable = trace_dir(root, trace_id)
        .map(|dir| dir.join(DISTILLED_FILENAME).is_file())
        .unwrap_or(false);
    log::debug!(
        "archive::is_durable: trace={trace_id} root={} durable={durable}",
        root.display()
    );
    durable
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ArchiveError> {
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    std::fs::write(&tmp, bytes).map_err(|source| ArchiveError::Io {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, path).map_err(|source| ArchiveError::Io {
        path: path.to_path_buf(),
        source,
    })
}

pub mod backfill;

#[cfg(test)]
mod tests;
