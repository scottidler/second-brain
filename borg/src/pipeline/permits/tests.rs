#![allow(clippy::unwrap_used)]

//! Tests for `PermitPool` and `TraceLeaseGuard`.
//!
//! Tests NEVER touch the production `PermitPool` statics (`GENERAL_PERMITS`,
//! `HEAVY_PERMITS`). `cargo test` shares a process and `OnceLock::set` only
//! fires once - the first init's cap would win for every subsequent test.
//! Each test builds its own local `PermitPool`. The `TraceLeaseGuard` tests
//! open their own on-disk receipts DB (a `TempDir`) so the guard's owned
//! connection and the test's inspecting connection see the same lease.

use super::*;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use vault::receipts::ReceiptKind;
use vault::schema::Method;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn permit_pool_caps_concurrency_at_two() {
    let pool = Arc::new(PermitPool::new("test-cap-2"));
    pool.init(2);

    let in_flight = Arc::new(AtomicUsize::new(0));
    let watermark = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..5 {
        let pool = Arc::clone(&pool);
        let in_flight = Arc::clone(&in_flight);
        let watermark = Arc::clone(&watermark);
        handles.push(tokio::spawn(async move {
            let _permit = pool.acquire().await;
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            watermark.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let peak = watermark.load(Ordering::SeqCst);
    assert!(peak <= 2, "peak in-flight = {peak}, expected <= 2");
    assert!(peak >= 1, "expected at least 1 concurrent run, got {peak}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permit_pool_cap_of_one_serializes_acquires() {
    let pool = Arc::new(PermitPool::new("test-cap-1"));
    pool.init(1);

    let order = Arc::new(Mutex::new(Vec::<u32>::new()));

    let pool_a = Arc::clone(&pool);
    let order_a = Arc::clone(&order);
    let first = tokio::spawn(async move {
        let _permit = pool_a.acquire().await;
        order_a.lock().unwrap().push(1);
        tokio::time::sleep(Duration::from_millis(80)).await;
        order_a.lock().unwrap().push(2);
    });

    // Give first a head start so it definitely holds the only permit.
    tokio::time::sleep(Duration::from_millis(10)).await;

    let pool_b = Arc::clone(&pool);
    let order_b = Arc::clone(&order);
    let second = tokio::spawn(async move {
        let _permit = pool_b.acquire().await;
        order_b.lock().unwrap().push(3);
    });

    first.await.unwrap();
    second.await.unwrap();

    let recorded = order.lock().unwrap().clone();
    assert_eq!(recorded, vec![1, 2, 3], "second acquire must wait for first to drop");
}

#[test]
fn permit_pool_init_twice_is_noop() {
    let pool = PermitPool::new("test-init-twice");
    pool.init(4);
    // Second init is logged at warn and ignored; the original cap stands.
    pool.init(99);
    // Verify by checking available_permits via an acquire round-trip on
    // a Tokio runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let sem_available = rt.block_on(async {
        let _p1 = pool.acquire().await;
        let _p2 = pool.acquire().await;
        let _p3 = pool.acquire().await;
        let _p4 = pool.acquire().await;
        pool.inner.get().unwrap().available_permits()
    });
    assert_eq!(sem_available, 0, "init(99) must NOT have raised the cap above 4");
}

/// Build an on-disk receipts DB in a fresh `TempDir` with one `received` row,
/// returning the tempdir (keep it alive), the DB path, and a connection to
/// inspect it. The guard-under-test opens its OWN connection over the same
/// path so cross-connection lease writes are visible here.
fn seeded_receipts(trace_id: &str) -> (tempfile::TempDir, std::path::PathBuf, Connection) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("receipts.db");
    let conn = receipts::open_at(&path).unwrap();
    receipts::record_received(&conn, trace_id, Method::Http, ReceiptKind::Url, "u").unwrap();
    (tmp, path, conn)
}

/// Read `(lease_owner_pid, lease_until)` for a trace via the inspecting conn.
fn lease_of(conn: &Connection, trace_id: &str) -> (Option<i64>, Option<String>) {
    conn.query_row(
        "SELECT lease_owner_pid, lease_until FROM receipts WHERE trace_id=?",
        [trace_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

#[test]
fn trace_lease_guard_writes_lease_on_acquire() {
    let (_tmp, path, inspect) = seeded_receipts("tr-acq");
    let guard = TraceLeaseGuard::acquire_with_conn(receipts::open_at(&path).unwrap(), "tr-acq", 1800).unwrap();
    let (pid, until) = lease_of(&inspect, "tr-acq");
    assert_eq!(
        pid,
        Some(i64::from(std::process::id())),
        "acquire stamps the owning pid"
    );
    assert!(until.is_some(), "acquire writes a lease_until expiry");
    // Keep the guard cancelled so Drop does not clear behind the assertions.
    guard.cancel();
}

#[test]
fn trace_lease_guard_drop_without_cancel_clears_lease() {
    // The panic/future-cancel path: a guard dropped WITHOUT cancel() clears the
    // lease so a genuinely dead trace becomes reap-eligible (fail-closed).
    let (_tmp, path, inspect) = seeded_receipts("tr-drop");
    {
        let _guard = TraceLeaseGuard::acquire_with_conn(receipts::open_at(&path).unwrap(), "tr-drop", 1800).unwrap();
        let (_pid, until) = lease_of(&inspect, "tr-drop");
        assert!(until.is_some(), "lease is live while the guard is held");
    }
    let (pid, until) = lease_of(&inspect, "tr-drop");
    assert!(pid.is_none(), "Drop (uncancelled) NULLs lease_owner_pid");
    assert!(until.is_none(), "Drop (uncancelled) NULLs lease_until");
}

#[test]
fn trace_lease_guard_cancel_disarms_drop() {
    // BITE for the happy-path invariant: after cancel() the guard's Drop must
    // do NO write. If cancel failed to disarm Drop, Drop would clear the lease
    // and the assertions below (lease still present) would fail.
    let (_tmp, path, inspect) = seeded_receipts("tr-cancel");
    let guard = TraceLeaseGuard::acquire_with_conn(receipts::open_at(&path).unwrap(), "tr-cancel", 1800).unwrap();
    guard.cancel();
    let (pid, until) = lease_of(&inspect, "tr-cancel");
    assert_eq!(
        pid,
        Some(i64::from(std::process::id())),
        "cancel() disarms Drop: the lease it wrote is untouched"
    );
    assert!(until.is_some(), "cancel() disarms Drop: lease_until is not cleared");
}

#[test]
fn trace_lease_guard_renew_restamps_expiry() {
    let (_tmp, path, inspect) = seeded_receipts("tr-renew");
    // Deadline 0 => lease_until == now at acquire; a fresh renew re-stamps it.
    let guard = TraceLeaseGuard::acquire_with_conn(receipts::open_at(&path).unwrap(), "tr-renew", 1800).unwrap();
    let (_pid, before) = lease_of(&inspect, "tr-renew");
    guard.renew();
    let (pid, after) = lease_of(&inspect, "tr-renew");
    assert_eq!(
        pid,
        Some(i64::from(std::process::id())),
        "renew never touches lease_owner_pid"
    );
    assert!(before.is_some() && after.is_some(), "both lease_until values are set");
    guard.cancel();
}
