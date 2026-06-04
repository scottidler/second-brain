# Design Document: Excise borg's legacy markdown intake/DLQ layer

**Author:** Scott Idler
**Date:** 2026-06-03
**Status:** Implemented (code; the per-host `bin/migrate-receipts --prune-legacy` vault-file deletion is run out-of-band)
**Review Passes Completed:** 5/5 + Architect rounds 1-2 (findings absorbed)

## Summary

The 2026-05-20 receipts-log design made the SQLite receipts DB
(`~/.local/share/sb/borg/receipts.db`) borg's durable system of record and
specified deleting the legacy markdown bookkeeping (`borg-intake.md`,
`borg-dlq.md`, `borg-dlq-archive.md`, `borg-orphans.md`) plus the
`sb borg intake` / `sb borg dlq` CLI verbs. That teardown was deferred to a
"rollout window" and never carried out, so borg still triple-writes every
input (markdown + sidecar + receipts) and double-writes every rejection
(markdown DLQ + receipts). This document completes the migration: strip the
markdown writers/readers while preserving the two things that are still
load-bearing - the raw-input sidecar and the receipts row - then prune the
markdown files via the existing `bin/migrate-receipts --prune-legacy`.

## TL;DR

- **The markdown layer is already redundant.** Pipeline failures are
  receipts-only today (`make_failure` writes no markdown). The markdown DLQ now
  receives only three stages via `record_dlq` - `IntakeReject` (front-door
  validation), `FetchFailed` (Signal payload-materialization, `signal.rs`), and
  `WatchdogOrphan` - and **all three already mirror into the receipts DB**
  (`record_dlq` calls `receipts::mark_failed` via `failure_stage_from_dlq`; the
  watchdog promotes to `crashed`). No input's durability depends on any
  markdown file.
- **Two things must survive the teardown:** the raw-input sidecar
  (`system/intake/<trace>.txt`, read by `sb borg replay`) and the receipts
  row. Every edit removes a *markdown* call, never a `receipts::*` or
  `write_raw_input` call. That is the cross-cutting invariant.
- **Front doors stop triple-writing.** `record_intake` /
  `record_intake_with_sidecar` drop the markdown `append_entry` and keep the
  sidecar write (unchanged) + receipts. The `record_dlq` sites replace it with
  a direct `receipts::mark_failed` **that preserves each site's own
  `FailureStage`** (not a hardcoded `IntakeRejected` - `signal.rs` uses
  `FetchFailed`).
- **The watchdog goes pure-receipts.** It already runs `receipts_run_once`;
  remove the legacy markdown orphan scan and the `borg-orphans.md` write.
- **CLI verbs collapse into `sb borg log`.** Remove `sb borg intake`,
  `sb borg dlq`, and `audit --invariant`; their data is already queryable via
  `sb borg log [--status …] [--method …] [--since …] [--stage …]`.
- **`GET /health/audit` + the `sb doctor` borg check are reworked, not
  deleted** (gap the doc originally missed; Architect round 2). They read the
  markdown this excision removes, so `audit_health_stats` is repointed at the
  receipts DB. The `AuditHealth` shape becomes receipts state counts -
  `received`/`succeeded`/`failed`/`crashed` (lifetime) plus `failed_24h` and
  `crashed_24h` (the actionable "actively breaking" gauge). `orphan_count` and
  `dlq_pending` are dropped (see below).
- **`vault::dlq` is deleted; `vault::intake` shrinks** to the sidecar helpers
  + the `IntakeKind` classification enum.
- **File deletion is bash, not Rust** - `bin/migrate-receipts --prune-legacy`
  already exists and is the only thing that touches the vault `.md` files.
- **Closes a latent bug for free:** `triage.rs::intake_rows` `--since` has the
  same unparsed-string lexicographic comparison bug just fixed in
  `sb borg log` (v0.8.45); it disappears with the command, so no separate fix.

## Problem Statement

### Background

Borg's ingestion bookkeeping accreted six markdown files in the vault. The
2026-05-20 receipts-log design (`docs/design/2026-05-20-receipts-log.md`,
Status: Implemented) collapsed them into two layers:

- **Layer 1 (durable, never user-facing):** a borg-owned SQLite receipts DB
  with one row per `trace_id`, `status ∈ {received, succeeded, failed}` and a
  seven-value `failure_stage` taxonomy.
- **Layer 2 (vault, success-only):** `borg-ledger.md` + `borg-dashboard.md`.

That design shipped the receipts DB, the dual-write, and the success-only
ledger. It explicitly *deferred* deleting the four failure/intake markdown
files and the `sb borg intake`/`sb borg dlq` verbs to a "rollout window,"
gated on `bin/migrate-receipts --prune-legacy`. The prune was never run and
the Rust teardown was never done, so the legacy layer is still wired and
running in parallel with receipts.

### Problem

The half-finished migration is the actual problem:

- **Triple-write at every door.** `record_intake` /
  `record_intake_with_sidecar` (called from `routes`, `telegram`, `discord`,
  `ntfy`, `signal`, `lib`) write the markdown intake table *and* the sidecar
  *and* the receipts row. The markdown table only ever held an 80-char
  `preview`, never the raw input - so it was never the raw-capture store; the
  sidecar is (verbatim text/URL, or a short descriptor for large binaries by
  documented convention - `routes.rs`, `vault::intake::write_raw_input`). The
  markdown table has no remaining reader except the verbs we are removing.
- **Double-write on rejection.** `record_dlq` appends a markdown DLQ row and
  mirrors to `receipts::mark_failed`. The markdown half is pure redundancy.
- **The watchdog parses markdown it no longer needs.** `watchdog::run_once`
  parses `borg-intake.md` + `borg-dlq.md` to detect orphans *and* runs
  `receipts_run_once` (the SQLite promotion). The markdown half is dead weight.
- **Stale, misleading comments.** Code comments still call the markdown DLQ
  "the rich-reason source of truth," but pipeline failures already bypass it
  entirely (`make_failure`: "Failures live in the receipts log only"). The
  comments describe a state that no longer exists and mislead the next reader -
  exactly the failure mode the 2026-05-20 doc itself called out.
- **A live `--since` bug.** `intake_rows` filters with
  `r.date.as_str() >= since` against an unparsed `since` string - the same
  lexicographic bug just fixed in `sb borg log`. It is reachable today via
  `sb borg intake list --since 5m`.
- **Vault clutter and edit risk.** `borg-intake.md` (a daemon-internal
  structure) and the DLQ files sit in the user's vault, inviting accidental
  edits and Syncthing churn.

### Goals

- **Preserve the immediate raw-capture invariant (the reason intake exists).**
  Every input that reaches the capture checkpoint at a door (http, telegram,
  discord, ntfy, cli, and accepted Signal envelopes) has its raw input recorded
  *synchronously, before any pipeline dispatch, with error propagation* - if
  the capture write fails, the door returns `Failed` and does not proceed.
  After this change the capture is the sidecar (raw-input record, unchanged
  payload) **plus** the receipts row (the findable record); both must succeed.
  This is non-negotiable and is pinned by a Phase-1 test per door.
  - *Signal exception (by design, not a regression):* `signal.rs`'s
    `accepted_envelope` privacy gate drops disallowed senders *before*
    `trace::generate`/capture, so those envelopes are never recorded anywhere
    today and remain so. Only *accepted* envelopes reach the checkpoint.
- Remove every legacy-markdown **write** path (intake table, DLQ table,
  DLQ-archive, orphans) from the borg crate.
- Remove every legacy-markdown **read** path (`parse_entries`, `find_by_trace`,
  orphan audit) from the borg crate.
- Remove the `sb borg intake` and `sb borg dlq` CLI verbs and the
  `audit --invariant` mode; route all of it through `sb borg log`.
- Preserve, untouched, the raw-input sidecar and every `receipts::*` call.
- Delete `vault::dlq`; shrink `vault::intake` to the sidecar + `IntakeKind`.
- Prune the four markdown files via `bin/migrate-receipts --prune-legacy` and
  update `CLAUDE.md` to describe the single-store reality.
- Prove, by test, that the receipts side independently records every input and
  every failure stage that the markdown layer used to.

### Non-Goals

- **Authoritative per-site failure-stage classification (typed-`PipelineError`
  refactor).** Today `record_terminal_to_receipts` derives the receipts
  `failure_stage` from `classify_terminal_failure`, a coarse substring
  heuristic over the free-form reason. This is a receipts-side wart, *not* part
  of the markdown layer: the markdown DLQ never carried pipeline-failure
  stages, so excising it cannot regress stage precision, and the verbatim
  `failure_reason` is preserved in the receipts row regardless. Making each
  pipeline error site emit its own typed stage is a separate quality
  improvement, tracked outside this teardown. (Flagged for architect review -
  if the reviewer judges the heuristic unacceptable to leave standing, it
  folds in as an additional phase.)
- **Any change to `borg-ledger.md` or `borg-dashboard.md`.** The success-only
  ledger and the Dataview dashboard stay exactly as they are.
- **Any change to the sidecar format, path, or payload semantics.**
  `system/intake/<trace>.txt`, the `system/intake/` directory name, and *what
  each door writes into it* (verbatim text/URL, descriptor for binaries) are
  all unchanged (the directory keeps the `intake` name even though the markdown
  "intake" table is gone; renaming it would orphan existing sidecars and is not
  worth it).
- **Retry semantics.** Failures remain terminal; `sb borg replay` remains the
  manual reinjection path.
- **Schema migration of receipts.** No receipts-DB schema change; this is pure
  removal on the producer/consumer side.

## Proposed Solution

### Overview

The teardown is mechanical once the invariant is fixed in mind: **every edit
deletes a markdown operation and leaves the sidecar + receipts operations
intact.** Because receipts writes already sit at every door, the pipeline
chokepoint, and the watchdog, removing markdown never opens a durability gap -
there is no commit at which an input would go unrecorded.

### Architecture

Current write topology (per door / per terminal event):

```
front door (routes/telegram/discord/ntfy/signal/lib)
  └─ record_intake[_with_sidecar]
       ├─ vault::intake::append_entry        →  borg-intake.md      [REMOVE]
       ├─ vault::intake::write_raw_input      →  system/intake/<t>   [KEEP, propagating]
       └─ receipts::record_received           →  receipts.db         [KEEP, PROMOTE to propagating]

rejection / fetch-fail at a door (record_dlq sites)
  └─ record_dlq(stage)                          [stage ∈ IntakeReject, FetchFailed]
       ├─ vault::dlq::append_entry            →  borg-dlq.md         [REMOVE]
       └─ receipts::mark_failed(mapped stage) →  receipts.db         [KEEP, inline]

pipeline terminal (chokepoint)
  └─ record_terminal_to_receipts
       └─ receipts::mark_succeeded/mark_failed →  receipts.db        [KEEP, unchanged]

watchdog tick
  ├─ parse borg-intake.md + borg-dlq.md, write borg-orphans.md      [REMOVE]
  └─ receipts_run_once (list_stale → promote_single_to_crashed)     [KEEP]
```

Target write topology:

```
front door
  └─ record_received_with_sidecar            (sidecar + receipts; BOTH propagate)
rejection / fetch-fail at a door
  └─ receipts::mark_failed(per-site stage)   (direct; no DLQ indirection)
pipeline terminal
  └─ record_terminal_to_receipts             (unchanged)
watchdog tick
  └─ receipts_run_once                       (only remaining body)
```

### Data Model

No new structures. Net deletions:

- `vault::dlq` entirely: `DlqStage`, `DlqStatus`, `DlqEntry`, `ParsedDlqRow`,
  `dlq_path`, `dlq_archive_path`, `ensure_dlq_exists`, `append_entry`,
  `parse_entries`, `find_by_trace`, `update_status`, `archive_resolved`.
- `vault::intake` markdown half: `IntakeEntry`, `ParsedIntakeRow`,
  `intake_path`, `ensure_intake_exists`, `append_entry`, `parse_entries`,
  `find_by_trace`.
- `vault::intake` **kept**: `IntakeKind` (+ `as_str`), `intake_raw_dir`,
  `raw_input_path`, `write_raw_input`.
- `borg::triage` types deleted: `OrphanAuditReport`, `IntakeRowDetail`,
  `IntakeSidecar`/`SidecarContent` (if only used by intake show),
  `DlqRowDetail`, `DlqArchiveOutcome`, `DlqReplayOutcome`.

The receipts `FailureStage` taxonomy is unchanged; the door `record_dlq` sites
construct the **appropriate** `FailureStage` directly - `IntakeRejected` at the
front-door validation sites (`routes`, `telegram`, `discord`, `ntfy`, `lib`,
`triage`), and `FetchFailed` at the Signal payload-materialization site
(`signal.rs`) - instead of mapping from `DlqStage` via `failure_stage_from_dlq`
(which is then removed if it has no other caller). The per-site stage must be
preserved; collapsing all sites to `IntakeRejected` would silently regress
Signal's `FetchFailed` classification.

### API Design

**`borg::intake`** (the door-facing helper module) loses `record_dlq` and the
markdown append; `record_intake[_with_sidecar]` are renamed to reflect that
they now write sidecar + receipts only:

```rust
// Captures raw bytes (sidecar) + findable record (receipts) at the door.
// BOTH writes propagate: a failure in either returns Err, the door returns
// Failed, and the input is NOT dispatched. This is the immediate-capture
// invariant; receipts::record_received is promoted from today's best-effort
// (which was only safe while the markdown row was the guaranteed layer).
pub fn record_received_with_sidecar(
    config: &Config, method: IngestMethod, kind: IntakeKind,
    preview: &str, sidecar_bytes: &[u8], trace_id: &str,
) -> Result<()>;   // preview → receipts.raw_input column; sidecar_bytes → sidecar

// rejection / fetch-fail at a door, replacing record_dlq. Carries the
// per-site stage so signal.rs's FetchFailed is not collapsed to IntakeRejected.
// UPSERT semantics (gap-proof): INSERT-OR-IGNORE a `received` row, then
// mark_failed. This guarantees the rejection lands a failed row whether or not
// a prior record_received ran in this control flow - mark_failed alone is
// `WHERE status='received'` and would silently no-op (total loss) on a cold
// site, where today's unconditional markdown DLQ write still captured it.
pub fn record_failure_at_door(
    method: IngestMethod, trace_id: &str, stage: FailureStage, reason: &str,
);  // → record_received(INSERT OR IGNORE) ; mark_failed(stage, reason)
    //   (no Config: the receipts path is resolved by open_default, not config)
```

Today every `record_dlq` site is preceded by a `record_intake` for the same
trace (verified across `routes`, `telegram`, `discord`, `ntfy`, `signal`,
`lib`, `triage`), so `mark_failed` alone would suffice *now*. The upsert is
defense-in-depth: it makes correctness independent of call-site ordering, now
and as the doors evolve. The INSERT-OR-IGNORE never clobbers an existing row,
so the real kind/raw_input captured by the preceding `record_received` is
preserved; the cold-path values (kind=`Text`, raw_input=`reason`) apply only if
no prior row exists. `record_failure_at_door` stays best-effort (the input's
durability is already guaranteed by the preceding sidecar + `received` row; a
failed `mark_failed` just leaves the row `received` for the watchdog to
crash-promote).

**The sidecar payload is unchanged.** `sidecar_bytes` is whatever each door
passes today - verbatim text for text bodies, a short descriptor for large
binaries (telegram/signal/http-multipart fetch the actual attachment *after*
this checkpoint and never have the raw bytes here; forcing them in would bloat
`system/intake/` and is structurally impossible anyway). This plan does **not**
change what lands in the sidecar; it only removes the markdown `append_entry`
beside it.

Sequencing inside `record_received_with_sidecar`: write the sidecar first
(the raw-input record lands on disk), then `receipts::record_received`. If the
sidecar write succeeds but the receipts write fails, return Err - the door
reports `Failed`; the orphaned sidecar is harmless (swept by retention). The
contract is "no *silent* capture," matching today's behavior where a
markdown-or-sidecar write failure already surfaced to the caller.

*Note on replay:* the sidecar is a durable raw-input record, **not** the modern
replay source. `sb borg replay` re-injects from the trace's `source` via the
daemon (`replay.rs::reingest_via_daemon`), not from `system/intake/`. The
legacy `triage::dlq_replay` (deleted in Phase 4) was the only sidecar-reading
replay path.

The `origin_ctx` argument (a markdown-column-only field, e.g. the telegram
chat id) is dropped. `preview` stays - it becomes the receipts `raw_input`
column (a small UTF-8 summary); the full bytes live in the sidecar, exactly as
`record_intake_with_sidecar` splits them today.

**CLI** (`sb/src/cli/borg.rs`): remove the `Intake` and `Dlq` subcommands and
their `Args`/`Action` enums, the `--invariant` arm of `audit`, and the
`print_intake_*` / `print_dlq_*` / `print_orphan_audit_report` helpers.
`sb borg audit` keeps its `--fix` behavior (which already reads the ledger /
receipts, not the intake markdown).

**Health endpoint rework** (`GET /health/audit`, `audit_health_stats`, the
`sb doctor` borg check). This was NOT in the original deletion list - it is a
gap surfaced during Phase 4 and resolved per Architect round 2. Today
`audit_health_stats` cross-references `borg-intake.md` × `borg-ledger.md` ×
`borg-dlq.md`; after Phase 2 nothing writes those, so it would silently report
`orphan_count=0 / all healthy` - the exact silent-wrong-data failure this
migration exists to kill. Repoint it at the receipts DB. New shape:

```rust
pub struct AuditHealth {
    pub received: usize,    // lifetime status counts (count_by_status)
    pub succeeded: usize,
    pub failed: usize,      // includes crashed
    pub crashed: usize,     // lifetime subset, failure_stage='crashed'
    pub failed_24h: usize,  // terminal_at within last 24h - the actionable gauge
    pub crashed_24h: usize,
}
```

- **`orphan_count` is dropped, not ported.** A receipts "orphan" = a `received`
  row past the watchdog deadline. But (a) the watchdog promotes those to
  `crashed` every 60s, so a poll almost always sees 0 - it misses the drops it
  is meant to catch; and (b) a naive `received-past-deadline` query duplicates
  `receipts::list_stale` *without* the watchdog's `active_traces` permit
  filter, so a heavy item legitimately queued for a permit would report as a
  false-positive orphan. `crashed` (a state the watchdog has definitively ruled
  on) is the correct silent-drop signal.
- **`dlq_pending` is dropped.** Failures are terminal in receipts (no
  retry/pending state - a Non-Goal of the 2026-05-20 doc); a field that is
  always 0 is its own lie.
- **`failed_24h` / `crashed_24h` filter on `terminal_at`, not `received_at`** (a
  row received 25h ago but crashed 1h ago must count). The existing
  `since`-based `query` filters `received_at`, so this needs two focused
  receipts count helpers (below), not the `query` path.
- **`sb doctor`** reports the counts as info and **warns when `crashed_24h > 0`**
  (a recent crash = the watchdog had to declare an input lost; lifetime
  `crashed` would warn forever, so the window is load-bearing for actionability).
  Remediation text points at `sb borg log --stage crashed --since 24h`,
  replacing the deleted `sb borg dlq` / `audit --invariant` references.

New `borg::receipts` helpers (terminal_at-windowed counts, alongside
`count_by_status`):

```rust
pub fn count_failed_since(conn: &Connection, since_iso: &str) -> Result<i64>;
//   SELECT COUNT(*) WHERE status='failed' AND terminal_at >= ?
pub fn count_crashed_since(conn: &Connection, since_iso: &str) -> Result<i64>;
//   SELECT COUNT(*) WHERE status='failed' AND failure_stage='crashed' AND terminal_at >= ?
```

### Implementation Plan

Phases ship back-to-back; there is no soak/burn-in gate between them. The
cross-cutting rule for every phase: **delete markdown calls only - never a
`receipts::*` or `write_raw_input` call.**

#### Phase 1: Build the testable capture seam + pin behavior (tests first)
**Model:** opus
- *Seam (characterization-test prerequisite):* the door functions hardcode
  `receipts::open_default()`, which can't be tested without polluting the live
  DB or racing on `XDG_DATA_HOME`. Introduce inner `*_to(conn, vault_root, …)`
  variants of the new `record_received_with_sidecar` / `record_failure_at_door`
  (the public fns open the conn + resolve the root and delegate). This is a
  small DI refactor; the old `record_intake`/`record_dlq` stay in place and
  wired this phase so the build/behavior is unchanged.
- **Immediate-capture test (the load-bearing one):** call the inner capture fn
  with `open_memory()` + a tempdir vault; assert it leaves both a sidecar file
  with the bytes *and* a `received` receipts row, and that a sidecar-write
  failure (unwritable root) returns `Err` (must-succeed, not best-effort).
- **Rejection tests, including the no-prior-row case:** assert the failure
  helper lands `status=failed` with the **given** stage when a prior `received`
  row exists (and does NOT clobber its kind/raw_input), AND when **no** prior
  row exists (the upsert path - the gap the advisor flagged). Pin both
  `IntakeRejected` and `FetchFailed` pass through faithfully.
- Add a watchdog/receipts test asserting a stale `received` row is promoted to
  `failed/crashed` via the receipts path alone, and that
  `sb borg log --stage crashed` (the `borg-orphans.md` replacement) returns it.

#### Phase 2: Strip markdown from the front-door write path
**Model:** opus
- Rewrite `record_intake[_with_sidecar]` → `record_received_with_sidecar`:
  drop `append_entry`, **keep `write_raw_input` byte-for-byte as today** (do
  NOT change the sidecar payload - verbatim for text/URL, descriptor for
  binaries; the remote doors don't even hold the bytes at this checkpoint), and
  **promote `receipts_record_received` from best-effort to propagating**
  (return Err on open/insert failure so the door reports `Failed`).
- Replace `record_dlq` at **all** its call sites with `record_failure_at_door`,
  **preserving each site's stage**: `IntakeReject` →
  `FailureStage::IntakeRejected` at `routes` ×3, `telegram` ×2, `discord` ×2,
  `ntfy` ×1, `signal` ×1 (the reject site), `lib` ×1, `triage::dlq_replay`;
  and `FetchFailed` → `FailureStage::FetchFailed` at `signal.rs` (the
  payload-materialization site, `signal.rs:537`). Do **not** collapse to a
  hardcoded stage.
- Update every call site; delete `record_dlq` and (if now unused)
  `failure_stage_from_dlq`.

#### Phase 3: Make the watchdog pure-receipts
**Model:** sonnet
- Delete the `borg-intake.md`/`borg-dlq.md` parse + orphan detection +
  `borg-orphans.md` write from `watchdog::run_once`; keep `receipts_run_once`
  as the whole body.
- Remove the now-dead `WatchdogOrphan` construction.

#### Phase 4: Remove the markdown readers + CLI verbs; rework health endpoint
**Model:** sonnet
- Delete `triage::{intake_rows, intake_row, orphan_audit, dlq_rows, dlq_row,
  dlq_archive, dlq_replay}` and their report types; keep `receipts_log` /
  `receipts_show`. With `dlq_replay` gone (the last caller), also delete the
  legacy `intake::{record_intake, record_intake_with_sidecar, record_dlq}` and
  (if unused) `failure_stage_from_dlq` - the Phase-2-deferred deletion lands here.
- Remove `Command::{Intake, Dlq}`, their arg/action enums, the `audit
  --invariant` arm, and the `print_*` helpers in `sb/src/cli/borg.rs`.
- Rework the health endpoint (see API Design): add `count_failed_since` /
  `count_crashed_since` to `borg::receipts`; repoint `audit_health_stats` at
  receipts with the new `AuditHealth` shape; update the `sb doctor` borg check
  to report the new counts and warn on `crashed_24h > 0`.

#### Phase 5: Shrink the vault crate
**Model:** sonnet
- Delete `vault/src/dlq.rs` and its `mod` declaration; remove `failure_stage_
  from_dlq` from `vault::receipts` if unused.
- Strip the markdown half of `vault/src/intake.rs`, keeping `IntakeKind`, the
  sidecar dir/path helpers, and `write_raw_input`.
- Fix the fallout in `borg` re-exports (`crate::intake::{Kind, Stage, Status}`)
  - `Stage`/`Status` were `vault::dlq` re-exports and go away.

#### Phase 6: Prune files + docs
**Model:** sonnet
- **Backfill first:** run `bin/migrate-receipts` (the copy step) before
  `--prune-legacy`, so any pre-receipts-era rows that exist only in the
  markdown files land in the receipts DB before the files are deleted. The
  dual-write means recent data is already in receipts; this covers the tail of
  rows written before the receipts DB existed. `--prune-legacy` only deletes.
- Run `bin/migrate-receipts --prune-legacy` to remove `borg-intake.md`,
  `borg-dlq.md`, `borg-dlq-archive.md`, `borg-orphans.md` from the vault.
- Update `CLAUDE.md`: drop the "dual-written for safety during the rollout
  window" language and the legacy-markdown bullet; state that receipts is the
  sole failure store and the sidecar is the durable raw-input record (modern
  replay re-injects from `source` via the daemon, not from the sidecar).
- `otto ci` green; ship via `bump` + `otto install` + `systemctl --user
  restart borg` (no extension change → not `otto deploy`).

## Alternatives Considered

### Alternative 1: Narrow excision - delete only the `sb borg intake` command
- **Description:** Remove just the stranded CLI verb and `intake_rows`, leaving
  the dual/triple-write and the markdown files in place.
- **Pros:** Smallest diff; kills the surfaced `--since` bug.
- **Cons:** Leaves the markdown writers running with zero readers - a
  write-only file that grows forever, still cluttering the vault and still
  carrying the misleading "source of truth" comments. Doesn't finish the
  migration; the next reader re-discovers the same confusion.
- **Why not chosen:** It treats the symptom (a buggy verb) and leaves the
  disease (a half-done migration). The 2026-05-20 doc already mandated the full
  teardown.

### Alternative 2: Fold the typed-`PipelineError` stage refactor into this work
- **Description:** Also replace `classify_terminal_failure` with per-site typed
  stages so the receipts `failure_stage` is authoritative.
- **Pros:** Removes the coarse heuristic at the same time; fully realizes the
  2026-05-20 "classify every failure by stage" goal.
- **Cons:** Substantially larger and orthogonal to "remove markdown" - touches
  every pipeline error site, not the bookkeeping layer. Risk of conflating two
  unrelated changes in one review.
- **Why not chosen (tentatively):** Scoped out as a Non-Goal because the
  markdown never carried pipeline-failure stages, so excision can't regress
  precision, and reasons are preserved verbatim. Surfaced for architect review;
  promotable to a phase if the reviewer disagrees.

### Alternative 3: Leave it; it's working
- **Description:** Accept the dual-write indefinitely.
- **Pros:** Zero work.
- **Cons:** Permanent redundant I/O on every ingest, permanent vault clutter,
  permanent stale comments, a live `--since` bug, and a migration the project
  already decided to finish.
- **Why not chosen:** The cost is ongoing and the decision was already made.

## Technical Considerations

### Dependencies
No new crates. Net removal of `vault::dlq` and most of `vault::intake`.

### Performance
Removes one markdown file-lock + full-table append from the hot path of every
ingest, and one markdown parse from every watchdog tick. Strictly faster; no
regression surface.

### Unaffected consumers
The receipts DB schema and contents are unchanged, so read-only consumers keep
working without modification: oracle's `failure_history` MCP tool (opens the
receipts DB read-only) and `sb borg audit --fix` (reads `borg-ledger.md` + the
vault, never the intake markdown). The `IngestRequest` HTTP contract the
Firefox extension posts to is untouched. Binary-input sidecar semantics
(what each door passes as `raw_bytes`) are preserved exactly as today.

`GET /health/audit` IS affected (it read the markdown) and is reworked in
Phase 4 - see "Health endpoint rework" above. Its JSON shape changes
(`orphan_count`/`dlq_pending` removed; `received`/`succeeded`/`failed`/`crashed`
+ `failed_24h`/`crashed_24h` added). The endpoint is localhost-only; the
verified consumers are `routes.rs`, `triage.rs`, and `sb doctor` (`checks.rs`) -
the Firefox extension does not call it. Any external bash/jq alerting against
the old shape must be updated.

### Security / Privacy
Removing `borg-intake.md` from the vault removes a daemon-internal record from
the Syncthing-synced surface, a small privacy/clutter win. No new exposure.

### Testing Strategy
Phase 1 is test-first: lock the receipts-side behavior (rejection rows, crash
promotion, `--stage crashed` query) against current code before deleting
anything. Each subsequent phase keeps `cargo test --workspace` green. Existing
markdown-fixture tests (`watchdog/tests.rs`, `triage/tests.rs`,
`vault::{intake,dlq}` tests) are removed alongside the code they cover. A final
manual check: drive one input through each live door on a scratch vault and
confirm a receipts row + sidecar appear and no `borg-*.md` failure file is
recreated.

### Rollout Plan
Single PR / branch, all phases. After merge: `bin/migrate-receipts
--prune-legacy` on each host (desk daemon; laptop runs the old extension only),
then `bump` + `otto install` + `systemctl --user restart borg`. The prune is
idempotent and safe to re-run.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| An edit deletes a `receipts::*`/sidecar call mistaking it for markdown | Low | High | The cross-cutting invariant; Phase-1 tests fail loudly if a door stops recording to receipts |
| A rejection is lost entirely (no `received` row → `mark_failed` no-ops) | Low | High | Advisor caught this: today's markdown DLQ write was unconditional. `record_failure_at_door` uses INSERT-OR-IGNORE + `mark_failed` (upsert), gap-proof regardless of call-site ordering. Phase-1 test includes the no-prior-row case |
| A `FailureStage` is silently regressed when `record_dlq` is replaced | Med | High | Architect round 1 caught this: `signal.rs:537` logs `FetchFailed` via `record_dlq`, not just `IntakeReject`. `record_failure_at_door` carries the per-site stage; Phase 2 enumerates each site's stage explicitly. A Phase-1 test pins Signal's `FetchFailed` reaching receipts |
| A `FailureStage` was only reachable via the markdown DLQ path | Low | High | Verified in code: pipeline failures already go receipts-only via `classify_terminal_failure`; the three `record_dlq` stages (`IntakeReject`, `FetchFailed`, `WatchdogOrphan`) all mirror to receipts today. Phase-1 tests pin this |
| Sidecar accidentally removed with the intake markdown | Low | High | `write_raw_input`/`raw_input_path` explicitly retained in `vault::intake`; replay integration test covers it |
| `--prune-legacy` deletes a file something still reads | Low | Med | Files are pruned only in Phase 6, after all Rust readers are gone; `sb borg log` reads SQLite only |
| Promoting receipts to must-succeed makes a door fail if SQLite is unavailable | Low | Med | Symmetric with today: a markdown vault-write failure already returns `Failed`. SQLite is local, WAL, with `busy_timeout`; open failure means the box is already broken. The alternative (silent best-effort) is the worse outcome - a captured-but-unfindable input |
| Coarse `classify_terminal_failure` stage proves inadequate post-excision | Med | Low | `failure_reason` preserved verbatim; typed-stage refactor available as a fast follow / promotable phase |

## Open Questions
- [ ] Architect call on Non-Goal #1: leave `classify_terminal_failure` as-is,
      or fold the typed-`PipelineError` stage refactor into this teardown?
- [x] *Resolved (Architect round 1):* `sb borg replay` re-injects from the
      trace `source` via the daemon (`replay.rs::reingest_via_daemon`); the
      deleted `triage::dlq_replay` was the only sidecar-reading replay path, so
      nothing replay-related is lost. No DLQ-replay-only path to preserve.
- [ ] Any external doc / muscle-memory references to `sb borg intake` /
      `sb borg dlq` to update beyond `CLAUDE.md` (READMEs, the dashboard's
      help text)?

## References
- `docs/design/2026-05-20-receipts-log.md` (the migration this completes)
- `docs/design/2026-04-19-staged-ingestion-pipeline.md` (sidecar contract)
- `bin/migrate-receipts` (`--prune-legacy` branch)
- `borg/src/replay.rs` (`reingest_via_daemon` - modern replay reads `source`, not the sidecar)
- v0.8.45 `fix(borg): parse sb borg log --since` (the surfaced-bug fix)
