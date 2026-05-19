# Design Document: Fabric Pattern Resolve, Hard-Failure DLQ, and Asset Sync

**Author:** Scott Idler
**Date:** 2026-05-18
**Status:** Draft

## Summary

Between 2026-05-16 20:34 and 2026-05-18 09:17, borg published 8 notes whose body is `[fabric-error]\n\n<transcript snippet>` instead of a real summary. The borg ledger marked all 8 as ✅; the DLQ stayed empty; the user only noticed when reading the Aerion note (`tg-1774af`) and seeing the sentinel. Three independent defects combine to cause this: the Phase 9c L2 distillers refactor dropped the pattern-name → file-path resolver, `otto deploy` does not sync `borg/patterns/*.md` to `~/.config/borg/patterns/`, and the distill stage publishes degraded notes instead of routing hard failures to DLQ. This doc proposes four targeted fixes plus a one-shot cleanup of the 8 damaged notes.

## Problem Statement

### What broke and when

`dc51970` (2026-05-16 17:58 "Phase 8 cleanup + mark L2 design doc Implemented") was the cutover where the Phase-3-through-Phase-6 distillers became the publish path for article / repo / thread / video / image ingests. From that moment forward, every URL ingest of those kinds hits the regression. First `fallback=fabric-error` log line: 2026-05-16 20:34. 8 ingests across 2026-05-16 / 17 / 18 all produced degraded notes.

### Defect 1 - Phase 9c dropped `resolve_pattern`

Pre-Phase-9c, `borg::fabric::run_pattern("distill-article", ...)` called `resolve_pattern("distill-article")` (`borg/src/fabric.rs:9-22`), which mapped the bare name to a path like `/home/saidler/.config/borg/patterns/distill-article.md` (only when the file existed with the literal name; missing the `.md` fallback) and forwarded the resolved *path* to `vault::fabric::run_pattern`. Fabric accepts both pattern names and file paths via `-p`, so `fabric -p /path/to/distill-article.md` worked.

Phase 9c introduced `distillers::FabricShell::call` (`distillers/src/fabric.rs:50-75`), which shells `vault::fabric::run_pattern` **directly**, bypassing `borg::fabric::run_pattern` and therefore `resolve_pattern`. Pattern names like `"distill-article"` (per the `PATTERN` consts in `distillers/src/article.rs:19`, `repo.rs:22`, `thread.rs:23`, etc.) go straight to `fabric -p distill-article`, fabric looks in its own configured patterns dir, finds nothing, and returns:

```
could not get pattern distill-article: pattern 'distill-article' not found.
Run 'fabric -l' to see available patterns
```

`resolve_pattern` is now dead code on the path that matters.

### Defect 2 - `otto deploy` does not sync patterns or shared config

`.otto.yml:219-234` `deploy` task builds binaries, installs them to `~/.cargo/bin/`, and restarts any matching systemd user units. It does **not** copy `borg/patterns/*.md` to `~/.config/borg/patterns/` or `config/*.yml` to `~/.config/second-brain/`. Both are documented as manual `cp` steps in CLAUDE.md that nobody runs.

The result: `~/.config/borg/patterns/` is missing 6 files added to source-of-truth in Phases 3-6 (Mar-Apr 2026) - `distill-article.md`, `distill-repo.md`, `distill-thread.md`, `distill-video.md`, `distill-video-chunk.md`, `distill-video-reduce.md`. Even with Defect 1 fixed, `resolve_pattern` would still find nothing for these names.

### Defect 3 - hard distill failures publish instead of DLQ

`distillers/src/validate.rs:3-5`:

```rust
//! The pipeline never gates on validation: degraded `Distilled`s always
//! publish so the user can see something in the vault and the staged
//! artifact preserves enough breadcrumbs for replay.
```

`distill_for_publish_*` in `borg/src/stages/distill.rs` returns `Distilled` unconditionally. When the distiller returns a `fallback_distilled("fabric-error", ...)` with `summary = "[fabric-error]\n\n<snippet>"`, the pipeline renders that to a note body and writes it. The ledger row is ✅ because the note exists. The intake → ledger XOR DLQ invariant (`borg audit`) cannot see the degradation.

This design conflates two failure classes:

| Class | Examples | Distilled exists? | Today | Should be |
|---|---|---|---|---|
| Hard | `fabric-error`, `fabric-timeout`, `yaml-parse-error`, `missing-summary`, `empty-transcript`, `chunk-failures`, `dispatch-error` | No - only a sentinel | Publish degraded note | Halt + DLQ |
| Soft | `empty-claims` (article >500 words) | Yes - summary/claims/tags exist; one validation canary tripped | Publish with WARN log | Unchanged |

### Goals

- `fabric -p distill-article` (or any of the 12 L2 pattern names) resolves to the correct `~/.config/borg/patterns/<name>.md` on every distiller call path, not just the legacy `borg::fabric::run_pattern` path.
- `otto deploy` is the single command that gets a developer machine into a working state. After running it, `~/.config/borg/patterns/` and `~/.config/second-brain/` match source-of-truth.
- A distill stage that produces a hard-failure sentinel never publishes a note. The trace_id is routed to `borg-dlq.md` with `stage=distill, reason=<fallback_reason>`. Staged artifacts at `~/.local/share/borg/stages/<trace>/distilled.yml` continue to preserve breadcrumbs for replay.
- The 8 currently-damaged notes are deleted from the vault and their traces moved from ledger ✅ → DLQ so `borg replay` (when implemented per `2026-04-19-staged-ingestion-pipeline.md`) or a manual re-ingest can re-process them.

### Non-Goals

- Restructuring the install layout to use fabric's native `~/.config/fabric/patterns/<name>/system.md` directory form. Second-brain owns its install paths and treats fabric as a shell that accepts file paths via `-p`. See [[feedback_self_contained]].
- Shipping systemd unit files, config templates (`borg.yml.example`), or a bootstrap script. The broader deploy story is deferred to a separate design doc (`project_deploy_debt` flagged "soon" 2026-05-18). This doc closes only the patterns-sync and shared-config-sync gaps.
- Implementing `borg replay` to re-process the staged artifacts of the 8 damaged ingests. The cleanup is one-shot manual for now.
- Changing soft-canary behavior (`empty-claims`, future similar validators). Soft canaries continue to publish with WARN.

## Proposed Solution

### Fix 1 - Move `resolve_pattern` into `vault::fabric::run_pattern` with `.md` fallback

In `vault/src/fabric.rs`, add a private `resolve_pattern(name) -> String` (or extend `resolve_binary`'s file) and call it from `run_pattern` before the `Command::new(...)` line. Logic:

```rust
fn resolve_pattern(name: &str) -> String {
    // Path-like inputs pass through unchanged.
    if name.starts_with('/') || name.starts_with('.') || name.starts_with('~') {
        return name.to_string();
    }
    let Some(home) = dirs::home_dir() else { return name.to_string() };
    let base = home.join(".config/borg/patterns");
    // Try literal name first (e.g. "condense.md" already has .md),
    // then with .md appended (e.g. "distill-article" -> "distill-article.md").
    for candidate in [base.join(name), base.join(format!("{name}.md"))] {
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }
    // Fall back to the bare name so fabric's own resolution can try.
    name.to_string()
}
```

Delete the existing `resolve_pattern` and the wrapper-only `borg::fabric::run_pattern` if nothing else uses them; verify by grep. Distillers' `FabricShell` inherits the resolver because it calls `vault::fabric::run_pattern` directly.

### Fix 2 - Halt distill stage on hard-failure reasons, route to DLQ

Define in `distillers/src/validate.rs`:

```rust
pub const HARD_FAILURE_REASONS: &[&str] = &[
    "fabric-error",
    "fabric-timeout",
    "yaml-parse-error",
    "missing-summary",
    "empty-transcript",
    "chunk-failures",
    "dispatch-error",
];

pub fn is_hard_failure(reason: &str) -> bool {
    HARD_FAILURE_REASONS.contains(&reason)
}
```

In each `distill_for_publish_*` in `borg/src/stages/distill.rs`, after the `info!` log line, inspect `distilled.meta.validation.fallback_reason`. If set and `is_hard_failure`, return `Err(eyre!("distill hard failure: {reason}"))` instead of returning the `Distilled`. The `distilled.yml` is still persisted before the error returns - the staged artifact remains intact for replay.

Change the function signatures from `pub async fn ... -> Distilled` to `pub async fn ... -> Result<Distilled>`. Each of the 9 call sites in `pipeline.rs` gets a `?` appended. The existing pipeline-boundary failure handler (post-`2026-05-08-borg-pipeline-resilience.md`) already writes a DLQ row on `Err`; the reason string from the `eyre::Error` becomes the DLQ `Reason` cell.

Update `validate.rs:3-5` comment to reflect the new policy: hard failures halt, soft canaries publish with WARN.

### Fix 3 - `otto deploy` syncs assets

In `.otto.yml:219-234`, add to the `deploy` task before the systemd restart line:

```bash
# Sync borg patterns from source-of-truth.
mkdir -p "$HOME/.config/borg/patterns"
cp -f borg/patterns/*.md "$HOME/.config/borg/patterns/"

# Sync shared config from source-of-truth.
mkdir -p "$HOME/.config/second-brain"
cp -f config/*.yml "$HOME/.config/second-brain/"
```

Remove the manual `cp` lines from CLAUDE.md's "Install (for /shipit)" section; replace with "run `otto deploy`."

`cp -f` (not `rsync --delete`) because we don't want to delete a stray local pattern a developer may be testing. Drift detection is a separate concern for the deploy-story design doc.

### Fix 4 - Clean up the 8 damaged notes

One-shot manual cleanup, executed once after fixes 1-3 land and patterns are synced:

1. List affected traces from the ledger: grep `borg-ledger.md` for any row pointing at a note whose body contains `[fabric-error]`. Expected set: `tg-93d54e`, `tg-19f821`, `tg-8fd106`, `tg-af403a`, `tg-1774af`, `tg-319a18`, `ht-026281`, `ht-c618aa` (plus the two `chunk-failures` traces `ht-fd20d3`, `ht-afb6cf` for video distillers - same hard-failure class).
2. For each affected trace: delete the vault note, remove its row from `borg-ledger.md`, append a row to `borg-dlq.md` with `stage=distill, reason=fabric-error` (or `chunk-failures` for the two video ones).
3. Re-ingest by replaying the source URL through Telegram / `borg http` as if it were a fresh submission. Once `borg replay` (per the staged-pipeline design doc) ships, this becomes a one-liner; for now it's manual.

The staged artifacts at `~/.local/share/borg/stages/<trace>/` are not deleted - they remain available for forensics and future `borg replay`.

## Test Plan

- **Unit:** `vault/src/fabric.rs::tests` adds cases for `resolve_pattern`: bare name with `.md` file present, bare name with no extension file present, path-like input passes through, missing file falls back to bare name.
- **Unit:** `distillers/src/validate.rs::tests` adds cases for `is_hard_failure` covering each reason in `HARD_FAILURE_REASONS` and a few negatives (`empty-claims`, `none`).
- **Integration:** `borg/src/stages/distill/tests.rs` adds a test that injects a `FakeFabric` returning an error, calls `distill_for_publish_article`, and asserts the function returns `Err` with the reason `fabric-error` in the message.
- **Manual smoke:** after `otto deploy`, verify `ls ~/.config/borg/patterns/` shows all 14 source-of-truth files (no missing distill-*). Submit a fresh URL via Telegram, confirm the published note has a real `## Summary` section (not `[fabric-error]`). Submit a URL with fabric temporarily renamed (so `which fabric` fails) and confirm the trace lands in DLQ with `reason=dispatch-error`.

## Open Questions

1. **`chunk-failures` as hard vs partial.** Current code returns `fallback_distilled("chunk-failures", ...)` only when **all** chunks fail, so it's effectively hard - treating it as hard in this doc. If a future change wants partial publish (e.g. 7/10 chunks succeeded), the classification needs to move from a constant list to a per-distiller decision. Flag for the staged-pipeline design doc.
2. **Should `borg audit` learn about the hard-failure reasons?** Once Fix 2 lands, the audit walk continues to enforce intake ↔ (ledger XOR DLQ). It will naturally pick up the new DLQ rows. No new code needed in `borg audit`. Reconfirm during implementation.
3. **Damaged-note cleanup atomicity.** The Fix 4 sequence (delete note, edit ledger, edit DLQ) is three filesystem writes. If interrupted, the vault could end up in a half-cleaned state. For 8 notes this is acceptable manual risk. Worth noting if the count were larger.

## Rollout

Single phase, no gating (per `feedback_no_phase_gating`). Order within the phase:

1. Fix 1 (resolver) + Fix 2 (halt + DLQ) land in one commit; `cargo test --workspace` passes.
2. Fix 3 (`otto deploy` syncs) lands in the same PR.
3. `otto deploy` is run. Verify `~/.config/borg/patterns/` is complete.
4. Submit a fresh article URL; verify clean publish.
5. Fix 4 (cleanup of 8 notes) executed manually.
6. `git commit` the vault changes (ledger/DLQ edits, deleted notes).

Memory updates: [[project_halt_on_hard_distill]] already captures the design decision; nothing to update post-implementation. [[project_deploy_debt]] stays "imminent next thread" - this doc does not close it.
