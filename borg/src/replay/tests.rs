#![allow(clippy::unwrap_used)]

use super::*;
use std::io::Write;
use tempfile::{NamedTempFile, TempDir};

use crate::config::Config;
use crate::stages::artifact::{ArtifactStore, FsArtifactStore, new_envelope};

#[test]
fn parse_duration_days() {
    assert_eq!(parse_duration("7d").unwrap(), Duration::days(7));
}

#[test]
fn parse_duration_hours() {
    assert_eq!(parse_duration("24h").unwrap(), Duration::hours(24));
}

#[test]
fn parse_duration_minutes() {
    assert_eq!(parse_duration("30m").unwrap(), Duration::minutes(30));
}

#[test]
fn parse_duration_seconds() {
    assert_eq!(parse_duration("90s").unwrap(), Duration::seconds(90));
}

#[test]
fn parse_duration_rejects_bare_number() {
    assert!(parse_duration("7").is_err());
}

#[test]
fn parse_duration_rejects_multibyte_unit_without_panic() {
    // A malformed duration whose last char is multi-byte must error, not panic
    // (the old `split_at(len - 1)` split mid-codepoint).
    assert!(parse_duration("5é").is_err());
    assert!(parse_duration("30°").is_err());
}

#[test]
fn parse_duration_rejects_empty() {
    assert!(parse_duration("").is_err());
}

#[test]
fn read_source_from_note_extracts_url() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        "---\ntitle: Example\nsource: https://example.com/article\ntags: []\n---\nbody"
    )
    .unwrap();
    let source = read_source_from_note(file.path()).unwrap();
    assert_eq!(source, "https://example.com/article");
}

#[test]
fn read_source_from_note_errors_on_missing_source() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "---\ntitle: Example\n---\nbody").unwrap();
    let result = read_source_from_note(file.path());
    assert!(result.is_err());
}

#[test]
fn read_source_from_note_errors_on_no_frontmatter() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "Just a plain markdown file.").unwrap();
    let result = read_source_from_note(file.path());
    assert!(result.is_err());
}

#[test]
fn read_source_handles_quoted_value() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        "---\ntitle: Example\nsource: \"https://example.com/a b\"\n---\nbody"
    )
    .unwrap();
    let source = read_source_from_note(file.path()).unwrap();
    assert_eq!(source, "https://example.com/a b");
}

#[test]
fn read_method_from_note_extracts_telegram() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        "---\ntitle: Example\nmethod: telegram\nsource: https://example.com\n---\nbody"
    )
    .unwrap();
    let method = read_method_from_note(file.path()).unwrap();
    assert_eq!(method, Some("telegram".to_string()));
}

#[test]
fn read_method_from_note_extracts_cli() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        "---\ntitle: Example\nmethod: cli\nsource: https://example.com\n---\nbody"
    )
    .unwrap();
    let method = read_method_from_note(file.path()).unwrap();
    assert_eq!(method, Some("cli".to_string()));
}

#[test]
fn read_method_from_note_returns_none_when_missing() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "---\ntitle: Example\nsource: https://example.com\n---\nbody").unwrap();
    let method = read_method_from_note(file.path()).unwrap();
    assert_eq!(method, None);
}

#[test]
fn read_method_from_note_handles_quoted_value() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "---\ntitle: Example\nmethod: \"telegram\"\n---\nbody").unwrap();
    let method = read_method_from_note(file.path()).unwrap();
    assert_eq!(method, Some("telegram".to_string()));
}

/// Phase 2 (harvest-run integrity), acceptance: "Replay during a held harvest
/// lock fails with `HarvestLockHeld`, not a race." A session-trace
/// (`--from-stage 2`) replay must take the SAME exclusive harvest state lock
/// the nightly `sb borg harvest` run holds for its whole run - this test
/// holds that lock itself (standing in for a concurrent nightly run) and
/// asserts the replay fails instantly and by TYPE, not merely by message.
#[tokio::test]
async fn session_trace_replay_fails_loudly_when_harvest_lock_is_held() {
    let _guard = crate::harvest::TEST_XDG_LOCK.lock().await;
    let data_home = TempDir::new().unwrap();
    let prior_xdg = std::env::var("XDG_DATA_HOME").ok();
    unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };

    let staging_dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.staging.enabled = true;
    config.staging.root = staging_dir.path().to_path_buf();

    let store = FsArtifactStore::from_config(&config.staging);
    let trace_id = "hv-abc123";
    let envelope = new_envelope(trace_id, IngestKind::Session, IngestMethod::Harvest);
    store.write_envelope(trace_id, &envelope).unwrap();
    store
        .write_body(trace_id, b"irrelevant - the lock check runs before any staged read")
        .unwrap();

    // Hold the SAME harvest state lock a nightly `sb borg harvest` run would
    // hold for its whole run (`harvest::run_with`).
    let state_path = vault::paths::borg_harvest_state();
    let _held = crate::harvest::watermark::acquire_lock(&state_path).expect("test holds the lock first");

    let opts = ReplayOptions {
        trace_id: Some(trace_id.to_string()),
        from_stage: 2,
        ..Default::default()
    };
    let err = run(config, opts, |_| {})
        .await
        .expect_err("replay must fail while the harvest lock is held, not race it");
    assert!(
        err.downcast_ref::<crate::harvest::watermark::HarvestLockHeld>()
            .is_some(),
        "expected a typed HarvestLockHeld, got: {err:#}"
    );

    match prior_xdg {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
}

/// A URL replay (`from_stage == 0`, no session envelope) must NEVER take the
/// harvest lock - only a session-trace (`--from-stage 2`) replay does. Holding
/// the harvest lock must not block an ordinary URL replay's dispatch up to
/// (and including) the daemon HTTP call; this test only needs to prove the
/// lock is never acquired for a non-session trace, so a daemon connection
/// failure past that point is an expected, harmless error here.
#[tokio::test]
async fn url_trace_replay_never_takes_the_harvest_lock() {
    let _guard = crate::harvest::TEST_XDG_LOCK.lock().await;
    let data_home = TempDir::new().unwrap();
    let prior_xdg = std::env::var("XDG_DATA_HOME").ok();
    unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };

    let staging_dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.staging.enabled = true;
    config.staging.root = staging_dir.path().to_path_buf();
    // Point the daemon call at an ephemeral port nothing is listening on
    // (bind then immediately drop) - `hotkey.port` otherwise defaults to
    // 8181, which is the REAL borg daemon's port, and this test must never
    // risk hitting a daemon that happens to be running on the test machine.
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    config.hotkey.host = "127.0.0.1".to_string();
    config.hotkey.port = reserved.local_addr().unwrap().port();
    drop(reserved);

    let store = FsArtifactStore::from_config(&config.staging);
    let trace_id = "url-abc123";
    let envelope = new_envelope(trace_id, IngestKind::ArticleUrl, IngestMethod::Cli);
    store.write_envelope(trace_id, &envelope).unwrap();
    store.write_body(trace_id, b"https://example.com/article").unwrap();

    // Hold the harvest lock for the whole test - if a URL replay took it too,
    // this would deadlock/fail on re-entrancy; instead it must never even try.
    let state_path = vault::paths::borg_harvest_state();
    let _held = crate::harvest::watermark::acquire_lock(&state_path).expect("test holds the lock first");

    let opts = ReplayOptions {
        trace_id: Some(trace_id.to_string()),
        from_stage: 0,
        ..Default::default()
    };
    // No daemon is running in this test, so the HTTP call itself fails - the
    // point under test is that the failure is a connection error, never
    // `HarvestLockHeld` (proving no lock was attempted for this URL trace).
    let err = run(config, opts, |_| {})
        .await
        .expect_err("no daemon listening in this test");
    assert!(
        err.downcast_ref::<crate::harvest::watermark::HarvestLockHeld>()
            .is_none(),
        "a URL replay must never take the harvest lock: {err:#}"
    );

    match prior_xdg {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
}
