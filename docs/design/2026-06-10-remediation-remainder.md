# Design Document: Remediation Remainder

**Author:** Scott Idler
**Date:** 2026-06-10
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

The post-implementation adherence audit of `docs/design/2026-06-09-code-review-remediation.md` (15 phases, ~140 items) found three residuals: the Phase 14 inline-test-mod extraction sweep was deferred (47 files still carry inline `mod tests {}` blocks), the Phase 8 borg `config.rs` per-transport submodule split was silently dropped (only the test extraction landed), and two production sites still compare schema values against string literals. This doc closes all three. Everything here is mechanical relocation or one-line substitution; no behavior changes.

## Problem Statement

### Background

The 2026-06-09 remediation marked its design doc `Status: Implemented` with one disclosed deferral: "Item 5 (inline `#[cfg(test)] mod tests` extraction sweep) NOT done in this commit... tracked as the one remaining Phase 14 item and is best done as a focused mechanical pass." This is that pass. The adherence audit (10 parallel verification agents, 2026-06-10) confirmed the deferral and surfaced two more residuals the notes did not disclose.

### Problem

1. **47 files carry inline `#[cfg(test)] mod tests {}` blocks** in direct violation of the rust.md test-placement rule (tests live in sibling `tests.rs` files, extracted on sight): vault 11, borg 19, cortex 17. oracle, sb, and distillers are already clean.
2. **borg `config.rs` (1034 lines) was never decomposed.** The remediation doc's Phase 8 item had two halves: extract `config/tests.rs` (done) and "split the per-transport config structs into `config/` submodules" (not done; `borg/src/config/` contains only `tests.rs`).
3. **Two production sites still match schema values by string literal**, the exact class Phase 6 ("schema is law") eliminated elsewhere:
   - `cortex/src/memgraph.rs:79` - `matches!(note.frontmatter.origin.as_deref(), Some("assisted"))`
   - `borg/src/backfill.rs:165` - `origin.as_deref() != Some("assisted")`

### Goals

- Zero files with inline `mod tests {}` blocks across the workspace; verified by a grep gate.
- Per-file test counts preserved exactly (nothing dropped, nothing duplicated in the move).
- borg transport config structs each live in their own `config/` submodule; every existing `crate::config::*` path keeps working unchanged.
- No schema-value string literals remain in production code; `vault::schema` enums are the only source.

### Non-Goals

- No test rewrites, renames, or improvements; this is relocation only. A test that was weak inline is the same weak test in `tests.rs`.
- No decomposition of borg `config.rs` beyond the per-transport structs the original doc named (YoutubeConfig, StagingConfig, PipelineConfig, etc. stay put; the file is under the 1500 gate).
- No new lint/CI tooling to police test placement or literals; the grep gates below are run as verification steps, not installed as pipeline stages.
- Test fixtures and cross-crate test-support helpers keep their string literals (rust.md exempts tests). Specifically exempt: `vault/src/search/vector.rs::insert_test_note_full`, `vault/src/search/graph.rs::insert_test_note_graph` (pub test-support helpers used by other crates' tests, so they cannot be `#[cfg(test)]`), and `oracle/src/server.rs:443` (`"unread"` there is a JSON output key, not a schema comparison).

## Proposed Solution

### Overview

Five phases: the two one-line literal fixes first (smallest, ships the only semantic-adjacent change alone), the borg config split second (touches the same crate the borg sweep will touch, so it lands before that churn), then the test-mod sweep as three per-crate phases (vault, borg, cortex) so each lands as its own commit with its own CI gate and test-count parity check.

### Architecture

No new components. The structural pattern for both relocations is the one this workspace already uses:

- **Test extraction:** `src/foo.rs` keeps a trailing `#[cfg(test)] mod tests;` declaration; the block body moves verbatim to `src/foo/tests.rs`. This is the existing shape of `borg/src/fabric/tests.rs`, `borg/src/telegram/tests.rs`, `borg/src/config/tests.rs`, `cortex/src/classify/tests.rs`, and every distillers module. Because `tests.rs` is the same child module the inline block was, `use super::*;` and access to parent privates are unchanged; the move cannot alter visibility.
- **Config split:** `borg/src/config.rs` gains `mod telegram; mod discord; mod signal; mod ntfy; mod desktop;` plus `pub use` re-exports, so `crate::config::TelegramConfig` etc. resolve exactly as before. Each submodule holds one transport's config struct and its private serde-default fns.

### Data Model

None. No struct, schema, or serde changes; serde attributes move verbatim with their structs.

### API Design

None. `pub use` re-exports keep every existing path (`borg::config::TelegramConfig`, `borg::config::SignalConfig`, ...) byte-for-byte compatible; zero call-site edits anywhere in the workspace.

### Implementation Plan

#### Phase 1: Schema literal stragglers
**Model:** sonnet

- [ ] `cortex/src/memgraph.rs:79` - `is_ingested` compares via `vault::schema::Origin::Assisted.as_str()` instead of the `"assisted"` literal.
- [ ] `borg/src/backfill.rs:165` - same substitution (the surrounding source-presence logic is untouched).
- [ ] Workspace sweep to confirm no other production comparisons remain: `grep -rn '"assisted"\|"unread"' */src --include='*.rs'` filtered to non-test code must show only `vault/src/schema.rs` (the enum definitions), the two exempt test-support helpers, and the oracle JSON key.

#### Phase 2: borg config per-transport submodules
**Model:** sonnet

- [ ] Create `borg/src/config/{telegram,discord,signal,ntfy,desktop}.rs`; move each transport's struct and its private default fns verbatim:
  - `telegram.rs` - `TelegramConfig`
  - `discord.rs` - `DiscordConfig`
  - `signal.rs` - `SignalConfig` + `default_signal_rate_threshold`
  - `ntfy.rs` - `NtfyConfig` + `default_ntfy_server`
  - `desktop.rs` - `DesktopConfig` + its `impl Default`
- [ ] Each submodule also takes along any private serde-default fns only it references (the two named above are the known ones; sweep the moved structs' `#[serde(default = "...")]` attributes for stragglers).
- [ ] `pub(crate) const APP_NAME` (config.rs:9) does NOT move: it is shared by `DesktopConfig::default`, the config-path fallback, and `lib.rs`'s notification path (Phase 9 of the parent doc deliberately unified on it). `desktop.rs` references it as `super::APP_NAME`.
- [ ] `config.rs` declares the five modules and `pub use`-re-exports each struct, keeping all existing paths working with zero call-site changes.
- [ ] `config/tests.rs` stays where it is; any of its tests that referenced the moved structs continue to work via the re-exports (they are in scope through `use super::*`).
- [ ] Verify: `cargo test -p borg` test count identical to the pre-phase baseline; `otto ci` exit 0.

#### Phase 3: Inline test-mod extraction - vault (11 files)
**Model:** sonnet

- [ ] For each of: `canonical.rs`, `config.rs`, `detail.rs`, `fabric.rs`, `frontmatter.rs`, `hygiene.rs`, `ledger.rs`, `note.rs`, `schema.rs`, `trace.rs`, `watcher.rs` - cut the inline `#[cfg(test)] mod tests { ... }` block body into `vault/src/<name>/tests.rs` and leave `#[cfg(test)] mod tests;` behind. Pure relocation: no reformatting, no renames, no edits inside the block.
- [ ] Only the `mod tests` block moves. Any standalone `#[cfg(test)]`-gated helpers outside the block stay in place. Each of the 47 files carries exactly one inline `mod tests` block; if a second `#[cfg(test)]` mod surfaces anywhere, move it the same way under its own name.
- [ ] Verify: `cargo test -p vault` count matches baseline; `otto ci` exit 0.

#### Phase 4: Inline test-mod extraction - borg (19 files)
**Model:** sonnet

- [ ] Same mechanics for: `assets.rs`, `backoff.rs`, `description.rs`, `discord.rs`, `extension/schema.rs`, `extraction.rs`, `health.rs`, `hygiene.rs`, `jina.rs`, `markdown.rs`, `migrate.rs`, `ntfy.rs`, `ocr.rs`, `pipeline/atomic.rs`, `pipeline/inflight.rs`, `quality.rs`, `router.rs`, `types.rs`, `youtube.rs`. Nested modules nest naturally (`extension/schema.rs` -> `extension/schema/tests.rs`).
- [ ] Verify: `cargo test -p borg` count matches baseline; `otto ci` exit 0.

#### Phase 5: Inline test-mod extraction - cortex (17 files)
**Model:** sonnet

- [ ] Same mechanics for: `autotag.rs`, `daemon.rs`, `duplicates.rs`, `fabric.rs`, `frontmatter.rs`, `intel.rs`, `lib.rs`, `linking.rs`, `links.rs`, `llm.rs`, `migrate.rs`, `naming.rs`, `quality.rs`, `scope.rs`, `state.rs`, `sweep.rs`, `tags.rs`. The `lib.rs` block moves to `cortex/src/tests.rs` (the sibling a crate-root `mod tests;` resolves to). `sweep.rs`'s `#[serial_test::serial(xdg_data_home)]` attributes move verbatim with their tests.
- [ ] Verify: `cargo test -p cortex` count matches baseline; `otto ci` exit 0.
- [ ] Final workspace gate: `grep -rln 'mod tests {' */src --include='*.rs' | grep -v '/tests.rs'` returns nothing.

## Alternatives Considered

### Alternative 1: Leave the inline test mods (rescind the sweep)
- **Description:** Accept the 47 files as-is; the tests run fine where they are.
- **Pros:** Zero churn, zero risk.
- **Cons:** Permanent violation of the rust.md test-placement rule; the 2026-06-09 doc's own no-deferments rule already rejected this once, and the deferral was explicitly tracked as remaining work.
- **Why not chosen:** The rule exists and the debt is named; a focused mechanical pass is exactly what the deferral note prescribed.

### Alternative 2: One mega-commit for all 47 files
- **Description:** Run the whole sweep plus the config split as a single commit.
- **Pros:** One CI round, one commit message.
- **Cons:** A compile break or dropped test anywhere is hard to bisect across 50+ file moves; per-crate phases keep each diff reviewable with `git diff --color-moved`.
- **Why not chosen:** The 2026-06-09 notes flagged exactly this: bundling mass file-moves is how silent regressions get introduced. Per-crate commits with count parity are cheap insurance.

### Alternative 3: Single `config/transports.rs` instead of per-transport files
- **Description:** Move all five transport structs into one submodule file.
- **Pros:** Fewer files (one instead of five).
- **Cons:** Compound concerns in one file; the original Phase 8 item said "submodules" (plural), and each transport's config already evolves independently (signal grew a rate-threshold default, ntfy a server default).
- **Why not chosen:** Per-transport files are each single-word names, match the original doc's intent, and mirror how the transports themselves live in separate modules.

### Alternative 4: Add a CI lint to enforce test placement and ban schema literals
- **Description:** Install a grep-based otto task failing on inline `mod tests {` or `"assisted"`-style literals.
- **Pros:** Prevents recurrence mechanically.
- **Cons:** The literal ban needs an exemption list (test-support helpers, JSON keys, the schema enum itself) that would rot; the test-placement grep is trivially run ad hoc and rust.md already states the rule for sessions to follow.
- **Why not chosen:** Verification-step grep gates give the one-time guarantee this doc needs without adding pipeline machinery to maintain. Revisit if either class recurs.

## Technical Considerations

### Dependencies

None added, none removed. `serial_test` is already a workspace dependency used by the moving cortex tests.

### Performance

None. `#[cfg(test)]` code does not exist in release builds either way; the config split is compile-time module structure only.

### Security

None. No runtime behavior changes.

### Testing Strategy

The tests being moved ARE the artifact under protection, so the verification is parity, not new coverage:

1. **Baseline capture before each sweep phase:** record the per-crate totals via `cargo test -p <crate> 2>&1 | grep 'test result'` (every harness's `N passed; M failed` summary line, including doc-tests).
2. **Post-move parity:** identical totals after the move. Any delta means a test was dropped or duplicated; fix before commit.
3. **Relocation-only diffs:** review each phase with `git diff --color-moved=dimmed-zebra`; non-moved (added/changed) lines should be limited to the `#[cfg(test)] mod tests;` declarations, `use` adjustments at the top of new `tests.rs` files if rustfmt requires them, and the config re-export block.
4. **Phase 1 sweep check and Phase 5 grep gate** as listed in their phases.
5. `otto ci` (lint + bloat + clippy + fmt + test) gates every phase; verify by exit code.

### Rollout Plan

Pure refactor of test/code placement: per-phase commits on main, shipped with the standard flow once all five phases land (`otto ci` -> commit per phase -> `bump` -> push with tags -> `otto install`). No daemon behavior changes, but the borg/cortex binaries are rebuilt, so the usual `systemctl restart borg cortex` applies at install. No extension re-sign (nothing touches `IngestRequest` or the extension). No vault or receipts migrations.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| A test silently dropped or duplicated during a move | Low | Med | Per-crate test-count parity against captured baselines; `--color-moved` diff review |
| Path-sensitive macros (`include_str!` etc.) break when test code changes directories | Low | Low | Audited up front: no `include_str!`/`include_bytes!` in any of the 47 inline test mods (only `borg/src/receipts.rs` production code uses it, and it is not in the sweep) |
| Config split changes serde behavior or a default | Low | Med | Structs and default fns move verbatim; existing `config/tests.rs` and the Phase 13 template-parse test (`serde_yaml::from_str::<Config>(TEMPLATE)`) pin the wire format |
| A `pub use` re-export misses a name and breaks a downstream path | Low | Low | `cargo check --workspace` catches any missed re-export at compile time |
| Inline test mod references a sibling private item in a way that breaks across the move | Low | Low | It cannot: `foo/tests.rs` is the same child module as the inline block, with identical visibility; any failure is a compile error, not a silent change |

## Open Questions

- [ ] None. All three items have a single obvious mechanical shape; the only judgment call (per-transport files vs one transports.rs) is decided above.

## References

- `docs/design/2026-06-09-code-review-remediation.md` - parent doc (Phase 8 item 1, Phase 6 literal sweep, Phase 14 item 5)
- `docs/design/2026-06-09-code-review-remediation-implementation-notes.md` - the Phase 14 deferral disclosure this doc discharges
- Adherence audit, 2026-06-10 (this session): 10-agent verification pass that sized the 47-file gap and found the two undisclosed residuals
- `~/repos/.claude/rules/rust.md` - test-placement rule (sibling `tests.rs`, extract on sight)
- `~/repos/.claude/rules/general.md` - naming conventions (single-word module files)
