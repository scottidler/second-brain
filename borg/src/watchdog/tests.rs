#![allow(clippy::unwrap_used)]

use super::*;
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

/// A stale `received` row whose trace is not active is promoted to crashed.
#[test]
fn run_once_conn_promotes_stale_received_to_crashed() {
    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, "old", Method::Http, ReceiptKind::Url, "u").unwrap();
    backdate(&conn, "old");

    let promoted = run_once_conn(&conn, 60, &|_| false).unwrap();
    assert_eq!(promoted, 1);
    assert_eq!(status_of(&conn, "old"), "failed");
    let r = receipts::get(&conn, "old").unwrap().unwrap();
    assert_eq!(r.failure_stage.as_deref(), Some("crashed"));
}

/// A stale row whose trace is still active in the pipeline is NOT promoted -
/// it is legitimately mid-flight (e.g. queued for a HEAVY_PERMIT for hours).
#[test]
fn run_once_conn_skips_active_traces() {
    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, "busy", Method::Http, ReceiptKind::Url, "u").unwrap();
    backdate(&conn, "busy");

    let promoted = run_once_conn(&conn, 60, &|t| t == "busy").unwrap();
    assert_eq!(promoted, 0);
    assert_eq!(status_of(&conn, "busy"), "received");
}

/// A fresh `received` row (within deadline) is left alone.
#[test]
fn run_once_conn_skips_fresh_rows() {
    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, "fresh", Method::Http, ReceiptKind::Url, "u").unwrap();
    let promoted = run_once_conn(&conn, 60, &|_| false).unwrap();
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

    let promoted = run_once_conn(&conn, 60, &|_| false).unwrap();
    assert_eq!(promoted, 0);
    assert_eq!(status_of(&conn, "done"), "succeeded");
}
