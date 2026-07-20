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
    let kind = classify(&ContentKind::Url {
        url: "https://github.com/rust-lang/rust".to_string(),
        note: None,
    });
    assert_eq!(kind, IngestKind::GitHubUrl);
}

#[test]
fn classify_youtube_url() {
    let kind = classify(&ContentKind::Url {
        url: "https://www.youtube.com/watch?v=abc".to_string(),
        note: None,
    });
    assert_eq!(kind, IngestKind::YoutubeUrl);
    let kind = classify(&ContentKind::Url {
        url: "https://youtu.be/abc".to_string(),
        note: None,
    });
    assert_eq!(kind, IngestKind::YoutubeUrl);
}

#[test]
fn classify_thread_url() {
    let kind = classify(&ContentKind::Url {
        url: "https://x.com/user/status/1".to_string(),
        note: None,
    });
    assert_eq!(kind, IngestKind::ThreadUrl);
    let kind = classify(&ContentKind::Url {
        url: "https://www.reddit.com/r/rust/comments/xyz".to_string(),
        note: None,
    });
    assert_eq!(kind, IngestKind::ThreadUrl);
    let kind = classify(&ContentKind::Url {
        url: "https://news.ycombinator.com/item?id=123".to_string(),
        note: None,
    });
    assert_eq!(kind, IngestKind::ThreadUrl);
}

#[test]
fn classify_article_url_as_default() {
    let kind = classify(&ContentKind::Url {
        url: "https://example.com/blog".to_string(),
        note: None,
    });
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
fn classify_session() {
    let kind = classify(&ContentKind::Session {
        body: "human: hi\nassistant: hello".to_string(),
    });
    assert_eq!(kind, IngestKind::Session);
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
        &ContentKind::Url {
            url: url.to_string(),
            note: None,
        },
        IngestMethod::Http,
        None,
        HashMap::new(),
    )
    .unwrap();
    assert_eq!(env.kind, IngestKind::ArticleUrl);
    let body = store.read_body("tg-url").unwrap();
    assert_eq!(body, url.as_bytes());
}

#[test]
fn run_gate_1_clean_body_returns_ok() {
    // Staging disabled → no-op Ok.
    let config = crate::config::Config::default();
    let result = run_gate_1(
        &config,
        "tg-test",
        "https://example.com",
        b"<html><h1>Real article</h1></html>",
        200,
    );
    assert!(result.is_ok());
}

#[test]
fn run_gate_1_block_body_when_enabled_persists_blocklist_and_rejection() {
    // Point staging.root at a tempdir so the rejection/blocklist artifacts
    // land somewhere we can inspect.
    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = crate::config::Config::default();
    config.staging.enabled = true;
    config.staging.root = tmp.path().join("stages");

    // Pre-seed a trace directory so run_gate_1 can write rejection.yml next to it.
    let store = FsArtifactStore::from_config(&config.staging);
    let env = crate::stages::artifact::new_envelope("tg-gate1", IngestKind::ArticleUrl, IngestMethod::Telegram);
    store.write_envelope(&env.trace, &env).unwrap();

    // Redirect the blocklist default path into the tempdir so persistence
    // doesn't leak into the user's real dotfiles.
    //
    // run_gate_1 uses blocklist::default_path() which resolves via
    // dirs::data_local_dir(). We set XDG_DATA_HOME for this test.
    // SAFETY: single-threaded test.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", tmp.path());
    }

    // Block until far in the future so the test is stable across actual wall-clock.
    let err = run_gate_1(
        &config,
        &env.trace,
        "https://www.xda-developers.com/7-docker-containers/",
        b"anonymous access to domain blocked until 2099-01-01T00:00:00Z",
        200,
    )
    .expect_err("expected gate-1 to reject");
    assert!(format!("{err:#}").contains("gate-1"));

    // Rejection record written.
    let rec = store.read_rejection(&env.trace).unwrap().expect("rejection missing");
    assert_eq!(rec.gate, crate::types::GateId::BlockPage);
    assert_eq!(rec.domain.as_deref(), Some("xda-developers.com"));
    assert!(rec.blocklist_updated);

    // Blocklist persisted and contains the domain.
    let bl_path = crate::blocklist::default_path();
    let bl = crate::blocklist::Blocklist::from_file(&bl_path).unwrap();
    assert!(bl.is_blocked("xda-developers.com", chrono::Utc::now()));

    // Clean up env var for other tests.
    unsafe {
        std::env::remove_var("XDG_DATA_HOME");
    }
}
