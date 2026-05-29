//! Phase 2.2: replay/reingest cleanup.
//!
//! When a note that previously had embedded slides is re-published (replay
//! or fresh-trace re-ingestion of the same URL), the old slide JPEGs become
//! orphans the moment the new note's `slides:` frontmatter list points
//! elsewhere. We delete those orphans via `rkvr rmrf` (per the safety rule;
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

/// Pull the `slides:` list out of a vault note's YAML frontmatter. Returns
/// an empty vec if the note has no frontmatter or no `slides:` field, or if
/// the file does not exist (a fresh slug has no prior state).
pub fn read_old_slides_frontmatter(note_path: &Path) -> Result<Vec<String>> {
    if !note_path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(note_path)
        .with_context(|| format!("read note for slides frontmatter: {}", note_path.display()))?;
    let trimmed = content.trim_start_matches('\n');
    let Some(rest) = trimmed.strip_prefix("---\n") else {
        return Ok(Vec::new());
    };
    let Some(end_idx) = rest.find("\n---") else {
        return Ok(Vec::new());
    };
    let yaml = &rest[..end_idx];
    // Parse loosely - if frontmatter is malformed don't crash the pipeline.
    let parsed: serde_yaml::Value = match serde_yaml::from_str(yaml) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("failed to parse frontmatter at {}: {e:#}", note_path.display());
            return Ok(Vec::new());
        }
    };
    let Some(slides_val) = parsed.get("slides") else {
        return Ok(Vec::new());
    };
    let Some(seq) = slides_val.as_sequence() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(seq.len());
    for v in seq {
        if let Some(s) = v.as_str() {
            out.push(s.to_string());
        }
    }
    Ok(out)
}

/// Compute the set difference `old - new`: paths the new note no longer
/// references, which are therefore orphans to clean up. Order is preserved
/// from `old` for determinism.
pub fn compute_orphans(old: &[String], new: &[String]) -> Vec<String> {
    let new_set: std::collections::HashSet<&str> = new.iter().map(String::as_str).collect();
    old.iter().filter(|p| !new_set.contains(p.as_str())).cloned().collect()
}

/// Resolve vault-relative slide paths to absolute paths under `vault_root`,
/// dropping any that don't actually exist (already-deleted, or never written).
pub fn resolve_existing(vault_root: &Path, vault_relative: &[String]) -> Vec<PathBuf> {
    vault_relative
        .iter()
        .map(|rel| vault_root.join(rel))
        .filter(|p| p.exists())
        .collect()
}

/// Delete orphan slide files, preferring `rkvr rmrf` (recoverable) and falling
/// back to non-recoverable removal with a WARN when rkvr is not on PATH. rkvr
/// is preferred, not required. See [`crate::rkvr`].
pub fn rkvr_remove(paths: &[PathBuf]) -> Result<()> {
    crate::rkvr::remove(paths)
}

/// End-to-end orphan cleanup. Reads the old note's `slides:` list from
/// `note_path`, diffs against `new_owned`, resolves to absolute paths under
/// `vault_root`, and runs `rkvr rmrf` on the diff. Returns the list of
/// orphans (vault-relative) that were targeted - useful for tests and
/// audit logs.
///
/// Caller must invoke this AFTER the new note is durable on disk (`fsync` +
/// rename), so a crash never leaves both the new note pointing at deleted
/// slides AND the old note still on disk.
pub fn cleanup_orphans(vault_root: &Path, note_path: &Path, new_owned: &[String]) -> Result<Vec<String>> {
    let old = read_old_slides_frontmatter(note_path)?;
    let orphans = compute_orphans(&old, new_owned);
    let abs = resolve_existing(vault_root, &orphans);
    rkvr_remove(&abs)?;
    Ok(orphans)
}

#[cfg(test)]
mod tests;
