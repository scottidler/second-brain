use super::*;

#[test]
fn test_transcription_request_roundtrip() {
    let req = TranscriptionRequest {
        audio_bytes: vec![1, 2, 3],
        language: Some("en".to_string()),
        format: AudioFormat::Mp3,
    };
    let json = serde_yaml::to_string(&req).expect("serialize");
    let deserialized: TranscriptionRequest = serde_yaml::from_str(&json).expect("deserialize");
    assert_eq!(deserialized.audio_bytes, vec![1, 2, 3]);
    assert_eq!(deserialized.language, Some("en".to_string()));
}

#[test]
fn test_ingest_request_roundtrip() {
    let req = IngestRequest {
        url: "https://youtube.com/watch?v=abc".to_string(),
        tags: Some(vec!["ai".to_string(), "rust".to_string()]),
        priority: Some(Priority::High),
        force: false,
        method: Some(IngestMethod::Clipboard),
    };
    let json = serde_yaml::to_string(&req).expect("serialize");
    let deserialized: IngestRequest = serde_yaml::from_str(&json).expect("deserialize");
    assert_eq!(deserialized.url, "https://youtube.com/watch?v=abc");
    assert_eq!(deserialized.tags, Some(vec!["ai".to_string(), "rust".to_string()]));
}

#[test]
fn test_content_kind_url() {
    let kind = ContentKind::Url("https://example.com".to_string());
    assert!(matches!(kind, ContentKind::Url(ref u) if u == "https://example.com"));
}

#[test]
fn test_content_kind_image() {
    let kind = ContentKind::Image {
        data: vec![1, 2, 3],
        filename: "test.png".to_string(),
    };
    assert!(matches!(kind, ContentKind::Image { ref filename, .. } if filename == "test.png"));
}

#[test]
fn test_content_kind_text() {
    let kind = ContentKind::Text("hello world".to_string());
    assert!(matches!(kind, ContentKind::Text(ref t) if t == "hello world"));
}

#[test]
fn test_ingest_result_with_failed_status() {
    let result = IngestResult {
        status: IngestStatus::Failed {
            reason: "network error".to_string(),
        },
        note_path: None,
        title: None,
        tags: vec![],
        ..Default::default()
    };
    let json = serde_yaml::to_string(&result).expect("serialize");
    let deserialized: IngestResult = serde_yaml::from_str(&json).expect("deserialize");
    match deserialized.status {
        IngestStatus::Failed { reason } => assert_eq!(reason, "network error"),
        _ => panic!("expected Failed status"),
    }
}
