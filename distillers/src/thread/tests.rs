use super::*;
use crate::FakeFabric;
use std::sync::Arc;

fn make_distiller(fake: FakeFabric) -> ThreadDistiller<Arc<FakeFabric>> {
    ThreadDistiller::new(Arc::new(fake), ThreadConfig::default())
}

#[tokio::test]
async fn happy_path_parses_yaml_and_attaches_platform() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        r#"
summary: "Thread arguing for typed cross-stage contracts."
claims:
  - text: "Markdown is for humans; cross-stage contracts should be typed."
    anchor: null
  - text: "Reply pushed back on YAML as a contract format."
    anchor: null
tags: []
links:
  - url: "https://example.com/paper"
    label: null
author: "@simonw"
post-count: 7
"#,
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Original post plus replies.",
            source_url: Some("https://x.com/simonw/status/12345"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.meta.extractor, "distill-thread-v1");
    assert!(distilled.summary.starts_with("Thread arguing"));
    assert_eq!(distilled.claims.len(), 2);
    assert_eq!(distilled.links.len(), 1);
    assert!(distilled.meta.validation.fallback_reason.is_none());

    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.author.as_deref(), Some("@simonw"));
    assert_eq!(payload.post_count, 7);
    assert_eq!(payload.platform, "x");
}

#[tokio::test]
async fn infers_reddit_platform_from_source_url() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: \"u/spez\"\npost-count: 12\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Reddit thread body.",
            source_url: Some("https://www.reddit.com/r/rust/comments/abc/def/"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.platform, "reddit");
    assert_eq!(payload.post_count, 12);
}

#[tokio::test]
async fn infers_hn_platform_from_source_url() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: \"pg\"\npost-count: 30\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "HN thread body.",
            source_url: Some("https://news.ycombinator.com/item?id=12345"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.platform, "hn");
}

#[tokio::test]
async fn infers_twitter_legacy_host() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: null\npost-count: 1\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: Some("https://twitter.com/user/status/1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.platform, "x");
}

#[tokio::test]
async fn unknown_platform_when_source_url_is_none() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: null\npost-count: 0\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.platform, "unknown");
}

#[tokio::test]
async fn fabric_timeout_falls_back_with_platform_attached() {
    let fake = FakeFabric::new();
    fake.set_timeout(PATTERN);
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Thread body.",
            source_url: Some("https://x.com/u/status/1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-timeout")
    );
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("fallback should still attach a Thread payload so cortex sees the platform");
    };
    assert_eq!(payload.platform, "x");
    assert_eq!(payload.post_count, 0);
}

#[tokio::test]
async fn fabric_error_falls_back_with_error_reason() {
    let fake = FakeFabric::new();
    fake.set_error(PATTERN, "fabric -p distill-thread failed: 1");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: Some("https://www.reddit.com/r/x/comments/y/z/"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-error")
    );
}

#[tokio::test]
async fn malformed_yaml_falls_back_with_raw_output() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, "this is not yaml: [unclosed");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: Some("https://news.ycombinator.com/item?id=1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("yaml-parse-error")
    );
    assert!(distilled.meta.validation.raw_output.is_some());
}

#[tokio::test]
async fn empty_summary_falls_back() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"\"\nclaims: []\ntags: []\nlinks: []\nauthor: null\npost-count: 0\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("missing-summary")
    );
}

#[tokio::test]
async fn strips_yaml_code_fence() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "```yaml\nsummary: \"Fenced.\"\nclaims: []\ntags: []\nlinks: []\nauthor: null\npost-count: 0\n```\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");
    assert_eq!(distilled.summary, "Fenced.");
}

#[tokio::test]
async fn truncates_excess_claims_via_enforce_bounds() {
    let fake = FakeFabric::new();
    let mut yaml = String::from("summary: \"S.\"\nclaims:\n");
    for i in 0..15 {
        yaml.push_str(&format!("  - text: \"Claim {i}\"\n    anchor: null\n"));
    }
    yaml.push_str("tags: []\nlinks: []\nauthor: null\npost-count: 0\n");
    fake.set_response(PATTERN, yaml);

    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.claims.len(), crate::validate::max_claims(1));
    assert!(
        distilled
            .meta
            .validation
            .bounds_truncations
            .iter()
            .any(|t| t.starts_with("claims:"))
    );
}

#[tokio::test]
async fn drops_empty_author_and_zero_post_count() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: \"   \"\npost-count: 0\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: Some("https://x.com/u/status/1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert!(payload.author.is_none());
    assert_eq!(payload.post_count, 0);
}

#[tokio::test]
async fn records_request_pattern_in_fake_history() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: null\npost-count: 1\n",
    );
    let distiller = ThreadDistiller::new(fake.clone(), ThreadConfig::default());
    distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: Some("https://x.com/u/status/1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].pattern, PATTERN);
}

#[test]
fn infer_platform_handles_subdomain_hosts() {
    assert_eq!(infer_platform(Some("https://mobile.twitter.com/u/status/1")), "x");
    assert_eq!(
        infer_platform(Some("https://old.reddit.com/r/x/comments/y/z/")),
        "reddit"
    );
    assert_eq!(infer_platform(Some("https://www.reddit.com/r/x/")), "reddit");
    assert_eq!(infer_platform(Some("https://news.ycombinator.com/item?id=1")), "hn");
    assert_eq!(infer_platform(Some("https://example.com/anything")), "unknown");
    assert_eq!(infer_platform(None), "unknown");
}
