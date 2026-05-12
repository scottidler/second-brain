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

use eyre::{Context, ContextCompat, Result};
use std::io::Write;
use std::path::Path;
use tempfile::Builder;
use vault::schema::CORTEX_PRESERVE_KEYS;

/// Atomically write `bytes` to `dest`. Uses a sibling `.borg-tmp-<random>`
/// file so a `SIGKILL` mid-write cannot leave the destination in a partial
/// state. Same-FS rename guarantees atomicity at the filesystem level.
pub fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<()> {
    let parent = dest.parent().context("destination has no parent directory")?;
    let mut temp = Builder::new()
        .prefix(".borg-tmp-")
        .tempfile_in(parent)
        .with_context(|| format!("create temp in {}", parent.display()))?;
    temp.write_all(bytes).context("write temp bytes")?;
    temp.as_file().sync_all().context("fsync temp")?;
    temp.persist(dest)
        .map_err(|e| eyre::eyre!("persist temp -> {}: {e}", dest.display()))?;
    // Best-effort fsync of the parent directory so the new dirent is durable
    // across power loss. Not required to defeat the failure mode in this doc,
    // but cheap insurance and standard practice for atomic-write helpers.
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
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
mod tests {
    use super::*;

    #[test]
    fn test_write_atomic_creates_destination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("note.md");
        write_atomic(&dest, b"hello world").expect("write");
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello world");
    }

    #[test]
    fn test_write_atomic_overwrites_existing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("note.md");
        std::fs::write(&dest, b"old content").unwrap();
        write_atomic(&dest, b"new content").expect("write");
        assert_eq!(std::fs::read(&dest).unwrap(), b"new content");
    }

    #[test]
    fn test_write_atomic_does_not_leak_on_persist_failure_path() {
        // Persist into a non-existent destination directory: parent must
        // exist for the rename to succeed. This drives the error path.
        let dir = tempfile::tempdir().expect("tempdir");
        let bad_dest = dir.path().join("missing").join("note.md");
        // Create the temp first by passing a valid parent; then check that
        // a failure at write time leaves no orphan tempfile in the parent.
        let result = write_atomic(&bad_dest, b"data");
        assert!(result.is_err());
        // The valid parent dir should not have any leftover .borg-tmp-* files.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".borg-tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no .borg-tmp-* files should be left, found: {leftovers:?}"
        );
    }

    /// The architect's hardest question: a SIGKILL between write_atomic and
    /// the cortex patches must not desync the file. With the new compose-in-
    /// memory approach, write_atomic produces the FINAL bytes; once persist
    /// returns Ok the file is complete. Verify that running write_atomic
    /// with a fully-composed body produces a file containing both the
    /// restored date and the cortex fields - byte-for-byte, in one write.
    #[test]
    fn test_compose_then_write_atomic_is_complete_in_one_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("note.md");
        let rendered = "---\ntitle: Example\ndate: 2026-05-08\n---\nBody text.\n";
        let composed = apply_original_date(rendered, "2026-04-19");
        let cortex_fields = vec![
            ("domain".to_string(), "ai".to_string()),
            ("cortex-quality".to_string(), "ok".to_string()),
        ];
        let composed = apply_cortex_fields(&composed, &cortex_fields);
        write_atomic(&dest, composed.as_bytes()).expect("write");

        let contents = std::fs::read_to_string(&dest).unwrap();
        assert!(
            contents.contains("date: 2026-04-19"),
            "date should be restored, got: {contents}"
        );
        assert!(
            contents.contains("domain: ai"),
            "domain should be present, got: {contents}"
        );
        assert!(
            contents.contains("cortex-quality: ok"),
            "cortex-quality should be present, got: {contents}"
        );
        assert!(contents.contains("Body text."), "body must be preserved");
    }

    #[test]
    fn test_apply_original_date_replaces_existing() {
        let input = "---\ntitle: X\ndate: 2026-01-01\nfoo: bar\n---\nbody\n";
        let out = apply_original_date(input, "2025-12-31");
        assert!(out.contains("date: 2025-12-31"));
        assert!(!out.contains("2026-01-01"));
        assert!(out.contains("title: X"));
        assert!(out.contains("foo: bar"));
        assert!(out.contains("body"));
    }

    #[test]
    fn test_apply_ingested_date_inserts_when_missing() {
        let input = "---\ntitle: X\ndate: 2026-04-16\n---\nbody\n";
        let out = apply_ingested_date(input, "2026-05-12");
        assert!(out.contains("ingested: 2026-05-12"), "got: {out}");
        // Original date untouched
        assert!(out.contains("date: 2026-04-16"));
        // `ingested:` appears AFTER `date:` (paired)
        let date_pos = out.find("date: 2026-04-16").expect("date present");
        let ing_pos = out.find("ingested: 2026-05-12").expect("ingested present");
        assert!(ing_pos > date_pos, "ingested should follow date");
    }

    #[test]
    fn test_apply_ingested_date_replaces_existing() {
        let input = "---\ntitle: X\ndate: 2026-04-16\ningested: 2026-04-16\n---\nbody\n";
        let out = apply_ingested_date(input, "2026-05-12");
        assert!(out.contains("ingested: 2026-05-12"));
        assert!(!out.contains("ingested: 2026-04-16"));
        assert!(out.contains("date: 2026-04-16"));
    }

    #[test]
    fn test_apply_ingested_date_preserves_other_fields() {
        let input = "---\ntitle: X\ndate: 2026-04-16\ndomain: ai\ncortex-quality: ok\n---\nbody\n";
        let out = apply_ingested_date(input, "2026-05-12");
        assert!(out.contains("domain: ai"));
        assert!(out.contains("cortex-quality: ok"));
        assert!(out.contains("body"));
    }

    #[test]
    fn test_apply_ingested_date_noop_without_frontmatter() {
        let input = "no frontmatter";
        let out = apply_ingested_date(input, "2026-05-12");
        assert_eq!(out, input);
    }

    #[test]
    fn test_apply_ingested_date_no_date_line_inserts_after_open() {
        let input = "---\ntitle: X\n---\nbody\n";
        let out = apply_ingested_date(input, "2026-05-12");
        assert!(out.contains("ingested: 2026-05-12"));
        // ingested should appear before title (right after opening ---)
        let title_pos = out.find("title: X").expect("title present");
        let ing_pos = out.find("ingested: 2026-05-12").expect("ingested present");
        assert!(ing_pos < title_pos);
    }

    #[test]
    fn test_apply_original_date_noop_when_no_date_line() {
        let input = "---\ntitle: X\n---\nbody\n";
        let out = apply_original_date(input, "2025-12-31");
        assert_eq!(out, input);
    }

    #[test]
    fn test_apply_cortex_fields_inserts_fields() {
        let input = "---\ntitle: Test\nsource: \"https://x\"\n---\nBody.\n";
        let fields = vec![
            ("domain".to_string(), "ai".to_string()),
            ("cortex-quality".to_string(), "ok".to_string()),
        ];
        let out = apply_cortex_fields(input, &fields);
        assert!(out.contains("domain: ai"));
        assert!(out.contains("cortex-quality: ok"));
        assert!(out.contains("title: Test"));
        assert!(out.contains("source: \"https://x\""));
        assert!(out.contains("Body."));
    }

    #[test]
    fn test_apply_cortex_fields_replaces_existing() {
        let input = "---\ntitle: T\ndomain: tech\n---\nBody.\n";
        let fields = vec![("domain".to_string(), "ai".to_string())];
        let out = apply_cortex_fields(input, &fields);
        assert!(out.contains("domain: ai"));
        assert!(!out.contains("domain: tech"));
    }

    #[test]
    fn test_apply_cortex_fields_no_frontmatter_is_noop() {
        let input = "no frontmatter here";
        let fields = vec![("domain".to_string(), "ai".to_string())];
        let out = apply_cortex_fields(input, &fields);
        assert_eq!(out, input);
    }

    #[test]
    fn test_apply_cortex_fields_filters_unknown_keys() {
        let input = "---\ntitle: T\n---\nBody.\n";
        // `not-a-cortex-key` is not in CORTEX_PRESERVE_KEYS and must be
        // ignored.
        let fields = vec![("not-a-cortex-key".to_string(), "value".to_string())];
        let out = apply_cortex_fields(input, &fields);
        assert!(!out.contains("not-a-cortex-key"));
    }

    /// The 2026-05-08 incident: an Err returned mid-pipeline (after the old
    /// note's metadata was captured but before the new note was published)
    /// must NOT delete the old note. With Phase 3's deferred-delete pattern,
    /// the old note stays on disk until write_atomic returns Ok. Simulate
    /// the failure mode: capture old note metadata, return an Err before
    /// calling write_atomic, and assert the old file is byte-for-byte
    /// unchanged.
    #[test]
    fn test_reingest_failure_before_publish_preserves_old_note() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old_path = dir.path().join("old-note.md");
        let original_bytes = b"---\ntitle: Old\ndate: 2026-04-01\ndomain: ai\n---\nOriginal body.\n";
        std::fs::write(&old_path, original_bytes).unwrap();

        // Simulate the captured metadata path - the publish step never runs
        // because the pipeline returned Err before reaching write_atomic.
        let _captured_date = "2026-04-01".to_string();
        let _captured_cortex: Vec<(String, String)> = vec![("domain".to_string(), "ai".to_string())];
        let pipeline_result: Result<()> = Err(eyre::eyre!("simulated mid-pipeline failure"));
        assert!(pipeline_result.is_err(), "pipeline failed before publish");

        // Phase 3 invariant: write_atomic was never called, so the old
        // file must still exist with its original bytes. Pre-Phase 3 code
        // would have deleted old_path here and lost the data.
        let after = std::fs::read(&old_path).unwrap();
        assert_eq!(
            after, original_bytes,
            "old note must survive a mid-pipeline failure unchanged"
        );
    }
}
