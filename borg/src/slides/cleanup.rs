//! Replay/reingest cleanup.
//!
//! When a note that previously owned slides or images is re-published (replay
//! or fresh-trace re-ingestion of the same URL), the old files become
//! orphans the moment the new note's `slides:` / `images:` frontmatter lists
//! point elsewhere. We delete those orphans via `rkvr rmrf` (per the safety rule;
//! the tool archives before deleting, enabling recovery).
//!
//! The order matters and is the same as the design doc:
//!   1. Read the old note's `slides:` list (in memory).
//!   2. Run `publish_slides` to copy new JPEGs (different filenames if any
//!      collision with old ones).
//!   3. Write the new note (atomic tmpfile + rename).
//!   4. ONLY THEN: delete the old slide files via `rkvr rmrf`.
//!
//! A crash between (3) and (4) leaves orphans, but the new note is on disk
//! so readers see correct content. Crashes between earlier steps leave the
//! old state intact.

use eyre::{Context, Result};
use std::path::{Path, PathBuf};

/// The vault files a note owns, read from its frontmatter: `slides:` (YouTube
/// slide JPEGs) and `images:` (figures from X posts and articles). Each key
/// keeps its own list and is diffed on its own, so slides behave as before.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Owned {
    pub slides: Vec<String>,
    pub images: Vec<String>,
}

/// Pull the `slides:` and `images:` lists out of a vault note's YAML
/// frontmatter. Both are empty if the note does not exist (a fresh slug has
/// no prior state), has no frontmatter block at all (it does not open with a
/// `---` line), or lacks the fields. CRLF line endings are read as LF. A note
/// that exists but cannot be read, or whose frontmatter block is unterminated
/// or not YAML, is an error: guessing "owns nothing" would orphan its files
/// and let a degraded reingest replace a note that owns images.
pub fn read_old_owned(note_path: &Path) -> Result<Owned> {
    if !note_path.exists() {
        return Ok(Owned::default());
    }
    let content = std::fs::read_to_string(note_path)
        .with_context(|| format!("read note for owned-files frontmatter: {}", note_path.display()))?;
    let content = content.replace("\r\n", "\n");
    let trimmed = content.trim_start_matches('\n');
    let Some(rest) = trimmed.strip_prefix("---\n") else {
        log::debug!("read_old_owned: {} has no frontmatter block", note_path.display());
        return Ok(Owned::default());
    };
    let yaml = if rest.starts_with("---") {
        ""
    } else {
        let end_idx = rest
            .find("\n---")
            .ok_or_else(|| eyre::eyre!("unterminated frontmatter in {}", note_path.display()))?;
        &rest[..end_idx]
    };
    let parsed: serde_yaml::Value = serde_yaml::from_str(yaml)
        .with_context(|| format!("parse frontmatter for owned files: {}", note_path.display()))?;
    Ok(Owned {
        slides: string_list(&parsed, "slides"),
        images: string_list(&parsed, "images"),
    })
}

fn string_list(frontmatter: &serde_yaml::Value, key: &str) -> Vec<String> {
    frontmatter
        .get(key)
        .and_then(serde_yaml::Value::as_sequence)
        .map(|seq| seq.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Compute the set difference `old - new`: paths the new note no longer
/// references, which are therefore orphans to clean up. Order is preserved
/// from `old` for determinism.
pub fn compute_orphans(old: &[String], new: &[String]) -> Vec<String> {
    let new_set: std::collections::HashSet<&str> = new.iter().map(String::as_str).collect();
    old.iter().filter(|p| !new_set.contains(p.as_str())).cloned().collect()
}

/// Resolve vault-relative paths to absolute paths under `vault_root`,
/// dropping any that don't actually exist (already-deleted, or never written).
pub fn resolve_existing(vault_root: &Path, vault_relative: &[String]) -> Vec<PathBuf> {
    vault_relative
        .iter()
        .map(|rel| vault_root.join(rel))
        .filter(|p| p.exists())
        .collect()
}

/// Delete orphan files, preferring `rkvr rmrf` (recoverable) and falling
/// back to non-recoverable removal with a WARN when rkvr is not on PATH. rkvr
/// is preferred, not required. See [`crate::rkvr`].
pub fn rkvr_remove(paths: &[PathBuf]) -> Result<()> {
    crate::rkvr::remove(paths)
}

/// Orphans across both keys: `old - new` for `slides:` then for `images:`,
/// vault-relative, in `old` order.
pub fn compute_all_orphans(old: &Owned, new: &Owned) -> Vec<String> {
    let mut orphans = compute_orphans(&old.slides, &new.slides);
    orphans.extend(compute_orphans(&old.images, &new.images));
    orphans
}

/// End-to-end orphan cleanup: diffs the old note's owned files against
/// `new`, resolves the orphans to absolute paths under `vault_root`, and runs
/// `rkvr rmrf` on them. Returns the orphans (vault-relative) that were
/// targeted - useful for tests and audit logs.
///
/// Caller must invoke this AFTER the new note is durable on disk (`fsync` +
/// rename), so a crash never leaves both the new note pointing at deleted
/// files AND the old note still on disk.
pub fn cleanup_orphans(vault_root: &Path, old: &Owned, new: &Owned) -> Result<Vec<String>> {
    let orphans = compute_all_orphans(old, new);
    let abs = resolve_existing(vault_root, &orphans);
    rkvr_remove(&abs)?;
    Ok(orphans)
}

#[cfg(test)]
mod tests;
