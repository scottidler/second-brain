use eyre::{Context, ContextCompat, Result};
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::config::ScanConfig;
use crate::frontmatter::{Frontmatter, parse_frontmatter};

/// Parsed representation of a vault note.
#[derive(Debug, Clone)]
pub struct Note {
    /// Path relative to vault root.
    pub path: PathBuf,
    pub frontmatter: Frontmatter,
    /// Everything after the closing ---.
    pub body: String,
    /// Original file contents.
    pub raw: String,
}

/// Parse a single markdown file into a Note.
pub fn parse_note(vault_root: &Path, path: &Path) -> Result<Note> {
    let raw = fs::read_to_string(path).context(format!("failed to read {}", path.display()))?;
    let relative = path.strip_prefix(vault_root).unwrap_or(path).to_path_buf();

    let (frontmatter, body) = parse_frontmatter(&raw)?;

    Ok(Note {
        path: relative,
        frontmatter,
        body,
        raw,
    })
}

/// Collect the absolute paths of every `.md` file in the vault, respecting `ignore` directories.
///
/// Sequential by design: WalkDir is fast and the I/O is cheap (directory enumeration only).
/// The expensive work - opening, reading, and YAML-parsing each note - happens later in
/// `scan_vault` via `rayon::par_iter`.
fn collect_md_paths(vault_root: &Path, scan_config: &ScanConfig) -> Result<Vec<PathBuf>> {
    log::debug!(
        "note::collect_md_paths: vault_root={} ignore={:?}",
        vault_root.display(),
        scan_config.ignore
    );
    let mut paths = Vec::new();
    for entry in WalkDir::new(vault_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                return !scan_config.ignore.iter().any(|ig| name == *ig);
            }
            true
        })
    {
        let entry = entry.context("failed to read directory entry")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        paths.push(path.to_path_buf());
    }
    log::debug!("note::collect_md_paths: collected {} md path(s)", paths.len());
    Ok(paths)
}

/// Scan an entire vault and return all parsed notes.
///
/// Walks the vault sequentially to discover `.md` paths, then parses them in parallel via
/// `rayon::par_iter`. Parse failures are logged at `warn!` (matching the pre-parallel behavior)
/// and excluded from the result. The returned vector is sorted by path for deterministic output
/// regardless of parallel completion order.
pub fn scan_vault(vault_root: &Path, scan_config: &ScanConfig) -> Result<Vec<Note>> {
    log::debug!("note::scan_vault: vault_root={}", vault_root.display());
    let paths = collect_md_paths(vault_root, scan_config)?;

    let mut notes: Vec<Note> = paths
        .par_iter()
        .filter_map(|path| match parse_note(vault_root, path) {
            Ok(note) => Some(note),
            Err(e) => {
                log::warn!("failed to parse note {}: {e}", path.display());
                None
            }
        })
        .collect();

    notes.sort_by(|a, b| a.path.cmp(&b.path));
    log::info!("vault parsed: {} notes", notes.len());

    Ok(notes)
}

/// Atomically write `bytes` to `dest`: write to a uniquely-named temp file in
/// the destination's OWN directory, fsync it, rename it into place, then
/// fsync the parent directory. A reader of `dest` therefore sees either the
/// complete old file or the complete new file - never a torn write.
///
/// This is THE shared note-write primitive for the workspace. The vault is
/// Syncthing'd; a non-atomic in-place `fs::write` can replicate a torn write
/// to every machine. The temp file MUST live in the target's own directory
/// (a cross-filesystem rename fails - `/tmp` is a different mount) and carry a
/// unique name (cortex writes notes concurrently via rayon, so a fixed
/// `.tmp` name would collide).
pub fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = dest.parent().context("destination has no parent directory")?;
    let mut temp = tempfile::Builder::new()
        .prefix(".sb-tmp-")
        .tempfile_in(parent)
        .with_context(|| format!("create temp in {}", parent.display()))?;
    temp.write_all(bytes).context("write temp bytes")?;
    temp.as_file().sync_all().context("fsync temp")?;
    temp.persist(dest)
        .map_err(|e| eyre::eyre!("persist temp -> {}: {e}", dest.display()))?;
    // Best-effort fsync of the parent directory so the new dirent is durable
    // across power loss.
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
