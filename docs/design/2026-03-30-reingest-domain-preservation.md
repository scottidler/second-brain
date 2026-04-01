# Design Document: Reingest Domain Preservation

**Author:** Scott Idler
**Date:** 2026-03-30
**Status:** Implemented
**Review Passes Completed:** 3/5

## Summary

When borg reingests a URL that cortex has already classified and promoted to `notes/`, the reingest pipeline deletes the old note and writes a fresh one - preserving the original date and location but discarding domain, status, and all cortex metadata. Cortex classify then never reclassifies it because it only scans `inbox/`. This leaves orphaned notes in `notes/` with no domain. Fix both: make borg preserve cortex fields on reingest, and make cortex catch domainless notes in `notes/`.

## Problem Statement

### Background

The ingestion and classification pipeline has a clean separation of concerns:
- **Borg** captures content and writes notes to `inbox/` with no domain (by design)
- **Cortex classify** scans `inbox/`, assigns domain via tag-mapping or LLM, promotes to `notes/`

Borg also supports **reingest** - when the same URL is submitted again, it finds the existing note (via ledger + filesystem search), preserves its date and directory, deletes the old file, and writes a fresh note in place. This avoids demoting an already-promoted note back to `inbox/`.

### Problem

The reingest path preserves date and location but not cortex-managed metadata. The sequence:

1. First ingest: borg writes note to `inbox/` (no domain)
2. Cortex classify: assigns domain, status, cortex-classified, cortex-classified-by, cortex-confidence; moves to `notes/`
3. Reingest of same URL: borg finds note in `notes/`, reads `date`, saves parent dir as `reingest_dest`, **deletes old file**, renders fresh markdown (no domain), writes to `notes/`
4. Result: note in `notes/` with no domain, no status, no cortex metadata

Cortex classify never fixes this because `filter_inbox_notes()` at `classify.rs:650-655` only matches paths starting with `inbox/`. Notes already in `notes/` are invisible to the classify pipeline.

**Current impact:** 16 notes in `notes/` have no domain. 10 of those are confirmed reingests (have `🔄` entries in the borg ledger). The remaining 6 are system-generated notes that never went through borg.

### Relevant code

| Component | File | Lines | What it does |
|-----------|------|-------|-------------|
| Reingest preserve date | `borg/src/pipeline.rs` | 347-377 | Reads date and parent dir, deletes old note |
| Reingest write | `borg/src/pipeline.rs` | 467-475 | Renders fresh note, writes to `reingest_dest` |
| Date read | `borg/src/pipeline.rs` | 2179-2186 | `read_note_date()` - line-by-line frontmatter scan |
| Date patch | `borg/src/pipeline.rs` | 2190-2205 | `patch_note_date()` - find-and-replace in frontmatter |
| Classify filter | `cortex/src/classify.rs` | 649-676 | `filter_inbox_notes()` - only selects `inbox/` paths |
| Enrichment fields | `cortex/src/classify.rs` | 679-696 | `build_enrichment_fields()` - domain, status, cortex-* |
| Frontmatter insert | `cortex/src/scope.rs` | 99-130 | `insert_frontmatter_fields()` - adds fields before closing `---` |

### Goals

- Borg reingest preserves cortex-managed frontmatter fields (domain, status, cortex-classified, cortex-classified-by, cortex-confidence) from the old note into the new note
- Cortex classify can detect and reclassify notes in `notes/` that are missing a domain
- The 16 existing domainless notes in `notes/` get classified on the next cortex sweep

### Non-Goals

- Changing the general principle that borg does not assign domains (cortex's job)
- Modifying borg's reingest location-preservation logic
- Adding a full "diff and merge" system for frontmatter on reingest
- Changing how the borg ledger records domain (always `-` for borg entries, by design)

## Proposed Solution

### Overview

Two complementary fixes:

1. **Borg (source fix):** Before deleting the old note during reingest, read and stash cortex-managed frontmatter fields. After writing the fresh note, patch those fields back in - the same way `patch_note_date` already works.

2. **Cortex (catch-up fix):** Add a `filter_unclassified_notes()` function that finds notes in `notes/` missing a domain field. The classify pipeline calls this alongside `filter_inbox_notes()` when processing, so domainless notes get classified regardless of how they arrived.

### Fix 1: Borg reingest field preservation

#### Fields to preserve

These fields are managed by cortex classify and should survive reingest:

| Field | Example value |
|-------|--------------|
| `domain` | `ai`, `tech`, `life` |
| `status` | `unread`, `read`, `archived` |
| `cortex-classified` | `true` |
| `cortex-classified-by` | `deterministic`, `llm` |
| `cortex-confidence` | `high`, `medium` |
| `cortex-quality` | `high`, `medium`, `low` |
| `cortex-quality-issues` | `[no-outbound-links]` |

Note: preserving `status` means a reingest does NOT reset it to `unread`. If the user marked a note as `read` and then reingests it, it stays `read`. This is intentional - reingest refreshes content, not reading state.

#### Implementation

Add a `read_cortex_fields()` function (sibling to `read_note_date()`) that scans the old note's frontmatter and returns a `Vec<(String, String)>` of key-value pairs for the fields above. Only return fields that are actually present - do not inject defaults.

Add a `patch_cortex_fields()` function (sibling to `patch_note_date()`) that inserts these fields into the freshly written note's frontmatter before the closing `---`. This runs after `patch_note_date()`.

Changes to `borg/src/pipeline.rs`:

```
// Before the delete (line ~367):
let mut cortex_fields: Vec<(String, String)> = Vec::new();
if let Some(ref old_path) = old_note_path {
    original_date = read_note_date(old_path);
    cortex_fields = read_cortex_fields(old_path);   // NEW
    reingest_dest = old_path.parent().map(|p| p.to_path_buf());
    ...
}

// After the write + date patch (line ~481):
if !cortex_fields.is_empty() {
    patch_cortex_fields(&note_path, &cortex_fields)?;
    log::info!("[{trace_id}] Restored cortex fields: {:?}", cortex_fields.iter().map(|(k, _)| k).collect::<Vec<_>>());
}
```

`read_cortex_fields` implementation:

```rust
const CORTEX_PRESERVE_KEYS: &[&str] = &[
    "domain",
    "status",
    "cortex-classified",
    "cortex-classified-by",
    "cortex-confidence",
    "cortex-quality",
    "cortex-quality-issues",
];

/// Read cortex-managed fields from frontmatter.
/// Assumes all values are single-line (inline YAML). This holds for all current
/// cortex fields: `cortex-quality-issues` uses inline arrays like `[no-outbound-links]`.
/// If cortex ever writes multi-line YAML lists, this reader would need to be extended.
fn read_cortex_fields(path: &std::path::Path) -> Vec<(String, String)> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut fields = Vec::new();
    let mut in_frontmatter = false;
    for line in content.lines() {
        if line.trim() == "---" {
            if in_frontmatter { break; }
            in_frontmatter = true;
            continue;
        }
        if !in_frontmatter { continue; }
        for key in CORTEX_PRESERVE_KEYS {
            if let Some(val) = line.strip_prefix(&format!("{key}:")) {
                fields.push((key.to_string(), val.trim().to_string()));
            }
        }
    }
    fields
}
```

`patch_cortex_fields` implementation:

```rust
fn patch_cortex_fields(
    path: &std::path::Path,
    fields: &[(String, String)],
) -> eyre::Result<()> {
    let content = std::fs::read_to_string(path)
        .context("Failed to read note for cortex field patching")?;
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Ok(());
    }
    let after_opening = trimmed.trim_start_matches("---").trim_start_matches(['\r', '\n']);
    let end_pos = match after_opening.find("\n---") {
        Some(p) => p,
        None => return Ok(()),
    };
    let fm_block = &after_opening[..end_pos];
    let rest = &after_opening[end_pos..];

    let mut lines: Vec<String> = fm_block.lines().map(String::from).collect();
    for (key, value) in fields {
        // Remove existing line for this key if present
        lines.retain(|line| !line.starts_with(&format!("{key}:")));
        lines.push(format!("{key}: {value}"));
    }

    let offset = content.len() - trimmed.len();
    let prefix = &content[..offset];
    let result = format!("{prefix}---\n{}\n{rest}", lines.join("\n"));
    std::fs::write(path, result)
        .context("Failed to write cortex-patched note")?;
    Ok(())
}
```

### Fix 2: Cortex classify catch-up for `notes/`

#### Implementation

Add a `filter_unclassified_notes()` function that selects notes in `notes/` that have no domain:

```rust
/// Filter notes in notes/ that are missing a domain field (orphaned by reingest or other means).
fn filter_unclassified_notes(notes: &[Note]) -> Vec<&Note> {
    notes
        .iter()
        .filter(|n| {
            let path_str = n.path.to_string_lossy();
            path_str.starts_with("notes/") || path_str.starts_with("notes\\")
        })
        .filter(|n| n.frontmatter.domain.is_none())
        .collect()
}
```

Modify `lint_classify()` and `apply_classify()` to include these notes. In `apply_classify()`, unclassified notes from `notes/` should be enriched in-place (like reclassify mode) rather than moved, since they are already in the right directory.

Changes to `cortex/src/classify.rs`:

In `apply_classify()` (line ~303):

```rust
let mut target_notes: Vec<&Note> = if let Some(domain) = reclassify_domain {
    filter_domain_notes(notes, domain)
} else {
    let mut targets = filter_inbox_notes(notes, force, review_only);
    targets.extend(filter_unclassified_notes(notes));  // NEW
    targets
};
```

In the processing loop (inside the `for note in &target_notes` at line ~312), compute `already_in_notes` at the top of the loop body (before classify_note), then add an early branch for enriching in-place:

```rust
let already_in_notes = note.path.to_string_lossy().starts_with("notes/");

if already_in_notes {
    // Enrich in place - same logic as reclassify path (line ~332)
    let abs_path = vault_root.join(&note.path);
    let content = std::fs::read_to_string(&abs_path)?;
    let fields = build_enrichment_fields(&result);
    if let Some(new_content) = insert_frontmatter_fields(&content, &fields) {
        std::fs::write(&abs_path, new_content)?;
    }

    report.add(Violation {
        path: note.path.clone(),
        rule: "classify".to_string(),
        severity: Severity::Info,
        message: format!(
            "catch-up classified domain={} (method={})",
            result.domain.as_str(),
            result.method.as_str(),
        ),
        fix: None,
    });

    log::info!(
        "catch-up classified {} (domain={}, method={})",
        note.path.display(),
        result.domain.as_str(),
        result.method.as_str(),
    );
    continue;
}

// ... existing inbox promotion logic (unchanged) ...
```

**Important:** `already_in_notes` must be computed at the TOP of the loop body, before the `classify_note()` call. Both the no-signal handler (line ~316) and the low-confidence handler (line ~324) need it to avoid calling `mark_needs_review` on notes already in `notes/`:

```rust
// At top of loop body, before classify_note():
let already_in_notes = note.path.to_string_lossy().starts_with("notes/");

// No signal handler (line ~316):
let result = match classify_note(note, config, search_index) {
    Some(r) => r,
    None => {
        if !is_reclassify && !already_in_notes {
            mark_needs_review(vault_root, note)?;
        }
        continue;
    }
};

// Low confidence handler (line ~324):
if result.confidence == ClassifyConfidence::Low {
    if !is_reclassify && !already_in_notes {
        mark_needs_review(vault_root, note)?;
    }
    log::info!("held for review (low confidence): {}", note.path.display());
    continue;
}
```

Notes in `notes/` that repeatedly fail classification are simply skipped each cycle. They will be retried on subsequent sweeps (the LLM may produce different results), but never marked as needs-review.

In `lint_classify()` (line ~243), make the same filter change:

```rust
let inbox_notes = filter_inbox_notes(notes, false, false);
let unclassified_notes = filter_unclassified_notes(notes);  // NEW
let all_targets: Vec<&Note> = inbox_notes.into_iter()
    .chain(unclassified_notes.into_iter())
    .collect();

for note in &all_targets {
    // ... existing classification logic unchanged ...
}
```

### Implementation Plan

**Phase 1: Cortex catch-up (safety net)**

1. Add `filter_unclassified_notes()` to `classify.rs`
2. Wire it into `lint_classify()` and `apply_classify()`
3. Handle in-place enrichment for notes already in `notes/`
4. Test: run `cortex classify` dry-run, verify it finds the 16 domainless notes
5. Test: run `cortex classify --apply`, verify domains are assigned

**Phase 2: Borg reingest preservation (source fix)**

1. Add `CORTEX_PRESERVE_KEYS` constant, `read_cortex_fields()`, and `patch_cortex_fields()` to `pipeline.rs`
2. Wire into the reingest path between delete and write
3. Test: ingest a URL, let cortex classify it, reingest the same URL, verify domain persists
4. Test: ingest a brand new URL, verify no cortex fields are injected (no-op when old note had none)

Phase 1 first because it immediately fixes all 16 existing notes and provides ongoing protection. Phase 2 prevents the problem at the source.

## Alternatives Considered

### Alternative 1: Borg-only fix (preserve fields, no cortex change)

- **Description:** Only fix borg's reingest to preserve cortex fields
- **Pros:** Single component change; root cause fix
- **Cons:** Does not fix the 6 system-generated notes with no domain; does not handle edge cases where notes end up in `notes/` by other means (manual moves, future tools); requires running borg reingest on all 16 notes to fix them
- **Why not chosen:** Leaves a gap - cortex should be resilient to domainless notes regardless of cause

### Alternative 2: Cortex-only fix (catch-up, no borg change)

- **Description:** Only add catch-up classification in cortex
- **Pros:** Simpler; single component; immediately fixes all existing notes
- **Cons:** Every reingest triggers an unnecessary classify cycle; cortex has to redo work that was already done; domain is temporarily missing between reingest and next cortex sweep (visible in Obsidian views)
- **Why not chosen:** The temporary gap is user-visible and the redundant work is wasteful. Preserving at the source is the proper fix.

### Alternative 3: Full frontmatter merge on reingest

- **Description:** Instead of preserving a fixed list of fields, diff the old and new frontmatter and merge all non-borg fields
- **Pros:** Future-proof; handles any field cortex might add later
- **Cons:** Complex; risk of preserving stale/incorrect fields; hard to reason about which fields "win" in a conflict; borg would need a YAML parser for frontmatter
- **Why not chosen:** Over-engineered. The explicit allowlist (`CORTEX_PRESERVE_KEYS`) is simple, predictable, and easy to extend if cortex adds new fields.

## Technical Considerations

### Dependencies

- No new crate dependencies for either fix
- Both fixes use existing patterns already in the codebase (`read_note_date`/`patch_note_date` for borg, `filter_domain_notes`/`insert_frontmatter_fields` for cortex)

### Performance

- `read_cortex_fields()` reads the file that is already about to be deleted - negligible overhead
- `patch_cortex_fields()` is one additional file read+write, same as `patch_note_date()`
- `filter_unclassified_notes()` is a linear scan of `notes/` that cortex already loads; no extra I/O
- Cortex catch-up only fires for notes with `domain: None`, so it is a no-op once notes are classified

### Testing Strategy

- **Unit tests for `read_cortex_fields()`:** parse various frontmatter shapes; handle missing fields, empty file, no frontmatter
- **Unit tests for `patch_cortex_fields()`:** verify fields are correctly inserted; verify existing fields are replaced not duplicated
- **Unit test for `filter_unclassified_notes()`:** verify it selects notes in `notes/` without domain and ignores inbox, already-classified notes
- **Integration test:** end-to-end reingest scenario in a temp vault
- **Manual verification:** run `cortex classify` after deploy, confirm all 16 domainless notes get classified

### Rollout Plan

1. Implement and test Phase 1 (cortex catch-up)
2. Deploy cortex, run `cortex classify --apply` to fix existing 16 notes
3. Implement and test Phase 2 (borg reingest preservation)
4. Deploy borg (`cargo install --path borg && systemctl --user restart borg`)
5. Verify by reingesting a known-classified URL and checking domain persists

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `patch_cortex_fields` corrupts frontmatter | Low | High | Uses same pattern as proven `patch_note_date`; unit tests cover edge cases |
| Cortex catch-up reclassifies notes to wrong domain | Low | Low | Same classify pipeline used for inbox notes; incorrect classifications can be fixed with `--reclassify-domain` |
| System-generated notes (overviews, usage reports) get classified incorrectly | Medium | Low | These notes likely lack tags for Tier 1 matching; Tier 2 LLM should handle them reasonably; worst case they get `cortex-needs-review` |
| Future cortex field additions not in `CORTEX_PRESERVE_KEYS` | Medium | Low | The constant is a clear, documented allowlist; add new fields to it when cortex adds new metadata |

## Open Questions

- [ ] Should the cortex catch-up run as part of the daemon sweep (auto-apply), or only on manual `cortex classify`? Recommendation: include it in daemon sweeps since it is idempotent.

## References

- `borg/src/pipeline.rs` lines 347-377: reingest detection and old note deletion
- `borg/src/pipeline.rs` lines 2179-2205: `read_note_date()` and `patch_note_date()` - the pattern to follow
- `cortex/src/classify.rs` lines 649-676: `filter_inbox_notes()` - the filter to extend
- `cortex/src/classify.rs` lines 679-696: `build_enrichment_fields()` - the fields cortex writes
- `docs/design/2026-03-21-cortex-classify-promote.md`: original classify pipeline design
- `docs/design/2026-03-23-classify-pipeline-fix.md`: previous classify pipeline fix
