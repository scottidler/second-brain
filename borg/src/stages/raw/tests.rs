#![allow(clippy::unwrap_used)]

use super::*;
use crate::stages::artifact::MemArtifactStore;
use crate::types::{ContentKind, IngestKind, IngestMethod};
use std::collections::HashMap;

#[test]
fn classify_idea_prefix() {
    let kind = classify(&ContentKind::Text("idea: build a thing".to_string()));
    assert_eq!(kind, IngestKind::Idea);
}

#[test]
fn classify_vocab_en_prefix() {
    let kind = classify(&ContentKind::Text("vocab:en perro".to_string()));
    assert_eq!(kind, IngestKind::VocabularyEn);
}

#[test]
fn classify_vocab_es_prefix() {
    let kind = classify(&ContentKind::Text("vocab:es mañana".to_string()));
    assert_eq!(kind, IngestKind::VocabularyEs);
}

#[test]
fn classify_github_url() {
    let kind = classify(&ContentKind::Url("https://github.com/rust-lang/rust".to_string()));
    assert_eq!(kind, IngestKind::GitHubUrl);
}

#[test]
fn classify_youtube_url() {
    let kind = classify(&ContentKind::Url("https://www.youtube.com/watch?v=abc".to_string()));
    assert_eq!(kind, IngestKind::YoutubeUrl);
    let kind = classify(&ContentKind::Url("https://youtu.be/abc".to_string()));
    assert_eq!(kind, IngestKind::YoutubeUrl);
}

#[test]
fn classify_thread_url() {
    let kind = classify(&ContentKind::Url("https://x.com/user/status/1".to_string()));
    assert_eq!(kind, IngestKind::ThreadUrl);
    let kind = classify(&ContentKind::Url(
        "https://www.reddit.com/r/rust/comments/xyz".to_string(),
    ));
    assert_eq!(kind, IngestKind::ThreadUrl);
    let kind = classify(&ContentKind::Url(
        "https://news.ycombinator.com/item?id=123".to_string(),
    ));
    assert_eq!(kind, IngestKind::ThreadUrl);
}

#[test]
fn classify_article_url_as_default() {
    let kind = classify(&ContentKind::Url("https://example.com/blog".to_string()));
    assert_eq!(kind, IngestKind::ArticleUrl);
}

#[test]
fn classify_image_and_audio() {
    let kind = classify(&ContentKind::Image {
        data: vec![1, 2, 3],
        filename: "a.jpg".to_string(),
    });
    assert_eq!(kind, IngestKind::Image);
    let kind = classify(&ContentKind::Audio {
        data: vec![1, 2, 3],
        filename: "a.ogg".to_string(),
    });
    assert_eq!(kind, IngestKind::VoiceNote);
}

#[test]
fn classify_text_with_embedded_url() {
    let body = "Interesting: https://example.com/article thoughts later.";
    let kind = classify(&ContentKind::Text(body.to_string()));
    assert_eq!(kind, IngestKind::ArticleUrl);
}

#[test]
fn extract_first_url_strips_trailing_punctuation() {
    let body = "Check https://example.com/foo.";
    let url = extract_first_url(body).unwrap();
    assert_eq!(url, "https://example.com/foo");
}

#[test]
fn write_capture_for_text_note() {
    let store = MemArtifactStore::new();
    let env = write_capture(
        &store,
        "tg-text",
        &ContentKind::Text("idea: hello world".to_string()),
        IngestMethod::Telegram,
        None,
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(env.kind, IngestKind::Idea);
    let body = store.read_body("tg-text").unwrap();
    assert_eq!(body, b"idea: hello world");
}

#[test]
fn write_capture_for_image() {
    let store = MemArtifactStore::new();
    let _ = write_capture(
        &store,
        "tg-img",
        &ContentKind::Image {
            data: vec![0xff, 0xd8],
            filename: "photo.jpg".to_string(),
        },
        IngestMethod::Telegram,
        None,
        HashMap::new(),
    )
    .unwrap();
    let raw = store.read_raw("tg-img").unwrap();
    assert_eq!(raw.envelope.kind, IngestKind::Image);
    assert_eq!(raw.attachments.get("photo.jpg").unwrap(), &[0xff, 0xd8]);
}

#[test]
fn write_capture_for_url_stores_url_as_body() {
    let store = MemArtifactStore::new();
    let url = "https://example.com/blog/post";
    let env = write_capture(
        &store,
        "tg-url",
        &ContentKind::Url(url.to_string()),
        IngestMethod::Http,
        None,
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(env.kind, IngestKind::ArticleUrl);
    let body = store.read_body("tg-url").unwrap();
    assert_eq!(body, url.as_bytes());
}
