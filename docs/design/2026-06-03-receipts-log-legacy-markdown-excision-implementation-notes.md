# Implementation Notes: legacy markdown intake/DLQ excision

Append-only record of decisions, deviations, tradeoffs, and open questions per
phase. Companion to `2026-06-03-receipts-log-legacy-markdown-excision.md`.

## Phase 1: Build the testable capture seam + pin behavior

### Design decisions
- New capture/failure entry points live in `borg::intake` alongside the legacy
  `record_intake`/`record_dlq` (not replacing them yet) so the build/behavior
  is unchanged this phase and the call-site switch is isolated to Phase 2.
- `record_failure_at_door` takes no `Config` — `borg/src/intake.rs`. The
  receipts path is resolved by `receipts::open_default()`, not config, so a
  `Config` param would be unused (`deny(unused_variables)`).
- Crashed-promotion-is-queryable pin lives in `receipts/tests.rs`
  (`watchdog_crash_promotion_is_queryable_by_stage_crashed`) since it is a
  receipts-level behavior, not an intake one.

### Deviations
- The doc's original Phase 1 said "test the current door behavior first." The
  door fns hardcode `receipts::open_default()` and cannot be tested without
  polluting the live DB or racing on `XDG_DATA_HOME`. Deviation: introduce
  conn/root-injectable inner seams (`record_received_with_sidecar_to`,
  `record_failure_at_door_to`) and test those with `open_memory()` + a tempdir
  vault. This is standard characterization-test scaffolding; the doc's Phase 1
  was updated to match.
- `record_failure_at_door` uses UPSERT semantics (`record_received` INSERT OR
  IGNORE, then `mark_failed`) rather than `mark_failed` alone. This closes a
  durability gap the advisor flagged: today's markdown DLQ write was
  unconditional, so a cold rejection (no prior `received` row) was still
  captured; `mark_failed` alone (`WHERE status='received'`) would silently
  no-op. The doc was updated (API Design + risk table) to specify the upsert.

### Tradeoffs
- Incidental flake fix bundled in: `vault/src/paths/tests.rs`
  `cwd_with_obsidian_marker_wins` / `cwd_without_marker_errors` race on
  process-global `set_current_dir`. Added a `CWD_LOCK` mutex (the `rust.md`
  global-state-serialization pattern). Bundled because it flakes the CI gate on
  every phase commit; alternative (separate PR) would block this execution.
- `record_failure_at_door` stays best-effort (logs, does not propagate),
  matching the old `record_dlq` contract — the input's durability is already
  guaranteed by the preceding sidecar + `received` row, so a failed
  `mark_failed` just leaves the row `received` for the watchdog to crash-promote.

### Open questions
- None.

## Phase 2: Strip markdown from the front-door write path

### Design decisions
- Switched all six door files (`routes`, `telegram`, `discord`, `ntfy`,
  `signal`, `lib`) from `record_intake[_with_sidecar]` → `record_received_with_sidecar`
  and from `record_dlq` → `record_failure_at_door`. Each rejection passes its
  real stage: `IntakeRejected` at every front-door validation site,
  `FetchFailed` at `signal.rs`'s payload-materialization site.
- Sidecar bytes preserved exactly as before per site: `request.url.as_bytes()`
  (http url), full text bytes (note/text bodies), the descriptor's bytes for
  binaries (multipart/discord/cli files) - never the raw binary, per the
  documented `system/intake/` convention.
- Dropped the `origin_ctx` argument and its now-unused bindings
  (`chat_id_ctx`, `channel=` / `topic=` / `source.display()` strings); the
  receipts row has no origin_ctx column and never did.

### Deviations
- **Sequencing:** the doc's Phase 2 said "delete `record_dlq` / `record_intake`."
  Deferred to Phase 4: `borg::triage::dlq_replay` still calls the legacy
  `record_dlq` / `record_intake_with_sidecar`, and that function is itself
  deleted in Phase 4. Deleting the legacy intake fns now would orphan a live
  caller. They remain (pub, in a lib crate → no dead-code warning) alongside
  the new fns until their last caller (triage) is removed in Phase 4.

### Tradeoffs
- None.

### Open questions
- None.

## Phase 3: Make the watchdog pure-receipts

### Design decisions
- Rewrote `watchdog.rs` to a pure-receipts scan: `run_once` resolves the
  deadline and opens the DB, delegating to a conn-injectable `run_once_conn`
  (the `list_stale` → `active_traces` filter → `promote_single_to_crashed`
  loop). Deleted the markdown half (`borg-intake.md`/`borg-dlq.md` parse,
  `ledger_trace_ids`, `intake_age_secs`, the `WatchdogOrphan` DLQ append, and
  the now-unused `intake`/`dlq`/`ledger`/`table`/`chrono` imports).
- Rewrote `watchdog/tests.rs` to test `run_once_conn` with `open_memory()` +
  backdated rows + `active_traces` closures (stale→crashed, active-skip,
  fresh-skip, terminal-skip), replacing the markdown-fixture tests.

### Deviations
- None beyond the Phase-2 sequencing note.

### Tradeoffs
- Dropped a `run_once`-against-default-DB smoke test: it would call
  `list_stale`/`promote_single_to_crashed` on the **real** receipts DB and
  could promote live stale rows to crashed (production-data mutation from a
  test). The conn-level tests cover the logic without that risk.

### Open questions
- None.

## Phase 4: Remove markdown readers + CLI verbs; rework health endpoint

### Design decisions
- `audit_health_stats` split into a conn-injectable `audit_health_stats_conn`
  (tested with open_memory) + a thin public wrapper, matching the Phase-1/3
  seam pattern.
- Health windowed counters (`failed_24h`/`crashed_24h`) filter on `terminal_at`
  via new `borg::receipts::count_{failed,crashed}_since`; the existing
  `since`-based `query` filters `received_at` and would miss a row received
  long before it failed.
- `sb doctor` warns only on `crashed_24h > 0` (not lifetime crashed, which is
  monotonic and would warn forever); lifetime counts come from `receipts_summary`.

### Deviations
- The legacy `record_intake`/`record_intake_with_sidecar`/`record_dlq` deletion
  (doc'd as Phase 2) landed here, once `triage::dlq_replay` (their last caller)
  was removed.

### Tradeoffs
- Folded a `cargo fmt` pass that corrected external-crate import ordering
  (`use vault::receipts::FailureStage;`) which had been red in the `check` task
  since Phase 2. Root cause: per-phase verification confirmed test/lint/bloat
  but not the check (clippy+fmt) task. Going forward, verify with `otto ci`
  exit code, not a grep of task lines.

### Open questions
- None.

## Phase 5: Shrink the vault crate

### Design decisions
- Deleted `vault::dlq` wholesale and `vault::receipts::failure_stage_from_dlq`
  (its only caller was the deleted `record_dlq`). `vault::intake` kept only
  `IntakeKind` + the sidecar helpers.

### Deviations / Tradeoffs / Open questions
- None.

## Phase 6: Prune files + docs

### Design decisions
- `CLAUDE.md` "Borg durable-capture stores" rewritten to describe the
  sidecar+receipts door capture, the upsert failure path, the watchdog
  promotion, and the new `/health/audit` shape; removed the dual-write /
  rollout-window language and the `sb borg intake`/`dlq` references.

### Deviations
- The live `bin/migrate-receipts --prune-legacy` (deletes the four markdown
  files from the real Obsidian vault) is intentionally NOT run by the agent -
  it is an irreversible action on the user's notes, held for the user to run
  per host. The `--prune-legacy` tooling already exists in `bin/`.

### Tradeoffs / Open questions
- None.
