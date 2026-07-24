# Implementation Notes: watchdog cross-process false-crash (shared trace lease)

Design doc: `docs/design/2026-07-24-harvest-watchdog-cross-process-reaping.md`

## Phase 0: Spike -- bound daemon permit-starvation, decide heartbeat

### Design decisions
- **No heartbeat; renew-at-permit only.** Verified `DEFAULT_MAX_CONCURRENT_TRACES
  = 8` (`borg/src/config.rs`), `hard_timeout_secs = 1800`, and the harvest publish
  loop is sequential (`for record in export.sessions`, `borg/src/harvest.rs`). A
  trace's false-reap exposure is the pre-permit-grant queue-wait only (renew at
  grant resets the clock; the handler's own 1800s bounds work). With 8 permits
  each held <= 1800s, the 1860s entry-lease expires before grant only behind >= 17
  simultaneously in-flight traces each consuming ~full hard-timeout -- not a
  single-user daemon reality (sporadic human-paced live ingest; sequential harvest
  in its own process; typical hold ~13s). renew-at-permit is sufficient.

### Deviations
- None (spike, no code).

### Tradeoffs
- Chose renew-at-permit over a queued heartbeat: the heartbeat is dead weight at
  single-user scale and adds a background task + more DB writes. Documented the
  revisit trigger (high-throughput/multi-tenant) rather than building speculative
  capacity.

### Open questions
- None. Replay `--from-stage 2` runs `process_session` in-CLI (`replay.rs:443`)
  but writes no `received` receipt, so it needs no lease -- confirmed against the
  code path; the lease covers it regardless.

## Phase 1: Additive schema migration (trace lease columns)

### Design decisions
- **`lease_owner_pid INTEGER DEFAULT NULL` + `lease_until TEXT DEFAULT NULL`**
  added via the established `has_column` + single `ALTER TABLE ADD COLUMN`
  pattern (`receipts.rs::run_migrations`), mirroring the v2 `degraded` column
  precedent. Two separate idempotent probes (one per column) rather than one
  combined guard, so a partial prior failure (one column landed, the other
  didn't) still self-heals on the next open.
- **v4 block placed AFTER the v3 rebuild, BEFORE the `schema_version` match**
  (`receipts.rs:273` onward, following the v3 `CREATE TABLE receipts_v3 ...`
  block). Confirmed by a dedicated test that seeds a v1 DB (no `degraded`
  column, narrower CHECK constraints -- deliberately older than the existing
  v3-seeded precedent test) and drives it through the *real* `open_at()`
  entry point end-to-end, so the full v2 -> v3 -> v4 chain is exercised, not
  just v4 in isolation.
- **`SCHEMA_VERSION` bumped 3 -> 4.** `schema.sql` baseline updated so a fresh
  DB gets both columns directly (`fresh_db_has_lease_columns` proves this
  without touching the migration path at all).
- **Rollback-safety guard.** The existing `match current { Some(v) if v >=
  SCHEMA_VERSION => Ok(()), _ => ... }` arm already treats a stored version
  `>=` the code's `SCHEMA_VERSION` as a no-op -- never a downgrade, never a
  panic. Added a comment making this explicit as a named invariant, and a
  test (`migrations_do_not_panic_on_a_newer_stored_schema_version`) that bumps
  a fresh DB's stored version to `SCHEMA_VERSION + 95` and asserts
  `run_migrations` neither panics nor rewrites it back down. The column/table
  probes above are gated on structural presence, not on this counter, so they
  still run (and stay idempotent) regardless of the stored version.
- Function-level `log::debug!` on both new `ALTER TABLE` sites, consistent
  with the existing migration logging style in this file.

### Deviations
- None. `lease_owner_pid`/`lease_until` names, types, and ordering match the
  design doc's Data Model section exactly; no primitives (`write_lease`,
  `renew_lease`, predicate changes to `list_stale`/`promote_single_to_crashed`)
  were added -- those are Phase 2/3 scope and Phase 1 is schema-only, per the
  design doc's own phase split.

### Tradeoffs
- Considered folding both column adds into one `if` guarded by a single
  `has_column` check on `lease_until` only (since both are added together).
  Chose two independent probes instead: cheaper to reason about under a crash
  between the two `ALTER TABLE` statements (SQLite DDL is not transactional
  across statements in `execute_batch`, and these are two separate `execute`
  calls), and it costs nothing extra since `has_column` is a cheap
  `PRAGMA table_info` scan already paid on every open.

### Open questions
- None.

## Phase 2: Receipts lease primitives + atomic promotion + terminal-clear

### Design decisions
- **`write_lease(conn, trace_id, pid, lease_until)` / `renew_lease(conn,
  trace_id, lease_until)`** added exactly as specced in the API Design
  section: `params![]`-bound, no clock read inside either function (the
  caller computes and passes the already-formatted `lease_until` string).
  Both are guarded `WHERE trace_id=? AND status='received'` -- a lease can
  never be stamped onto (or renewed on) a row that has already reached a
  terminal state, matching the existing status-guard convention every other
  mutating function in this file already follows. 0-rows-affected logs at
  WARN rather than erroring (mirrors `mark_succeeded`/`mark_failed`'s
  no-op-on-terminal-row logging) -- Phase 4's guard is where "0 rows on the
  INITIAL write" becomes a fail-closed abort; this primitive stays a plain
  UPDATE.
- **Lease clear folded into `mark_succeeded`'s and `mark_failed`'s EXISTING
  UPDATE** (`SET ..., lease_owner_pid=NULL, lease_until=NULL`) rather than a
  second statement -- exactly the Resolved Decision's "no separate clear on
  the happy path, no redundant I/O" requirement. One transaction per terminal
  write, unchanged row-count semantics (`rows > 0` still means "this call
  performed the transition").
- **`list_stale(conn, deadline_secs, now: DateTime<Utc>)`** gains `AND
  (lease_until IS NULL OR lease_until < ?)` in the SELECT, bound from the
  caller-supplied `now` (formatted once via the existing `TIMESTAMP_FMT`).
  `now` is a new required parameter -- the function no longer reads
  `Utc::now()` internally, so a test (or the watchdog, which now reads the
  clock once per scan in `watchdog.rs::run_once_conn`) can hold the deadline
  and the lease-liveness check to the SAME instant.
- **`promote_single_to_crashed(conn, trace_id, deadline_secs, now)`** repeats
  the identical `(lease_until IS NULL OR lease_until < ?)` predicate directly
  in the UPDATE's `WHERE` clause -- this is the TOCTOU fix from the Resolved
  Decisions: the SELECT and the promotion UPDATE now agree on both "is this
  row old enough" and "is this row unleased", checked against the caller's
  single `now`, atomically as part of the same UPDATE that flips the status.
  A renew landing between the SELECT and this UPDATE makes the UPDATE match 0
  rows, so the trace is not reaped.
- **Distinct `failure_reason` for a lease-specific reap.** The UPDATE's SET
  clause uses a SQL `CASE WHEN lease_until IS NOT NULL AND lease_until < ?
  THEN 'lease-expired' ELSE <generic "no terminal event..."> END` so the
  distinction (an EXPIRED lease vs a row that never held one) is made by the
  same query that performs the promotion, rather than a second read-then-branch
  round trip. `LEASE_EXPIRED_REASON` is a named module constant, not an inline
  literal, per the no-magic-values convention already used for
  `TIMESTAMP_FMT`.
- **`watchdog.rs::run_once_conn`** reads `Utc::now()` once per scan and
  threads it into both `list_stale` and each `promote_single_to_crashed`
  call, so all rows in one scan pass are judged against the same instant.
  This is mechanical signature-following (the functions' new `now` parameter
  forces every caller to supply one) and is NOT the Phase 4 guard-wiring --
  no lease is written or renewed anywhere yet, so every row the watchdog
  currently sees has `lease_until IS NULL` and reaps exactly as it did before
  this phase.

### Deviations
- None. Signatures, guard placement, and the atomic-predicate repetition
  match the design doc's API Design and Resolved Decisions sections; the
  `now`-injection requirement forced updating `watchdog.rs`'s two call sites
  and the pre-existing `receipts::tests` call sites for `list_stale`/
  `promote_single_to_crashed` to pass a value, which is a mechanical
  same-effect change (previously the functions read the clock internally),
  not a design deviation.

### Tradeoffs
- Considered a two-step promotion (SELECT the lease state, then branch in
  Rust to choose the `failure_reason` string before a plain UPDATE) instead
  of the in-SQL `CASE`. Rejected: a two-step read-then-write reopens exactly
  the TOCTOU window the atomic UPDATE predicate exists to close (the branch
  decision and the UPDATE would no longer be the same atomic statement).
- Considered guarding `write_lease`/`renew_lease` to only the row's current
  `lease_owner_pid` (so a second process could not silently overwrite
  another's lease). Rejected for Phase 2: `lease_owner_pid` is explicitly
  diagnostic-only per the design doc ("never the liveness gate, PID reuse"),
  and only one process at a time should ever hold an entry lease on a given
  `trace_id` by construction (a trace is created once, by exactly one door);
  Phase 4's guard is the correct place to decide whether an owner check adds
  value once the write sites are wired in.

### Open questions
- None.
