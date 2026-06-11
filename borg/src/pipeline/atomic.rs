//! Atomic publish + pure-string composition helpers.
//!
//! Phase 3 of the borg-pipeline-resilience design doc. The 2026-05-08 incident
//! that took out a vault note had three causes (atomicity, timeout, dedup);
//! this module owns the atomicity fix:
//!
//! 1. `write_atomic` writes bytes to a sibling temp file (dot-prefixed so it
//!    is invisible to Obsidian / the vault watcher), fsyncs the file and
//!    parent directory, and renames atomically over the destination.
//! 2. `apply_original_date` and `apply_cortex_fields` replace the previous
//!    `patch_*` helpers that read a file and rewrote it. The new helpers
//!    operate on `String` so the caller composes the final note in memory
//!    and writes it once via `write_atomic`. There is no window during
//!    which the file on disk is missing the date or cortex fields.

use eyre::Result;
use std::path::{Path, PathBuf};
use vault::schema::CORTEX_PRESERVE_KEYS;

/// Resolve a non-URL publish destination, honoring `force`. When `force` is
/// true (or the path is free) the destination is returned unchanged
/// (overwrite). Otherwise it is uniquified with a `-2`, `-3`, ... suffix so a
/// same-title note is not silently clobbered. Mirrors cortex's
/// `classify::resolve_collision`, minus the reingest/source-URL case that
/// non-URL content (images, audio, pasted text) has no concept of.
pub fn resolve_publish_path(dest: &Path, force: bool) -> PathBuf {
    if force || !dest.exists() {
        return dest.to_path_buf();
    }
    let stem = dest.file_stem().and_then(|s| s.to_str()).unwrap_or("note");
    let ext = dest.extension().and_then(|e| e.to_str()).unwrap_or("md");
    let parent = dest.parent().unwrap_or(Path::new("."));
    for i in 2..100 {
        let candidate = parent.join(format!("{stem}-{i}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dest.to_path_buf()
}

/// Atomically write `bytes` to `dest`. Thin re-export of the shared
/// [`vault::note::write_atomic`] primitive (tmp + fsync + rename + parent
/// fsync) so borg and cortex converge on ONE atomic-write implementation.
pub fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<()> {
    vault::note::write_atomic(dest, bytes)
}

/// Replace the `date:` line in `rendered` with `new_date`. If no `date:` line
/// is found, return `rendered` unchanged. Pure-string form of the previous
/// `patch_note_date` helper, kept compatible so the publish path can compose
/// the final note in memory before a single atomic write.
pub fn apply_original_date(rendered: &str, new_date: &str) -> String {
    let mut out = String::with_capacity(rendered.len());
    let mut found = false;
    let trailing_newline = rendered.ends_with('\n');
    for line in rendered.lines() {
        if !found && line.starts_with("date:") {
            out.push_str(&format!("date: {new_date}"));
            found = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !trailing_newline && out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Insert-or-replace the `ingested:` frontmatter line. Unlike
/// `apply_original_date` (which only replaces an existing `date:` line),
/// this helper INSERTS the field when missing - so notes written before
/// the intake-log + DLQ design shipped (which had no `ingested:` field)
/// receive one on every subsequent reingest. The field is positioned
/// directly after the existing `date:` line so the pair sits visually
/// together in the YAML block.
pub fn apply_ingested_date(rendered: &str, ingested_date: &str) -> String {
    let mut out = String::with_capacity(rendered.len() + 32);
    let trailing_newline = rendered.ends_with('\n');
    let mut found = false;
    let mut date_idx: Option<usize> = None;
    let lines: Vec<&str> = rendered.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("ingested:") {
            out.push_str(&format!("ingested: {ingested_date}"));
            found = true;
        } else {
            out.push_str(line);
            if date_idx.is_none() && line.starts_with("date:") {
                date_idx = Some(i);
            }
        }
        out.push('\n');
    }

    if !found {
        // Insert after the `date:` line if we found one; otherwise insert
        // right after the opening `---` (the frontmatter must already
        // exist for this to be useful - if it doesn't, fall through
        // unchanged).
        let mut new_lines: Vec<String> = lines.iter().map(|l| (*l).to_string()).collect();
        let insertion_idx = match date_idx {
            Some(i) => i + 1,
            None => {
                let opening = new_lines.iter().position(|l| l.trim() == "---");
                match opening {
                    Some(i) => i + 1,
                    None => {
                        // No frontmatter at all - return input unchanged.
                        let mut result = rendered.to_string();
                        if !trailing_newline && result.ends_with('\n') {
                            result.pop();
                        }
                        return result;
                    }
                }
            }
        };
        new_lines.insert(insertion_idx, format!("ingested: {ingested_date}"));
        let joined = new_lines.join("\n");
        let mut result = format!("{joined}\n");
        if !trailing_newline {
            result.pop();
        }
        return result;
    }

    if !trailing_newline && out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Apply (insert or replace) the given cortex-managed frontmatter fields
/// in `rendered`. Returns `rendered` unchanged when no `---` frontmatter
/// is present. Pure-string form of the previous `patch_cortex_fields`
/// helper. Only keys present in `CORTEX_PRESERVE_KEYS` are accepted; the
/// caller is expected to filter before calling.
pub fn apply_cortex_fields(rendered: &str, fields: &[(String, String)]) -> String {
    let trimmed = rendered.trim_start();
    if !trimmed.starts_with("---") {
        return rendered.to_string();
    }
    let after_opening = trimmed.trim_start_matches("---").trim_start_matches(['\r', '\n']);
    let end_pos = match after_opening.find("\n---") {
        Some(p) => p,
        None => return rendered.to_string(),
    };
    let fm_block = &after_opening[..end_pos];
    let rest = &after_opening[end_pos..];

    let mut lines: Vec<String> = fm_block.lines().map(String::from).collect();
    for (key, value) in fields {
        if !CORTEX_PRESERVE_KEYS.contains(&key.as_str()) {
            continue;
        }
        lines.retain(|line| !line.starts_with(&format!("{key}:")));
        lines.push(format!("{key}: {value}"));
    }

    let offset = rendered.len() - trimmed.len();
    let prefix = &rendered[..offset];
    format!("{prefix}---\n{}\n{rest}", lines.join("\n"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
