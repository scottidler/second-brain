use eyre::{Context, Result};
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_temp_vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path();

        // Create a note with frontmatter
        fs::write(
            root.join("test-note.md"),
            "---\ntitle: Test Note\ntype: note\ndate: 2026-01-01\ntags:\n  - rust\n---\nBody text here.\n",
        )
        .expect("write");

        // Create a bare note
        fs::write(root.join("bare.md"), "Just some text.\n").expect("write");

        // Create an ignored directory
        fs::create_dir_all(root.join(".obsidian")).expect("mkdir");
        fs::write(root.join(".obsidian/workspace.md"), "---\ntype: system\n---\n").expect("write");

        // Create a non-md file
        fs::write(root.join("readme.txt"), "not a note").expect("write");

        dir
    }

    #[test]
    fn test_parse_note_with_frontmatter() {
        let dir = setup_temp_vault();
        let note = parse_note(dir.path(), &dir.path().join("test-note.md")).expect("parse");
        assert_eq!(note.frontmatter.title.as_deref(), Some("Test Note"));
        assert_eq!(note.frontmatter.note_type.as_deref(), Some("note"));
        assert!(note.body.contains("Body text here."));
    }

    #[test]
    fn test_parse_note_without_frontmatter() {
        let dir = setup_temp_vault();
        let note = parse_note(dir.path(), &dir.path().join("bare.md")).expect("parse");
        assert!(note.frontmatter.is_empty());
        assert!(note.body.contains("Just some text."));
    }

    #[test]
    fn test_scan_vault_ignores_obsidian_dir() {
        let dir = setup_temp_vault();
        let config = ScanConfig {
            ignore: vec![".git".to_string(), ".obsidian".to_string()],
        };
        let notes = scan_vault(dir.path(), &config).expect("scan");
        assert!(!notes.iter().any(|n| n.path.to_string_lossy().contains(".obsidian")));
    }

    #[test]
    fn test_scan_vault_only_md_files() {
        let dir = setup_temp_vault();
        let config = ScanConfig {
            ignore: vec![".git".to_string(), ".obsidian".to_string()],
        };
        let notes = scan_vault(dir.path(), &config).expect("scan");
        assert!(!notes.iter().any(|n| n.path.to_string_lossy().contains("readme")));
    }

    #[test]
    fn test_scan_vault_finds_notes() {
        let dir = setup_temp_vault();
        let config = ScanConfig {
            ignore: vec![".git".to_string(), ".obsidian".to_string()],
        };
        let notes = scan_vault(dir.path(), &config).expect("scan");
        assert_eq!(notes.len(), 2);
    }

    /// Phase 1 determinism guard: parallel `scan_vault` must return notes sorted by path,
    /// independent of parallel completion order. Build a vault with notes whose unsorted
    /// natural traversal order would differ from the sorted order, then assert the result is
    /// sorted.
    #[test]
    fn scan_vault_returns_path_sorted_notes_under_par_iter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        // Names chosen so alphabetical order is not the FS-traversal order on most filesystems.
        for name in &["zeta.md", "alpha.md", "mike.md", "bravo.md", "yankee.md"] {
            fs::write(root.join(name), "---\ntitle: x\ntype: note\n---\nbody\n").expect("write");
        }
        let config = ScanConfig {
            ignore: vec![".git".to_string(), ".obsidian".to_string()],
        };
        let notes = scan_vault(root, &config).expect("scan");
        let mut sorted_paths: Vec<PathBuf> = notes.iter().map(|n| n.path.clone()).collect();
        let original = sorted_paths.clone();
        sorted_paths.sort();
        assert_eq!(
            original, sorted_paths,
            "scan_vault output must be path-sorted regardless of parallel completion order"
        );
    }

    /// Regression guard: when audit `--fix duplicate` moves notes into
    /// `system/quarantine/<source-key>/...`, those notes still carry valid
    /// frontmatter. The default `ScanConfig` includes `"quarantine"` so any
    /// downstream consumer (oracle's `index_vault`, cortex's vault scanner,
    /// etc.) excludes them automatically. Without this, the quarantined
    /// notes would re-enter the search index as if they were live knowledge.
    #[test]
    fn scan_vault_default_config_excludes_quarantine_subdirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(root.join("live.md"), "---\ntitle: live\ntype: note\n---\nbody\n").expect("write live");
        let quarantine_dir = root.join("system").join("quarantine").join("https-example-com").join("notes");
        fs::create_dir_all(&quarantine_dir).expect("mkdir quarantine");
        fs::write(
            quarantine_dir.join("quarantined.md"),
            "---\ntitle: quarantined\ntype: note\nsource: https://example.com\n---\nbody\n",
        )
        .expect("write quarantined");

        let notes = scan_vault(root, &crate::config::ScanConfig::default()).expect("scan");
        let paths: Vec<String> = notes.iter().map(|n| n.path.to_string_lossy().to_string()).collect();
        assert!(paths.iter().any(|p| p == "live.md"), "live note must be returned: {paths:?}");
        assert!(
            !paths.iter().any(|p| p.contains("quarantine")),
            "quarantined note must be excluded from scan: {paths:?}"
        );
    }

    /// Phase 1 error-path guard: an unreadable `.md` file (non-UTF-8 bytes) is warn-logged and
    /// skipped without aborting the whole scan; sibling notes still parse. This exercises the
    /// `parse_note` -> `fs::read_to_string` error branch, which is the only branch that returns
    /// `Err` in practice (frontmatter parsing is lenient by design).
    #[test]
    fn scan_vault_skips_unreadable_notes_and_keeps_good_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(root.join("good.md"), "---\ntitle: ok\ntype: note\n---\nbody\n").expect("write good");
        // Invalid UTF-8 bytes: read_to_string returns Err for this file.
        fs::write(root.join("invalid-utf8.md"), [0xFFu8, 0xFE, 0xFD, 0xFC]).expect("write bytes");
        let config = ScanConfig {
            ignore: vec![".git".to_string(), ".obsidian".to_string()],
        };
        let notes = scan_vault(root, &config).expect("scan should not propagate parse error");
        // Only the good note should be returned; the unreadable one is warn-logged and dropped.
        assert_eq!(notes.len(), 1, "expected only the good note to survive parse");
        assert_eq!(notes[0].path.to_string_lossy(), "good.md");
    }
}
