#![allow(clippy::unwrap_used)]

use super::*;
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
