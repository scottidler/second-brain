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
