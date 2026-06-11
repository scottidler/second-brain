use super::*;

#[test]
fn test_try_acquire_returns_none_when_already_held() {
    let url = "https://example.com/inflight-test-already-held";
    let g1 = InflightGuard::try_acquire(url).expect("first acquire");
    assert!(InflightGuard::try_acquire(url).is_none());
    drop(g1);
    // After drop, the URL is releaseable again.
    assert!(InflightGuard::try_acquire(url).is_some());
}

#[test]
fn test_drop_releases_entry_on_panic_unwind() {
    let url = "https://example.com/inflight-test-panic";
    let result = std::panic::catch_unwind(|| {
        let _guard = InflightGuard::try_acquire(url).expect("acquire");
        panic!("simulated mid-pipeline panic");
    });
    assert!(result.is_err(), "panic should propagate to catch_unwind");
    // Drop ran during unwind; URL must be releaseable.
    assert!(
        InflightGuard::try_acquire(url).is_some(),
        "guard should release inflight entry during panic-unwind"
    );
}

#[test]
fn test_drop_releases_after_timeout_drop() {
    // Simulates the path where tokio::time::timeout fires and drops the
    // future holding the InflightGuard. The future never completes, but
    // Drop runs as the future is unwound.
    let url = "https://example.com/inflight-test-timeout-drop";
    {
        let _guard = InflightGuard::try_acquire(url).expect("acquire");
        // Pretend the enclosing future is dropped here (e.g. timeout fired).
    }
    assert!(InflightGuard::try_acquire(url).is_some());
}
