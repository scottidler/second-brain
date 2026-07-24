#![allow(clippy::unwrap_used)]

use super::*;
use crate::pipeline::permits::TraceLeaseGuard;
use vault::receipts::ReceiptKind;
use vault::schema::Method;

fn backdate(conn: &Connection, trace_id: &str) {
    conn.execute(
        "UPDATE receipts SET received_at='2024-01-01T00:00:00Z' WHERE trace_id=?",
        [trace_id],
    )
    .unwrap();
}

fn status_of(conn: &Connection, trace_id: &str) -> String {
    receipts::get(conn, trace_id).unwrap().unwrap().status
}

/// A stale `received` row with no lease is promoted to crashed.
#[test]
fn run_once_conn_promotes_stale_received_to_crashed() {
    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, "old", Method::Http, ReceiptKind::Url, "u").unwrap();
    backdate(&conn, "old");

    let promoted = run_once_conn(&conn, 60).unwrap();
    assert_eq!(promoted, 1);
    assert_eq!(status_of(&conn, "old"), "failed");
    let r = receipts::get(&conn, "old").unwrap().unwrap();
    assert_eq!(r.failure_stage.as_deref(), Some("crashed"));
}

/// A stale row whose lease is still fresh is NOT promoted - a live owner
/// (possibly a SEPARATE `sb borg harvest` process) is still renewing it. This
/// is the lease-based replacement for the old process-local active-trace skip.
#[test]
fn run_once_conn_skips_freshly_leased_traces() {
    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, "busy", Method::Http, ReceiptKind::Url, "u").unwrap();
    backdate(&conn, "busy");
    // A fresh lease expiring well in the future (the owner keeps renewing it).
    let until = (Utc::now() + chrono::Duration::seconds(1800))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    receipts::write_lease(&conn, "busy", 4242, &until).unwrap();

    let promoted = run_once_conn(&conn, 60).unwrap();
    assert_eq!(promoted, 0);
    assert_eq!(status_of(&conn, "busy"), "received");
}

/// A fresh `received` row (within deadline) is left alone.
#[test]
fn run_once_conn_skips_fresh_rows() {
    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, "fresh", Method::Http, ReceiptKind::Url, "u").unwrap();
    let promoted = run_once_conn(&conn, 60).unwrap();
    assert_eq!(promoted, 0);
    assert_eq!(status_of(&conn, "fresh"), "received");
}

/// Terminal rows are never candidates - `list_stale` returns only `received`
/// rows, so a backdated succeeded row is untouched.
#[test]
fn run_once_conn_skips_terminal_rows() {
    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, "done", Method::Http, ReceiptKind::Url, "u").unwrap();
    receipts::mark_succeeded(&conn, "done", "inbox/n.md", false).unwrap();
    backdate(&conn, "done");

    let promoted = run_once_conn(&conn, 60).unwrap();
    assert_eq!(promoted, 0);
    assert_eq!(status_of(&conn, "done"), "succeeded");
}

/// End-to-end (criterion 1): a trace holding a FRESH lease via the RAII guard
/// is NOT reaped by a concurrent watchdog scan over the SAME on-disk DB - the
/// cross-process false-crash this feature exists to prevent. A file-backed DB
/// (not `:memory:`) is required so the guard's own connection and the
/// watchdog's connection observe the same lease.
#[test]
fn run_once_conn_does_not_reap_a_freshly_leased_trace() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("receipts.db");
    let conn = receipts::open_at(&path).unwrap();
    receipts::record_received(&conn, "live", Method::Http, ReceiptKind::Url, "u").unwrap();
    backdate(&conn, "live");

    // A live owner (stand-in for a separate harvest process) holds the lease.
    let guard = TraceLeaseGuard::acquire_with_conn(receipts::open_at(&path).unwrap(), "live", 1800).unwrap();

    let promoted = run_once_conn(&conn, 60).unwrap();
    assert_eq!(promoted, 0, "a freshly-leased trace must not be reaped");
    assert_eq!(status_of(&conn, "live"), "received");

    guard.cancel();
}

/// End-to-end (criterion 2): while the guard is held the trace is safe; once
/// the guard is Dropped WITHOUT cancel() (panic/future-cancel), its lease is
/// cleared and the next watchdog scan reaps the orphan (fail-closed).
#[test]
fn run_once_conn_reaps_a_trace_whose_guard_dropped_without_cancel() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("receipts.db");
    let conn = receipts::open_at(&path).unwrap();
    receipts::record_received(&conn, "dead", Method::Http, ReceiptKind::Url, "u").unwrap();
    backdate(&conn, "dead");

    {
        let _guard = TraceLeaseGuard::acquire_with_conn(receipts::open_at(&path).unwrap(), "dead", 1800).unwrap();
        // Leased -> the concurrent scan leaves it alone.
        assert_eq!(run_once_conn(&conn, 60).unwrap(), 0);
        assert_eq!(status_of(&conn, "dead"), "received");
        // guard dropped here WITHOUT cancel() -> lease cleared
    }

    let promoted = run_once_conn(&conn, 60).unwrap();
    assert_eq!(promoted, 1, "an orphan whose lease was cleared on Drop IS reaped");
    assert_eq!(status_of(&conn, "dead"), "failed");
    let r = receipts::get(&conn, "dead").unwrap().unwrap();
    assert_eq!(r.failure_stage.as_deref(), Some("crashed"));
}
