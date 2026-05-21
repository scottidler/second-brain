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
                    CHECK (kind IN ('url', 'text', 'binary')),
  raw_input       TEXT NOT NULL,
  status          TEXT NOT NULL
                    CHECK (status IN ('received', 'succeeded', 'failed')),
  terminal_at     TEXT,
  note_path       TEXT,
  failure_stage   TEXT
                    CHECK (failure_stage IS NULL OR failure_stage IN (
                      'intake-rejected', 'classify-failed', 'fetch-failed',
                      'quality-blocked', 'pipeline-timed-out', 'publish-failed',
                      'crashed'
                    )),
  failure_reason  TEXT,
  replay_of       TEXT
);

CREATE INDEX IF NOT EXISTS idx_receipts_status ON receipts(status);
CREATE INDEX IF NOT EXISTS idx_receipts_received_at ON receipts(received_at);
CREATE INDEX IF NOT EXISTS idx_receipts_method_status ON receipts(method, status);

CREATE TABLE IF NOT EXISTS schema_version (
  version INTEGER NOT NULL PRIMARY KEY,
  applied_at TEXT NOT NULL
);
