use super::*;
use crate::FakeFabric;

fn make_distiller(fake: FakeFabric) -> ArticleDistiller<std::sync::Arc<FakeFabric>> {
    ArticleDistiller::new(std::sync::Arc::new(fake), ArticleConfig::default())
}

#[tokio::test]
async fn happy_path_parses_yaml_into_distilled() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        r#"
summary: "An article on distributed consensus. It argues that Raft is easier to teach than Paxos."
claims:
  - text: "Raft uses a leader-based approach that simplifies replication."
    anchor: null
  - text: "Paxos's vocabulary obscures its operational semantics."
    anchor: null
tags: []
links:
  - url: "https://raft.github.io"
    label: "Raft homepage"
"#,
    );
    let distiller = ArticleDistiller::new(std::sync::Arc::new(fake), ArticleConfig::default());
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Article body about Raft and Paxos.",
            source_url: Some("https://example.com/raft-vs-paxos"),
            title_hint: Some("Raft vs Paxos"),
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.meta.extractor, "distill-article-v1");
    assert!(distilled.summary.starts_with("An article on distributed consensus"));
    assert_eq!(distilled.claims.len(), 2);
    assert_eq!(
        distilled.claims[0].text,
        "Raft uses a leader-based approach that simplifies replication."
    );
    assert!(distilled.tags.is_empty());
    assert_eq!(distilled.links.len(), 1);
    assert_eq!(distilled.links[0].url, "https://raft.github.io");
    assert!(distilled.meta.validation.fallback_reason.is_none());
}

#[tokio::test]
async fn fabric_timeout_falls_back() {
    let fake = FakeFabric::new();
    fake.set_error(PATTERN, "fabric -p distill-article timed out after 60s");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Article body text.",
            source_url: None,
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
    assert_eq!(distilled.meta.model, "fabric-timeout");
    assert!(distilled.summary.starts_with("[fabric-timeout]"));
    assert!(distilled.claims.is_empty());
}

#[tokio::test]
async fn fabric_error_falls_back_with_error_reason() {
    let fake = FakeFabric::new();
    fake.set_error(PATTERN, "fabric -p distill-article failed: 1");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Article body text.",
            source_url: None,
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
            transcript: "Article body text.",
            source_url: None,
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
    assert!(
        distilled.meta.validation.raw_output.is_some(),
        "raw_output must be preserved for forensics"
    );
}

#[tokio::test]
async fn empty_summary_falls_back() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, "summary: \"\"\nclaims: []\ntags: []\nlinks: []\n");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Article body text.",
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
async fn strips_yaml_code_fence_if_present() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "```yaml\nsummary: \"Fenced response\"\nclaims: []\ntags: []\nlinks: []\n```\n",
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
    assert_eq!(distilled.summary, "Fenced response");
}

#[tokio::test]
async fn strips_bare_code_fence_if_present() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "```\nsummary: \"Bare fenced\"\nclaims: []\ntags: []\nlinks: []\n```",
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
    assert_eq!(distilled.summary, "Bare fenced");
}

#[tokio::test]
async fn truncates_excess_claims_via_enforce_bounds() {
    let fake = FakeFabric::new();
    let mut yaml = String::from("summary: \"S\"\nclaims:\n");
    for i in 0..15 {
        yaml.push_str(&format!("  - text: \"Claim {i}\"\n    anchor: null\n"));
    }
    yaml.push_str("tags: []\nlinks: []\n");
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

    assert_eq!(distilled.claims.len(), crate::validate::MAX_CLAIMS);
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
async fn drops_empty_claim_texts_and_anchors() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S\"\nclaims:\n  - text: \"   \"\n    anchor: \"\"\n  - text: \"Real claim.\"\n    anchor: \"\"\ntags: []\nlinks: []\n",
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

    assert_eq!(distilled.claims.len(), 1);
    assert_eq!(distilled.claims[0].text, "Real claim.");
    assert!(distilled.claims[0].anchor.is_none());
}

#[tokio::test]
async fn lowercases_tag_strings() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S\"\nclaims: []\ntags: [\"Rust\", \"DistributedSystems\"]\nlinks: []\n",
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
    assert_eq!(distilled.tags, vec!["rust", "distributedsystems"]);
}

#[tokio::test]
async fn records_request_pattern_in_fake_history() {
    let fake = FakeFabric::new();
    let fake = std::sync::Arc::new(fake);
    fake.set_response(PATTERN, "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n");
    let distiller = ArticleDistiller::new(fake.clone(), ArticleConfig::default());
    distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
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
