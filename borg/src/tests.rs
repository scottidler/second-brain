use super::*;
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
    let app = test_router_with_auth(Config::default(), Some("secret".to_string()));
    let resp = app
        .oneshot(post("/ingest", serde_json::json!({"url": "https://x"}), Some("secret")))
        .await
        .expect("response");
    // Passes the gate (handler runs and returns HTTP 200; intake body may
    // be Failed because the default config has no vault root, which is
    // irrelevant here - the point is the request was NOT rejected at 401).
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
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
        err.contains("cannot reach obsidian-borg"),
        "expected connection error message, got: {err}"
    );
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

/// Run `body` with `XDG_DATA_HOME` pointed at `data_home`, restoring it after.
/// Serialized on the shared XDG test lock because the receipts DB path is
/// resolved from the process environment.
async fn with_xdg_data_home<F, Fut>(data_home: &std::path::Path, body: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let _guard = crate::harvest::TEST_XDG_LOCK.lock().await;
    let prior = std::env::var("XDG_DATA_HOME").ok();
    unsafe { std::env::set_var("XDG_DATA_HOME", data_home) };
    body().await;
    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
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

        let resp = test_router().oneshot(get_queue("/queue", None)).await.expect("response");
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
        let resp = test_router().oneshot(get_queue("/queue", None)).await.expect("response");
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
        let resp = test_router().oneshot(get_queue("/queue", None)).await.expect("response");
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(resp).await;
        assert!(json["error"].is_string(), "500 body must carry an error: {json}");
        assert!(json.get("state").is_none(), "a DB error must never look like a snapshot");
    })
    .await;
}
