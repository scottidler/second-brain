use super::*;
use vault::distilled::{
    Claim, Distilled, DistilledMeta, KindPayload, Link, RepoPayload, ThreadPayload, ValidationMeta, VideoPayload,
};

fn base_meta(extractor: &str) -> DistilledMeta {
    DistilledMeta {
        extractor: extractor.to_string(),
        model: "test".to_string(),
        input_tokens: 0,
        output_tokens: 0,
        produced_at: "2026-05-16T14:03:22Z".to_string(),
        validation: ValidationMeta::default(),
    }
}

#[test]
fn render_emits_managed_sections_in_canonical_order() {
    let distilled = Distilled {
        summary: "An idea about caches.".to_string(),
        claims: vec![
            Claim {
                text: "First claim.".to_string(),
                anchor: None,
            },
            Claim {
                text: "Second claim with anchor.".to_string(),
                anchor: Some("12:34".to_string()),
            },
        ],
        tags: vec![],
        links: vec![Link {
            url: "https://example.com".to_string(),
            label: Some("Ref".to_string()),
        }],
        kind_specific: None,
        meta: base_meta("distill-article-v1"),
        transcript: None,
    };

    let rendered = render(&distilled);
    let body = &rendered.body_markdown;
    let summary_pos = body.find("## Summary").expect("summary section");
    let claims_pos = body.find("## Claims").expect("claims section");
    let links_pos = body.find("## Links").expect("links section");
    assert!(summary_pos < claims_pos);
    assert!(claims_pos < links_pos);

    assert!(body.contains("An idea about caches."));
    assert!(body.contains("- First claim."));
    assert!(body.contains("- Second claim with anchor. [12:34]"));
    assert!(body.contains("- [Ref](https://example.com)"));
}

#[test]
fn render_omits_empty_sections() {
    let distilled = Distilled {
        summary: "Just a summary.".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: base_meta("distill-idea-v1"),
        transcript: None,
    };

    let body = render(&distilled).body_markdown;
    assert!(body.contains("## Summary"));
    assert!(!body.contains("## Claims"));
    assert!(!body.contains("## Links"));
}

#[test]
fn render_emits_control_frontmatter_fields() {
    let distilled = Distilled {
        summary: "x".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: base_meta("distill-idea-v1"),
        transcript: None,
    };

    let fm = render(&distilled).frontmatter_additions;
    assert_eq!(fm.get("distilled"), Some(&serde_yaml::Value::Bool(true)));
    assert_eq!(
        fm.get("distilled-extractor"),
        Some(&serde_yaml::Value::String("distill-idea-v1".to_string()))
    );
}

#[test]
fn render_emits_repo_frontmatter() {
    let distilled = Distilled {
        summary: "x".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: Some(KindPayload::Repo(RepoPayload {
            stars: Some(1432),
            primary_language: Some("Rust".to_string()),
            last_commit: Some("2026-05-10".to_string()),
            topics: vec!["cli".to_string(), "rust".to_string()],
            install: Some("cargo install foo".to_string()),
        })),
        meta: base_meta("distill-repo-v1"),
        transcript: None,
    };

    let fm = render(&distilled).frontmatter_additions;
    assert_eq!(
        fm.get("cortex-repo-stars"),
        Some(&serde_yaml::Value::Number(1432.into()))
    );
    assert_eq!(
        fm.get("cortex-repo-primary-language"),
        Some(&serde_yaml::Value::String("Rust".to_string()))
    );
    assert_eq!(
        fm.get("cortex-repo-last-commit"),
        Some(&serde_yaml::Value::String("2026-05-10".to_string()))
    );
    assert!(matches!(
        fm.get("cortex-repo-topics"),
        Some(serde_yaml::Value::Sequence(_))
    ));
    assert_eq!(
        fm.get("cortex-repo-install"),
        Some(&serde_yaml::Value::String("cargo install foo".to_string()))
    );
}

#[test]
fn render_drops_oversized_install_string() {
    let install = "x".repeat(501);
    let distilled = Distilled {
        summary: "x".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: Some(KindPayload::Repo(RepoPayload {
            stars: None,
            primary_language: None,
            last_commit: None,
            topics: Vec::new(),
            install: Some(install),
        })),
        meta: base_meta("distill-repo-v1"),
        transcript: None,
    };
    let fm = render(&distilled).frontmatter_additions;
    assert!(!fm.contains_key("cortex-repo-install"));
}

#[test]
fn render_emits_video_frontmatter() {
    let distilled = Distilled {
        summary: "x".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: Some(KindPayload::Video(VideoPayload {
            channel: Some("Some Channel".to_string()),
            duration_seconds: Some(3247),
            published_at: Some("2026-04-22".to_string()),
            repos: vec![],
        })),
        meta: base_meta("distill-video-v1"),
        transcript: None,
    };
    let fm = render(&distilled).frontmatter_additions;
    assert_eq!(
        fm.get("cortex-video-channel"),
        Some(&serde_yaml::Value::String("Some Channel".to_string()))
    );
    assert_eq!(
        fm.get("cortex-video-duration-seconds"),
        Some(&serde_yaml::Value::Number(3247.into()))
    );
    assert_eq!(
        fm.get("cortex-video-published-at"),
        Some(&serde_yaml::Value::String("2026-04-22".to_string()))
    );
}

#[test]
fn render_emits_thread_frontmatter() {
    let distilled = Distilled {
        summary: "x".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: Some(KindPayload::Thread(ThreadPayload {
            author: Some("@someone".to_string()),
            post_count: 47,
            platform: "x".to_string(),
        })),
        meta: base_meta("distill-thread-v1"),
        transcript: None,
    };
    let fm = render(&distilled).frontmatter_additions;
    assert_eq!(
        fm.get("cortex-thread-platform"),
        Some(&serde_yaml::Value::String("x".to_string()))
    );
    assert_eq!(
        fm.get("cortex-thread-post-count"),
        Some(&serde_yaml::Value::Number(47.into()))
    );
    assert_eq!(
        fm.get("cortex-thread-author"),
        Some(&serde_yaml::Value::String("@someone".to_string()))
    );
}

#[test]
fn render_round_trips_through_vault_body_parsers() {
    // Render -> body -> parse_body_summary / parse_body_claims must recover
    // the same summary and claim text we put in.
    let distilled = Distilled {
        summary: "Round-trip me cleanly.".to_string(),
        claims: vec![
            Claim {
                text: "Alpha claim.".to_string(),
                anchor: None,
            },
            Claim {
                text: "Beta claim.".to_string(),
                anchor: Some("00:42".to_string()),
            },
        ],
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: base_meta("distill-idea-v1"),
        transcript: None,
    };

    let body = render(&distilled).body_markdown;
    let parsed_summary = vault::search::parse_body_summary(&body).expect("summary parses");
    assert_eq!(parsed_summary, "Round-trip me cleanly.");

    let parsed_claims = vault::search::parse_body_claims(&body);
    assert_eq!(parsed_claims.len(), 2);
    assert_eq!(parsed_claims[0].text, "Alpha claim.");
    assert!(parsed_claims[0].anchor.is_none());
    assert_eq!(parsed_claims[1].text, "Beta claim.");
    assert_eq!(parsed_claims[1].anchor.as_deref(), Some("00:42"));
}

#[test]
fn render_emits_transcript_section_when_present() {
    // Phase 9c-hotfix: non-URL kinds populate `Distilled.transcript` with the
    // raw input so the published note is a verbatim archive even after the
    // LLM-distilled summary collapses the text.
    let distilled = Distilled {
        summary: "Distilled gloss.".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: base_meta("distill-idea-v2"),
        transcript: Some("The user's full original text, all five paragraphs of it.".to_string()),
    };

    let body = render(&distilled).body_markdown;
    let summary_pos = body.find("## Summary").expect("summary section");
    let transcript_pos = body.find("## Transcript").expect("transcript section");
    assert!(
        transcript_pos > summary_pos,
        "Transcript should follow Summary in canonical order"
    );
    assert!(
        body.contains("The user's full original text, all five paragraphs of it."),
        "transcript body missing: {body}"
    );
}

#[test]
fn render_omits_transcript_section_when_none() {
    // URL kinds (Article/Repo/Video/Thread) leave transcript: None — the
    // source URL is the recoverable archive, no verbatim section needed.
    let distilled = Distilled {
        summary: "URL article summary.".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: base_meta("distill-article-v1"),
        transcript: None,
    };

    let body = render(&distilled).body_markdown;
    assert!(
        !body.contains("## Transcript"),
        "URL-kind body should not contain ## Transcript: {body}"
    );
}

#[test]
fn render_omits_transcript_section_when_empty_string() {
    // Defensive: a distiller that fills transcript with whitespace-only text
    // should not produce an empty `## Transcript\n\n\n` block.
    let distilled = Distilled {
        summary: "x".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: base_meta("distill-idea-v2"),
        transcript: Some("   \n\n  ".to_string()),
    };
    let body = render(&distilled).body_markdown;
    assert!(!body.contains("## Transcript"));
}
