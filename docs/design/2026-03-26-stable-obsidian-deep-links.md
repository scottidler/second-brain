# Design Document: Stable Obsidian Deep Links

**Author:** Scott Idler
**Date:** 2026-03-26
**Status:** Implemented
**Review Passes Completed:** 3/3

## Summary

Switch borg's Obsidian deep links from path-based `obsidian://open` URLs to search-based `obsidian://search` URLs so that links in Telegram (and Discord) notifications remain valid after cortex moves notes from `inbox/` to `notes/`.

## Problem Statement

### Background

Borg generates an `obsidian://open?vault=obsidian&file=inbox%2Fmy-note.md` deep link for every ingested note and includes it as a clickable "Open in Obsidian" link in Telegram and Discord notifications. This works on both Ubuntu desktop and Android (Pixel).

Cortex's classify-promote pipeline moves notes from `inbox/` to `notes/` once they pass governance checks. This is the normal, expected lifecycle of a note.

### Problem

The deep link encodes the full vault-relative path (`inbox/my-note.md`). Once cortex moves the note to `notes/my-note.md`, the link breaks - clicking it opens nothing or shows an error. The user has no way to navigate directly to the ingested note from the Telegram notification.

### Goals

- Deep links in notifications survive note moves between `inbox/` and `notes/`
- Links work on Ubuntu (Obsidian desktop) and Android (Obsidian mobile)
- No new plugin dependencies
- No cross-service coordination (borg doesn't need to know about cortex, cortex doesn't need Telegram credentials)

### Non-Goals

- Editing or updating Telegram messages after the fact
- Adding UUID-based deep links (Advanced URI plugin dependency)
- Changing how cortex moves notes
- Deep links for non-Obsidian targets

## Proposed Solution

### Overview

Replace `obsidian://open?vault={vault}&file={relative_path}` with `obsidian://search?vault={vault}&query={filename_stem}`.

The filename stem (e.g., `my-note` from `inbox/my-note.md`) is stable across moves because cortex preserves the filename when relocating notes. The `obsidian://search` URI is a built-in Obsidian protocol - no plugins required.

### Architecture

No architectural changes. This is a single-function modification in `build_obsidian_url` (`pipeline.rs:2150`) with cascading test updates.

### Implementation Plan

**Phase 1: Modify `build_obsidian_url`** (single function change)

Current signature:
```rust
fn build_obsidian_url(vault_name: &str, note_path: &str, vault_root: &str) -> Option<String>
```

New implementation:
```rust
fn build_obsidian_url(vault_name: &str, note_path: &str) -> Option<String> {
    let path = std::path::Path::new(note_path);
    let stem = path.file_stem()?.to_str()?;
    let encoded_vault = urlencoding::encode(vault_name);
    let encoded_query = urlencoding::encode(stem);
    Some(format!("obsidian://search?vault={encoded_vault}&query={encoded_query}"))
}
```

Key changes:
- Extract filename stem from absolute path (no vault-root stripping needed)
- Use `obsidian://search` instead of `obsidian://open`
- Remove `vault_root` parameter entirely - stem extraction doesn't need it
- Update all 7 call sites to drop the `vault_root` argument

**Phase 2: Update call sites**

All 7 call sites in `pipeline.rs` change from:
```rust
let obsidian_url = build_obsidian_url(
    &config.vault.vault_name,
    &note_path.to_string_lossy(),
    &config.vault.root_path,
);
```
to:
```rust
let obsidian_url = build_obsidian_url(
    &config.vault.vault_name,
    &note_path.to_string_lossy(),
);
```

**Phase 3: Update tests**

Update all `build_obsidian_url` tests to expect `obsidian://search?vault=...&query=...` format:
- `test_build_obsidian_url_simple`: expect `query=my-note` (not `file=inbox%2Fmy-note.md`)
- `test_build_obsidian_url_no_trailing_slash`: same input, expect `query=my-note`
- `test_build_obsidian_url_notes_folder`: expect `query=claude-code-guide`
- `test_build_obsidian_url_nested_notes`: expect `query=my-note`
- `test_build_obsidian_url_path_mismatch`: remove (vault-root validation no longer applies)
- `test_build_obsidian_url_vault_name_with_spaces`: expect `query=note`
- Add `test_build_obsidian_url_no_extension`: verify stem extraction handles edge cases

Update notification tests in `notify.rs` and `discord.rs` that assert on URL format.

## Alternatives Considered

### Alternative 1: Cortex edits Telegram messages after moving notes
- **Description:** Cortex calls Telegram's `editMessageText` API to update the deep link path from `inbox/` to `notes/`
- **Pros:** Links remain `obsidian://open` (direct open, no search step)
- **Cons:** Cortex needs Telegram credentials; requires persisting message IDs; Telegram has a 48-hour edit window; couples cortex to borg's notification system
- **Why not chosen:** Adds significant complexity and cross-service coupling for marginal UX improvement

### Alternative 2: Advanced URI plugin with UUID in frontmatter
- **Description:** Add a `uid: <uuid>` field to note frontmatter at ingestion; use `obsidian://advanced-uri?vault=obsidian&uid=<uuid>`
- **Pros:** Direct open, path-independent, most robust
- **Cons:** Requires Advanced URI plugin on all devices; adds frontmatter field; plugin is community-maintained
- **Why not chosen:** Plugin dependency on both Ubuntu and Android; search URI achieves the goal with zero dependencies

### Alternative 3: Use filename only in `obsidian://open`
- **Description:** Use `obsidian://open?vault=obsidian&file=my-note` (bare filename, no directory)
- **Pros:** Direct open if it worked
- **Cons:** Obsidian's `open` treats `file` as a vault-relative path, not a search - a bare filename only resolves if the note is at the vault root, not in subdirectories
- **Why not chosen:** Does not work for notes in `inbox/` or `notes/` subdirectories

## Technical Considerations

### Dependencies

No new dependencies. `urlencoding` is already in use.

### Performance

No impact. String manipulation only.

### Security

The filename stem is URL-encoded, preventing injection into the URI.

### Testing Strategy

- Unit tests for `build_obsidian_url` with various path patterns
- Unit tests for `format_telegram_reply` and `format_discord_reply` asserting the new URL format
- Manual verification: ingest a note via Telegram, click the link on Ubuntu and Android, confirm it opens Obsidian search with the correct note

### Rollout Plan

1. Implement and test locally
2. `otto ci` to verify all tests pass
3. `cargo install --path borg && systemctl --user restart borg`
4. Send a test URL via Telegram and verify on both devices

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Filename collision (two notes with same stem) | Low | Low | Borg generates unique slugified titles from content; search shows all matches, user picks correct one |
| `obsidian://search` not supported on Android | Low | High | Obsidian mobile supports `obsidian://` URIs including search; verify during rollout |
| Search query matches note content, not just filename | Low | Low | Filename stems are specific enough (slugified article titles) to rank the target note first |
| Some Android apps strip custom URI schemes from links | Low | Medium | Telegram preserves `obsidian://` in HTML `<a>` tags; already working for `obsidian://open` today |

## Open Questions

- [ ] Confirm `obsidian://search` works on Obsidian mobile (Android) - expected yes, verify during testing

## References

- Current implementation: `borg/src/pipeline.rs:2150` (`build_obsidian_url`)
- Notification formatting: `borg/src/notify.rs:93` (`format_telegram_reply`)
- Discord formatting: `borg/src/discord.rs:55` (`format_discord_reply`)
- Obsidian URI docs: https://help.obsidian.md/Extending+Obsidian/Obsidian+URI
