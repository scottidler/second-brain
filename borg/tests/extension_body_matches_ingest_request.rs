use borg::types::IngestRequest;

#[test]
fn extension_post_body_deserializes_into_ingest_request() {
    let body = serde_json::json!({ "url": "https://example.com/" });
    let _req: IngestRequest = serde_json::from_value(body).expect(
        "extension POST body must remain compatible with IngestRequest; \
         if you added a required field to IngestRequest, update background.js \
         (and the canonical body in this test) at the same commit",
    );
}
