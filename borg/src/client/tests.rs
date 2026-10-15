use super::*;
use crate::harvest::with_xdg_data_home;

#[tokio::test]
async fn test_ingest_connection_refused() {
    let closed = vault::testnet::closed_port();
    let config = Config {
        hotkey: config::HotkeyConfig {
            host: "127.0.0.1".to_string(),
            port: closed.port(),
            ..config::HotkeyConfig::default()
        },
        ..Config::default()
    };
    let result = ingest(
        config,
        "https://example.com".to_string(),
        None,
        false,
        types::IngestMethod::Cli,
        None,
    )
    .await;
    assert!(result.is_err());
    let err = format!("{}", result.expect_err("expected error"));
    assert!(
        err.contains("cannot reach the borg daemon"),
        "expected connection error message, got: {err}"
    );
}

// ---- daemon clients: bounded, status-checked, authenticated -----------------

fn config_for_daemon(port: u16, timeout: std::time::Duration) -> Config {
    Config {
        hotkey: config::HotkeyConfig {
            host: "127.0.0.1".to_string(),
            port,
            request_timeout: timeout,
            ..config::HotkeyConfig::default()
        },
        ..Config::default()
    }
}

const DAEMON_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(400);

async fn ingest_against(behavior: crate::stub::Behavior) -> (Result<IngestOutcome>, std::time::Duration) {
    let (port, _) = crate::stub::serve(behavior).await;
    let started = std::time::Instant::now();
    let result = ingest(
        config_for_daemon(port, DAEMON_TIMEOUT),
        "https://example.com".to_string(),
        None,
        false,
        types::IngestMethod::Cli,
        None,
    )
    .await;
    (result, started.elapsed())
}

#[tokio::test]
async fn ingest_against_a_silent_daemon_errs_within_twice_the_timeout() {
    let (result, took) = ingest_against(crate::stub::Behavior::Silent).await;
    assert!(result.is_err());
    assert!(took < DAEMON_TIMEOUT * 2, "took {took:?}");
}

#[tokio::test]
async fn ingest_against_a_stalled_body_errs_within_twice_the_timeout() {
    let (result, took) = ingest_against(crate::stub::Behavior::HeadersThenStall).await;
    assert!(result.is_err());
    assert!(took < DAEMON_TIMEOUT * 2, "took {took:?}");
}

#[tokio::test]
async fn ingest_401_with_a_non_json_body_names_the_401() {
    let (result, _) = ingest_against(crate::stub::Behavior::Unauthorized).await;
    let msg = result.expect_err("401").to_string();
    assert!(msg.contains("(401)"), "{msg}");
    assert!(!msg.to_lowercase().contains("parse"), "{msg}");
}

#[tokio::test]
async fn reingest_against_a_silent_daemon_reports_the_item_error_within_twice_the_timeout() {
    let data_home = tempfile::TempDir::new().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        let ledger_path = ledger::ledger_path().expect("ledger path");
        ledger::append_entry(
            &ledger_path,
            &ledger::LedgerEntry {
                date: "2026-10-05".to_string(),
                time: "10:00".to_string(),
                method: vault::schema::Method::Http,
                filename: Some("a.md".to_string()),
                source: "https://example.com/a".to_string(),
                trace_id: None,
            },
        )
        .expect("append");
        let (port, _) = crate::stub::serve(crate::stub::Behavior::Silent).await;
        let started = std::time::Instant::now();
        let report = reingest(
            config_for_daemon(port, DAEMON_TIMEOUT),
            true,
            None,
            None,
            None,
            None,
            false,
            1,
            |_| {},
        )
        .await
        .expect("a per-item error is reported, not fatal");
        assert!(started.elapsed() < DAEMON_TIMEOUT * 2, "took {:?}", started.elapsed());
        assert_eq!(report.processed.len(), 1);
        assert!(
            matches!(report.processed[0].status, ReingestEntryStatus::Error(_)),
            "{:?}",
            report.processed[0].status
        );
    })
    .await;
}

fn queried(filename: &str) -> ledger::QueriedEntry {
    ledger::QueriedEntry {
        date: "2026-10-05".to_string(),
        method: "cli".to_string(),
        slug: "slug".to_string(),
        filename: filename.to_string(),
        source: "https://example.com".to_string(),
        line_number: 1,
    }
}

#[test]
fn note_has_type_matches_the_type_line_in_notes_or_inbox() {
    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("inbox")).unwrap();
    std::fs::write(vault.path().join("inbox/a.md"), "---\ntype: video\n---\n").unwrap();

    assert!(note_has_type(vault.path(), &queried("a.md"), "video"));
    assert!(!note_has_type(vault.path(), &queried("a.md"), "article"));
    assert!(!note_has_type(vault.path(), &queried("missing.md"), "video"));
    assert!(!note_has_type(vault.path(), &queried("-"), "video"));
}

#[test]
fn note_has_type_warns_with_the_path_when_the_note_is_unreadable() {
    vault::capture::install();
    let vault = tempfile::tempdir().unwrap();
    // A directory where the note belongs: it exists, but reading it as text fails.
    std::fs::create_dir_all(vault.path().join("notes/reingest-unreadable.md")).unwrap();

    assert!(!note_has_type(
        vault.path(),
        &queried("reingest-unreadable.md"),
        "video"
    ));

    assert_eq!(vault::capture::warns_containing("reingest-unreadable.md").len(), 1);
}

#[test]
fn ingest_body_carries_the_note() {
    let body = ingest_body(
        "https://example.com",
        None,
        false,
        types::IngestMethod::Cli,
        Some("  why I saved it  "),
    );
    assert_eq!(body["note"], "why I saved it");
    assert_eq!(body["url"], "https://example.com");
}

#[test]
fn ingest_body_omits_a_blank_or_absent_note() {
    for note in [None, Some(""), Some("   ")] {
        let body = ingest_body("https://example.com", None, false, types::IngestMethod::Cli, note);
        assert!(body.get("note").is_none(), "{body}");
    }
}

#[test]
fn ingest_body_deserializes_as_an_ingest_request() {
    let body = ingest_body("https://example.com", None, true, types::IngestMethod::Cli, Some("n"));
    let req: types::IngestRequest = serde_json::from_value(body).unwrap();
    assert_eq!(req.note.as_deref(), Some("n"));
}

/// `--jobs 4` keeps four entries in flight: four silent-daemon items finish
/// in about one timeout, where one-at-a-time takes four.
#[tokio::test]
async fn reingest_jobs_runs_entries_concurrently() {
    let data_home = tempfile::TempDir::new().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        let ledger_path = ledger::ledger_path().expect("ledger path");
        for n in 0..4 {
            ledger::append_entry(
                &ledger_path,
                &ledger::LedgerEntry {
                    date: "2026-10-05".to_string(),
                    time: "10:00".to_string(),
                    method: vault::schema::Method::Http,
                    filename: Some(format!("n{n}.md")),
                    source: format!("https://example.com/{n}"),
                    trace_id: None,
                },
            )
            .expect("append");
        }
        let (port, _) = crate::stub::serve(crate::stub::Behavior::Silent).await;
        let started = std::time::Instant::now();
        let mut starts = 0;
        let report = reingest(
            config_for_daemon(port, DAEMON_TIMEOUT),
            true,
            None,
            None,
            None,
            None,
            false,
            4,
            |e| {
                if matches!(e, ReingestEvent::ItemStart { .. }) {
                    starts += 1;
                }
            },
        )
        .await
        .expect("per-item errors are reported, not fatal");
        assert_eq!(report.processed.len(), 4);
        assert_eq!(starts, 4);
        assert!(started.elapsed() < DAEMON_TIMEOUT * 2, "took {:?}", started.elapsed());
    })
    .await;
}
