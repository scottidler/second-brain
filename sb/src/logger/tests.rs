#![allow(clippy::unwrap_used)]

use super::*;
use std::io::Write;
use tempfile::TempDir;
use vault::logging::LOG_ROTATE_MAX_BYTES;

/// Drives `rotating_non_blocking` - the exact writer stack `oracle serve`
/// installs - past the byte cap and asserts a backup appeared. No subscriber
/// init here: `tracing_subscriber::fmt().init()` is process-global and would
/// break every other test in this binary.
///
/// Writes are 1 MiB each, far under the 128,000-line channel bound, so the
/// lossy channel drops nothing and the background thread really writes every
/// byte. Dropping the guard signals shutdown and waits for the writer's ack.
#[test]
fn test_oracle_serve_writer_rotates_past_the_byte_cap() {
    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("oracle.log");
    let (writer, guard) = rotating_non_blocking(&log_path).unwrap();

    let chunk = vec![b'a'; 1024 * 1024];
    let mut sink = writer.clone();
    for _ in 0..(LOG_ROTATE_MAX_BYTES / chunk.len() + 2) {
        sink.write_all(&chunk).unwrap();
    }
    assert_eq!(writer.error_counter().dropped_lines(), 0, "channel bound was hit");
    drop(sink);
    drop(guard);

    let rotated = dir.path().join("oracle.log.1");
    assert!(
        rotated.exists(),
        "expected oracle.log.1 once total bytes exceeded LOG_ROTATE_MAX_BYTES"
    );
}

/// The non-serve paths install no drop probe, so the shutdown line reports 0
/// rather than a stale or fabricated count.
#[test]
fn test_dropped_log_lines_is_zero_without_a_probe() {
    assert_eq!(vault::logging::dropped_log_lines(), 0);
}

#[test]
fn pid_scoped_log_path_isolates_each_serve_process() {
    let base = std::path::Path::new("/tmp/sb/oracle.log");
    let scoped = super::pid_scoped_log_path(base);
    let expected = format!("oracle-{}.log", std::process::id());
    assert_eq!(scoped.file_name().unwrap().to_str().unwrap(), expected);
    assert_eq!(scoped.parent(), base.parent(), "must stay in the same directory");
    assert_ne!(scoped, base, "must not collide with the shared oracle.log");
}
