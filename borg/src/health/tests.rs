use super::*;

#[tokio::test]
async fn test_health_handler() {
    let Json(resp) = health_handler("test-service", "0.1.0").await;
    assert_eq!(resp.status, "ok");
    assert_eq!(resp.service, "test-service");
    assert_eq!(resp.version, "0.1.0");
}
