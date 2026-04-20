#![allow(clippy::unwrap_used)]

use super::*;
use crate::types::{FetchMeta, GateId, IngestKind, IngestMethod, RejectionRecord, StageKind, TraceMeta};
use std::collections::HashMap;
use tempfile::TempDir;

fn make_envelope() -> Envelope {
    Envelope {
        trace: "tg-abcdef".to_string(),
        kind: IngestKind::ArticleUrl,
        method: IngestMethod::Telegram,
        received_at: "2026-04-19T14:03:22Z".to_string(),
        origin_message_id: Some("123456".to_string()),
        extra: HashMap::new(),
    }
}

fn make_fetch_meta(source: &str) -> FetchMeta {
    FetchMeta {
        source: source.to_string(),
        extractor: "jina".to_string(),
        status: 200,
        content_type: Some("text/html".to_string()),
        bytes: 12345,
        sha256: "abc123".to_string(),
        fallbacks_attempted: vec![],
    }
}

fn make_rejection(trace_id: &str, gate: GateId) -> RejectionRecord {
    RejectionRecord {
        trace: trace_id.to_string(),
        stage: StageKind::Transcript,
        gate,
        reason: "anonymous access to domain blocked".to_string(),
        rejected_at: "2026-04-19T14:03:24Z".to_string(),
        raw_artifact: Some(format!("{trace_id}/fetched.html")),
        source: Some("https://xda-developers.com/".to_string()),
        domain: Some("xda-developers.com".to_string()),
        blocklist_updated: true,
        retriable_after: Some("2026-04-20T00:00:00Z".to_string()),
    }
}

#[test]
fn mem_envelope_roundtrip() {
    let store = MemArtifactStore::new();
    let env = make_envelope();
    store.write_envelope(&env.trace, &env).unwrap();
    let read = store.read_envelope(&env.trace).unwrap();
    assert_eq!(read.trace, env.trace);
    assert_eq!(read.kind, IngestKind::ArticleUrl);
    assert_eq!(read.method, IngestMethod::Telegram);
}

#[test]
fn mem_body_and_attachments_roundtrip() {
    let store = MemArtifactStore::new();
    let env = make_envelope();
    store.write_envelope(&env.trace, &env).unwrap();
    store.write_body(&env.trace, b"hello world").unwrap();
    store.write_attachment(&env.trace, "photo.jpg", &[0xff, 0xd8]).unwrap();
    let body = store.read_body(&env.trace).unwrap();
    assert_eq!(body, b"hello world");
    let raw = store.read_raw(&env.trace).unwrap();
    assert_eq!(raw.attachments.len(), 1);
    assert_eq!(raw.attachments.get("photo.jpg").unwrap(), &[0xff, 0xd8]);
}

#[test]
fn mem_fetched_roundtrip() {
    let store = MemArtifactStore::new();
    let env = make_envelope();
    store.write_envelope(&env.trace, &env).unwrap();
    let meta = make_fetch_meta("https://example.com/");
    store.write_fetched(&env.trace, b"<html/>", &meta).unwrap();
    let (bytes, read_meta) = store.read_fetched(&env.trace).unwrap().unwrap();
    assert_eq!(bytes, b"<html/>");
    assert_eq!(read_meta.source, "https://example.com/");
}

#[test]
fn mem_transcript_and_summary_roundtrip() {
    let store = MemArtifactStore::new();
    let env = make_envelope();
    store.write_envelope(&env.trace, &env).unwrap();
    let meta = TraceMeta {
        extractor: "markitdown".to_string(),
        ..TraceMeta::default()
    };
    store.write_transcript(&env.trace, "body", &meta).unwrap();
    let (text, read_meta) = store.read_transcript(&env.trace).unwrap();
    assert_eq!(text, "body");
    assert_eq!(read_meta.extractor, "markitdown");

    let summary_meta = TraceMeta {
        pattern: Some("summarize".to_string()),
        model: Some("claude".to_string()),
        ..TraceMeta::default()
    };
    store.write_summary(&env.trace, "summary body", &summary_meta).unwrap();
    let (text, read_meta) = store.read_summary(&env.trace).unwrap();
    assert_eq!(text, "summary body");
    assert_eq!(read_meta.pattern.as_deref(), Some("summarize"));
}

#[test]
fn mem_rejection_roundtrip_and_filter() {
    let store = MemArtifactStore::new();
    let env = make_envelope();
    store.write_envelope(&env.trace, &env).unwrap();
    let rec = make_rejection(&env.trace, GateId::BlockPage);
    store.write_rejection(&env.trace, &rec).unwrap();
    let read = store.read_rejection(&env.trace).unwrap().unwrap();
    assert_eq!(read.gate, GateId::BlockPage);
    assert!(read.blocklist_updated);

    let filter = TraceFilter {
        rejected_only: true,
        ..TraceFilter::default()
    };
    let traces = store.list_traces(&filter).unwrap();
    assert_eq!(traces, vec![env.trace.clone()]);
}

#[test]
fn mem_list_traces_filters_by_kind_and_method() {
    let store = MemArtifactStore::new();
    let article = Envelope {
        trace: "tg-art".to_string(),
        ..make_envelope()
    };
    let voice = Envelope {
        trace: "ds-voice".to_string(),
        kind: IngestKind::VoiceNote,
        method: IngestMethod::Discord,
        ..make_envelope()
    };
    store.write_envelope(&article.trace, &article).unwrap();
    store.write_envelope(&voice.trace, &voice).unwrap();

    let by_kind = store
        .list_traces(&TraceFilter {
            kind: Some(IngestKind::VoiceNote),
            ..TraceFilter::default()
        })
        .unwrap();
    assert_eq!(by_kind, vec!["ds-voice".to_string()]);

    let by_method = store
        .list_traces(&TraceFilter {
            method: Some(IngestMethod::Telegram),
            ..TraceFilter::default()
        })
        .unwrap();
    assert_eq!(by_method, vec!["tg-art".to_string()]);
}

#[test]
fn mem_has_and_delete_trace() {
    let store = MemArtifactStore::new();
    let env = make_envelope();
    assert!(!store.has_trace(&env.trace).unwrap());
    store.write_envelope(&env.trace, &env).unwrap();
    assert!(store.has_trace(&env.trace).unwrap());
    store.delete_trace(&env.trace).unwrap();
    assert!(!store.has_trace(&env.trace).unwrap());
}

#[test]
fn ensure_trace_dir_available_fails_on_collision() {
    let store = MemArtifactStore::new();
    let env = make_envelope();
    store.write_envelope(&env.trace, &env).unwrap();
    let result = ensure_trace_dir_available(&store, &env.trace);
    assert!(result.is_err());
}

#[test]
fn fs_artifact_store_writes_per_trace_layout() {
    let tmp = TempDir::new().unwrap();
    let store = FsArtifactStore::new(tmp.path(), StagingLayout::PerTrace);
    let env = make_envelope();
    store.write_envelope(&env.trace, &env).unwrap();
    store.write_body(&env.trace, b"payload").unwrap();
    store.write_attachment(&env.trace, "a.jpg", &[1, 2, 3]).unwrap();
    let meta = make_fetch_meta("https://example.com/");
    store.write_fetched(&env.trace, b"<html/>", &meta).unwrap();
    let tmeta = TraceMeta {
        extractor: "markitdown".to_string(),
        ..TraceMeta::default()
    };
    store.write_transcript(&env.trace, "t", &tmeta).unwrap();
    store.write_summary(&env.trace, "s", &tmeta).unwrap();

    let trace_dir = tmp.path().join(&env.trace);
    assert!(trace_dir.join("envelope.yml").exists());
    assert!(trace_dir.join("body.txt").exists());
    assert!(trace_dir.join("attachments").join("a.jpg").exists());
    assert!(trace_dir.join("fetched.html").exists());
    assert!(trace_dir.join("fetched.yml").exists());
    assert!(trace_dir.join("transcript.md").exists());
    assert!(trace_dir.join("transcript.yml").exists());
    assert!(trace_dir.join("summary.md").exists());
    assert!(trace_dir.join("summary.yml").exists());

    let raw = store.read_raw(&env.trace).unwrap();
    assert_eq!(raw.body, b"payload");
    assert_eq!(raw.attachments.get("a.jpg").unwrap(), &[1u8, 2, 3]);
    assert!(raw.fetched.is_some());
}

#[test]
fn fs_artifact_store_atomic_write_leaves_no_tmp_on_success() {
    let tmp = TempDir::new().unwrap();
    let store = FsArtifactStore::new(tmp.path(), StagingLayout::PerTrace);
    let env = make_envelope();
    store.write_envelope(&env.trace, &env).unwrap();
    let trace_dir = tmp.path().join(&env.trace);
    let entries: Vec<_> = std::fs::read_dir(&trace_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(entries.contains(&"envelope.yml".to_string()));
    assert!(!entries.iter().any(|n| n.ends_with(".tmp")));
}

#[test]
fn fs_list_traces_finds_existing_dirs() {
    let tmp = TempDir::new().unwrap();
    let store = FsArtifactStore::new(tmp.path(), StagingLayout::PerTrace);
    let env_a = make_envelope();
    let env_b = Envelope {
        trace: "tg-other".to_string(),
        kind: IngestKind::Idea,
        ..make_envelope()
    };
    store.write_envelope(&env_a.trace, &env_a).unwrap();
    store.write_envelope(&env_b.trace, &env_b).unwrap();
    let mut traces = store.list_traces(&TraceFilter::default()).unwrap();
    traces.sort();
    assert_eq!(traces, vec![env_a.trace.clone(), env_b.trace.clone()]);
}

#[test]
fn fs_delete_trace_removes_directory() {
    let tmp = TempDir::new().unwrap();
    let store = FsArtifactStore::new(tmp.path(), StagingLayout::PerTrace);
    let env = make_envelope();
    store.write_envelope(&env.trace, &env).unwrap();
    assert!(tmp.path().join(&env.trace).exists());
    store.delete_trace(&env.trace).unwrap();
    assert!(!tmp.path().join(&env.trace).exists());
}

#[test]
fn fs_filter_by_domain_matches_on_fetched_source() {
    let tmp = TempDir::new().unwrap();
    let store = FsArtifactStore::new(tmp.path(), StagingLayout::PerTrace);
    let env = make_envelope();
    store.write_envelope(&env.trace, &env).unwrap();
    let meta = make_fetch_meta("https://www.xda-developers.com/foo");
    store.write_fetched(&env.trace, b"<html/>", &meta).unwrap();
    let traces = store
        .list_traces(&TraceFilter {
            domain: Some("xda-developers.com".to_string()),
            ..TraceFilter::default()
        })
        .unwrap();
    assert_eq!(traces, vec![env.trace.clone()]);
    let miss = store
        .list_traces(&TraceFilter {
            domain: Some("other-site.com".to_string()),
            ..TraceFilter::default()
        })
        .unwrap();
    assert!(miss.is_empty());
}

#[test]
fn fs_list_traces_empty_when_root_missing() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("does-not-exist");
    let store = FsArtifactStore::new(&missing, StagingLayout::PerTrace);
    let traces = store.list_traces(&TraceFilter::default()).unwrap();
    assert!(traces.is_empty());
}

#[test]
fn sha256_hex_known_value() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn new_envelope_populates_basics() {
    let env = new_envelope("tg-12345", IngestKind::Idea, IngestMethod::Telegram);
    assert_eq!(env.trace, "tg-12345");
    assert_eq!(env.kind, IngestKind::Idea);
    assert!(env.received_at.contains('T'));
}

#[test]
fn retention_window_clamps() {
    assert_eq!(retention_window(0), chrono::Duration::days(0));
    assert_eq!(retention_window(60), chrono::Duration::days(60));
    assert_eq!(retention_window(99999), chrono::Duration::days(3650));
}
