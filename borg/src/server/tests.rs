use super::*;
use crate::harvest::with_xdg_data_home;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

fn test_router() -> Router {
    build_router(AppState {
        config: Arc::new(Config::default()),
        telegram: None,
        desktop: None,
        version: "0.0.0-test".to_string(),
        auth_token: None,
    })
}

/// Router with a resolved auth token and a caller-supplied config (so the
/// vault root can be redirected to a tempdir for side-effect assertions).
fn test_router_with_auth(config: Config, auth_token: Option<String>) -> Router {
    build_router(AppState {
        config: Arc::new(config),
        telegram: None,
        desktop: None,
        version: "0.0.0-test".to_string(),
        auth_token,
    })
}

fn post(uri: &str, body: serde_json::Value, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder
        .body(Body::from(serde_json::to_string(&body).expect("json")))
        .expect("request")
}

#[tokio::test]
async fn write_routes_reject_missing_token_when_configured() {
    for uri in ["/ingest", "/ingest/file", "/note"] {
        let app = test_router_with_auth(Config::default(), Some("secret".to_string()));
        let resp = app
            .oneshot(post(uri, serde_json::json!({"url": "https://x", "text": "x"}), None))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{uri} must 401 without token");
    }
}

#[tokio::test]
async fn write_route_rejects_wrong_token() {
    let app = test_router_with_auth(Config::default(), Some("secret".to_string()));
    let resp = app
        .oneshot(post("/ingest", serde_json::json!({"url": "https://x"}), Some("wrong")))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn write_route_accepts_correct_token() {
    // The handler records the receipt (`receipts::open_default`) before the
    // vault-root check, so the receipts DB is opened: sandbox it.
    let data_home = tempfile::tempdir().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        let app = test_router_with_auth(Config::default(), Some("secret".to_string()));
        let resp = app
            .oneshot(post("/ingest", serde_json::json!({"url": "https://x"}), Some("secret")))
            .await
            .expect("response");
        // Passes the gate (handler runs and returns HTTP 200; intake body may
        // be Failed because the default config has no vault root, which is
        // irrelevant here - the point is the request was NOT rejected at 401).
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    })
    .await;
}

#[tokio::test]
async fn health_routes_stay_open_with_token_configured() {
    let app = test_router_with_auth(Config::default(), Some("secret".to_string()));
    let req = Request::builder().uri("/health").body(Body::empty()).expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn rejected_request_writes_no_sidecar() {
    // The auth gate runs before any intake write, so a 401 must leave no
    // raw-input sidecar behind. Redirect the vault root at a tempdir and
    // confirm system/intake stays empty after an unauthenticated /note.
    let vault = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(vault.path().join(".obsidian")).expect("marker");
    let mut config = Config::default();
    config.vault.root_path = Some(vault.path().to_string_lossy().to_string());

    let app = test_router_with_auth(config, Some("secret".to_string()));
    let resp = app
        .oneshot(post("/note", serde_json::json!({"text": "hello"}), None))
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let intake_dir = vault.path().join("system").join("intake");
    let count = std::fs::read_dir(&intake_dir).map(|d| d.count()).unwrap_or(0);
    assert_eq!(count, 0, "a rejected request must not write an intake sidecar");
}

#[tokio::test]
async fn test_health_endpoint() {
    let app = test_router();
    let req = Request::builder().uri("/health").body(Body::empty()).expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_ingest_endpoint() {
    // `/ingest` opens the receipts DB before the vault-root check: sandbox it.
    let data_home = tempfile::tempdir().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        let app = test_router();
        let body = serde_json::json!({"url": "https://youtube.com/watch?v=test"});
        let req = Request::builder()
            .method("POST")
            .uri("/ingest")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).expect("json")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    })
    .await;
}

#[tokio::test]
async fn test_cors_preflight() {
    let app = test_router();
    let req = Request::builder()
        .method("OPTIONS")
        .uri("/ingest")
        .header("origin", "https://example.com")
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "content-type")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key("access-control-allow-origin"));
}

#[tokio::test]
async fn test_cors_on_response() {
    let app = test_router();
    let req = Request::builder()
        .uri("/health")
        .header("origin", "https://example.com")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let origin = resp.headers().get("access-control-allow-origin").expect("cors header");
    assert_eq!(origin, "*");
}

#[tokio::test]
async fn server_handle_wait_fails_fast_on_task_error() {
    // Regression: ServerHandle::wait used to log a failed task and keep
    // waiting on the survivors, so a transport/watcher task could die while
    // the process stayed "up". wait() must abort the remaining tasks and
    // propagate the first Err so Restart=always + sb doctor observe it.
    let mut tasks: tokio::task::JoinSet<Result<()>> = tokio::task::JoinSet::new();
    tasks.spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        Ok(())
    });
    tasks.spawn(async { Err(eyre::eyre!("transport died")) });

    let handle = ServerHandle { tasks };
    let result = handle.wait().await;
    assert!(result.is_err(), "wait() must propagate a failed task");
    assert!(format!("{:#}", result.expect_err("err")).contains("transport died"));
}

#[tokio::test]
async fn server_handle_wait_returns_ok_when_all_clean() {
    let mut tasks: tokio::task::JoinSet<Result<()>> = tokio::task::JoinSet::new();
    tasks.spawn(async { Ok(()) });
    let handle = ServerHandle { tasks };
    assert!(handle.wait().await.is_ok());
}

#[test]
fn constant_time_eq_matches_only_identical_bytes() {
    use crate::routes::constant_time_eq;
    assert!(constant_time_eq(b"secret-token", b"secret-token"));
    assert!(!constant_time_eq(b"secret-token", b"secret-toker"));
    assert!(!constant_time_eq(b"short", b"longer-token"));
    assert!(constant_time_eq(b"", b""));
}

fn get_queue(uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("request")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("body");
    serde_json::from_slice(&bytes).expect("json body")
}

#[tokio::test]
async fn queue_route_returns_snapshot() {
    let data_home = tempfile::TempDir::new().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        let conn = crate::receipts::open_default().expect("open receipts");
        crate::receipts::record_received(
            &conn,
            "20261004-215943-aaaa",
            vault::schema::Method::Http,
            vault::receipts::ReceiptKind::Url,
            "https://www.youtube.com/watch?v=abc",
        )
        .expect("record");
        drop(conn);

        let resp = test_router()
            .oneshot(get_queue("/queue", None))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("body");
        let snap: vault::queue::QueueSnapshot = serde_json::from_slice(&bytes).expect("parseable QueueSnapshot");
        assert_eq!(snap.state, vault::queue::QueueState::Draining);
        let batch = snap.batch.expect("batch");
        assert_eq!(batch.id, "20261004-215943-aaaa");
        assert_eq!((batch.total, batch.queued), (1, 1));

        let by_id = test_router()
            .oneshot(get_queue("/queue?batch=20261004-215943-aaaa", None))
            .await
            .expect("response");
        assert_eq!(by_id.status(), StatusCode::OK);

        let unknown = test_router()
            .oneshot(get_queue("/queue?batch=nope", None))
            .await
            .expect("response");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert!(body_json(unknown).await["error"].is_string());
    })
    .await;
}

#[tokio::test]
async fn queue_route_idle_is_exactly_idle() {
    let data_home = tempfile::TempDir::new().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        let resp = test_router()
            .oneshot(get_queue("/queue", None))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await, serde_json::json!({"state": "idle"}));
    })
    .await;
}

#[tokio::test]
async fn queue_route_requires_token_when_configured() {
    let data_home = tempfile::TempDir::new().expect("tempdir");
    with_xdg_data_home(data_home.path(), || async {
        let router = || test_router_with_auth(Config::default(), Some("secret".to_string()));
        let no_token = router().oneshot(get_queue("/queue", None)).await.expect("response");
        assert_eq!(no_token.status(), StatusCode::UNAUTHORIZED);
        let wrong = router()
            .oneshot(get_queue("/queue", Some("wrong")))
            .await
            .expect("response");
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        let ok = router()
            .oneshot(get_queue("/queue", Some("secret")))
            .await
            .expect("response");
        assert_eq!(ok.status(), StatusCode::OK);
    })
    .await;
}

#[tokio::test]
async fn queue_route_db_error_is_500_not_idle() {
    // XDG_DATA_HOME names a regular file, so the receipts directory cannot be
    // created and the DB open fails.
    let scratch = tempfile::TempDir::new().expect("tempdir");
    let blocker = scratch.path().join("not-a-dir");
    std::fs::write(&blocker, "x").expect("write blocker");
    with_xdg_data_home(&blocker, || async {
        let resp = test_router()
            .oneshot(get_queue("/queue", None))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(resp).await;
        assert!(json["error"].is_string(), "500 body must carry an error: {json}");
        assert!(
            json.get("state").is_none(),
            "a DB error must never look like a snapshot"
        );
    })
    .await;
}

#[tokio::test]
async fn trace_route_reports_source_durable_from_the_archive() {
    let data_home = tempfile::TempDir::new().expect("tempdir");
    let archive_root = tempfile::TempDir::new().expect("archive");
    with_xdg_data_home(data_home.path(), || async {
        let conn = crate::receipts::open_default().expect("open receipts");
        for trace in ["tr-archived", "tr-staged-only"] {
            crate::receipts::record_received(
                &conn,
                trace,
                vault::schema::Method::Http,
                vault::receipts::ReceiptKind::Url,
                "https://example.com/a",
            )
            .expect("record");
        }
        drop(conn);
        let distilled = vault::distilled::Distilled::default();
        crate::archive::SourceArchive {
            root: archive_root.path(),
            trace_id: "tr-archived",
            distilled: &distilled,
            source_md: None,
        }
        .write()
        .expect("archive");

        let router = || {
            let mut config = Config::default();
            config.archive.root = archive_root.path().to_path_buf();
            test_router_with_auth(config, None)
        };

        let durable = router()
            .oneshot(get_queue("/trace/tr-archived", None))
            .await
            .expect("response");
        assert_eq!(durable.status(), StatusCode::OK);
        let json = body_json(durable).await;
        assert_eq!(json["found"], serde_json::json!(true));
        assert_eq!(json["source-durable"], serde_json::json!(true), "{json}");

        let staged_only = router()
            .oneshot(get_queue("/trace/tr-staged-only", None))
            .await
            .expect("response");
        assert_eq!(body_json(staged_only).await["source-durable"], serde_json::json!(false));

        let unknown = router()
            .oneshot(get_queue("/trace/tr-unknown", None))
            .await
            .expect("response");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(unknown).await["source-durable"], serde_json::json!(false));
    })
    .await;
}

#[tokio::test]
async fn channel_search_route_requires_token_when_configured() {
    // An invalid handle keeps every authorized call short of yt-dlp.
    let uri = "/channels/search?handle=letsgetrusty&q=tokio";
    let router = || test_router_with_auth(Config::default(), Some("secret".to_string()));
    let no_token = router().oneshot(get_queue(uri, None)).await.expect("response");
    assert_eq!(no_token.status(), StatusCode::UNAUTHORIZED);
    let wrong = router().oneshot(get_queue(uri, Some("wrong"))).await.expect("response");
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    let ok = router()
        .oneshot(get_queue(uri, Some("secret")))
        .await
        .expect("response");
    assert_eq!(ok.status(), StatusCode::BAD_REQUEST, "past the gate, refused as input");
}

#[tokio::test]
async fn channel_search_route_refuses_a_bad_handle_with_400() {
    for uri in [
        "/channels/search?handle=letsgetrusty&q=tokio",
        "/channels/search?handle=%40lets%2Fget&q=tokio",
        "/channels/search?handle=%40letsgetrusty&q=tokio&limit=0",
    ] {
        let resp = test_router().oneshot(get_queue(uri, None)).await.expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        let json = body_json(resp).await;
        assert!(json["error"].is_string(), "{uri}: {json}");
        assert!(json["stderr-tail"].is_null(), "{uri}: nothing ran, no stderr: {json}");
    }
    let missing_query = test_router()
        .oneshot(get_queue("/channels/search?handle=%40letsgetrusty", None))
        .await
        .expect("response");
    assert_eq!(missing_query.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn channel_search_failure_is_502_with_the_stderr_tail() {
    use crate::youtube::ChannelSearchError;
    let failed = ChannelSearchError::Failed {
        status: "exit status: 1".into(),
        stderr_tail: "ERROR: [youtube:tab] @nope: This channel does not exist.".into(),
    };
    let resp = crate::routes::channel_search_failure(&failed);
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let body: vault::channel::ChannelSearchFailure =
        serde_json::from_value(body_json(resp).await).expect("ChannelSearchFailure body");
    assert_eq!(
        body.stderr_tail.as_deref(),
        Some("ERROR: [youtube:tab] @nope: This channel does not exist.")
    );
    assert!(body.error.contains("This channel does not exist."));

    for err in [
        ChannelSearchError::TimedOut {
            after: std::time::Duration::from_secs(8),
        },
        ChannelSearchError::Spawn {
            reason: "No such file or directory".into(),
        },
        ChannelSearchError::Parse {
            reason: "no `entries` array".into(),
        },
    ] {
        let resp = crate::routes::channel_search_failure(&err);
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY, "{err}");
        let json = body_json(resp).await;
        assert_eq!(json["error"], err.to_string());
        assert!(json["stderr-tail"].is_null(), "{err}: {json}");
    }
}

/// The gemini key check moved out of `Config::validate` (run by every `sb
/// borg` subcommand) to daemon start. `serve_init` must still refuse a gemini
/// engine with no fallback whose keys resolve empty, before it binds anything.
#[tokio::test]
async fn serve_init_rejects_gemini_without_fallback_when_keys_empty() {
    let mut config = Config::default();
    config.youtube.engine = crate::config::YoutubeEngine::Gemini;
    config.youtube.fallback = None;
    config.youtube.gemini.api_key = "SB_TEST_SERVE_INIT_GEMINI_KEY_UNSET".to_string();
    config.youtube.data_api.api_key = "SB_TEST_SERVE_INIT_YOUTUBE_KEY_UNSET".to_string();
    config.validate().expect("structurally valid: the CLI path loads it");
    let err = match serve_init(config, "0.0.0-test".to_string()).await {
        Ok(_) => panic!("serve_init must refuse empty gemini keys"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("youtube.gemini.api-key"), "{err}");
    assert!(err.contains("youtube.data-api.api-key"), "{err}");
}
