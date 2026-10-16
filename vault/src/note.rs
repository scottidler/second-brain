use eyre::{Context, ContextCompat, Result};
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
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

/// A Syncthing conflict copy: `<stem>.sync-conflict-<YYYYMMDD>-<HHMMSS>-<DEVICE>.md`.
/// Also matches the copies cortex's slug rule renamed before this existed
/// (`<stem>sync-conflict-…-<device>.md`, dot dropped and lowercased).
static SYNC_CONFLICT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"sync-conflict-\d{8}-\d{6}-[A-Za-z0-9]{7}").expect("valid regex"));

/// Whether `path` is a vault note: a `.md` file that is not a Syncthing
/// conflict copy. A conflict copy is a second writer's version of a note,
/// waiting for a human to merge it; every tool that treated it as a note
/// renamed it, enriched it, or linked to it, and it then synced everywhere.
/// Every vault walker filters through this one predicate.
pub fn is_note_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("md")
        && !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| SYNC_CONFLICT.is_match(n))
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
        if !is_note_file(path) {
            continue;
        }
        paths.push(path.to_path_buf());
    }
    log::debug!("note::collect_md_paths: collected {} md path(s)", paths.len());
    Ok(paths)
}

/// Vault-relative, `/`-separated paths of every non-markdown file a wikilink
/// can name (`![[x.png]]`, `[[doc.pdf]]`).
///
/// Obsidian resolves an attachment link against every file in the vault, so
/// this walk honours no `ignore` list: attachments live in exactly the
/// `attachments/` and `assets/` folders a note scan skips. Only dot-named
/// entries (`.git`, `.obsidian`, `.trash`), which Obsidian never indexes, are
/// skipped. A file with no extension is skipped too: an attachment link names
/// the full filename, so indexing `foo` would let a bare `[[foo]]` note link
/// resolve to it.
pub fn attachment_paths(vault_root: &Path) -> Result<Vec<String>> {
    log::debug!("note::attachment_paths: vault_root={}", vault_root.display());
    let mut paths = Vec::new();
    for entry in WalkDir::new(vault_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
    {
        let entry = entry.context("failed to read directory entry")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        match path.extension().and_then(|e| e.to_str()) {
            None | Some("md") => continue,
            Some(_) => {}
        }
        let relative = path.strip_prefix(vault_root).unwrap_or(path);
        paths.push(relative.to_string_lossy().replace('\\', "/"));
    }
    log::debug!("note::attachment_paths: collected {} attachment path(s)", paths.len());
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
    let parent = dest.parent().context("destination has no parent directory")?;
    let temp = stage_temp(parent, bytes)?;
    temp.persist(dest)
        .map_err(|e| eyre::eyre!("persist temp -> {}: {e}", dest.display()))?;
    sync_parent(parent);
    Ok(())
}

/// Like [`write_atomic`], but never replaces an existing `dest`: returns
/// `Ok(true)` when the note was created and `Ok(false)` when something already
/// lives at `dest` (left untouched, temp file removed). The exists-check and
/// the create are one filesystem operation (`link`), so two writers racing for
/// the same name cannot both win; a separate `exists()` followed by
/// `write_atomic` leaves a window where the later rename silently clobbers the
/// earlier note.
pub fn write_atomic_new(dest: &Path, bytes: &[u8]) -> Result<bool> {
    let parent = dest.parent().context("destination has no parent directory")?;
    let temp = stage_temp(parent, bytes)?;
    match temp.persist_noclobber(dest) {
        Ok(_) => {
            sync_parent(parent);
            Ok(true)
        }
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(eyre::eyre!("persist temp -> {}: {e}", dest.display())),
    }
}

/// Write `bytes` to a uniquely-named, fsynced temp file in `parent`.
fn stage_temp(parent: &Path, bytes: &[u8]) -> Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let mut temp = tempfile::Builder::new()
        .prefix(".sb-tmp-")
        .tempfile_in(parent)
        .with_context(|| format!("create temp in {}", parent.display()))?;
    temp.write_all(bytes).context("write temp bytes")?;
    temp.as_file().sync_all().context("fsync temp")?;
    Ok(temp)
}

/// Best-effort fsync of the parent directory so the new dirent is durable
/// across power loss.
fn sync_parent(parent: &Path) {
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
}

#[cfg(test)]
mod tests;
