//! Tests for the v8 `scratch` column: the migration, the INSERT-time flag, and
//! the ingest-health counters that must not count a scratch run.

use super::*;
use vault::receipts::{FailureStage, ReceiptKind};
use vault::schema::Method;

fn scratch_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM receipts WHERE scratch = 1", [], |r| r.get(0))
        .expect("count scratch")
}

/// A v7 DB (the baseline minus `scratch`, stamped version 7) keeps every row
/// through the migration and none of them reads as scratch.
#[test]
fn v8_migration_keeps_every_v7_row_and_marks_none_scratch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("receipts.db");
    {
        let conn = Connection::open(&path).expect("open raw v7 db");
        apply_pragmas(&conn).expect("pragmas");
        conn.execute_batch(
            "CREATE TABLE receipts (
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
               degraded        INTEGER NOT NULL DEFAULT 0,
               lease_owner_pid INTEGER DEFAULT NULL,
               lease_until     TEXT DEFAULT NULL,
               started_at      TEXT DEFAULT NULL,
               replay_origin   TEXT DEFAULT NULL
             );
             CREATE TABLE schema_version (version INTEGER NOT NULL PRIMARY KEY, applied_at TEXT NOT NULL);
             INSERT INTO schema_version (version, applied_at) VALUES (7, '2026-10-01T00:00:00Z');
             INSERT INTO receipts (trace_id, received_at, method, kind, raw_input, status)
               VALUES ('v7-a', '2026-10-01T00:00:00Z', 'http', 'url', 'https://a.example', 'succeeded'),
                      ('v7-b', '2026-10-02T00:00:00Z', 'cli', 'url', 'https://b.example', 'received');",
        )
        .expect("seed v7 schema");
    }
    let conn = open_at(&path).expect("open migrates v7 to current");
    assert!(has_column(&conn, "receipts", "scratch").expect("probe"));
    assert_eq!(row_count(&conn).expect("count"), 2, "every v7 row survives");
    assert_eq!(scratch_count(&conn), 0, "no migrated row is scratch");
    assert_eq!(get(&conn, "v7-a").expect("get").expect("row").status, "succeeded");
    let v: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .expect("max version");
    assert_eq!(v, SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 8);
    drop(conn);
    let again = open_at(&path).expect("second open is idempotent");
    assert_eq!(row_count(&again).expect("count"), 2);
}

#[test]
fn fresh_db_has_scratch_column_defaulting_to_zero() {
    let conn = open_memory().expect("open");
    assert!(has_column(&conn, "receipts", "scratch").expect("probe"));
    record_received(&conn, "t1", Method::Cli, ReceiptKind::Url, "u").expect("insert");
    assert!(!get(&conn, "t1").expect("get").expect("row").scratch);
}

#[test]
fn record_received_scratch_sets_the_flag_in_the_insert() {
    let conn = open_memory().expect("open");
    record_received_scratch(&conn, "s1", Method::Cli, ReceiptKind::Url, "https://x.example").expect("insert");
    let r = get(&conn, "s1").expect("get").expect("row");
    assert!(r.scratch, "the INSERT that creates the row carries scratch");
    assert_eq!(r.status, "received");
    assert_eq!(scratch_count(&conn), 1);
}

/// The count helpers behind `/health/audit` measure ingest health: a scratch
/// row that would otherwise count (failed, crashed, degraded-succeeded) moves
/// none of them.
#[test]
fn health_counters_exclude_scratch_rows() {
    let conn = open_memory().expect("open");
    record_received_scratch(&conn, "s-ok", Method::Cli, ReceiptKind::Url, "u").unwrap();
    mark_succeeded(&conn, "s-ok", "n.md", true).unwrap();
    record_received_scratch(&conn, "s-fail", Method::Cli, ReceiptKind::Url, "u").unwrap();
    mark_failed(&conn, "s-fail", FailureStage::FetchFailed, "x").unwrap();
    record_received_scratch(&conn, "s-crash", Method::Cli, ReceiptKind::Url, "u").unwrap();
    mark_failed(&conn, "s-crash", FailureStage::Crashed, "x").unwrap();
    record_received_scratch(&conn, "s-wait", Method::Cli, ReceiptKind::Url, "u").unwrap();

    assert_eq!(count_by_status(&conn).unwrap(), (0, 0, 0, 0));
    assert!(count_failed_by_stage(&conn).unwrap().is_empty());
    let since = hours_ago_iso(24);
    assert_eq!(count_failed_since(&conn, &since).unwrap(), 0);
    assert_eq!(count_crashed_since(&conn, &since).unwrap(), 0);
    assert_eq!(count_degraded_since(&conn, &since).unwrap(), 0);

    // The same rows as ingests do count, so the zeros above are the filter's doing.
    record_received(&conn, "i-fail", Method::Cli, ReceiptKind::Url, "u").unwrap();
    mark_failed(&conn, "i-fail", FailureStage::Crashed, "x").unwrap();
    assert_eq!(count_by_status(&conn).unwrap(), (0, 0, 1, 0));
    assert_eq!(count_failed_since(&conn, &since).unwrap(), 1);
    assert_eq!(count_crashed_since(&conn, &since).unwrap(), 1);
}
