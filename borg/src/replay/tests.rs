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

/// Harvest-run integrity acceptance: "Replay during a held harvest
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
    let _xdg_sandbox = crate::receipts::sandbox::enter();

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
    let _xdg_sandbox = crate::receipts::sandbox::enter();

    let staging_dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.staging.enabled = true;
    config.staging.root = staging_dir.path().to_path_buf();
    // Point the daemon call at a held closed port - `hotkey.port` otherwise
    // defaults to 8181, which is the REAL borg daemon's port, and this test must
    // never risk hitting a daemon that happens to be running on the test machine.
    let closed = vault::testnet::closed_port();
    config.hotkey.host = "127.0.0.1".to_string();
    config.hotkey.port = closed.port();

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

#[test]
fn replay_of_a_failed_trace_ingested_with_a_note_sends_the_note() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.staging.enabled = true;
    config.staging.root = dir.path().to_path_buf();
    let store = FsArtifactStore::from_config(&config.staging);

    // The capture as `--note` delivered it to the daemon; Stage 0 stages it
    // before any gate or fetch, so a trace that then failed still holds it.
    let content = crate::types::ContentKind::Url {
        url: "https://example.com/article".to_string(),
        note: Some("for the retry-budget idea\non page 3".to_string()),
    };
    crate::stages::raw::write_capture(
        &store,
        "trace-failed",
        &content,
        IngestMethod::Cli,
        None,
        crate::stages::raw::capture_extra(&content),
    )
    .unwrap();

    let envelope = store.read_envelope("trace-failed").unwrap();
    let note = replay_note(&config, &envelope, "trace-failed").unwrap();
    let body = replay_body("https://example.com/article", "cli", note.as_deref());
    assert_eq!(body["note"], "for the retry-budget idea\non page 3");
    assert_eq!(body["force"], true);
}

#[test]
fn replay_body_omits_note_when_there_is_none() {
    let body = replay_body("https://example.com", "cli", None);
    assert!(body.get("note").is_none(), "{body}");
}

#[test]
fn capture_note_from_a_published_note_is_read_from_frontmatter() {
    let mut file = NamedTempFile::new().unwrap();
    write!(
        file,
        "---\ntitle: T\nsource: https://example.com\ncapture-note: |-\n  line one\n  line two\n---\nbody"
    )
    .unwrap();
    assert_eq!(
        capture_note_from_note(file.path()).unwrap().as_deref(),
        Some("line one\nline two")
    );
}

#[test]
fn capture_note_from_a_note_without_one_is_none() {
    let mut file = NamedTempFile::new().unwrap();
    write!(file, "---\ntitle: T\nsource: https://example.com\n---\nbody").unwrap();
    assert_eq!(capture_note_from_note(file.path()).unwrap(), None);
}

mod daemon_timeouts {
    use super::*;
    use crate::stub::{Behavior, serve};
    use std::time::{Duration, Instant};

    const T: Duration = Duration::from_millis(400);

    fn config_for(port: u16) -> Config {
        let mut config = Config::default();
        config.hotkey.host = "127.0.0.1".to_string();
        config.hotkey.port = port;
        config.hotkey.request_timeout = T;
        config
    }

    #[tokio::test]
    async fn reingest_via_a_silent_daemon_errs_within_twice_the_timeout() {
        let (port, _) = serve(Behavior::Silent).await;
        let started = Instant::now();
        let result = reingest_via_daemon(&config_for(port), "https://example.com", "cli", None).await;
        assert!(result.is_err());
        assert!(started.elapsed() < T * 2, "took {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn reingest_via_a_stalled_body_errs_within_twice_the_timeout() {
        let (port, _) = serve(Behavior::HeadersThenStall).await;
        let started = Instant::now();
        let result = reingest_via_daemon(&config_for(port), "https://example.com", "cli", None).await;
        assert!(result.is_err());
        assert!(started.elapsed() < T * 2, "took {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn reingest_401_with_a_non_json_body_names_the_401() {
        let (port, _) = serve(Behavior::Unauthorized).await;
        let err = reingest_via_daemon(&config_for(port), "https://example.com", "cli", None)
            .await
            .expect_err("401");
        let msg = format!("{err:#}");
        assert!(msg.contains("(401)"), "{msg}");
        assert!(!msg.to_lowercase().contains("parse"), "{msg}");
    }

    #[tokio::test]
    async fn trace_polling_stops_on_a_stalled_body_instead_of_looping_to_the_ceiling() {
        // A 200 whose body never completes is a Parse error: polling stops with
        // Err instead of looping to the ceiling.
        let (port, _) = serve(Behavior::HeadersThenStall).await;
        let config = config_for(port);
        let daemon = DaemonClient::new(&config.hotkey, None).expect("client");
        let started = Instant::now();
        let result = poll_trace_terminal(&config, &daemon, "trace-1").await;
        assert!(result.is_err());
        assert!(started.elapsed() < T * 2, "took {:?}", started.elapsed());
    }

    /// A terminal `/trace` body is a `Terminal` outcome carrying the trace's
    /// own result, never the ceiling: a failed trace and "stopped waiting" are
    /// different answers.
    #[tokio::test]
    async fn trace_polling_reports_a_terminal_trace_as_terminal() {
        for (body, want_completed) in [
            (
                r#"{"found":true,"trace_id":"trace-1","status":"succeeded","failure_stage":null,"note_path":"inbox/n.md"}"#,
                true,
            ),
            (
                r#"{"found":true,"trace_id":"trace-1","status":"failed","failure_stage":"fetch-failed","failure_reason":"boom","note_path":null}"#,
                false,
            ),
        ] {
            let (port, _) = serve(Behavior::Json(body.to_string())).await;
            let config = config_for(port);
            let daemon = DaemonClient::new(&config.hotkey, None).expect("client");
            let outcome = poll_trace_terminal(&config, &daemon, "trace-1").await.expect("polled");
            let PollOutcome::Terminal(result) = outcome else {
                panic!("a terminal body must be Terminal, got {outcome:?}");
            };
            assert_eq!(result.trace_id.as_deref(), Some("trace-1"));
            match result.status {
                IngestStatus::Completed => {
                    assert!(want_completed);
                    assert_eq!(result.note_path.as_deref(), Some("inbox/n.md"));
                }
                IngestStatus::Failed { reason } => {
                    assert!(!want_completed);
                    assert_eq!(reason, "fetch-failed: boom");
                }
                other => panic!("unexpected status {other:?}"),
            }
        }
    }
}

/// Both pollers map `CeilingReached` to this result, so its reason is the one
/// replay and reingest have always printed for the ceiling.
#[test]
fn ceiling_failure_keeps_the_reason_replay_and_reingest_print() {
    let result = ceiling_failure("trace-9");
    assert_eq!(result.trace_id.as_deref(), Some("trace-9"));
    assert!(
        matches!(&result.status, IngestStatus::Failed { reason }
            if reason == "trace did not reach a terminal state within the polling ceiling"),
        "{:?}",
        result.status
    );
}

/// The client halts on a paused YouTube breaker, which only the failure text
/// carries; the stage alone (`fetch-failed`) was all `/trace` used to send.
#[test]
fn failed_trace_reason_carries_the_failure_text() {
    assert_eq!(
        failed_trace_reason(
            Some("fetch-failed".into()),
            Some("youtube paused for another 60s".into())
        ),
        "fetch-failed: youtube paused for another 60s"
    );
    assert_eq!(failed_trace_reason(Some("crashed".into()), None), "crashed");
    assert_eq!(failed_trace_reason(None, Some("boom".into())), "boom");
    assert_eq!(failed_trace_reason(None, None), "failed");
}

/// A scratch (`sb borg distill`) trace is refused by name before any daemon
/// call. The control trace has identical staging but an ingest receipt, and it
/// does reach `POST /ingest`, so the zero below is the refusal's doing.
#[tokio::test]
async fn replay_one_refuses_a_scratch_trace_and_posts_nothing() {
    use crate::stub::{Behavior, serve};
    let home = TempDir::new().unwrap();
    crate::harvest::with_xdg_data_home(home.path(), || async {
        let staging_dir = TempDir::new().unwrap();
        let (port, seen) = serve(Behavior::Unauthorized).await;
        let mut config = Config::default();
        config.staging.enabled = true;
        config.staging.root = staging_dir.path().to_path_buf();
        config.hotkey.host = "127.0.0.1".to_string();
        config.hotkey.port = port;
        config.hotkey.request_timeout = std::time::Duration::from_millis(400);
        let vault_dir = TempDir::new().unwrap();
        config.vault.root_path = Some(vault_dir.path().display().to_string());

        let store = FsArtifactStore::from_config(&config.staging);
        let conn = crate::receipts::open_default().unwrap();
        for trace in ["tr-scratch", "tr-ingest"] {
            store
                .write_envelope(trace, &new_envelope(trace, IngestKind::ArticleUrl, IngestMethod::Cli))
                .unwrap();
            store.write_body(trace, b"https://example.com/article").unwrap();
        }
        let url = vault::receipts::ReceiptKind::Url;
        crate::receipts::record_received_scratch(&conn, "tr-scratch", vault::schema::Method::Cli, url, "u").unwrap();
        crate::receipts::record_received(&conn, "tr-ingest", vault::schema::Method::Cli, url, "u").unwrap();

        let posts = |seen: &std::sync::Mutex<Vec<String>>| {
            seen.lock()
                .unwrap()
                .iter()
                .filter(|r| r.starts_with("POST /ingest"))
                .count()
        };

        let err = replay_one(&config, "tr-scratch", 0, false, false, &mut |_| {})
            .await
            .expect_err("a scratch trace must not replay");
        assert!(format!("{err:#}").contains("tr-scratch"), "{err:#}");
        assert_eq!(posts(&seen), 0, "refused before any POST /ingest");

        let _ = replay_one(&config, "tr-ingest", 0, false, false, &mut |_| {}).await;
        assert_eq!(posts(&seen), 1, "control: an ingest trace reaches POST /ingest");
    })
    .await;
}

/// A host with no receipts DB (a laptop that only POSTs to the daemon) has no
/// scratch trace: the check passes without creating an empty DB there.
#[tokio::test]
async fn refuse_scratch_creates_no_receipts_db_on_a_host_without_one() {
    let home = TempDir::new().unwrap();
    crate::harvest::with_xdg_data_home(home.path(), || async {
        let path = vault::receipts::receipts_db_path().unwrap();
        assert!(!path.exists(), "precondition: {}", path.display());
        refuse_scratch("cl-doesnotexist").expect("no DB, no scratch trace");
        assert!(!path.exists(), "the check created {}", path.display());
    })
    .await;
}
