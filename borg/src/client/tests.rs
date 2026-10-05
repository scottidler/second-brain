use super::*;
use crate::harvest::with_xdg_data_home;

#[tokio::test]
async fn test_ingest_connection_refused() {
    // Use a port that's almost certainly not listening
    let config = Config {
        hotkey: config::HotkeyConfig {
            host: "127.0.0.1".to_string(),
            port: 19999,
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
