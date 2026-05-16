use super::*;
use crate::{FakeFabric, RepoMetadata};
use vault::distilled::KindPayload;

fn sample_metadata() -> RepoMetadata {
    RepoMetadata {
        owner: "scottidler".to_string(),
        repo: "second-brain".to_string(),
        stars: Some(42),
        primary_language: Some("Rust".to_string()),
        last_commit: Some("2026-05-16T10:00:00Z".to_string()),
        topics: vec!["obsidian".to_string(), "knowledge".to_string()],
    }
}

fn make_distiller(fake: FakeFabric) -> RepoDistiller<std::sync::Arc<FakeFabric>> {
    RepoDistiller::new(std::sync::Arc::new(fake), RepoConfig::default())
}

#[tokio::test]
async fn happy_path_attaches_metadata_payload() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        r#"
summary: "A workspace consolidating borg, cortex, and oracle around a shared vault crate."
claims:
  - text: "Uses SQLite FTS5 for full-text search."
    anchor: null
  - text: "Borg writes notes; cortex governs them; oracle answers questions."
    anchor: null
tags: []
links:
  - url: "https://github.com/danielmiessler/fabric"
    label: "Fabric"
install: "cargo install --path borg"
"#,
    );
    let metadata = sample_metadata();
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Repository README content.",
            source_url: Some("https://github.com/scottidler/second-brain"),
            title_hint: None,
            repo_metadata: Some(&metadata),
        })
        .await
        .expect("distill");

    assert_eq!(distilled.meta.extractor, "distill-repo-v1");
    assert!(distilled.summary.starts_with("A workspace"));
    assert_eq!(distilled.claims.len(), 2);
    let Some(KindPayload::Repo(payload)) = distilled.kind_specific else {
        panic!("expected Repo payload, got {:?}", distilled.kind_specific);
    };
    assert_eq!(payload.stars, Some(42));
    assert_eq!(payload.primary_language.as_deref(), Some("Rust"));
    assert_eq!(payload.last_commit.as_deref(), Some("2026-05-16T10:00:00Z"));
    assert_eq!(payload.topics, vec!["obsidian", "knowledge"]);
    assert_eq!(payload.install.as_deref(), Some("cargo install --path borg"));
}

#[tokio::test]
async fn fabric_timeout_still_attaches_metadata() {
    let fake = FakeFabric::new();
    fake.set_error(PATTERN, "fabric -p distill-repo timed out after 60s");
    let metadata = sample_metadata();
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Repository README content.",
            source_url: None,
            title_hint: None,
            repo_metadata: Some(&metadata),
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-timeout")
    );
    let Some(KindPayload::Repo(payload)) = distilled.kind_specific else {
        panic!("expected Repo payload even on fallback");
    };
    assert_eq!(payload.stars, Some(42));
    assert!(payload.install.is_none());
}

#[tokio::test]
async fn fabric_error_falls_back_without_metadata() {
    let fake = FakeFabric::new();
    fake.set_error(PATTERN, "fabric -p distill-repo failed: 1");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Repository README content.",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-error")
    );
    assert!(distilled.kind_specific.is_none());
}

#[tokio::test]
async fn malformed_yaml_falls_back_with_raw_output() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, "this is not yaml: [unclosed");
    let metadata = sample_metadata();
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Repository README content.",
            source_url: None,
            title_hint: None,
            repo_metadata: Some(&metadata),
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("yaml-parse-error")
    );
    assert!(distilled.meta.validation.raw_output.is_some());
    // Metadata still attached on fallback so the published note isn't
    // missing the API-derived fields.
    let Some(KindPayload::Repo(payload)) = distilled.kind_specific else {
        panic!("expected Repo payload on yaml fallback");
    };
    assert_eq!(payload.stars, Some(42));
}

#[tokio::test]
async fn empty_summary_falls_back() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, "summary: \"\"\nclaims: []\ntags: []\nlinks: []\n");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("missing-summary")
    );
}

#[tokio::test]
async fn install_over_500_chars_is_dropped() {
    let fake = FakeFabric::new();
    let huge_install = "x".repeat(MAX_INSTALL_CHARS + 1);
    let yaml = format!("summary: \"S\"\nclaims: []\ntags: []\nlinks: []\ninstall: \"{huge_install}\"\n",);
    fake.set_response(PATTERN, yaml);
    let metadata = sample_metadata();
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: Some(&metadata),
        })
        .await
        .expect("distill");

    let Some(KindPayload::Repo(payload)) = distilled.kind_specific else {
        panic!("expected Repo payload");
    };
    assert!(payload.install.is_none(), "install over cap should be dropped");
}

#[tokio::test]
async fn no_metadata_no_install_leaves_kind_specific_unset() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
        })
        .await
        .expect("distill");

    assert!(distilled.kind_specific.is_none());
}

#[tokio::test]
async fn no_metadata_with_install_attaches_install_only_payload() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\ninstall: \"brew install foo\"\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
        })
        .await
        .expect("distill");

    let Some(KindPayload::Repo(payload)) = distilled.kind_specific else {
        panic!("expected install-only Repo payload");
    };
    assert_eq!(payload.install.as_deref(), Some("brew install foo"));
    assert!(payload.stars.is_none());
    assert!(payload.topics.is_empty());
}

#[tokio::test]
async fn strips_yaml_code_fence_if_present() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "```yaml\nsummary: \"Fenced repo summary\"\nclaims: []\ntags: []\nlinks: []\n```\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
        })
        .await
        .expect("distill");
    assert_eq!(distilled.summary, "Fenced repo summary");
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
        })
        .await
        .expect("distill");
    assert_eq!(distilled.tags, vec!["rust", "distributedsystems"]);
}
