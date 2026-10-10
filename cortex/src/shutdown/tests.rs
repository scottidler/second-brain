use super::*;
use std::time::Duration;

#[test]
fn never_is_not_stopped() {
    let handle = StopHandle::never();
    assert!(!handle.is_stopped());
    assert!(!handle.clone().is_stopped());
}

#[tokio::test]
async fn never_stopped_does_not_resolve() {
    let handle = StopHandle::never();
    let waited = tokio::time::timeout(Duration::from_millis(50), handle.stopped()).await;
    assert!(waited.is_err(), "stopped() resolved on a handle nothing set");
}

#[tokio::test]
async fn stop_is_seen_by_every_clone() {
    let handle = StopHandle::never();
    let clone = handle.clone();
    handle.stop();
    assert!(clone.is_stopped());
    tokio::time::timeout(Duration::from_secs(1), clone.stopped())
        .await
        .expect("stopped() after stop() resolves at once");
}

#[tokio::test]
async fn stop_wakes_a_waiter_registered_before_it() {
    let handle = StopHandle::never();
    let waiter = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.stopped().await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    handle.stop();
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("waiter woke")
        .expect("waiter task");
}
