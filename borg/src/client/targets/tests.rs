use std::cell::Cell;

use super::*;
use crate::config::HotkeyConfig;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn a_file() -> (tempfile::TempDir, String) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("a.pdf");
    std::fs::write(&path, b"x").expect("write");
    let path = path.display().to_string();
    (dir, path)
}

fn config_for(port: u16) -> Config {
    Config {
        hotkey: HotkeyConfig {
            host: "127.0.0.1".to_string(),
            port,
            request_timeout: std::time::Duration::from_millis(500),
            ..HotkeyConfig::default()
        },
        ..Config::default()
    }
}

// ---- classification -----------------------------------------------------------

#[test]
fn classifies_urls_and_existing_files() {
    let (_dir, file) = a_file();
    let got = classify_targets(&strings(&["https://a.example", &file, "http://b.example"]), None, None).unwrap();
    assert_eq!(
        got,
        vec![
            Target::Url("https://a.example".to_string()),
            Target::File(PathBuf::from(&file)),
            Target::Url("http://b.example".to_string()),
        ]
    );
}

#[test]
fn a_nonexistent_path_is_a_usage_error_naming_it() {
    let err = classify_targets(&strings(&["https://a.example", "/no/such/file.pdf"]), None, None).unwrap_err();
    assert!(err.0.contains("/no/such/file.pdf"), "{err}");
}

#[test]
fn a_directory_is_a_usage_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let err = classify_targets(&strings(&[&dir.path().display().to_string()]), None, None).unwrap_err();
    assert!(err.0.contains("not a file"), "{err}");
}

#[test]
fn no_targets_is_a_usage_error() {
    assert!(classify_targets(&[], None, None).is_err());
}

#[test]
fn a_tag_that_is_a_url_or_existing_path_is_a_swallowed_target() {
    let (_dir, file) = a_file();
    let url_tag = classify_targets(
        &strings(&["https://a.example"]),
        Some(&strings(&["ai", "https://x.example"])),
        None,
    )
    .unwrap_err();
    assert!(url_tag.0.contains("https://x.example"), "{url_tag}");
    let path_tag = classify_targets(&strings(&["https://a.example"]), Some(&strings(&[&file])), None).unwrap_err();
    assert!(path_tag.0.contains(&file), "{path_tag}");
    assert!(classify_targets(&strings(&["https://a.example"]), Some(&strings(&["ai", "rust"])), None).is_ok());
}

#[test]
fn a_note_with_a_file_target_is_a_usage_error_but_not_with_urls_only() {
    let (_dir, file) = a_file();
    let err = classify_targets(&strings(&["https://a.example", &file]), None, Some("why")).unwrap_err();
    assert!(err.0.contains("--note"), "{err}");
    assert!(classify_targets(&strings(&["https://a.example"]), None, Some("why")).is_ok());
    assert!(classify_targets(&strings(&["https://a.example", &file]), None, None).is_ok());
}

// ---- run against a stub daemon --------------------------------------------------

async fn no_file(_: PathBuf) -> Result<IngestOutcome> {
    unreachable!("no file target in this run")
}

#[tokio::test]
async fn every_url_is_posted_before_the_first_file_starts() {
    let (port, seen) = crate::stub::serve(crate::stub::Behavior::IngestDropping {
        drop_marker: "never-matches".to_string(),
    })
    .await;
    let posts_when_file_started = Cell::new(None);
    let targets = vec![
        Target::File(PathBuf::from("a.pdf")),
        Target::Url("https://a.example".to_string()),
        Target::Url("https://b.example".to_string()),
    ];
    let run = ingest_targets(
        &config_for(port),
        targets,
        None,
        false,
        None,
        |_, _| {},
        |_path| {
            posts_when_file_started.set(Some(seen.lock().unwrap().len()));
            async {
                Ok(IngestOutcome::Captured {
                    title: "t".into(),
                    path: "p".into(),
                })
            }
        },
        |_| {},
    )
    .await;
    assert_eq!(posts_when_file_started.get(), Some(2), "both URLs POSTed first");
    assert_eq!(run.reports.len(), 3);
    assert!(run.reports[2].target == "a.pdf" && !run.reports[2].is_url);
    assert!(!run.any_failed());
    match &run.reports[0].result {
        Ok(IngestOutcome::Queued { trace_id }) => assert!(trace_id.starts_with("stub-trace-")),
        other => panic!("expected Queued with a trace id, got {other:?}"),
    }
}

#[tokio::test]
async fn one_failed_post_of_three_toasts_exactly_once_and_reports_failure() {
    let (port, seen) = crate::stub::serve(crate::stub::Behavior::IngestDropping {
        drop_marker: "b.example".to_string(),
    })
    .await;
    let toasts = std::cell::RefCell::new(Vec::<String>::new());
    let targets = ["a", "b", "c"]
        .iter()
        .map(|n| Target::Url(format!("https://{n}.example")))
        .collect();
    let run = ingest_targets(
        &config_for(port),
        targets,
        None,
        false,
        None,
        |summary, _| toasts.borrow_mut().push(summary.to_string()),
        no_file,
        |_| {},
    )
    .await;
    assert_eq!(seen.lock().unwrap().len(), 3, "every URL was attempted");
    assert!(run.any_failed());
    assert_eq!((run.submits, run.failed_submits), (3, 1));
    assert_eq!(*toasts.borrow(), vec!["1 of 3 submits failed".to_string()]);
}

#[tokio::test]
async fn a_single_target_keeps_its_per_failure_toast_without_an_aggregate() {
    let (port, _) = crate::stub::serve(crate::stub::Behavior::IngestDropping {
        drop_marker: "a.example".to_string(),
    })
    .await;
    let toasts = std::cell::RefCell::new(Vec::<String>::new());
    let run = ingest_targets(
        &config_for(port),
        vec![Target::Url("https://a.example".to_string())],
        None,
        false,
        None,
        |summary, _| toasts.borrow_mut().push(summary.to_string()),
        no_file,
        |_| {},
    )
    .await;
    assert!(run.any_failed());
    assert_eq!(*toasts.borrow(), vec!["Error".to_string()]);
}

#[tokio::test]
async fn an_all_good_multi_run_toasts_nothing_and_file_errors_fail_the_run() {
    let (port, _) = crate::stub::serve(crate::stub::Behavior::IngestDropping {
        drop_marker: "never-matches".to_string(),
    })
    .await;
    let toasts = Cell::new(0);
    let run = ingest_targets(
        &config_for(port),
        vec![
            Target::Url("https://a.example".to_string()),
            Target::File(PathBuf::from("x.pdf")),
        ],
        None,
        false,
        None,
        |_, _| toasts.set(toasts.get() + 1),
        |_| async { Err(eyre::eyre!("Failed to read file")) },
        |_| {},
    )
    .await;
    assert_eq!(toasts.get(), 0);
    assert!(run.any_failed(), "a file error fails the run");
    assert_eq!(run.failed_submits, 0);
    assert!(matches!(&run.reports[1].result, Err(r) if r.contains("Failed to read file")));
}
