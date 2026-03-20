use eyre::{Context, Result};
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

/// Scan an entire vault and return all parsed notes.
pub fn scan_vault(vault_root: &Path, scan_config: &ScanConfig) -> Result<Vec<Note>> {
    let mut notes = Vec::new();

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

        match parse_note(vault_root, path) {
            Ok(note) => notes.push(note),
            Err(e) => {
                log::warn!("failed to parse note {}: {e}", path.display());
            }
        }
    }

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
}
