//! Route tests for `POST /distill`, `GET /distill/{trace}` and
//! `GET /distill/{trace}/file/{*path}`, through the real router.

use super::*;
use crate::harvest::with_xdg_data_home;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::path::PathBuf;
use tower::ServiceExt;
use vault::receipts::{FailureStage, ReceiptKind};
use vault::schema::Method;

fn router(auth_token: Option<&str>) -> Router {
    build_router(AppState {
        config: Arc::new(Config::default()),
        telegram: None,
        desktop: None,
        version: "0.0.0-test".to_string(),
        auth_token: auth_token.map(str::to_string),
        batch_summaries: tokio::sync::mpsc::unbounded_channel().0,
    })
}

fn get(uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("request")
}

fn post_distill(url: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/distill")
        .header("content-type", "application/json");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder
        .body(Body::from(serde_json::json!({ "url": url }).to_string()))
        .expect("request")
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body")
        .to_vec()
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    serde_json::from_slice(&body_bytes(resp).await).expect("json body")
}

/// `XDG_DATA_HOME/sb/borg/distill/<trace>`, the scratch root the routes read.
fn run_dir(trace: &str) -> PathBuf {
    vault::paths::borg_distill_dir().join(trace)
}

fn receipt_count() -> i64 {
    let conn = crate::receipts::open_default().expect("open receipts");
    conn.query_row("SELECT COUNT(*) FROM receipts", [], |r| r.get(0))
        .expect("count")
}

/// Seed a receipts row (`scratch` as given) and, when `terminal`, close it
/// `succeeded`; write `note.md` and a staged `distilled.yml` under its root.
fn seed_run(trace: &str, scratch: bool, terminal: bool) -> PathBuf {
    let conn = crate::receipts::open_default().expect("open receipts");
    if scratch {
        crate::receipts::record_received_scratch(&conn, trace, Method::Cli, ReceiptKind::Url, "https://example.com/a")
            .expect("record scratch");
    } else {
        crate::receipts::record_received(&conn, trace, Method::Cli, ReceiptKind::Url, "https://example.com/a")
            .expect("record");
    }
    if terminal {
        crate::receipts::mark_succeeded(&conn, trace, "note.md", false).expect("succeed");
    }
    let root = run_dir(trace);
    std::fs::create_dir_all(root.join("stages").join(trace)).expect("mkdir");
    std::fs::write(root.join("note.md"), b"# the note\n").expect("note");
    std::fs::write(root.join("stages").join(trace).join("distilled.yml"), b"summary: s\n").expect("distilled");
    root
}

#[tokio::test]
async fn distill_routes_without_a_valid_token_are_401_and_write_no_receipt() {
    let data_home = tempfile::tempdir().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        for bearer in [None, Some("wrong")] {
            let resp = router(Some("secret"))
                .oneshot(post_distill("https://example.com/a", bearer))
                .await
                .expect("response");
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "POST bearer={bearer:?}");
            for uri in ["/distill/cl-x", "/distill/cl-x/file/note.md"] {
                let resp = router(Some("secret"))
                    .oneshot(get(uri, bearer))
                    .await
                    .expect("response");
                assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{uri} bearer={bearer:?}");
            }
        }
        assert_eq!(receipt_count(), 0, "a refused request writes no receipts row");
        assert!(
            !vault::paths::borg_distill_dir().exists(),
            "a refused request writes no scratch root"
        );
    })
    .await;
}

#[tokio::test]
async fn post_distill_records_a_scratch_row_and_runs_under_the_scratch_policy() {
    let data_home = tempfile::tempdir().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        // An unreadable blocklist (a directory at its path) makes Stage 0
        // reject right after it stages the envelope: the run ends without a
        // network fetch, and the envelope shows which staging root it used.
        crate::pipeline::permits::GENERAL_PERMITS.init(4);
        let blocklist = crate::blocklist::default_path().expect("blocklist path");
        std::fs::create_dir_all(&blocklist).expect("blocklist dir");
        let config = Config::default();
        let shared_staging = config.staging.root.clone();

        let resp = router(Some("secret"))
            .oneshot(post_distill("https://example.com/distill-me", Some("secret")))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], serde_json::json!("Queued"), "{body}");
        let trace = body["trace_id"].as_str().expect("trace id").to_string();

        let conn = crate::receipts::open_default().expect("open receipts");
        let row = crate::receipts::get(&conn, &trace).expect("get").expect("row");
        assert!(row.scratch, "the INSERT sets scratch = 1");
        assert_eq!(row.method, "cli");
        let root = run_dir(&trace);
        assert_eq!(
            std::fs::read(vault::intake::raw_input_path(&root, &trace)).expect("sidecar"),
            b"https://example.com/distill-me"
        );

        // Let the detached pipeline reach its terminal write.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let terminal = loop {
            let row = crate::receipts::get(&conn, &trace).expect("get").expect("row");
            if row.status != "received" || std::time::Instant::now() > deadline {
                break row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert_eq!(terminal.status, "failed", "{terminal:?}");
        assert_eq!(terminal.failure_stage.as_deref(), Some("intake-rejected"));
        assert!(
            root.join("stages").join(&trace).join("envelope.yml").is_file(),
            "Scratch stages under the run's own root"
        );
        assert!(
            !shared_staging.join(&trace).exists(),
            "nothing lands in the shared staging root {}",
            shared_staging.display()
        );
    })
    .await;
}

#[tokio::test]
async fn manifest_and_file_routes_serve_a_finished_scratch_run() {
    let data_home = tempfile::tempdir().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        seed_run("cl-done", true, true);
        let resp = router(None)
            .oneshot(get("/distill/cl-done", None))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let manifest: crate::scratch::DistillManifest =
            serde_json::from_slice(&body_bytes(resp).await).expect("manifest");
        assert_eq!(manifest.trace_id, "cl-done");
        assert_eq!(manifest.status, "succeeded");
        let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["note.md", "stages/cl-done/distilled.yml"]);

        let resp = router(None)
            .oneshot(get("/distill/cl-done/file/stages/cl-done/distilled.yml", None))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_bytes(resp).await, b"summary: s\n");

        let missing = router(None)
            .oneshot(get("/distill/cl-done/file/nope.md", None))
            .await
            .expect("response");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    })
    .await;
}

#[tokio::test]
async fn a_failed_scratch_run_still_lists_its_partial_output() {
    let data_home = tempfile::tempdir().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        seed_run("cl-failed", true, false);
        let conn = crate::receipts::open_default().expect("open receipts");
        crate::receipts::mark_failed(&conn, "cl-failed", FailureStage::FetchFailed, "boom").expect("fail");
        let resp = router(None)
            .oneshot(get("/distill/cl-failed", None))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["status"], "failed");
        assert_eq!(json["failure-stage"], "fetch-failed");
        assert_eq!(json["failure-reason"], "boom");
        assert_eq!(json["files"].as_array().expect("files").len(), 2);
    })
    .await;
}

#[tokio::test]
async fn parent_and_absolute_file_paths_are_400() {
    let data_home = tempfile::tempdir().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        let root = seed_run("cl-done", true, true);
        std::fs::write(root.parent().expect("distill dir").join("outside.txt"), b"x").expect("outside");
        for uri in [
            "/distill/cl-done/file/../outside.txt",
            "/distill/cl-done/file/..%2Foutside.txt",
            "/distill/cl-done/file/stages/..%2F..%2Foutside.txt",
            "/distill/cl-done/file//etc/passwd",
            "/distill/cl-done/file/%2Fetc%2Fpasswd",
        ] {
            let resp = router(None).oneshot(get(uri, None)).await.expect("response");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
    })
    .await;
}

#[tokio::test]
async fn a_symlink_in_the_tree_is_never_listed_or_served() {
    let data_home = tempfile::tempdir().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        let root = seed_run("cl-done", true, true);
        let secret = data_home.path().join("secret.txt");
        std::fs::write(&secret, b"secret").expect("secret");
        std::os::unix::fs::symlink(&secret, root.join("leak.txt")).expect("symlink");
        std::os::unix::fs::symlink(data_home.path(), root.join("up")).expect("dir symlink");

        let resp = router(None)
            .oneshot(get("/distill/cl-done", None))
            .await
            .expect("response");
        let json = body_json(resp).await;
        let listed: Vec<&str> = json["files"]
            .as_array()
            .expect("files")
            .iter()
            .map(|f| f["path"].as_str().expect("path"))
            .collect();
        assert!(
            !listed.iter().any(|p| p.contains("leak") || p.starts_with("up/")),
            "{listed:?}"
        );

        for uri in ["/distill/cl-done/file/leak.txt", "/distill/cl-done/file/up/secret.txt"] {
            let resp = router(None).oneshot(get(uri, None)).await.expect("response");
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    })
    .await;
}

#[tokio::test]
async fn unknown_and_non_scratch_traces_are_404() {
    let data_home = tempfile::tempdir().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        // A finished vault ingest with files where a scratch root would be:
        // still never served.
        seed_run("ht-ingest", false, true);
        for trace in ["cl-unknown", "ht-ingest"] {
            for uri in [format!("/distill/{trace}"), format!("/distill/{trace}/file/note.md")] {
                let resp = router(None).oneshot(get(&uri, None)).await.expect("response");
                assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
            }
        }
    })
    .await;
}

#[tokio::test]
async fn a_received_trace_is_409_on_both_routes() {
    let data_home = tempfile::tempdir().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        seed_run("cl-running", true, false);
        for uri in ["/distill/cl-running", "/distill/cl-running/file/note.md"] {
            let resp = router(None).oneshot(get(uri, None)).await.expect("response");
            assert_eq!(resp.status(), StatusCode::CONFLICT, "{uri}");
        }
    })
    .await;
}

#[tokio::test]
async fn an_encoded_dot_dot_trace_is_404() {
    let data_home = tempfile::tempdir().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        for uri in ["/distill/%2E%2E", "/distill/%2E%2E/file/note.md"] {
            let resp = router(None).oneshot(get(uri, None)).await.expect("response");
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    })
    .await;
}
