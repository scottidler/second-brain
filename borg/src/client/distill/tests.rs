use super::*;
use crate::stub::{Behavior, DistillStub, serve};
use std::sync::{Arc, Mutex};

const TRACE: &str = "cl-distill-stub";
const FAST: Duration = Duration::from_millis(5);
const CEILING: Duration = Duration::from_secs(10);

fn config_for(port: u16) -> Config {
    Config {
        hotkey: crate::config::HotkeyConfig {
            host: "127.0.0.1".to_string(),
            port,
            ..crate::config::HotkeyConfig::default()
        },
        ..Config::default()
    }
}

/// The scratch tree the stub serves: a note, nested files and binary bytes.
fn scratch_tree() -> tempfile::TempDir {
    let tree = tempfile::tempdir().expect("tree");
    for (path, bytes) in [
        ("billboard-top-40.md", b"# note\n".as_slice()),
        ("frames/runs.yml", b"- start: 0\n  ocr: hi\n"),
        ("frames/frames/frame_0001.jpg", &[0xff, 0xd8, 0x00, 0x10, 0xff]),
        ("stages/cl-distill-stub/distilled.yml", b"summary: s\n"),
        ("system/attachments/2026-10/a b#c.jpg", &[1, 2, 3]),
    ] {
        let full = tree.path().join(path);
        std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        std::fs::write(full, bytes).expect("write");
    }
    tree
}

fn stub_for(tree: &Path, state: &'static str, features: &[&str], extra: &[&str]) -> Behavior {
    Behavior::Distill(DistillStub {
        features: features.iter().map(|f| f.to_string()).collect(),
        trace_id: TRACE.to_string(),
        state,
        tree: tree.to_path_buf(),
        extra_manifest_paths: extra.iter().map(|p| p.to_string()).collect(),
    })
}

fn count(seen: &Arc<Mutex<Vec<String>>>, prefix: &str) -> usize {
    seen.lock()
        .expect("log")
        .iter()
        .filter(|r| r.starts_with(prefix))
        .count()
}

/// Every file under `root` as `(relative path, bytes)`, sorted.
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    crate::scratch::regular_files(root)
        .expect("walk")
        .into_iter()
        .map(|(rel, file)| (file.path, std::fs::read(root.join(rel)).expect("read")))
        .collect()
}

#[tokio::test]
async fn dir_matches_the_scratch_tree_file_for_file() {
    let tree = scratch_tree();
    let (port, seen) = serve(stub_for(tree.path(), "succeeded", &["distill"], &[])).await;
    let out = tempfile::tempdir().expect("out");
    let dir = out.path().join("run");

    let run = distill_within(
        &config_for(port),
        "https://example.com/a",
        Some(dir.clone()),
        CEILING,
        FAST,
    )
    .await
    .expect("distill");

    assert_eq!(
        snapshot(&dir),
        snapshot(tree.path()),
        "DIR must mirror the scratch tree"
    );
    assert_eq!(run.dir, dir);
    assert_eq!(run.trace_id, TRACE);
    assert_eq!(run.note, Some(dir.join("billboard-top-40.md")));
    assert!(run.failure.is_none());
    assert_eq!(count(&seen, "POST /distill"), 1);
}

#[tokio::test]
async fn a_daemon_without_the_distill_feature_is_refused_before_any_post() {
    let tree = scratch_tree();
    let (port, seen) = serve(stub_for(tree.path(), "succeeded", &["replay-of"], &[])).await;
    let out = tempfile::tempdir().expect("out");

    let err = distill_within(
        &config_for(port),
        "https://example.com/a",
        Some(out.path().join("run")),
        CEILING,
        FAST,
    )
    .await
    .expect_err("must refuse");

    assert!(format!("{err:#}").contains("no `distill` feature"), "{err:#}");
    assert_eq!(count(&seen, "POST /distill"), 0);
    assert!(!out.path().join("run").exists());
}

#[tokio::test]
async fn a_non_empty_explicit_dir_is_refused_with_zero_requests() {
    let tree = scratch_tree();
    let (port, seen) = serve(stub_for(tree.path(), "succeeded", &["distill"], &[])).await;
    let out = tempfile::tempdir().expect("out");
    std::fs::write(out.path().join("keep.txt"), b"mine").expect("seed");

    let err = distill_within(
        &config_for(port),
        "https://example.com/a",
        Some(out.path().to_path_buf()),
        CEILING,
        FAST,
    )
    .await
    .expect_err("must refuse");

    assert!(format!("{err:#}").contains("is not empty"), "{err:#}");
    assert_eq!(seen.lock().expect("log").len(), 0, "no request of any kind");
    assert_eq!(std::fs::read(out.path().join("keep.txt")).expect("kept"), b"mine");
}

#[tokio::test]
async fn an_existing_empty_explicit_dir_is_accepted() {
    let tree = scratch_tree();
    let (port, _) = serve(stub_for(tree.path(), "succeeded", &["distill"], &[])).await;
    let out = tempfile::tempdir().expect("out");

    distill_within(
        &config_for(port),
        "https://example.com/a",
        Some(out.path().to_path_buf()),
        CEILING,
        FAST,
    )
    .await
    .expect("an empty dir is fine");
    assert_eq!(snapshot(out.path()), snapshot(tree.path()));
}

#[tokio::test]
async fn a_non_empty_default_dir_is_refused_after_the_post_and_before_any_download() {
    let tree = scratch_tree();
    let (port, seen) = serve(stub_for(tree.path(), "succeeded", &["distill"], &[])).await;
    // The default DIR is `./<trace>`; the test cwd is the crate dir.
    let default_dir = PathBuf::from(".").join(TRACE);
    std::fs::create_dir_all(&default_dir).expect("seed dir");
    std::fs::write(default_dir.join("keep.txt"), b"mine").expect("seed");

    let result = distill_within(&config_for(port), "https://example.com/a", None, CEILING, FAST).await;
    let posts = count(&seen, "POST /distill");
    let files = count(&seen, "GET /distill/cl-distill-stub/file/");
    let kept = std::fs::read(default_dir.join("keep.txt"));
    std::fs::remove_dir_all(&default_dir).expect("cleanup");

    let err = result.expect_err("must refuse");
    let msg = format!("{err:#}");
    assert!(msg.contains("is not empty") && msg.contains(TRACE), "{msg}");
    assert_eq!(posts, 1, "the check comes after the POST");
    assert_eq!(files, 0, "and before any download");
    assert_eq!(kept.expect("kept"), b"mine");
}

#[tokio::test]
async fn a_trace_that_never_terminates_exits_on_the_ceiling_naming_the_trace() {
    let tree = scratch_tree();
    let (port, _) = serve(stub_for(tree.path(), "received", &["distill"], &[])).await;
    let out = tempfile::tempdir().expect("out");
    let dir = out.path().join("run");

    let err = distill_within(
        &config_for(port),
        "https://example.com/a",
        Some(dir.clone()),
        Duration::from_millis(80),
        Duration::from_millis(10),
    )
    .await
    .expect_err("ceiling");

    let msg = format!("{err:#}");
    assert!(msg.contains(TRACE) && msg.contains("not terminal"), "{msg}");
    assert!(!dir.exists(), "nothing downloaded");
}

#[tokio::test]
async fn a_failed_trace_still_writes_its_manifest_files_and_reports_the_failure() {
    let tree = scratch_tree();
    let (port, _) = serve(stub_for(tree.path(), "failed", &["distill"], &[])).await;
    let out = tempfile::tempdir().expect("out");
    let dir = out.path().join("run");

    let run = distill_within(
        &config_for(port),
        "https://example.com/a",
        Some(dir.clone()),
        CEILING,
        FAST,
    )
    .await
    .expect("a failed trace is Ok-with-failure, not an error");

    let reason = run.failure.expect("failure carried");
    assert!(
        reason.contains("publish-failed") && reason.contains("stub failure"),
        "{reason}"
    );
    assert_eq!(snapshot(&dir), snapshot(tree.path()));
}

#[tokio::test]
async fn a_manifest_path_that_climbs_out_is_refused_and_nothing_is_written() {
    let tree = scratch_tree();
    let (port, seen) = serve(stub_for(tree.path(), "succeeded", &["distill"], &["../x"])).await;
    let out = tempfile::tempdir().expect("out");
    let dir = out.path().join("run");

    let err = distill_within(
        &config_for(port),
        "https://example.com/a",
        Some(dir.clone()),
        CEILING,
        FAST,
    )
    .await
    .expect_err("must refuse");

    let msg = format!("{err:#}");
    assert!(msg.contains("../x") && msg.contains(TRACE), "{msg}");
    assert!(!out.path().join("x").exists(), "nothing outside DIR");
    assert!(!dir.exists(), "nothing inside DIR either: the listing is checked first");
    assert_eq!(count(&seen, "GET /distill/cl-distill-stub/file/"), 0);
}

#[tokio::test]
async fn an_absolute_manifest_path_is_refused() {
    let tree = scratch_tree();
    let (port, _) = serve(stub_for(tree.path(), "succeeded", &["distill"], &["/etc/x"])).await;
    let out = tempfile::tempdir().expect("out");

    let err = distill_within(
        &config_for(port),
        "https://example.com/a",
        Some(out.path().join("run")),
        CEILING,
        FAST,
    )
    .await
    .expect_err("must refuse");
    assert!(format!("{err:#}").contains("absolute"), "{err:#}");
}

#[test]
fn checked_destination_accepts_nested_paths_and_refuses_the_rest() {
    let dir = Path::new("/d");
    assert_eq!(
        checked_destination(dir, "a/b.md").expect("ok"),
        PathBuf::from("/d/a/b.md")
    );
    for bad in ["../x", "a/../../x", "/abs", "", "."] {
        assert!(checked_destination(dir, bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn encode_path_keeps_separators_and_escapes_each_segment() {
    assert_eq!(encode_path("a b/c#d?.jpg"), "a%20b/c%23d%3F.jpg");
}

#[tokio::test]
async fn an_unreachable_daemon_is_an_error_naming_the_daemon() {
    let closed = vault::testnet::closed_port();
    let out = tempfile::tempdir().expect("out");
    let err = distill_within(
        &config_for(closed.port()),
        "https://example.com/a",
        Some(out.path().join("r")),
        CEILING,
        FAST,
    )
    .await
    .expect_err("unreachable");
    assert!(format!("{err:#}").contains("127.0.0.1"), "{err:#}");
}
