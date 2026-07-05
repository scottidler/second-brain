use super::*;
use vault::distilled::{
    Claim, ClaimKind, Distilled, DistilledMeta, KindPayload, Link, RepoPayload, ThreadPayload, ValidationMeta,
    VideoPayload,
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
                ..Default::default()
            },
            Claim {
                text: "Second claim with anchor.".to_string(),
                anchor: Some("12:34".to_string()),
                ..Default::default()
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
    // Empty repos renders no github key.
    assert!(!fm.contains_key("github"));
}

#[test]
fn render_emits_github_sequence_for_video_repos() {
    let distilled = Distilled {
        summary: "x".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: Some(KindPayload::Video(VideoPayload {
            channel: None,
            duration_seconds: None,
            published_at: None,
            repos: vec!["coleam00/archon".to_string(), "scottidler/second-brain".to_string()],
        })),
        meta: base_meta("distill-video-v1"),
        transcript: None,
    };
    let fm = render(&distilled).frontmatter_additions;
    assert_eq!(
        fm.get("github"),
        Some(&serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::String("coleam00/archon".to_string()),
            serde_yaml::Value::String("scottidler/second-brain".to_string()),
        ]))
    );
}

#[test]
fn render_omits_github_key_when_repos_empty() {
    let distilled = Distilled {
        summary: "x".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: Some(KindPayload::Video(VideoPayload {
            channel: Some("Chan".to_string()),
            duration_seconds: None,
            published_at: None,
            repos: vec![],
        })),
        meta: base_meta("distill-video-v1"),
        transcript: None,
    };
    let fm = render(&distilled).frontmatter_additions;
    assert!(!fm.contains_key("github"));
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
                ..Default::default()
            },
            Claim {
                text: "Beta claim.".to_string(),
                anchor: Some("00:42".to_string()),
                ..Default::default()
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
fn render_fact_claim_with_no_who_or_quote_is_byte_identical_to_legacy() {
    // Regression guard: the default (fact, no who, no quote) claim must render
    // exactly as it did pre-Phase-3 — no `**kind**` prefix, no `: ` separator.
    let distilled = Distilled {
        summary: "s".to_string(),
        claims: vec![
            Claim {
                text: "A plain fact.".to_string(),
                anchor: None,
                ..Default::default()
            },
            Claim {
                text: "A fact with an anchor.".to_string(),
                anchor: Some("12:34".to_string()),
                ..Default::default()
            },
        ],
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: base_meta("distill-idea-v1"),
        transcript: None,
    };
    let body = render(&distilled).body_markdown;
    assert!(body.contains("- A plain fact.\n"), "legacy fact shape changed: {body}");
    assert!(body.contains("- A fact with an anchor. [12:34]\n"));
    assert!(!body.contains("**fact**"), "fact kind must never render a prefix");
}

#[test]
fn render_decorates_kind_who_and_quote() {
    let distilled = Distilled {
        summary: "s".to_string(),
        claims: vec![Claim {
            text: "Orchestration beats autonomy for coding agents.".to_string(),
            anchor: Some("00:14:30".to_string()),
            kind: ClaimKind::Position,
            who: Some("@simonw".to_string()),
            quote: Some("the agents don't need to be smart, the harness does".to_string()),
        }],
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: base_meta("distill-video-v1"),
        transcript: None,
    };
    let body = render(&distilled).body_markdown;
    assert!(
        body.contains("- **position** (@simonw): Orchestration beats autonomy for coding agents. [00:14:30]\n"),
        "decorated bullet missing: {body}"
    );
    assert!(
        body.contains("  > \"the agents don't need to be smart, the harness does\"\n"),
        "quote blockquote missing: {body}"
    );
}

#[test]
fn render_omits_who_parens_when_absent() {
    let distilled = Distilled {
        summary: "s".to_string(),
        claims: vec![Claim {
            text: "You should pin the model version.".to_string(),
            anchor: None,
            kind: ClaimKind::Recommendation,
            who: None,
            quote: None,
        }],
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: base_meta("distill-article-v1"),
        transcript: None,
    };
    let body = render(&distilled).body_markdown;
    assert!(
        body.contains("- **recommendation**: You should pin the model version.\n"),
        "recommendation-only decoration wrong: {body}"
    );
    assert!(!body.contains("()"), "empty who parens must not render");
}

#[test]
fn render_fully_decorated_claim_round_trips_through_parse_body_claims() {
    // Success criterion: a rendered claim with all fields present parses back
    // via parse_body_claims and yields the claim text (plus the decoration).
    let distilled = Distilled {
        summary: "Round-trip.".to_string(),
        claims: vec![Claim {
            text: "Orchestration beats autonomy.".to_string(),
            anchor: Some("00:14:30".to_string()),
            kind: ClaimKind::Position,
            who: Some("@simonw".to_string()),
            quote: Some("the harness does the thinking".to_string()),
        }],
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: base_meta("distill-video-v1"),
        transcript: None,
    };
    let body = render(&distilled).body_markdown;
    let parsed = vault::search::parse_body_claims(&body);
    assert_eq!(parsed.len(), 1);
    let c = &parsed[0];
    assert_eq!(c.text, "Orchestration beats autonomy.");
    assert_eq!(c.anchor.as_deref(), Some("00:14:30"));
    assert_eq!(c.kind, ClaimKind::Position);
    assert_eq!(c.who.as_deref(), Some("@simonw"));
    assert_eq!(c.quote.as_deref(), Some("the harness does the thinking"));
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

#[test]
fn frontmatter_additions_escape_special_chars_on_serialize() {
    // A frontmatter value harvested from upstream metadata can carry YAML
    // structural characters (`:` `\n` `\`). render emits typed
    // serde_yaml::Value::String values, so serializing the additions map must
    // escape them and round-trip back to the exact same string - never break
    // the published frontmatter or silently mangle the value.
    let nasty = "Bad: value\nwith newline\\and backslash: 12:00";
    let distilled = Distilled {
        summary: "x".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: Some(KindPayload::Video(VideoPayload {
            channel: Some(nasty.to_string()),
            duration_seconds: None,
            published_at: None,
            repos: Vec::new(),
        })),
        meta: base_meta("distill-video-v1"),
        transcript: None,
    };

    let fm = render(&distilled).frontmatter_additions;
    // Serialize the additions map exactly as the publish layer would, then
    // re-parse and confirm the value survives byte-for-byte.
    let yaml = serde_yaml::to_string(&fm).expect("serialize frontmatter additions");
    let reparsed: std::collections::BTreeMap<String, serde_yaml::Value> =
        serde_yaml::from_str(&yaml).expect("re-parse serialized frontmatter");
    assert_eq!(
        reparsed.get("cortex-video-channel"),
        Some(&serde_yaml::Value::String(nasty.to_string()))
    );
}
