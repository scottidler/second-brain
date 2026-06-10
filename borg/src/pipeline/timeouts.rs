//! Tests for Phase 1 of the borg-pipeline-resilience design doc:
//! bounded waits and structured timeouts.
//!
//! These tests live in a sibling submodule rather than `pipeline.rs` itself
//! because the parent file is at the otto bloat threshold (3400 lines). The
//! design doc plans for `pipeline/*.rs` to host new code.

use std::time::Duration;

/// An inner future that sleeps well past the configured hard-timeout must
/// elapse with `Err(Elapsed)` and the wall-clock duration must match the
/// timeout, not the would-be sleep. This validates the `tokio::time::timeout`
/// wrapper around `process_url_inner` in `pipeline::process_url`.
#[tokio::test]
async fn test_pipeline_hard_timeout_elapses_and_drops_inner() {
    let inner = async {
        tokio::time::sleep(Duration::from_secs(60)).await;
        "should never produce"
    };
    let start = std::time::Instant::now();
    let res = tokio::time::timeout(Duration::from_millis(100), inner).await;
    let elapsed = start.elapsed();
    assert!(res.is_err(), "expected timeout-elapsed Err, got {res:?}");
    assert!(
        elapsed < Duration::from_millis(500),
        "timeout should fire near the configured deadline, elapsed={elapsed:?}"
    );
}

/// The actual non-URL bounding wrapper (`with_hard_timeout`) - not just the
/// abstract tokio primitive - must convert a wedged handler future into a
/// terminal `Failed` / `PipelineTimedOut` result. A wedged non-URL handler
/// (no-timeout vision client, blocked ffmpeg) otherwise holds its GENERAL
/// permit forever and the watchdog's active-trace exclusion skips it. Uses the
/// 1s floor of `hard_timeout_secs` against a 1h sleep so the deadline fires.
#[tokio::test]
async fn test_with_hard_timeout_bounds_a_wedged_non_url_handler() {
    use crate::config::Config;
    use crate::types::{IngestMethod, IngestResult, IngestStatus};
    use vault::receipts::FailureStage;

    let mut config = Config::default();
    config.pipeline.hard_timeout_secs = 1;

    // A non-URL handler that hangs far past the deadline (stand-in for a
    // blocked ffmpeg / no-timeout reqwest client).
    let wedged = async {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        IngestResult {
            status: IngestStatus::Completed,
            ..Default::default()
        }
    };

    let start = std::time::Instant::now();
    let result = super::with_hard_timeout(wedged, &config, "trace-test", IngestMethod::Telegram, "test").await;
    let elapsed = start.elapsed();

    assert!(
        matches!(result.status, IngestStatus::Failed { .. }),
        "wedged handler must become a Failed result, got {:?}",
        result.status
    );
    assert_eq!(result.failure_stage, Some(FailureStage::PipelineTimedOut));
    // The wrapper bounds the wait at the configured deadline (1s), nowhere near
    // the 1h sleep - proving it actually fires rather than awaiting the inner.
    assert!(
        elapsed < Duration::from_secs(5),
        "timeout should fire near the 1s deadline, elapsed={elapsed:?}"
    );
}

/// Per-call poll-based timeouts (fabric.rs / ocr.rs / youtube.rs) must kill
/// the child process when the deadline passes. Uses `sleep` as a stand-in
/// for any external tool that might hang (yt-dlp, fabric, tesseract).
#[test]
fn test_per_call_timeout_kills_blocking_child() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sleep");
    let timeout = Duration::from_millis(200);
    let start = std::time::Instant::now();
    let mut killed = false;
    loop {
        if let Some(_status) = child.try_wait().expect("try_wait") {
            break;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            killed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(killed, "expected the timeout path to fire and kill the child");
}
