use super::*;
use crate::types::{IngestResult, IngestStatus};

#[test]
fn test_format_discord_reply_with_obsidian_url() {
    let result = IngestResult {
        status: IngestStatus::Completed,
        title: Some("Test Article".to_string()),
        tags: vec!["ai".to_string()],
        elapsed_secs: Some(3.5),
        obsidian_url: Some("obsidian://open?vault=obsidian&file=test-article".to_string()),
        ..Default::default()
    };
    let reply = format_discord_reply(&result, "https://example.com");
    assert!(reply.contains("Saved: Test Article"));
    assert!(reply.contains("obsidian://open?vault=obsidian&file=test-article"));
}

#[test]
fn test_format_discord_reply_without_obsidian_url() {
    let result = IngestResult {
        status: IngestStatus::Failed {
            reason: "network error".to_string(),
        },
        ..Default::default()
    };
    let reply = format_discord_reply(&result, "https://example.com");
    assert!(reply.contains("Failed"));
    assert!(!reply.contains("obsidian://"));
}
