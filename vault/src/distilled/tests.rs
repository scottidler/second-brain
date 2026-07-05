use super::*;

fn sample_meta() -> DistilledMeta {
    DistilledMeta {
        extractor: "distill-article-v1".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        input_tokens: 1847,
        output_tokens: 312,
        produced_at: "2026-05-16T14:03:22Z".to_string(),
        validation: ValidationMeta::default(),
    }
}

#[test]
fn roundtrip_article_distilled() {
    let original = Distilled {
        summary: "Two-sentence article summary.".to_string(),
        claims: vec![
            Claim {
                text: "First claim.".to_string(),
                anchor: None,
                ..Default::default()
            },
            Claim {
                text: "Second claim.".to_string(),
                anchor: Some("section-three".to_string()),
                ..Default::default()
            },
        ],
        tags: vec!["rust".to_string(), "cli".to_string()],
        links: vec![Link {
            url: "https://example.com".to_string(),
            label: Some("Reference".to_string()),
        }],
        kind_specific: None,
        meta: sample_meta(),
        transcript: None,
    };

    let yaml = serde_yaml::to_string(&original).expect("serialize");
    let decoded: Distilled = serde_yaml::from_str(&yaml).expect("deserialize");

    assert_eq!(decoded.summary, original.summary);
    assert_eq!(decoded.claims.len(), 2);
    assert_eq!(decoded.claims[0].text, "First claim.");
    assert_eq!(decoded.claims[1].anchor.as_deref(), Some("section-three"));
    assert_eq!(decoded.tags, original.tags);
    assert_eq!(decoded.links.len(), 1);
    assert!(decoded.kind_specific.is_none());
    assert_eq!(decoded.meta.extractor, "distill-article-v1");
}

#[test]
fn roundtrip_repo_payload() {
    let original = Distilled {
        summary: "A Rust CLI for X.".to_string(),
        claims: vec![],
        tags: vec![],
        links: vec![],
        kind_specific: Some(KindPayload::Repo(RepoPayload {
            stars: Some(1432),
            primary_language: Some("Rust".to_string()),
            last_commit: Some("2026-05-10".to_string()),
            topics: vec!["cli".to_string(), "rust".to_string()],
            install: Some("cargo install foo".to_string()),
        })),
        meta: sample_meta(),
        transcript: None,
    };

    let yaml = serde_yaml::to_string(&original).expect("serialize");
    let decoded: Distilled = serde_yaml::from_str(&yaml).expect("deserialize");

    match decoded.kind_specific {
        Some(KindPayload::Repo(p)) => {
            assert_eq!(p.stars, Some(1432));
            assert_eq!(p.primary_language.as_deref(), Some("Rust"));
            assert_eq!(p.last_commit.as_deref(), Some("2026-05-10"));
            assert_eq!(p.topics, vec!["cli".to_string(), "rust".to_string()]);
            assert_eq!(p.install.as_deref(), Some("cargo install foo"));
        }
        other => panic!("expected Repo payload, got {other:?}"),
    }
}

#[test]
fn roundtrip_video_payload() {
    let original = Distilled {
        summary: "A talk on distributed systems.".to_string(),
        claims: vec![Claim {
            text: "Consensus is hard.".to_string(),
            anchor: Some("12:34".to_string()),
            ..Default::default()
        }],
        tags: vec![],
        links: vec![],
        kind_specific: Some(KindPayload::Video(VideoPayload {
            channel: Some("Some Channel".to_string()),
            duration_seconds: Some(3247),
            published_at: Some("2026-04-22".to_string()),
            repos: vec![],
        })),
        meta: sample_meta(),
        transcript: None,
    };

    let yaml = serde_yaml::to_string(&original).expect("serialize");
    let decoded: Distilled = serde_yaml::from_str(&yaml).expect("deserialize");

    match decoded.kind_specific {
        Some(KindPayload::Video(p)) => {
            assert_eq!(p.channel.as_deref(), Some("Some Channel"));
            assert_eq!(p.duration_seconds, Some(3247));
            assert_eq!(p.published_at.as_deref(), Some("2026-04-22"));
        }
        other => panic!("expected Video payload, got {other:?}"),
    }
}

#[test]
fn roundtrip_thread_payload() {
    let original = Distilled {
        summary: "Twitter thread about Rust async.".to_string(),
        claims: vec![],
        tags: vec![],
        links: vec![],
        kind_specific: Some(KindPayload::Thread(ThreadPayload {
            author: Some("@someone".to_string()),
            post_count: 47,
            platform: "x".to_string(),
        })),
        meta: sample_meta(),
        transcript: None,
    };

    let yaml = serde_yaml::to_string(&original).expect("serialize");
    let decoded: Distilled = serde_yaml::from_str(&yaml).expect("deserialize");

    match decoded.kind_specific {
        Some(KindPayload::Thread(p)) => {
            assert_eq!(p.author.as_deref(), Some("@someone"));
            assert_eq!(p.post_count, 47);
            assert_eq!(p.platform, "x");
        }
        other => panic!("expected Thread payload, got {other:?}"),
    }
}

#[test]
fn missing_optional_fields_deserialize_as_defaults() {
    let yaml = r#"
summary: "Just a summary."
meta:
  extractor: distill-idea-v1
  model: passthrough
  produced-at: "2026-05-16T14:03:22Z"
"#;

    let decoded: Distilled = serde_yaml::from_str(yaml).expect("deserialize");
    assert_eq!(decoded.summary, "Just a summary.");
    assert!(decoded.claims.is_empty());
    assert!(decoded.tags.is_empty());
    assert!(decoded.links.is_empty());
    assert!(decoded.kind_specific.is_none());
    assert_eq!(decoded.meta.input_tokens, 0);
    assert_eq!(decoded.meta.output_tokens, 0);
    assert!(decoded.meta.validation.fallback_reason.is_none());
}

#[test]
fn kebab_case_keys_on_serialize() {
    let original = Distilled {
        summary: "X".to_string(),
        claims: vec![],
        tags: vec![],
        links: vec![],
        kind_specific: Some(KindPayload::Repo(RepoPayload {
            stars: Some(10),
            primary_language: None,
            last_commit: Some("2026-05-10".to_string()),
            topics: vec![],
            install: None,
        })),
        meta: DistilledMeta {
            extractor: "distill-repo-v1".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            input_tokens: 100,
            output_tokens: 50,
            produced_at: "2026-05-16T14:03:22Z".to_string(),
            validation: ValidationMeta {
                fallback_reason: None,
                bounds_truncations: vec!["claims:10>7".to_string()],
                anchors_stripped: 2,
                raw_output: None,
            },
        },
        transcript: None,
    };

    let yaml = serde_yaml::to_string(&original).expect("serialize");
    assert!(yaml.contains("kind-specific:"));
    assert!(yaml.contains("primary-language:"));
    assert!(yaml.contains("last-commit:"));
    assert!(yaml.contains("input-tokens:"));
    assert!(yaml.contains("produced-at:"));
    assert!(yaml.contains("bounds-truncations:"));
    assert!(yaml.contains("anchors-stripped:"));
    assert!(yaml.contains("kind: repo"));
}

#[test]
fn validation_meta_records_fallback_reason() {
    let yaml = r#"
summary: "[Fabric timeout after 60s]\n\nSnippet of transcript."
meta:
  extractor: distill-article-v1
  model: timeout
  produced-at: "2026-05-16T14:03:22Z"
  validation:
    fallback-reason: fabric-timeout
    bounds-truncations: []
    anchors-stripped: 0
"#;
    let decoded: Distilled = serde_yaml::from_str(yaml).expect("deserialize");
    assert_eq!(
        decoded.meta.validation.fallback_reason.as_deref(),
        Some("fabric-timeout")
    );
    assert_eq!(decoded.meta.model, "timeout");
}

// ---- Phase 3: Claim schema back/forward compatibility ---------------------

/// Old-shape claim: no `kind` / `who` / `quote` field, exactly as every
/// pre-Phase-3 staged `distilled.yml` and `cortex summarize --backfill` input
/// looks. Must deserialize to `kind=fact, who=None, quote=None`.
const FIXTURE_OLD_SHAPE: &str = r#"
summary: "A pre-Phase-3 article."
claims:
  - text: "The first claim."
  - text: "The second claim."
    anchor: "section-two"
meta:
  extractor: distill-article-v1
  model: claude-sonnet-4-6
  produced-at: "2026-05-16T14:03:22Z"
"#;

/// New-shape claim: all Phase 3 fields present.
const FIXTURE_NEW_SHAPE: &str = r#"
summary: "A Phase-3 article with rich claims."
claims:
  - text: "Orchestration beats autonomy for coding agents."
    anchor: "00:14:30"
    kind: position
    who: "@simonw"
    quote: "the agents don't need to be smart, the harness does"
  - text: "Latency dropped 40% after the rewrite."
    kind: number
meta:
  extractor: distill-video-v1
  model: claude-sonnet-4-6
  produced-at: "2026-05-16T14:03:22Z"
"#;

/// Unknown-kind claim: a drifting LLM emits a claim kind outside the
/// vocabulary. The whole `Distilled` MUST still deserialize (not fall to the
/// yaml-parse-error path); the offending claim degrades to `kind=fact`.
const FIXTURE_UNKNOWN_KIND: &str = r#"
summary: "A distillation with a drifting claim kind."
claims:
  - text: "This is speculative."
    kind: speculation
  - text: "This is fine."
    kind: recommendation
meta:
  extractor: distill-article-v1
  model: claude-sonnet-4-6
  produced-at: "2026-05-16T14:03:22Z"
"#;

#[test]
fn fixture_old_shape_claims_default_to_fact() {
    let decoded: Distilled = serde_yaml::from_str(FIXTURE_OLD_SHAPE).expect("old-shape deserialize");
    assert_eq!(decoded.claims.len(), 2);
    for claim in &decoded.claims {
        assert_eq!(claim.kind, ClaimKind::Fact, "old-shape claim must default to fact");
        assert!(claim.who.is_none(), "old-shape claim must have who=None");
        assert!(claim.quote.is_none(), "old-shape claim must have quote=None");
    }
    assert_eq!(decoded.claims[0].text, "The first claim.");
    assert!(decoded.claims[0].anchor.is_none());
    assert_eq!(decoded.claims[1].anchor.as_deref(), Some("section-two"));
}

#[test]
fn fixture_new_shape_claims_carry_all_fields() {
    let decoded: Distilled = serde_yaml::from_str(FIXTURE_NEW_SHAPE).expect("new-shape deserialize");
    assert_eq!(decoded.claims.len(), 2);

    let first = &decoded.claims[0];
    assert_eq!(first.kind, ClaimKind::Position);
    assert_eq!(first.who.as_deref(), Some("@simonw"));
    assert_eq!(first.anchor.as_deref(), Some("00:14:30"));
    assert_eq!(
        first.quote.as_deref(),
        Some("the agents don't need to be smart, the harness does")
    );

    let second = &decoded.claims[1];
    assert_eq!(second.kind, ClaimKind::Number);
    assert!(second.who.is_none());
    assert!(second.quote.is_none());
}

#[test]
fn fixture_unknown_kind_degrades_to_fact_without_failing_parse() {
    // The whole payload deserializes (forward-compat shim); the unknown kind
    // becomes fact, the known kind survives.
    let decoded: Distilled = serde_yaml::from_str(FIXTURE_UNKNOWN_KIND).expect("unknown-kind deserialize");
    assert_eq!(decoded.claims.len(), 2);
    assert_eq!(
        decoded.claims[0].kind,
        ClaimKind::Fact,
        "unknown kind must degrade to fact, not error"
    );
    assert_eq!(decoded.claims[1].kind, ClaimKind::Recommendation);
}

#[test]
fn claim_kind_deserializes_unknown_value_to_fact() {
    // Direct probe of the shim at the enum level.
    let kind: ClaimKind = serde_yaml::from_str("nonsense-kind").expect("unknown kind deserialize");
    assert_eq!(kind, ClaimKind::Fact);
}

#[test]
fn claim_kind_deserialize_is_case_insensitive() {
    let kind: ClaimKind = serde_yaml::from_str("POSITION").expect("uppercase kind deserialize");
    assert_eq!(kind, ClaimKind::Position);
}

#[test]
fn claim_kind_serializes_lowercase() {
    assert_eq!(
        serde_yaml::to_string(&ClaimKind::Position).expect("serialize").trim(),
        "position"
    );
    assert_eq!(
        serde_yaml::to_string(&ClaimKind::Fact).expect("serialize").trim(),
        "fact"
    );
}

#[test]
fn claim_kind_roundtrips_all_known_values() {
    for kind in [
        ClaimKind::Fact,
        ClaimKind::Position,
        ClaimKind::Recommendation,
        ClaimKind::Number,
    ] {
        let yaml = serde_yaml::to_string(&kind).expect("serialize");
        let decoded: ClaimKind = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(decoded, kind);
    }
}
