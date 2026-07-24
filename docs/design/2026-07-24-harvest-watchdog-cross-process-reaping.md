# Design Document: watchdog cross-process false-crash (shared trace lease)

**Author:** Scott Idler (via agent)
**Date:** 2026-07-24
**Status:** Implemented
**Review Passes Completed:** 5/5 + review-panel (Architect + Staff Engineer) + consensus loop

## Summary

The borg daemon's watchdog reaps in-flight harvest traces as `crashed` because
"is this trace still being worked on?" is answered from a **process-local
in-memory set** (`ACTIVE_TRACES`) the separate `sb borg harvest` CLI process
cannot write to. Harvest and the daemon are two OS processes driving
`process_content` against the SAME receipts DB; the daemon watchdog cannot see
harvest's active traces, so it promotes a trace harvest is still distilling. Fix:
move liveness into the shared receipts row as a renewable lease
(`lease_owner_pid` + `lease_until`); the watchdog reaps only rows whose lease is
absent or expired, checked ATOMICALLY in the promotion UPDATE (not just the
SELECT). Cross-process-correct, fail-closed, no new deps.

## Problem Statement

### Background

- Every input is recorded `status=received` in the shared receipts SQLite DB at
  the door, then mutated to `succeeded`/`failed` at terminal time.
- A watchdog promotes any row left `received` past `hard_timeout_secs + 60s` to
  `crashed`, excluding traces it believes are still inside `process_content`.
- That exclusion reads `permits::is_trace_active` (`watchdog.rs:94`,`:57`), a
  process-local `static ACTIVE_TRACES: OnceLock<Mutex<HashSet<String>>>`
  (`permits.rs:96`), populated by `ActiveTraceGuard::acquire` (`pipeline.rs:186`).

### Problem

- The nightly harvest runs as a SEPARATE process (`sb-harvest.service`, oneshot
  `sb borg harvest`) from the daemon (`sb-borg.service`). Harvest runs the
  pipeline IN-PROCESS: `harvest/publish.rs:163` calls `pipeline::process_content`
  after door-capture at `:140`, with its own permit pools (`sb/src/cli/borg.rs:347`).
- The watchdog runs ONLY in the daemon (spawned once at `lib.rs:447` in
  `serve_init`; harvest never calls `serve_init`). So during a harvest run, two
  processes drive `process_content` against the shared receipts DB, and the
  daemon's `ACTIVE_TRACES` NEVER holds a harvest-process trace.
- Every harvest trace older than `deadline = hard_timeout_secs (1800) +
  WATCHDOG_BUFFER_SECS (60) = 1860s` is thus a reap candidate the daemon cannot
  exclude. It calls `promote_single_to_crashed` (`receipts.rs:493`) while harvest
  is still inside `process_content`. Harvest's later `mark_succeeded`/`mark_failed`
  then no-ops on `WHERE ... status='received'` (`receipts.rs:411`,`:447`): the
  note can land, but the receipt permanently lies `crashed`.
- Confirmed by `hv-741468`: door-captured 01:22:59, promoted `crashed` 01:54:40
  (~1860s later), 40 daemon watchdog scans in that window. `FabricShell::call`
  logs BEFORE `spawn_blocking` (`fabric.rs:70` vs `:78`) and hv-741468 logged NO
  such line -- not hung inside its own processing; reaped from OUTSIDE.

Processes stomping on each other through shared state; it corrupts the
authoritative ingest record and any future concurrent ingest path inherits it.

### Goals

- The watchdog NEVER reaps a trace another live process is actively working on,
  across process boundaries -- including under concurrency (no TOCTOU).
- The watchdog STILL reaps a trace whose owning process genuinely died (fail
  closed) -- an orphan must not live forever.
- Liveness is shared state, not per-process memory: correct at N processes,
  collapses to correct at N=1.
- A deterministic regression test reproduces the cross-process false-crash and
  bites when the check is removed.

### Non-Goals

- The distill-parsing failures -- separate doc
  (`2026-07-24-harvest-distill-parsing-robustness.md`).
- Changing the watchdog cadence, the 1800s hard timeout, or the 60s buffer.
- A distributed lock manager or any new dependency. SQLite (WAL + busy_timeout,
  already configured) is the substrate.
- Making harvest POST to the daemon instead of running in-process (larger
  re-architecture; the lease fixes the bug without it).

## Proposed Solution

### Overview

Retire the process-local `ACTIVE_TRACES`. Record liveness as a renewable lease on
the shared receipts row: the owning process writes `lease_owner_pid` +
`lease_until` when it starts a trace, renews once at permit grant, and the lease
is cleared as part of the terminal write on the happy path (or by RAII Drop on
panic/cancel). The watchdog excludes any row with a live lease -- and the
exclusion is applied ATOMICALLY in the promotion UPDATE, not only the SELECT, so
a renew that races a scan cannot be clobbered.

### Architecture

- **Schema (additive, idempotent, correctly ordered):** add
  `lease_owner_pid INTEGER DEFAULT NULL` + `lease_until TEXT DEFAULT NULL`
  (`TIMESTAMP_FMT`) to `receipts` via `has_column` + single `ALTER TABLE ADD
  COLUMN`. **The v4 ADD COLUMN must run AFTER the v3 table-rebuild block**
  (`receipts.rs:217`): the v3 migration rebuilds the table with a hardcoded
  12-column `INSERT...SELECT`, so a v4 column added at the `degraded`-precedent
  location (`:193`, BEFORE the rebuild) would be DROPPED by the rebuild on a
  pre-v3 DB. Bump `SCHEMA_VERSION` 3 -> 4; the migration test seeds a PRE-v3 DB
  (not just a v3 one) and re-opens twice for idempotency.
- **Lease-writing RAII guard (replaces `ActiveTraceGuard`'s HashSet).** Same RAII
  shape, but carries a receipts handle + computed `lease_until`, and adds a
  `cancel()` that disarms Drop.
  - **Acquire at trace entry** (`pipeline.rs:186`, before the permit so a
    permit-queued trace already holds a lease): write
    `lease_owner_pid = std::process::id()`, `lease_until = now + hard_timeout_secs
    + WATCHDOG_BUFFER_SECS`. **Acquire fails CLOSED:** if the initial `write_lease`
    fails, the trace aborts to a terminal failure (recorded via the door-failure
    path) rather than continuing with a NULL lease that is instantly reap-eligible
    (`process_content` returns `IngestResult`, not `Result` -- `pipeline.rs:167` --
    so this is an explicit early terminal, not a `?`).
  - **Renew once at permit grant** (`pipeline.rs:190`): re-stamp `lease_until` so
    the actual-processing window is measured from when work truly starts.
  - **Clear folded into the terminal write** (happy path): `mark_succeeded` /
    `mark_failed` (`receipts.rs:411`/`:447`) SET `lease_owner_pid=NULL,
    lease_until=NULL` in the SAME UPDATE. After the terminal write, the guard's
    `cancel()` is called so Drop does NOTHING on the happy path (no redundant I/O,
    no blocking SQLite UPDATE on a Tokio worker). Drop clears the lease ONLY when
    the guard was never cancelled -- i.e. panic/future-cancel -- making a genuinely
    dead trace immediately reap-eligible. Drop-clear failure is WARN-not-panic
    (the lease still expires on its own).
- **Watchdog reap is lease-aware AND atomic.** `list_stale` (`receipts.rs:468`)
  gains `AND (lease_until IS NULL OR lease_until < :now)` for the candidate SELECT
  -- AND `promote_single_to_crashed` (`receipts.rs:493`) repeats the SAME
  predicate in its UPDATE: `... SET status='crashed', ... WHERE trace_id=? AND
  status='received' AND (lease_until IS NULL OR lease_until < :now)`. This closes
  the TOCTOU: if the owner renews between the scan's SELECT and the promotion
  UPDATE, the UPDATE matches 0 rows and the live trace is NOT reaped. Delete
  `is_trace_active` and `ACTIVE_TRACES`; `run_once` no longer needs the `&dyn Fn`
  liveness closure in production (the seam may remain for tests).
- **Fails closed:** `lease_until` is a fixed expiry renewed only by a live
  process. A dead harvest/daemon stops renewing; once `now > lease_until` the row
  is reaped (SELECT and UPDATE agree). The expiry is pinned to the same
  `hard_timeout + buffer` the watchdog already uses, so "slow but alive" and
  "dead" diverge only AFTER the handler's own hard timeout should have fired.

### Data Model

`receipts` gains:

- `lease_owner_pid INTEGER DEFAULT NULL` -- owning `std::process::id()`;
  DIAGNOSTIC only (which process holds it), never the liveness gate (PID reuse).
- `lease_until TEXT DEFAULT NULL` -- lease expiry (`TIMESTAMP_FMT`); the liveness
  gate. `NULL` = no live lease -> reap-eligible if past `received_at` deadline.

`lease_until` is liveness STATE, not identity -- pinning wall-clock to an expiry
is allowed (the no-wall-clock-in-identity rule targets identities/content hashes).

### API Design

New `receipts` primitives (lib-only, `params![]`-bound, `now` injected for tests):

- `write_lease(conn, trace_id, pid, lease_until) -> Result<()>`
- `renew_lease(conn, trace_id, lease_until) -> Result<()>`
- `mark_succeeded` / `mark_failed` -- extended to also NULL the lease columns in
  their existing UPDATE (no separate clear on the happy path).
- `list_stale(conn, deadline_secs, now)` and `promote_single_to_crashed(conn,
  trace_id, deadline_secs, now)` -- BOTH gain the lease predicate; the promotion
  UPDATE is the atomic guard.

Guard type (renamed, e.g. `TraceLeaseGuard`): `acquire(conn, trace_id, deadline)
-> Result<Self>` (fails closed), `renew(&self)`, `cancel(self)`; Drop clears only
if not cancelled.

### Implementation Plan

#### Phase 0: Spike -- bound daemon permit-starvation, decide heartbeat (reasoning, no code)
**Model:** opus
- Produce a NUMBER: worst-case daemon-side queue-wait before a permit grants,
  given `GENERAL_PERMITS` default 8 (`config.rs:326`) and per-POST detached tasks
  (`routes.rs:154/278/475`), vs the 1860s entry-lease. Harvest and replay are
  sequential (safe); the daemon burst is the only path where an entry-lease could
  expire before renew-at-permit.
- **Success criteria (falsifiable):** a stated worst-case queue-wait figure at the
  configured cap. If < 1860s -> renew-at-permit suffices, NO heartbeat (record the
  number). If it can exceed 1860s -> Phase 4 adds a periodic heartbeat that renews
  the lease while queued. Also confirm the `--from-stage 2` replay path
  (`replay.rs:443`, in-CLI `process_session`) writes no `received` receipt, so it
  needs no lease.

#### Phase 1: Additive schema migration (ordered after v3 rebuild)
**Model:** sonnet
- Add the two columns AFTER the v3 rebuild block; bump `SCHEMA_VERSION` -> 4;
  update `receipts/schema.sql`. Add a rollback-safety assertion that the
  `schema_version` match arm does not panic on a newer stored version.
- **Success criteria:** a PRE-v3-seeded DB and a fresh DB both open clean, gain
  both columns, and re-open idempotently (twice); `has_column("lease_until")` true
  for both; `cargo test --workspace` green.

#### Phase 2: Receipts lease primitives + terminal-clear + unit tests
**Model:** sonnet
- `write_lease`/`renew_lease`; extend `mark_succeeded`/`mark_failed` to NULL the
  lease; add the lease predicate to `list_stale` AND `promote_single_to_crashed`
  (atomic), `now` injected, `params![]`-bound.
- **Success criteria:** fresh-lease row excluded from `list_stale` AND unmatched
  by `promote_single_to_crashed`'s UPDATE; expired/NULL-lease past-deadline row
  included and promoted; a terminal `mark_succeeded` NULLs the lease in one UPDATE.

#### Phase 3: TOCTOU + fail-closed regression tests (predicate level)
**Model:** opus
- Seed receipts rows to drive the atomic-promotion predicate directly: (a) a
  backdated row whose lease is renewed AFTER the candidate SELECT but BEFORE the
  promotion UPDATE -> assert NOT promoted (the UPDATE's own predicate catches it);
  (b) an expired/NULL-lease backdated row -> promoted. (Labeled a predicate/TOCTOU
  test, not "fails before Phase 4" -- `run_once_conn` delegates to `list_stale`, so
  the pure-predicate case would pass post-Phase-2; the BITE here is inverting the
  promotion-UPDATE predicate.)
- **Success criteria:** the renew-races-scan case is NOT promoted; the expired
  case IS; removing the predicate from the promotion UPDATE makes test (a) fail.

#### Phase 4: Wire lease into guard + watchdog; remove ACTIVE_TRACES (GREEN)
**Model:** opus
- Convert the guard to write/renew the lease and clear-on-Drop-unless-cancelled;
  acquire (fail-closed) at `pipeline.rs:186`, renew at `:190`, `cancel()` after
  the terminal write. Switch `run_once_conn` to the lease predicate; delete
  `is_trace_active`/`ACTIVE_TRACES`; adapt existing watchdog tests. Give the
  lease-reap a distinct `failure_reason` (e.g. `lease-expired`) so lease reaps are
  distinguishable in `sb borg log`. Add the heartbeat ONLY if Phase 0 required it.
- **Success criteria:** an end-to-end test driving `process_content` in one
  process with a fresh lease is NOT reaped by a concurrent `run_once`; a
  panicked/cancelled trace (guard Dropped, not cancelled) IS reaped after its
  lease clears; no `#[allow(dead_code)]` from the removed static; `otto ci` green.

## Acceptance Criteria

- [ ] A backdated `received` row whose lease is renewed BETWEEN the watchdog SELECT
      and its promotion UPDATE is NOT promoted to `crashed` (atomic predicate;
      TOCTOU closed).
- [ ] A backdated `received` row with an EXPIRED or NULL lease IS promoted to
      `crashed` (fail-closed).
- [ ] A trace whose initial `write_lease` fails aborts to a terminal failure, not
      a NULL-lease continuation.
- [ ] A happy-path terminal write NULLs the lease in the SAME UPDATE; the guard's
      Drop performs no I/O after `cancel()`.
- [ ] `ACTIVE_TRACES` / `is_trace_active` are deleted; liveness read only from the
      shared receipts row.
- [ ] The migration is additive/idempotent from a PRE-v3 DB (columns survive the
      v3 rebuild ordering); `SCHEMA_VERSION` = 4.
- [ ] `otto ci` green.

## Resolved Decisions

- **2026-07-24 -- shared lease, retire ACTIVE_TRACES (fix A, Scott: must-fix).**
- **2026-07-24 -- lease check is ATOMIC in the promotion UPDATE, not just the
  SELECT (panel finding 1, verified receipts.rs:493).** Closes the TOCTOU where a
  renew races a scan and a live trace is falsely reaped.
- **2026-07-24 -- clear folded into the terminal UPDATE; Drop clears only on
  panic/cancel via `cancel()` (panel finding 1).** Removes happy-path Drop I/O
  (blocking SQLite on a Tokio worker) and the clear-ordering gap.
- **2026-07-24 -- acquire fails CLOSED (panel finding 4, pipeline.rs:167).** A
  failed initial `write_lease` aborts to terminal failure, never a NULL-lease
  continuation.
- **2026-07-24 -- v4 columns added AFTER the v3 rebuild; test seeds pre-v3 (panel
  finding 2, receipts.rs:217).** The v3 fixed-column `INSERT...SELECT` would drop
  columns added before it.
- **2026-07-24 -- Phase 0 CLOSED: no heartbeat (the number).** Permit cap 8
  (`DEFAULT_MAX_CONCURRENT_TRACES`), hard_timeout 1800s. The entry-lease (1860s)
  expires before permit grant only behind >= 2 full batches of permit-holders each
  running ~1800s -> >= 17 simultaneously in-flight traces each consuming ~full
  hard-timeout. Does not occur single-user: live ingest is human-paced/sporadic;
  harvest (the volume path) is SEQUENTIAL in its own process (one general permit
  at a time). Typical hold ~13s. So renew-at-permit suffices; NO heartbeat.
  Revisit only if the deployment becomes high-throughput/multi-tenant. Replay
  `--from-stage 2` writes no `received` receipt -> no lease needed (confirmed).
- **2026-07-24 -- rejected owner-PID liveness (fix B) and watchdog-scoping (fix
  C).** PID reuse / "alive != working on THIS trace"; scoping orphans a dead
  harvest CLI (violates fail-closed). `lease_owner_pid` is diagnostic-only.
- **2026-07-24 -- lease reap gets a distinct `failure_reason` (panel finding 7).**
- **2026-07-24 -- SQLite is the substrate, no new deps.** WAL + `busy_timeout` +
  NORMAL already set (`receipts.rs:99`).

## Alternatives Considered

### Alternative 1: owner-PID liveness gate
- **Cons:** PID reuse masks a dead trace; "alive" != "working on THIS trace"; not
  fail-closed. **Why not:** unsafe signal; PID kept diagnostic-only.

### Alternative 2: scope the watchdog to daemon-owned traces
- **Cons:** a harvest CLI that dies mid-run leaves an orphan no one reaps.
  **Why not:** violates fail-closed; relocates the problem.

### Alternative 3: harvest POSTs to the daemon
- **Cons:** substantial re-architecture; serializes harvest through the daemon.
  **Why not:** the lease fixes the bug without re-plumbing harvest; parked.

## Technical Considerations

### Dependencies
- `borg::receipts` (schema + lease primitives + atomic promotion), `borg::pipeline`
  (guard sites), `borg::watchdog` (predicate). No external crates.

### Performance
- Two small UPDATEs per trace (acquire + renew); clear rides the existing terminal
  UPDATE (no extra happy-path I/O). One added SQL clause on the watchdog scan.

### Security
- No new secrets, no network. Local shared SQLite only.

### Testing Strategy
- `receipts` unit tests: write/renew, terminal-clear-in-one-UPDATE, `list_stale`
  and `promote_single_to_crashed` predicates (fresh excluded/unmatched, expired
  included/promoted), `now` injected. `watchdog/tests.rs` TOCTOU regression
  (renew-races-scan not promoted; inverting the UPDATE predicate bites) + an
  end-to-end `process_content` liveness test (Phase 4). Migration idempotency from
  a PRE-v3 seeded DB. Each test fails against pre-fix code.

### Rollout Plan
- Ship in the sb binary via direct-to-main + tag. `otto deploy` restarts the
  daemon (new watchdog) and installs the new `sb` (lease-writing harvest). The
  migration runs on first open of the daemon-host receipts DB (per-host). Blast
  radius: additive receipts columns, back-compatible (older `sb` ignores them;
  explicit-column SELECTs unaffected). Single binary, single tag, no cross-repo
  coupling.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| TOCTOU: renew races scan -> live trace reaped | Med | High | Lease predicate repeated ATOMICALLY in the promotion UPDATE (finding 1) |
| Pre-permit queue-wait exceeds the lease | Low | Med | Phase 0 produces the number; heartbeat added only if it exceeds 1860s |
| v4 columns dropped by the v3 rebuild | Med | High | v4 ADD ordered AFTER the v3 block; pre-v3-seeded idempotency test (finding 2) |
| Acquire fails open -> instant false reap | Med | High | Acquire fails CLOSED: write_lease failure aborts to terminal failure (finding 4) |
| Drop blocks a Tokio worker on SQLite | Low | Med | Happy-path clear rides the terminal UPDATE; `cancel()` disarms Drop; Drop I/O only on panic/cancel |
| Lease clear fails (DB busy) | Low | Low | Lease still EXPIRES; next scan reaps; WARN-not-panic |

## Open Questions

None. Fix A is chosen with rationale; the atomic-promotion, fail-closed-acquire,
migration-ordering, and terminal-clear findings are folded. The one mechanism
unknown -- renew-at-permit vs a queued heartbeat -- is a Phase-0 spike deliverable
with a FALSIFIABLE criterion (a worst-case queue-wait number vs 1860s) and a
stated default (renew-at-permit) the design is robust to. Replay's `--from-stage
2` runs `process_session` in-CLI (`replay.rs:443`) but writes no `received`
receipt, so it needs no lease (confirmed in Phase 0).

## References

- `borg/src/watchdog.rs:24,34,53,57,82,94` (deadline, `run_once`, predicate seam)
- `borg/src/pipeline/permits.rs:96,110,119,137,153` (`ACTIVE_TRACES`, guard, `is_trace_active` -- removed)
- `borg/src/pipeline.rs:167,186,190,323,351,358` (IngestResult return, guard acquire, permit grant, hard-timeout, terminal write)
- `borg/src/receipts.rs:42,99,173,188,193,217,411,447,468,493` (SCHEMA_VERSION, pragmas, `has_column`, migrations, `degraded` precedent, v3 REBUILD, `mark_succeeded`/`mark_failed`, `list_stale`, `promote_single_to_crashed`)
- `borg/src/receipts/schema.sql`; `vault/src/search/schema.rs:438` (`ensure_superseded_by_column` precedent)
- `borg/src/harvest/publish.rs:140,163`; `borg/src/harvest.rs:354`; `borg/src/replay.rs:443`; `sb/src/cli/borg.rs:347`; `borg/src/lib.rs:447`; `borg/src/config.rs:326` (GENERAL_PERMITS); `borg/src/routes.rs:154,278,475`
- `borg/src/watchdog/tests.rs` (conn-injectable fixture)
- Companion: `docs/design/2026-07-24-harvest-distill-parsing-robustness.md`
- Evidence: `~/.local/share/sb/borg.log` (hv-741468 reaped 01:54:40, door 01:22:59)
