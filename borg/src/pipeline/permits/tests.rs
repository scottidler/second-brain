#![allow(clippy::unwrap_used)]

//! Tests for `PermitPool` and `ActiveTraceGuard`.
//!
//! Tests NEVER touch the production statics (`GENERAL_PERMITS`,
//! `HEAVY_PERMITS`, `ACTIVE_TRACES`). `cargo test` shares a process and
//! `OnceLock::set` only fires once - the first init's cap would win for
//! every subsequent test. Each test builds its own local `PermitPool` or
//! local `Mutex<HashSet>` and verifies behavior in isolation.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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

#[test]
fn active_trace_guard_inserts_and_removes_on_drop() {
    let set: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    {
        let _guard = ActiveTraceGuard::acquire_in(&set, "trace-abc");
        assert!(set.lock().unwrap().contains("trace-abc"));
    }
    assert!(!set.lock().unwrap().contains("trace-abc"));
}

#[test]
fn active_trace_guard_release_on_panic_unwind() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let set: Mutex<HashSet<String>> = Mutex::new(HashSet::new());

    let result = catch_unwind(AssertUnwindSafe(|| {
        let _guard = ActiveTraceGuard::acquire_in(&set, "trace-panic");
        assert!(set.lock().unwrap().contains("trace-panic"));
        panic!("simulated mid-pipeline panic");
    }));
    assert!(result.is_err(), "panic should propagate to catch_unwind");
    assert!(
        !set.lock().unwrap().contains("trace-panic"),
        "guard must release entry during panic-unwind"
    );
}

#[test]
fn active_trace_guard_multiple_traces_independent() {
    let set: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    let g1 = ActiveTraceGuard::acquire_in(&set, "trace-1");
    let g2 = ActiveTraceGuard::acquire_in(&set, "trace-2");
    assert_eq!(set.lock().unwrap().len(), 2);
    drop(g1);
    assert!(!set.lock().unwrap().contains("trace-1"));
    assert!(set.lock().unwrap().contains("trace-2"));
    drop(g2);
    assert!(set.lock().unwrap().is_empty());
}
