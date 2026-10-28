-- borg receipts schema.
--
-- One row per trace_id ever delivered to any borg front door. Mutated in
-- place at terminal time: received -> succeeded, or received -> failed
-- with one of seven failure_stage values.
--
-- Current schema version is recorded in the `schema_version` table (see
-- borg::receipts::SCHEMA_VERSION). Future migrations land in a sibling
-- `migrations/` directory and are run in lexical order; this file is the
-- baseline.
--
-- The four mandatory PRAGMAs (journal_mode=WAL, synchronous=NORMAL,
-- busy_timeout=5000, foreign_keys=ON) are applied per-connection in
-- borg::receipts::apply_pragmas; this file holds the schema only.

CREATE TABLE IF NOT EXISTS receipts (
  trace_id        TEXT NOT NULL PRIMARY KEY,
  received_at     TEXT NOT NULL,
  method          TEXT NOT NULL,
  kind            TEXT NOT NULL
                    CHECK (kind IN ('url', 'text', 'binary', 'session')),
  raw_input       TEXT NOT NULL,
  status          TEXT NOT NULL
                    CHECK (status IN ('received', 'succeeded', 'failed', 'rejected')),
  terminal_at     TEXT,
  note_path       TEXT,
  failure_stage   TEXT
                    CHECK (failure_stage IS NULL OR failure_stage IN (
                      'intake-rejected', 'classify-failed', 'fetch-failed',
                      'quality-blocked', 'pipeline-timed-out', 'publish-failed',
                      'crashed'
                    )),
  failure_reason  TEXT,
  replay_of       TEXT,
  -- 1 when the note was published from a distill FALLBACK (the distiller
  -- could not produce a clean structured artifact and degraded gracefully
  -- rather than halting). Queryable via `sb borg log --degraded`. This
  -- formally retires the old "hard distill failures halt and route to DLQ"
  -- policy: degraded publishes are the documented behavior, now visible.
  degraded        INTEGER NOT NULL DEFAULT 0,
  -- Renewable cross-process liveness lease (harvest-watchdog-cross-process-
  -- reaping design). `lease_owner_pid` is the owning process's
  -- `std::process::id()` -- DIAGNOSTIC only (which process holds it), never
  -- the liveness gate, because a PID can be reused after its process dies.
  -- `lease_until` (TIMESTAMP_FMT) IS the liveness gate: NULL means no live
  -- lease; a non-NULL value in the past means the owning process stopped
  -- renewing (fail closed, reap-eligible past the received_at deadline).
  lease_owner_pid INTEGER DEFAULT NULL,
  lease_until     TEXT DEFAULT NULL,
  -- When the trace got its general permit (TIMESTAMP_FMT); NULL = never
  -- granted. History, not liveness: never cleared at terminal time.
  started_at      TEXT DEFAULT NULL,
  -- Why a replay was sent (`borg::types::ReplayOrigin`, e.g.
  -- 'auto-remediation'); NULL for a first ingest or a replay sent without
  -- one. Written with `replay_of` in the INSERT that creates the row.
  replay_origin   TEXT DEFAULT NULL,
  -- 1 for a `sb borg distill` run (no note is published); set in the INSERT
  -- that creates the row. Ingest-health readers filter `scratch = 0`.
  scratch         INTEGER NOT NULL DEFAULT 0,
  -- The `sb borg ingest` bulk this URL item belongs to (`batches.id`); NULL
  -- for every unbatched ingest. Set in the INSERT that creates the row. Its
  -- index is created in `run_migrations`, after the column exists on old DBs.
  batch_id        TEXT DEFAULT NULL
);

CREATE INDEX IF NOT EXISTS idx_receipts_status ON receipts(status);
CREATE INDEX IF NOT EXISTS idx_receipts_received_at ON receipts(received_at);
CREATE INDEX IF NOT EXISTS idx_receipts_method_status ON receipts(method, status);

-- One row per source URL `sb borg eval --remediate --apply` has claimed.
-- The claim is inserted in the same transaction that counts prior
-- auto-remediation attempts for the URL (this table plus receipts rows with
-- replay_origin='auto-remediation'), so two concurrent runs cannot both
-- post. `trigger_*` freezes the eval_scores key (run_kind 'baseline',
-- run_id '') of the current row that failed, the "before" score.
CREATE TABLE IF NOT EXISTS remediations (
  source_url           TEXT NOT NULL PRIMARY KEY,
  original_trace       TEXT NOT NULL,
  claimed_at           TEXT NOT NULL,
  -- The trace /ingest returned; NULL while the POST is in flight or when
  -- its response was lost (the receipts row still carries the lineage).
  remediation_trace    TEXT,
  trigger_judge_model  TEXT NOT NULL,
  trigger_check        TEXT NOT NULL,
  trigger_rubric       TEXT NOT NULL,
  trigger_content_hash TEXT NOT NULL,
  -- The trigger row's metrics_json, copied at claim time: the frozen "before".
  -- An eval_scores row on the same key can be overwritten later (a rescore,
  -- `--inject --rebuild-cache`); this copy never is. NULL only on a claim
  -- made before the column existed.
  trigger_metrics_json TEXT,
  -- Set when the POST's outcome is unknown (timeout, unreadable answer).
  post_error           TEXT
);

-- One row per `sb borg ingest` bulk (docs/design/2026-10-27-multi-target-ingest.md).
-- Created at intake by the first member (INSERT OR IGNORE); membership is
-- every receipts row carrying the id. The client's seal records the trace ids
-- it saw queued (a JSON array) so the daemon knows submission is over.
-- `summarized_at` is the exactly-once claim on the one summary.
CREATE TABLE IF NOT EXISTS batches (
  id               TEXT NOT NULL PRIMARY KEY,
  created_at       TEXT NOT NULL,
  sealed_trace_ids TEXT NULL,
  sealed_at        TEXT NULL,
  summarized_at    TEXT NULL
);

CREATE TABLE IF NOT EXISTS schema_version (
  version INTEGER NOT NULL PRIMARY KEY,
  applied_at TEXT NOT NULL
);
