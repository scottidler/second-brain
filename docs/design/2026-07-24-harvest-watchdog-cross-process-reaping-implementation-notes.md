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
