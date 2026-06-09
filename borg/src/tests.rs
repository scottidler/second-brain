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
