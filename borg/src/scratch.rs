//! The scratch tree a `sb borg distill` run leaves on the daemon host:
//! `vault::paths::borg_distill_dir()/<trace>/`, holding the rendered note, its
//! attachments, the frames, the run's staging and its raw-input sidecar
//! (design: docs/design/2026-10-25-borg-distill.md).
//!
//! `GET /distill/{trace}` lists it and `GET /distill/{trace}/file/{*path}`
//! serves one file of it. Both read it through [`regular_files`], a fresh
//! `symlink_metadata` walk that yields regular files only, so a symlink or a
//! special file in the tree is never listed and never served, and a request
//! path is checked by [`requested_path`] before any filesystem call.

use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// `POST /distill` body. URL-only: distill refuses every other input kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistillRequest {
    pub url: String,
}

/// `GET /distill/{trace}` body: the trace's terminal receipts state and every
/// regular file under its scratch root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct DistillManifest {
    pub trace_id: String,
    /// `succeeded` or `failed`, from receipts.
    pub status: String,
    pub failure_stage: Option<String>,
    pub failure_reason: Option<String>,
    pub files: Vec<ManifestFile>,
}

/// One regular file under a scratch root. `path` is relative to the root,
/// `/`-separated, and carries no `..` or root component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    pub path: String,
    pub size: u64,
}

/// Why a requested file path was refused before any filesystem call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRefusal {
    /// The path climbs out with a `..` component.
    ParentDir,
    /// The path is absolute (a root or prefix component).
    Absolute,
}

impl std::fmt::Display for PathRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathRefusal::ParentDir => write!(f, "path has a `..` component"),
            PathRefusal::Absolute => write!(f, "path is absolute"),
        }
    }
}

/// The scratch root of `trace_id` under `distill_dir`, or `None` when the id
/// is not a single plain path component (`..`, `a/b`, empty), so a trace id
/// can never name a directory outside `distill_dir`.
pub fn run_root(distill_dir: &Path, trace_id: &str) -> Option<PathBuf> {
    let mut components = Path::new(trace_id).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) if name == std::ffi::OsStr::new(trace_id) => Some(distill_dir.join(name)),
        _ => None,
    }
}

/// Check a requested file path, relative to a scratch root, without touching
/// the filesystem. Any `..` component or an absolute path is refused; `.`
/// components are dropped. The result names a file only if [`regular_files`]
/// lists it.
pub fn requested_path(raw: &str) -> Result<PathBuf, PathRefusal> {
    let mut clean = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir => return Err(PathRefusal::ParentDir),
            Component::RootDir | Component::Prefix(_) => return Err(PathRefusal::Absolute),
        }
    }
    Ok(clean)
}

/// Every regular file under `root`, found by a fresh `symlink_metadata` walk:
/// symlinks (to files or directories) and special files are skipped, never
/// followed. A file whose relative path is not UTF-8 is skipped with a WARN,
/// since the manifest carries paths as strings. Sorted by path. A missing
/// `root` (the run was swept) is an empty list; an unreadable one is an error.
pub fn regular_files(root: &Path) -> Result<Vec<(PathBuf, ManifestFile)>> {
    log::debug!("scratch::regular_files: root={}", root.display());
    let mut found = Vec::new();
    match std::fs::symlink_metadata(root) {
        Ok(meta) if meta.file_type().is_dir() => walk(root, Path::new(""), &mut found)?,
        Ok(_) => log::warn!("scratch: {} is not a directory; listing nothing", root.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("symlink_metadata {}", root.display())),
    }
    found.sort_by(|a, b| a.1.path.cmp(&b.1.path));
    log::debug!("scratch::regular_files: root={} files={}", root.display(), found.len());
    Ok(found)
}

fn walk(dir: &Path, rel: &Path, found: &mut Vec<(PathBuf, ManifestFile)>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry.with_context(|| format!("read_dir entry in {}", dir.display()))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).with_context(|| format!("symlink_metadata {}", path.display()))?;
        let child_rel = rel.join(entry.file_name());
        let file_type = meta.file_type();
        if file_type.is_dir() {
            walk(&path, &child_rel, found)?;
        } else if file_type.is_file() {
            let Some(text) = child_rel.to_str() else {
                log::warn!("scratch: skipping non-UTF-8 path {}", path.display());
                continue;
            };
            let manifest_path = text.replace(std::path::MAIN_SEPARATOR, "/");
            found.push((
                child_rel,
                ManifestFile {
                    path: manifest_path,
                    size: meta.len(),
                },
            ));
        } else {
            log::debug!("scratch: skipping non-regular entry {}", path.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
