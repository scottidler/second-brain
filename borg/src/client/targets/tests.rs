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

/// The flag, not its content, conflicts with a file target: a blank or
/// whitespace `--note` is the same usage error.
#[test]
fn a_blank_note_with_a_file_target_is_still_a_usage_error() {
    let (_dir, file) = a_file();
    for note in ["", "   "] {
        let err = classify_targets(&strings(&["https://a.example", &file]), None, Some(note)).unwrap_err();
        assert!(err.0.contains("--note"), "note={note:?}: {err}");
    }
    assert!(classify_targets(&strings(&["https://a.example"]), None, Some("")).is_ok());
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
    .await
    .unwrap();
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
    .await
    .unwrap();
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
    .await
    .unwrap();
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
    .await
    .unwrap();
    assert_eq!(toasts.get(), 0);
    assert!(run.any_failed(), "a file error fails the run");
    assert_eq!(run.failed_submits, 0);
    assert!(matches!(&run.reports[1].result, Err(r) if r.contains("Failed to read file")));
}

// ---- bulk rule ----------------------------------------------------------------

#[test]
fn absent_ingest_block_defaults_bulk_threshold_to_five() {
    let config: Config = serde_yaml::from_str("log-level: info\n").expect("parse");
    assert_eq!(config.ingest.bulk_threshold, 5);
    let config: Config = serde_yaml::from_str("ingest:\n  bulk-threshold: 0\n").expect("parse");
    assert_eq!(config.ingest.bulk_threshold, 0);
}

#[test]
fn bulk_needs_more_than_threshold_urls() {
    assert!(!is_bulk(5, 5, 5, false));
    assert!(is_bulk(6, 6, 5, false));
}

#[test]
fn threshold_zero_makes_one_url_a_bulk_but_not_clipboard() {
    assert!(is_bulk(1, 1, 0, false));
    assert!(!is_bulk(1, 1, 0, true));
}

#[test]
fn file_only_runs_never_open_a_batch() {
    assert!(!is_bulk(0, 6, 5, false));
    assert!(!is_bulk(0, 1, 0, false));
}

#[test]
fn mixed_run_counts_files_toward_the_threshold() {
    assert!(is_bulk(1, 6, 5, false));
}

// ---- bulk batch -----------------------------------------------------------------

fn bulk_daemon(features: &[&str], seal_status: &'static str) -> crate::stub::Behavior {
    crate::stub::Behavior::Bulk {
        features: strings(features),
        seal_status,
    }
}

fn urls(n: usize) -> Vec<Target> {
    (1..=n).map(|i| Target::Url(format!("https://u{i}.example"))).collect()
}

fn posts_to<'a>(seen: &'a [String], prefix: &str) -> Vec<&'a String> {
    seen.iter().filter(|r| r.starts_with(prefix)).collect()
}

fn batch_field(request: &str) -> Option<String> {
    let body = request.split("\r\n\r\n").nth(1)?;
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    json.get("batch")?.as_str().map(str::to_string)
}

#[tokio::test]
async fn a_bulk_sends_one_batch_id_on_every_url_and_seals_the_queued_ids() {
    let (port, seen) = crate::stub::serve(bulk_daemon(&["bulk-batch"], "200 OK")).await;
    let run = ingest_targets(
        &config_for(port),
        urls(6),
        None,
        false,
        None,
        |_, _| {},
        no_file,
        |_| {},
    )
    .await
    .unwrap();
    let seen = seen.lock().unwrap().clone();
    let ingests = posts_to(&seen, "POST /ingest");
    assert_eq!(ingests.len(), 6);
    let batch = run.batch.clone().expect("a bulk has a batch");
    for request in &ingests {
        assert_eq!(batch_field(request).as_deref(), Some(batch.id.as_str()), "{request}");
    }
    let seals = posts_to(&seen, &format!("POST /batch/{}/seal", batch.id));
    assert_eq!(seals.len(), 1, "{seen:?}");
    let body: serde_json::Value = serde_json::from_str(seals[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["trace_ids"].as_array().unwrap().len(), 6);
    assert_eq!(batch.seal, SealStatus::Sealed { items: 6 });
    assert!(!run.any_failed());
}

#[tokio::test]
async fn a_bulk_refuses_a_daemon_without_bulk_batch_before_any_submit() {
    let (port, seen) = crate::stub::serve(bulk_daemon(&["replay-of"], "200 OK")).await;
    let err = ingest_targets(
        &config_for(port),
        urls(6),
        None,
        false,
        None,
        |_, _| {},
        no_file,
        |_| {},
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("bulk-batch"), "{err:#}");
    assert!(posts_to(&seen.lock().unwrap(), "POST").is_empty(), "nothing submitted");
}

#[tokio::test]
async fn a_failed_seal_fails_the_run_and_file_targets_still_run() {
    let (port, _) = crate::stub::serve(bulk_daemon(&["bulk-batch"], "409 Conflict")).await;
    let files_run = Cell::new(0);
    let mut targets = urls(5);
    targets.push(Target::File(PathBuf::from("a.pdf")));
    let run = ingest_targets(
        &config_for(port),
        targets,
        None,
        false,
        None,
        |_, _| {},
        |_| {
            files_run.set(files_run.get() + 1);
            async {
                Ok(IngestOutcome::Captured {
                    title: "t".into(),
                    path: "p".into(),
                })
            }
        },
        |_| {},
    )
    .await
    .unwrap();
    assert!(matches!(run.batch.as_ref().unwrap().seal, SealStatus::Failed(ref r) if r.contains("409")));
    assert!(run.any_failed(), "a failed seal is exit 1");
    assert_eq!(files_run.get(), 1);
}

#[tokio::test]
async fn a_run_at_the_threshold_is_not_a_bulk_and_never_asks_health() {
    let (port, seen) = crate::stub::serve(bulk_daemon(&[], "200 OK")).await;
    let run = ingest_targets(
        &config_for(port),
        urls(5),
        None,
        false,
        None,
        |_, _| {},
        no_file,
        |_| {},
    )
    .await
    .unwrap();
    assert!(run.batch.is_none());
    let seen = seen.lock().unwrap().clone();
    assert!(posts_to(&seen, "GET /health").is_empty());
    assert!(seen.iter().all(|r| batch_field(r).is_none()));
}

// ---- --from list files --------------------------------------------------------

#[test]
fn parse_target_list_skips_blank_and_comment_lines() {
    let text = "https://a.example\n\n  # a comment\n  https://b.example  \n./c.pdf\n";
    assert_eq!(
        parse_target_list(text),
        strings(&["https://a.example", "https://b.example", "./c.pdf"])
    );
}

#[test]
fn read_target_lists_concatenates_files_in_order() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let one = dir.path().join("one.txt");
    let two = dir.path().join("two.txt");
    std::fs::write(&one, "https://a.example\nhttps://b.example\n").expect("write");
    std::fs::write(&two, "# header\nhttps://c.example\n").expect("write");
    assert_eq!(
        read_target_lists(&[one, two]).expect("lists read"),
        strings(&["https://a.example", "https://b.example", "https://c.example"])
    );
}

#[test]
fn read_target_lists_rejects_a_missing_file() {
    let err = read_target_lists(&[PathBuf::from("/no/such/list.txt")]).expect_err("missing list");
    assert!(err.0.contains("--from /no/such/list.txt"), "{err}");
}

#[test]
fn read_target_lists_rejects_an_empty_list() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let empty = dir.path().join("empty.txt");
    std::fs::write(&empty, "\n# nothing here\n").expect("write");
    let err = read_target_lists(&[empty]).expect_err("empty list");
    assert!(err.0.contains("no targets in the list"), "{err}");
}

#[test]
fn read_target_lists_with_no_sources_is_empty() {
    assert!(read_target_lists(&[]).expect("no sources").is_empty());
}
