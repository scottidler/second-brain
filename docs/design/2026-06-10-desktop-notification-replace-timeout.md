# Design Memo: Desktop Notification Replace-Timeout

**Author:** Scott Idler
**Date:** 2026-06-10
**Status:** Implemented

## Summary

borg's desktop notification sink (`borg/src/notify.rs`) shows a "processing"
placeholder popup at ingest dispatch and later **replaces it in place** with a
"done" popup by reusing the same D-Bus notification id. The placeholder is
created with display timeout = `cfg.timeout_ms` (default **5000ms = 5s**). For
any ingest longer than 5s - i.e. effectively every YouTube video - the
notification daemon expires and closes the placeholder after 5s and frees its
id. When `result()` later replaces by that freed id, the daemon returns
`org.freedesktop.DBus.Error.InvalidArgs: Invalid notification ID`. The code
catches this and falls back to a fresh popup, so no notification is lost, but
(a) it logs a guaranteed-on-every-video **WARN**, and (b) the in-place replace
UX never actually happens for video. We need to decide the placeholder's
timeout.

## Problem Statement

### Background

- `Desktop::processing(trace, desc)` (`notify.rs:230`) shows the placeholder
  with `self.timeout = Timeout::Milliseconds(cfg.timeout_ms)` and returns the
  `NotificationHandle`.
- `Desktop::result(result, src, prior)` (`notify.rs:277`) tries
  `n.id(prior_id).show_async()` to replace in place; on error it logs WARN and
  falls back to a fresh popup (`show(..., None, ...)`).
- `cfg.timeout_ms` default = 5000 (`borg/src/config/desktop.rs`,
  `DesktopConfig::default`).

### The bug

`timeout_ms` is being used as the lifetime of BOTH the transient placeholder
(which must survive until the pipeline finishes, tens of seconds to minutes)
AND the terminal "done" toast (which should auto-dismiss after ~5s). The 5s
value is correct for the latter and wrong for the former. The placeholder dies
mid-pipeline, its id is freed, and the replace fails with `Invalid notification
ID` on every video ingest. The WARN-level log on a routine, fully-handled path
is also log-hygiene noise (the rest of the codebase reserves WARN for genuinely
recoverable-but-unexpected failures).

## Evidence (measured, not assumed)

From the receipts DB (`~/.local/share/sb/borg/receipts.db`), end-to-end
duration = `terminal_at - received_at` across **217 succeeded YouTube ingests**:

- min **0s**, avg **75s**, **max 580s (9m 40s)**
- **7 ingests exceeded 300s**: 580, 421, 407, 385, 371, 370, 319 s.

The long tail is not purely content length: it includes **heavy-permit queue
wait** (`pipeline.max_concurrent_heavy_traces`, default 4, `config.rs:192`), so
a batch of long videos pushes later items' total window up regardless of any
single video's length. The pipeline's own hard ceiling is
`pipeline.hard_timeout_secs` (default **1800s**, `config.rs:224`); the watchdog
kills anything beyond it, and on that kill `result()` still fires (a pipeline
*failure* still produces an `IngestResult` and replaces the popup).

## Options

1. **`Timeout::Never` for the placeholder.** Keep `cfg.timeout_ms` for the
   terminal toast only. Replace always succeeds (id never freed by timeout).
   Cost: if the process hard-crashes (SIGKILL / panic / daemon restart) between
   dispatch and `result()`, the placeholder hangs until the user dismisses it.
   (Normal pipeline *failures* are unaffected - they still call `result()`.)
2. **Bind placeholder timeout to `pipeline.hard_timeout_secs`.** Always >= any
   real pipeline duration (the watchdog enforces it), so the replace always
   succeeds, AND an orphaned placeholder self-clears after that window on a true
   crash. Cost: thread the pipeline hard-timeout value into the Desktop sink at
   construction (couples two config sections).
3. **Fixed 600s.** Covers 100% of measured history (max 580s) - zero failures
   to date. Cost: 20s margin over the observed max; the queue-driven tail is not
   bounded by content length, so a large batch or a multi-hour source can exceed
   it; magic number (would need a named const / config field per rust.md).
4. **Fixed 300s (status quo intent).** Disproven: 7 historical ingests exceeded
   it; reintroduces the exact failure for ~3% of past ingests.

Independent of the timeout choice: **demote the replace-failure WARN to
`debug!`** - the fresh-popup fallback is the real safety net and even the
crash-orphan case is not WARN-worthy.

## Recommendation

`Timeout::Never` for the placeholder (Option 1), or Option 2 if the
orphan-on-crash cleanup is judged worth the config coupling. Reject fixed
numbers: 300s is disproven and 600s is a thin bet against a queue-driven tail.
Demote the WARN regardless.

## Decision (reviewed by /architect + /staff-engineer, 2026-06-10)

**Chosen: Option 1.** The placeholder is shown with `Timeout::Never`
(`PLACEHOLDER_TIMEOUT` const); `cfg.timeout_ms` governs the terminal toast
only. The replace-attempt failure WARN is demoted to `debug!` *only* on the
path that has a fresh fallback (keyed off `prior_id.is_some()` in
`Desktop::show`); fresh/no-prior popup failures and all D-Bus timeouts stay
`WARN`.

Both reviewers converged on Option 1. Staff Engineer (Codex) added a correction
that strengthens it: **Option 2 is not actually bounded.** The placeholder is
shown before `process_content`, which blocks on the general permit *before* the
handler hard-timeout clock starts (`pipeline.rs:89,93`), and the watchdog skips
permit-queued traces (`watchdog.rs:29`) - so the placeholder->result wall-clock
can exceed `hard_timeout_secs` under queue saturation. That makes Option 2 the
same failure class as the fixed numbers; only `Never` is correct by
construction. Orphan-on-true-crash (process SIGKILL/panic before `result()`) is
the only cost and is a dismissible local-desktop artifact.

## Questions for reviewers

1. Is `Timeout::Never` on a transient placeholder an anti-pattern given the
   orphan-on-hard-crash case, or is that risk acceptable for a single-host
   desktop sink? Would you prefer Option 2's bounded-by-`hard_timeout_secs`?
2. Is coupling the Desktop sink to `pipeline.hard_timeout_secs` (Option 2)
   worth it, or does it leak pipeline concerns into the notification layer?
3. Any failure mode in the id-based replace pattern we're missing (e.g. daemon
   restart reassigning ids, multiple concurrent placeholders colliding)?
