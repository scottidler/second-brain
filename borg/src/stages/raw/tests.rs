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
        members: Vec::new(),
        primary_id: "sess-1".to_string(),
        body_truncated: false,
        intent: crate::harvest::identity::ResolveIntent::NewNote,
        follows_prior: None,
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
    // xdg_data_dir(); we point XDG_DATA_HOME at the tempdir. Cargo runs test
    // FUNCTIONS concurrently across threads (the old "single-threaded test"
    // comment was wrong), and XDG_DATA_HOME is process-global, so hold the ONE
    // shared xdg lock the harvest publish/skip tests use for the whole window
    // this var is redirected. Without it, this test clobbers XDG_DATA_HOME
    // mid-body of `harvest::publish::tests` and their `receipts::open_default()`
    // reads the wrong DB - the intermittent `receipts row` failure.
    let _xdg_guard = crate::harvest::TEST_XDG_LOCK.blocking_lock();
    let prior_xdg = std::env::var("XDG_DATA_HOME").ok();
    unsafe {
        std::env::set_var("XDG_DATA_HOME", tmp.path());
    }
    let _xdg_sandbox = crate::receipts::sandbox::enter();

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
    let bl_path = crate::blocklist::default_path().unwrap();
    let bl = crate::blocklist::Blocklist::from_file(&bl_path).unwrap();
    assert!(bl.is_blocked("xda-developers.com", chrono::Utc::now()));

    // Restore the prior env (the `_xdg_guard` drops at fn end, releasing the
    // shared lock only AFTER the var is restored, so the next holder starts clean).
    unsafe {
        match prior_xdg {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
}

// ---- blocklist read-modify-write fails closed ----

use vault::capture::{install as install_warn_capture, warns_containing};

const CORRUPT_BLOCKLIST: &[u8] = b"domains: [this is not: a map\n";

/// Run `body` with XDG_DATA_HOME at `data_home` under the shared XDG lock.
fn with_xdg<R>(data_home: &std::path::Path, body: impl FnOnce() -> R) -> R {
    let _guard = crate::harvest::TEST_XDG_LOCK.blocking_lock();
    let prior = std::env::var("XDG_DATA_HOME").ok();
    unsafe { std::env::set_var("XDG_DATA_HOME", data_home) };
    let _xdg_sandbox = crate::receipts::sandbox::enter();
    let out = body();
    unsafe {
        match prior {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
    out
}

fn staging_config(root: &std::path::Path) -> crate::config::Config {
    let mut config = crate::config::Config::default();
    config.staging.enabled = true;
    config.staging.root = root.join("stages");
    config
}

/// Seed a corrupt blocklist at the path `blocklist::default_path()` resolves to
/// under the current XDG_DATA_HOME. Must be called inside `with_xdg`.
fn seed_corrupt_blocklist() -> std::path::PathBuf {
    let path = crate::blocklist::default_path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, CORRUPT_BLOCKLIST).unwrap();
    path
}

#[test]
fn gate_1_with_a_corrupt_blocklist_still_rejects_and_leaves_the_file_alone() {
    install_warn_capture();
    let tmp = tempfile::TempDir::new().unwrap();
    let config = staging_config(tmp.path());
    let store = FsArtifactStore::from_config(&config.staging);
    let trace = "tg-gate1-corrupt-blocklist";
    let env = crate::stages::artifact::new_envelope(trace, IngestKind::ArticleUrl, IngestMethod::Telegram);
    store.write_envelope(&env.trace, &env).unwrap();

    let (err, bl_path) = with_xdg(tmp.path(), || {
        let bl_path = seed_corrupt_blocklist();
        let err = run_gate_1(
            &config,
            trace,
            "https://www.xda-developers.com/7-docker-containers/",
            b"anonymous access to domain blocked until 2099-01-01T00:00:00Z",
            200,
        )
        .expect_err("gate-1 still rejects the capture");
        (err, bl_path)
    });

    assert!(format!("{err:#}").contains("gate-1"));
    assert_eq!(
        std::fs::read(&bl_path).unwrap(),
        CORRUPT_BLOCKLIST,
        "the corrupt blocklist's bytes are unchanged"
    );
    let rec = store.read_rejection(trace).unwrap().expect("rejection record written");
    assert!(!rec.blocklist_updated, "no write-back happened");
    let warns = warns_containing(trace);
    assert!(
        warns
            .iter()
            .any(|w| w.contains(&bl_path.display().to_string()) && w.contains("update skipped")),
        "a WARN names the blocklist path, got {warns:?}"
    );
}

#[test]
fn gate_0_with_a_corrupt_blocklist_rejects_the_url_capture_and_fires_one_alert() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = staging_config(tmp.path());
    let store = FsArtifactStore::from_config(&config.staging);
    let trace = "tg-gate0-corrupt-blocklist";
    let content = ContentKind::Url {
        url: "https://gate0-corrupt.example/post".to_string(),
        note: None,
    };

    let (err, bl_path) = with_xdg(tmp.path(), || {
        let bl_path = seed_corrupt_blocklist();
        let err = stage_0_init(&config, &content, IngestMethod::Telegram, trace)
            .expect_err("an unloadable blocklist fails closed");
        (err, bl_path)
    });

    let msg = format!("{err:#}");
    assert!(
        msg.contains(&bl_path.display().to_string()),
        "the error names the blocklist path: {msg}"
    );
    assert_eq!(std::fs::read(&bl_path).unwrap(), CORRUPT_BLOCKLIST, "bytes unchanged");
    assert_eq!(crate::stages::alert::fired_count(trace), 1, "exactly one gate alert");
    assert!(
        store.read_envelope(trace).is_err(),
        "a rejected capture writes no staged artifacts"
    );
}

#[test]
fn gate_0_with_a_missing_blocklist_lets_the_url_capture_through() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = staging_config(tmp.path());
    let store = FsArtifactStore::from_config(&config.staging);
    let trace = "tg-gate0-missing-blocklist";
    let content = ContentKind::Url {
        url: "https://gate0-missing.example/post".to_string(),
        note: None,
    };

    let bl_path = with_xdg(tmp.path(), || {
        assert!(
            !crate::blocklist::default_path().unwrap().exists(),
            "precondition: no blocklist file"
        );
        stage_0_init(&config, &content, IngestMethod::Telegram, trace).expect("a missing blocklist is empty");
        crate::blocklist::default_path().unwrap()
    });

    assert!(!bl_path.exists(), "a missing blocklist stays missing");
    assert!(store.read_envelope(trace).is_ok(), "the capture was staged");
    assert_eq!(crate::stages::alert::fired_count(trace), 0, "no alert fired");
}
