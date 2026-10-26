use super::*;

#[tokio::test]
async fn test_health_handler() {
    let Json(resp) = health_handler("test-service", "0.1.0").await;
    assert_eq!(resp.status, "ok");
    assert_eq!(resp.service, "test-service");
    assert_eq!(resp.version, "0.1.0");
}

#[tokio::test]
async fn health_advertises_replay_of_and_an_older_body_supports_nothing() {
    let Json(resp) = health_handler("test-service", "0.1.0").await;
    assert!(resp.supports(FEATURE_REPLAY_OF));
    let older: HealthResponse =
        serde_json::from_value(serde_json::json!({"status": "ok", "service": "obsidian-borg", "version": "v0.17.7"}))
            .expect("an older daemon's body still parses");
    assert!(!older.supports(FEATURE_REPLAY_OF));
}

#[tokio::test]
async fn health_advertises_distill_and_an_older_body_does_not() {
    let Json(resp) = health_handler("test-service", "0.1.0").await;
    assert!(resp.supports(FEATURE_DISTILL));
    let older: HealthResponse = serde_json::from_value(serde_json::json!({
        "status": "ok", "service": "obsidian-borg", "version": "v0.19.1", "features": ["replay-of"]
    }))
    .expect("a pre-distill daemon's body parses");
    assert!(!older.supports(FEATURE_DISTILL));
}
