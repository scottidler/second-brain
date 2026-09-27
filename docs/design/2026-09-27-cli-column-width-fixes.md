# CLI Column-Width Fixes

Status: Implemented

No design doc preceded this work; it is a retroactive record of a small bugfix chain plus the reconciled findings from an Architect (Gemini) + Staff Engineer (Codex) implementation audit, written because the audit surfaced a real defect that needs to be tracked to closure.

## Problem

`sb oracle stats` printed misaligned columns for tag/type/status counts. Two distinct bugs:

1. Columns were padded with Rust's `{:<N}` format spec, which pads by Unicode scalar count, not terminal display width. A wide/combining Unicode character in a tag name would misalign the count column after it.
2. The column width itself was a hardcoded guess (15 chars for tags/types/statuses, 10 for schema-gap field names). A real tag, `software-engineering` (20 display columns), exceeded the guess and ran directly into its count.

The same hardcoded-width pattern was found in `borg/src/eval/report.rs`: fixture-name columns in the listicle-survival and note-size FAIL tables were padded to hardcoded 60/55 chars, and fixture names are open-vocabulary session/video-title slugs, not bounded like an enum.

A fourth, separate issue surfaced while verifying the shipped version locally: `sb/build.rs` watched `.git/HEAD`, `.git/refs/`, `.git/packed-refs` and `../.git/packed-refs`, none of which exist (paths resolve relative to `sb/`, and this repo is a worktree whose `.git` is a file). Cargo treats a missing `rerun-if-changed` path as always stale, so the build script reran and `sb` recompiled on **every** build. `GIT_DESCRIBE` was therefore never stale; the cost was needless recompiles. See Corrections.

## What shipped (commits on `main`, ungated repo, since `v0.15.5`)

- `9a14704` / `2b1cc6f` (fmt fixup), tagged `v0.15.6`: `fix(sb): align oracle stats/tool-list columns by display width, not char count`. Added `unicode-width` crate (workspace + `sb/Cargo.toml`). Added `pad_display(s, width)` using `UnicodeWidthStr::width()`. Replaced `{field:<10}`, `{tag:<15}`, `{note_type:<15}`, `{status:<15}` in `print_vault_stats` with `pad_display(..., width)`, and fixed `print_tool_list`'s `name_col` (was `t.name.len()`, byte length) to use `.width()`.
- `8bc6170`, tagged `v0.15.7`: `fix(sb): size oracle stats columns to the longest name, not a hardcoded guess`. Added `col_width(names, floor) -> usize`, computing `max(longest name's display width, floor)` per section, replacing the hardcoded 10/15.
- `55a5987`, tagged `v0.15.8`: `fix(borg): size eval report fixture-name columns to the longest name`. Same pattern in `borg/src/eval/report.rs`'s two fixture tables, using `.chars().count()` (borg has no `unicode-width` dependency; fixture names are ASCII in the current corpus).
- Tagged and pushed through `v0.15.8`, CI green on that SHA (`git ls-remote`/`gh run list` confirmed).

## Review: Architect + Staff Engineer (Implementation Audit, no prior design doc)

### Agreed, no action needed

- All 6 table-width fixes (4 in `oracle.rs`: schema-gaps, by-tag, by-type, by-status; 1 tool-list; 2 in `report.rs`: listicle-survival, note-size FAIL) are correctly implemented and wired end to end. Both reviewers read the live files and traced callers independently; no completeness gaps.
- Independent hardcoded-width sweep (both reviewers, separately): `sb borg log`'s method/status/kind/stage columns, `oracle`'s eval mode-name column, and `cortex`'s asymmetry bucket column are all closed enums well within their widths. `sb borg --help`'s REQUIRED TOOLS table already computes width dynamically (byte-length, but ASCII-only tool names make that safe). `cortex --help`'s REQUIRED TOOLS table is hardcoded to 10 but has exactly one caller (`"fabric"`), so it's safe today, just not general.
- `pad_display`'s ANSI-escape edge case (would overestimate width if fed a colorized string) is dormant: no colorized string reaches these callsites today.
- Empty-list boundary in `col_width` is safe (`.unwrap_or(0).max(floor)`).
- The dynamic-width change means a short failing-fixture list now renders narrower than the old 55/60-char floor (e.g., a lone `video/b` failure pads to 7 chars, not 55). This is an observable behavior change but cosmetic, not a bug: no data, ordering, or exit-code change.

### Disagreement, resolved in the Staff Engineer's favor

- **The first `sb/build.rs` fix (`../.git/HEAD`, `../.git/refs/`) was a no-op.** Cargo does resolve `rerun-if-changed` relative to the package root (`sb/`), as the Architect confirmed, but this repo is a git worktree: `.git` at the workspace root is a file (`gitdir: /home/saidler/repos/scottidler/second-brain/.bare/worktrees/main`), so `../.git/...` still points at nothing. The Staff Engineer caught this; the Architect reasoned from single-crate repos (`otto-rs/otto`, `scottidler/scaffold`) where `.git` is a directory.
  ```
  $ git rev-parse --path-format=absolute --git-path HEAD --git-path refs --git-path packed-refs
  /home/saidler/repos/scottidler/second-brain/.bare/worktrees/main/HEAD
  /home/saidler/repos/scottidler/second-brain/.bare/refs
  /home/saidler/repos/scottidler/second-brain/.bare/packed-refs
  ```

## Corrections (post-audit re-investigation)

Both reviewers and the original session missed that a missing watched path makes Cargo rerun the build script every time, not never. Verified on a throwaway workspace with the same layout: the build-script output changed on every `cargo build`, and `CARGO_LOG=cargo::core::compiler::fingerprint=info` logged `stale: missing ".../c/.git/HEAD"`. Two claims from the session were wrong:

- **"`sb --version` stuck on v0.15.7 because of `build.rs`."** False. The build that preceded that check (`cargo build --release -p sb 2>&1 | tail -60`) failed with `error: could not compile sb`; the reported `exit 0` was `tail`'s. The v0.15.7 binary was a leftover from an earlier build.
- **"`otto install` failed on shared-`target/`-dir contention."** False. The `sweep-repos-watchdog` user service ran `cargo sweep --maxsize 20GB` on this repo's `target/` at 09:00, 09:02, 09:04, 09:06, 09:08, 09:10, 09:13, 09:17, 09:21 and 09:24 PDT (journal), because `/media/saidler/intel-480gb-ssd` sat at its 40GiB floor. Every failed build ran 09:08-09:25 PDT and lost `.rlib`/fingerprint files mid-build. The dotfiles doc `2026-09-27-cargo-target-orphan-sweep.md` records these sweeps in its timeline but does not fix the watchdog sweeping an in-flight build.

### Noted, deferred (pre-existing, out of scope for this fix)

- `borg`'s `kind` column (`{:<12}` in `report.rs`) is theoretically unbounded: fixture directory names flow through `--fixtures` and are not a closed enum (unlike Oracle's mode names). All 8 currently-checked-in kind names fit within 12 chars; a longer custom fixture kind would misalign. Predates these commits, not introduced by them.
- Tool-description wrapping in `print_tool_list` still uses byte length for wrap width, not Unicode-aware. Pre-existing, unrelated to the name-column fix.

## Resolution

1. **`sb/build.rs`:** now shells out to `git rev-parse --path-format=absolute --git-path HEAD --git-path refs --git-path packed-refs` and emits `cargo:rerun-if-changed` only for paths that exist. Correct in a plain checkout, this worktree, and any other worktree name; `sb` no longer recompiles on every build.
2. **Regression tests:** `sb/src/cli/oracle/tests.rs` covers `pad_display` (wide, combining, no-truncate) and `col_width` (longest name over floor, floor for short/empty, wide names). `borg/src/eval/report/tests.rs` asserts the listicle and note-size FAIL value columns line up for names past the old 60/55-char floors.
3. **`borg`'s `kind` column:** `94bbf48` sizes it to the widest of the header, every kind, and the overall row (floor 12); the divider follows. Tests cover the 12-wide default and a kind past 12 chars.
4. **Open, outside this repo:** `sweep-repos-watchdog` can `cargo sweep` a target during a live build (dotfiles).
