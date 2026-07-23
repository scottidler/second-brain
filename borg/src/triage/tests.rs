#![allow(clippy::unwrap_used)]

use super::*;
use crate::harvest::watermark::WatermarkState;
use vault::receipts::ReceiptKind;
use vault::schema::Method;

#[test]
fn audit_health_stats_conn_counts_by_status_and_window() {
    let conn = receipts::open_memory().unwrap();
    // 2 received, 1 succeeded, 2 failed (1 of which crashed) - all terminal now.
    receipts::record_received(&conn, "r1", Method::Http, ReceiptKind::Url, "u").unwrap();
    receipts::record_received(&conn, "r2", Method::Http, ReceiptKind::Url, "u").unwrap();
    receipts::record_received(&conn, "s1", Method::Http, ReceiptKind::Url, "u").unwrap();
    receipts::mark_succeeded(&conn, "s1", "n.md", false).unwrap();
    // d1 succeeds but degraded (distill fallback) - it counts as succeeded, yet
    // is the silent-quality signal degraded_24h must surface.
    receipts::record_received(&conn, "d1", Method::Http, ReceiptKind::Url, "u").unwrap();
    receipts::mark_succeeded(&conn, "d1", "d.md", true).unwrap();
    receipts::record_received(&conn, "f1", Method::Http, ReceiptKind::Url, "u").unwrap();
    receipts::mark_failed(&conn, "f1", FailureStage::FetchFailed, "x").unwrap();
    receipts::record_received(&conn, "x1", Method::Http, ReceiptKind::Url, "u").unwrap();
    receipts::mark_failed(&conn, "x1", FailureStage::Crashed, "x").unwrap();

    let h = audit_health_stats_conn(&conn).unwrap();
    assert_eq!(h.received, 2);
    assert_eq!(h.succeeded, 2, "s1 + the degraded d1");
    assert_eq!(h.failed, 2, "fetch-failed + crashed");
    assert_eq!(h.crashed, 1);
    assert_eq!(h.failed_24h, 2, "both failures are terminal just now");
    assert_eq!(h.crashed_24h, 1);
    assert_eq!(h.degraded_24h, 1, "d1 landed degraded just now");
}

// ---- harvest-completion Phase 6: the harvest drift guard (Opus SE K2). Never
// touches the real `~/.local/share/sb/borg/` state; drives the path/conn-
// injectable core directly with a temp state file + an in-memory receipts DB.

#[test]
fn harvest_drift_never_warns_before_the_timer_has_ever_run() {
    // Fresh install: no state file at all -> WatermarkState::load returns the
    // default (cursor: None). No prior run to have gone silent, so this is
    // never a warning regardless of the receipts DB.
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("harvest-state.json");
    let conn = receipts::open_memory().unwrap();

    let stats = harvest_drift_stats_at(&state_path, &conn, 3).unwrap();
    assert!(!stats.timer_has_run, "no state file -> the timer has never run");
    assert_eq!(stats.session_receipts_in_window, 0);
    assert!(
        !stats.should_warn(),
        "never-run harvest must never warn, even with zero receipts"
    );
}

#[test]
fn harvest_drift_does_not_warn_when_recent_session_receipts_exist() {
    // The timer HAS run (a cursor is set) and produced a session receipt
    // within the window -> healthy, no warning.
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("harvest-state.json");
    WatermarkState {
        cursor: Some(1500),
        ..Default::default()
    }
    .save(&state_path)
    .unwrap();
    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, "s1", Method::Cli, ReceiptKind::Session, "body").unwrap();

    let stats = harvest_drift_stats_at(&state_path, &conn, 3).unwrap();
    assert!(stats.timer_has_run);
    assert_eq!(stats.session_receipts_in_window, 1);
    assert!(!stats.should_warn(), "recent session activity -> no drift warning");
}

#[test]
fn harvest_drift_warns_when_timer_has_run_but_zero_recent_session_receipts() {
    // The exact structural signature of a FUTURE clyde contract drift: the
    // timer proved it can run (a cursor from a prior successful run is on
    // disk), but the recent window has ZERO session receipts of any status -
    // a total-abort drift dies before even a rejected receipt gets written.
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("harvest-state.json");
    WatermarkState {
        cursor: Some(1500),
        ..Default::default()
    }
    .save(&state_path)
    .unwrap();
    let conn = receipts::open_memory().unwrap();
    // Only an OLD session receipt, outside the window.
    receipts::record_received(&conn, "old", Method::Cli, ReceiptKind::Session, "body").unwrap();
    conn.execute(
        "UPDATE receipts SET received_at='2024-01-01T00:00:00Z' WHERE trace_id='old'",
        [],
    )
    .unwrap();
    // A non-session receipt inside the window must not satisfy the guard.
    receipts::record_received(&conn, "u1", Method::Http, ReceiptKind::Url, "u").unwrap();

    let stats = harvest_drift_stats_at(&state_path, &conn, 3).unwrap();
    assert!(stats.timer_has_run);
    assert_eq!(stats.session_receipts_in_window, 0);
    assert!(
        stats.should_warn(),
        "timer has run but zero recent session receipts -> WARN"
    );
}
