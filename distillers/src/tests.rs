use super::*;

#[tokio::test]
async fn fake_fabric_records_calls_and_returns_canned_response() {
    let fake = FakeFabric::new();
    fake.set_response(
        "distill-article-v1",
        "summary: hello\nmeta:\n  extractor: distill-article-v1\n  model: fake\n  produced-at: 2026-05-16T00:00:00Z\n",
    );
    let out = fake
        .call(FabricRequest {
            pattern: "distill-article-v1".to_string(),
            input: "in".to_string(),
            model: "fake".to_string(),
            max_chars: 0,
            timeout_secs: 60,
        })
        .await
        .expect("call");
    assert!(out.contains("summary: hello"));
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].pattern, "distill-article-v1");
}

#[tokio::test]
async fn fake_fabric_returns_canned_error() {
    let fake = FakeFabric::new();
    fake.set_error("distill-article-v1", "fabric-timeout");
    let err = fake
        .call(FabricRequest {
            pattern: "distill-article-v1".to_string(),
            input: "in".to_string(),
            model: "fake".to_string(),
            max_chars: 0,
            timeout_secs: 60,
        })
        .await
        .expect_err("expected error");
    assert!(format!("{err}").contains("fabric-timeout"));
}
