use super::*;
use vault::distilled::{
    Claim, ClaimKind, Distilled, DistilledMeta, EnumeratedItem, Enumeration, KindPayload, Link, RepoPayload,
    ThreadPayload, ValidationMeta, VideoPayload,
};

/// Transcript-emission policies used at the render seams. `NO_TRANSCRIPT` is the
/// Video/Article/Repo publish policy; `WITH_TRANSCRIPT` is the verbatim-kind and
/// cortex-backfill policy.
const WITH_TRANSCRIPT: RenderOptions = RenderOptions {
    include_transcript: true,
};
const NO_TRANSCRIPT: RenderOptions = RenderOptions {
    include_transcript: false,
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
        links: vec![Link {
            url: "https://example.com".to_string(),
            label: Some("Ref".to_string()),
        }],
        meta: base_meta("distill-article-v1"),
        ..Default::default()
    };

    let rendered = render(&distilled, WITH_TRANSCRIPT);
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
        meta: base_meta("distill-idea-v1"),
        ..Default::default()
    };

    let body = render(&distilled, WITH_TRANSCRIPT).body_markdown;
    assert!(body.contains("## Summary"));
    assert!(!body.contains("## Claims"));
    assert!(!body.contains("## Links"));
    assert!(!body.contains("## Enumerated Points"));
    assert!(!body.contains("## Key Ideas"));
    assert!(!body.contains("[!tldr]"));
}

#[test]
fn render_emits_control_frontmatter_fields() {
    let distilled = Distilled {
        summary: "x".to_string(),
        meta: base_meta("distill-idea-v1"),
        ..Default::default()
    };

    let fm = render(&distilled, WITH_TRANSCRIPT).frontmatter_additions;
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
        kind_specific: Some(KindPayload::Repo(RepoPayload {
            stars: Some(1432),
            primary_language: Some("Rust".to_string()),
            last_commit: Some("2026-05-10".to_string()),
            topics: vec!["cli".to_string(), "rust".to_string()],
            install: Some("cargo install foo".to_string()),
        })),
        meta: base_meta("distill-repo-v1"),
        ..Default::default()
    };

    let fm = render(&distilled, NO_TRANSCRIPT).frontmatter_additions;
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
        kind_specific: Some(KindPayload::Repo(RepoPayload {
            stars: None,
            primary_language: None,
            last_commit: None,
            topics: Vec::new(),
            install: Some(install),
        })),
        meta: base_meta("distill-repo-v1"),
        ..Default::default()
    };
    let fm = render(&distilled, NO_TRANSCRIPT).frontmatter_additions;
    assert!(!fm.contains_key("cortex-repo-install"));
}

#[test]
fn render_emits_video_frontmatter() {
    let distilled = Distilled {
        summary: "x".to_string(),
        kind_specific: Some(KindPayload::Video(VideoPayload {
            channel: Some("Some Channel".to_string()),
            duration_seconds: Some(3247),
            published_at: Some("2026-04-22".to_string()),
            repos: vec![],
        })),
        meta: base_meta("distill-video-v1"),
        ..Default::default()
    };
    let fm = render(&distilled, NO_TRANSCRIPT).frontmatter_additions;
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
        kind_specific: Some(KindPayload::Video(VideoPayload {
            channel: None,
            duration_seconds: None,
            published_at: None,
            repos: vec!["coleam00/archon".to_string(), "scottidler/second-brain".to_string()],
        })),
        meta: base_meta("distill-video-v1"),
        ..Default::default()
    };
    let fm = render(&distilled, NO_TRANSCRIPT).frontmatter_additions;
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
        kind_specific: Some(KindPayload::Video(VideoPayload {
            channel: Some("Chan".to_string()),
            duration_seconds: None,
            published_at: None,
            repos: vec![],
        })),
        meta: base_meta("distill-video-v1"),
        ..Default::default()
    };
    let fm = render(&distilled, NO_TRANSCRIPT).frontmatter_additions;
    assert!(!fm.contains_key("github"));
}

#[test]
fn render_emits_thread_frontmatter() {
    let distilled = Distilled {
        summary: "x".to_string(),
        kind_specific: Some(KindPayload::Thread(ThreadPayload {
            author: Some("@someone".to_string()),
            post_count: 47,
            platform: "x".to_string(),
        })),
        meta: base_meta("distill-thread-v1"),
        ..Default::default()
    };
    let fm = render(&distilled, WITH_TRANSCRIPT).frontmatter_additions;
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
        meta: base_meta("distill-idea-v1"),
        ..Default::default()
    };

    let body = render(&distilled, WITH_TRANSCRIPT).body_markdown;
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
        meta: base_meta("distill-idea-v1"),
        ..Default::default()
    };
    let body = render(&distilled, WITH_TRANSCRIPT).body_markdown;
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
        meta: base_meta("distill-video-v1"),
        ..Default::default()
    };
    let body = render(&distilled, WITH_TRANSCRIPT).body_markdown;
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
        meta: base_meta("distill-article-v1"),
        ..Default::default()
    };
    let body = render(&distilled, NO_TRANSCRIPT).body_markdown;
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
        meta: base_meta("distill-video-v1"),
        ..Default::default()
    };
    let body = render(&distilled, WITH_TRANSCRIPT).body_markdown;
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
fn render_emits_transcript_section_when_present_and_included() {
    // Verbatim kinds populate `Distilled.transcript` with the raw input so the
    // published note is a verbatim archive, and pass `include_transcript: true`.
    let distilled = Distilled {
        summary: "Distilled gloss.".to_string(),
        meta: base_meta("distill-idea-v2"),
        transcript: Some("The user's full original text, all five paragraphs of it.".to_string()),
        ..Default::default()
    };

    let body = render(&distilled, WITH_TRANSCRIPT).body_markdown;
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
    // No transcript at all — nothing to emit regardless of the include flag.
    let distilled = Distilled {
        summary: "URL article summary.".to_string(),
        meta: base_meta("distill-article-v1"),
        ..Default::default()
    };

    let body = render(&distilled, WITH_TRANSCRIPT).body_markdown;
    assert!(
        !body.contains("## Transcript"),
        "no-transcript body should not contain ## Transcript: {body}"
    );
}

#[test]
fn render_omits_transcript_section_when_empty_string() {
    // Defensive: a distiller that fills transcript with whitespace-only text
    // should not produce an empty `## Transcript\n\n\n` block.
    let distilled = Distilled {
        summary: "x".to_string(),
        meta: base_meta("distill-idea-v2"),
        transcript: Some("   \n\n  ".to_string()),
        ..Default::default()
    };
    let body = render(&distilled, WITH_TRANSCRIPT).body_markdown;
    assert!(!body.contains("## Transcript"));
}

// ---- 2026-07-07 distillation-output-restore: transcript render is caller-gated

#[test]
fn render_omits_transcript_section_when_include_false_even_if_field_is_some() {
    // The load-bearing new behavior: Video/Article publish keeps the transcript
    // FIELD populated (staging + embeddings) but emits NO `## Transcript` body
    // section. Field Some, section absent — correct by design.
    let distilled = Distilled {
        summary: "A long video's distilled summary.".to_string(),
        meta: base_meta("distill-video-v1"),
        transcript: Some("Full spoken transcript that must NOT reach the note body.".to_string()),
        ..Default::default()
    };
    assert!(distilled.transcript.is_some(), "field must stay populated");
    let body = render(&distilled, NO_TRANSCRIPT).body_markdown;
    assert!(
        !body.contains("## Transcript"),
        "include_transcript=false must drop the section even when the field is Some: {body}"
    );
    assert!(
        !body.contains("Full spoken transcript"),
        "verbatim transcript text must never reach the note body when suppressed: {body}"
    );
}

#[test]
fn render_keeps_transcript_section_when_include_true_and_field_is_some() {
    // The backfill / verbatim-kind path: same Distilled, opposite policy, must
    // DO contain the transcript section (guards the summarize backfill from
    // silently destroying a legacy note body).
    let distilled = Distilled {
        summary: "A long video's distilled summary.".to_string(),
        meta: base_meta("distill-video-v1"),
        transcript: Some("Full spoken transcript that must survive a backfill re-render.".to_string()),
        ..Default::default()
    };
    let body = render(&distilled, WITH_TRANSCRIPT).body_markdown;
    assert!(
        body.contains("## Transcript"),
        "include_transcript=true must emit the section: {body}"
    );
    assert!(body.contains("Full spoken transcript that must survive a backfill re-render."));
}

// ---- New section: tldr callout, Enumerated Points, Key Ideas ---------------

#[test]
fn render_emits_tldr_callout() {
    let distilled = Distilled {
        summary: "The full summary paragraph.".to_string(),
        tldr: Some("Ten CLI tools every agent engineer should know.".to_string()),
        meta: base_meta("distill-video-v1"),
        ..Default::default()
    };
    let body = render(&distilled, NO_TRANSCRIPT).body_markdown;
    // The literal `> [!tldr]` marker is what cortex::quality keys on.
    assert!(body.contains("> [!tldr]"), "tldr callout marker missing: {body}");
    assert!(
        body.contains("> Ten CLI tools every agent engineer should know."),
        "tldr content missing: {body}"
    );
    // Callout precedes the summary.
    let tldr_pos = body.find("[!tldr]").expect("tldr");
    let summary_pos = body.find("## Summary").expect("summary");
    assert!(tldr_pos < summary_pos, "tldr callout must precede Summary: {body}");
}

#[test]
fn render_emits_enumerated_points_numbered_bold_and_anchored() {
    let distilled = Distilled {
        summary: "A Top-3 tools video.".to_string(),
        enumeration: Some(Enumeration {
            lead_in: Some("The creator covers 3 essential tools:".to_string()),
            declared_count: Some(3),
            items: vec![
                EnumeratedItem {
                    name: "Codex Plugin".to_string(),
                    text: "In-editor autonomous edits.".to_string(),
                    anchor: Some("02:10".to_string()),
                },
                EnumeratedItem {
                    name: "Ripgrep".to_string(),
                    text: "Fast recursive search.".to_string(),
                    anchor: None,
                },
            ],
        }),
        meta: base_meta("distill-video-v1"),
        ..Default::default()
    };
    let body = render(&distilled, NO_TRANSCRIPT).body_markdown;
    assert!(body.contains("## Enumerated Points"), "section heading missing: {body}");
    assert!(
        body.contains("The creator covers 3 essential tools:"),
        "lead-in missing: {body}"
    );
    assert!(
        body.contains("1. **Codex Plugin**: In-editor autonomous edits. [02:10]\n"),
        "first item shape wrong: {body}"
    );
    assert!(
        body.contains("2. **Ripgrep**: Fast recursive search.\n"),
        "second item (no anchor) shape wrong: {body}"
    );
    // Ordering: Enumerated Points sits between Summary and Claims.
    let summary_pos = body.find("## Summary").expect("summary");
    let enum_pos = body.find("## Enumerated Points").expect("enum");
    assert!(summary_pos < enum_pos, "Enumerated Points must follow Summary: {body}");
}

#[test]
fn render_omits_enumerated_points_when_absent_or_empty() {
    // None enumeration -> no section.
    let none = Distilled {
        summary: "Not a listicle.".to_string(),
        meta: base_meta("distill-video-v1"),
        ..Default::default()
    };
    assert!(
        !render(&none, NO_TRANSCRIPT)
            .body_markdown
            .contains("## Enumerated Points")
    );

    // Some enumeration but empty items -> still no section (no forced listicle).
    let empty = Distilled {
        summary: "Not a listicle.".to_string(),
        enumeration: Some(Enumeration {
            lead_in: Some("stray lead-in".to_string()),
            declared_count: None,
            items: Vec::new(),
        }),
        meta: base_meta("distill-video-v1"),
        ..Default::default()
    };
    let body = render(&empty, NO_TRANSCRIPT).body_markdown;
    assert!(
        !body.contains("## Enumerated Points"),
        "empty items must emit no section: {body}"
    );
    assert!(
        !body.contains("stray lead-in"),
        "lead-in must not leak without items: {body}"
    );
}

#[test]
fn render_emits_key_ideas_and_omits_when_empty() {
    let with_ideas = Distilled {
        summary: "s".to_string(),
        key_ideas: vec![
            "Harness quality dominates raw model capability.".to_string(),
            "Tight feedback loops beat larger context windows.".to_string(),
        ],
        meta: base_meta("distill-article-v1"),
        ..Default::default()
    };
    let body = render(&with_ideas, NO_TRANSCRIPT).body_markdown;
    assert!(body.contains("## Key Ideas"), "key ideas heading missing: {body}");
    assert!(body.contains("- Harness quality dominates raw model capability.\n"));
    assert!(body.contains("- Tight feedback loops beat larger context windows.\n"));

    // Empty key_ideas -> no section.
    let none = Distilled {
        summary: "s".to_string(),
        meta: base_meta("distill-article-v1"),
        ..Default::default()
    };
    assert!(!render(&none, NO_TRANSCRIPT).body_markdown.contains("## Key Ideas"));

    // Whitespace-only entries are filtered; an all-blank list emits no section.
    let blank = Distilled {
        summary: "s".to_string(),
        key_ideas: vec!["   ".to_string(), "\n".to_string()],
        meta: base_meta("distill-article-v1"),
        ..Default::default()
    };
    assert!(!render(&blank, NO_TRANSCRIPT).body_markdown.contains("## Key Ideas"));
}

#[test]
fn render_emits_full_april_section_order() {
    // The restored April shape, all sections present: tldr callout, Summary,
    // Enumerated Points, Key Ideas, Claims, Links, then Transcript last.
    let distilled = Distilled {
        summary: "The full summary.".to_string(),
        tldr: Some("One-line hook.".to_string()),
        enumeration: Some(Enumeration {
            lead_in: None,
            declared_count: Some(1),
            items: vec![EnumeratedItem {
                name: "Item One".to_string(),
                text: "First item.".to_string(),
                anchor: None,
            }],
        }),
        key_ideas: vec!["A theme.".to_string()],
        claims: vec![Claim {
            text: "A claim.".to_string(),
            ..Default::default()
        }],
        links: vec![Link {
            url: "https://example.com".to_string(),
            label: None,
        }],
        meta: base_meta("distill-video-v1"),
        transcript: Some("The transcript.".to_string()),
        ..Default::default()
    };
    let body = render(&distilled, WITH_TRANSCRIPT).body_markdown;
    let tldr = body.find("[!tldr]").expect("tldr");
    let summary = body.find("## Summary").expect("summary");
    let enumerated = body.find("## Enumerated Points").expect("enumerated");
    let ideas = body.find("## Key Ideas").expect("key ideas");
    let claims = body.find("## Claims").expect("claims");
    let links = body.find("## Links").expect("links");
    let transcript = body.find("## Transcript").expect("transcript");
    assert!(
        tldr < summary
            && summary < enumerated
            && enumerated < ideas
            && ideas < claims
            && claims < links
            && links < transcript,
        "section order wrong:\n{body}"
    );
}

#[test]
fn render_round_trips_new_sections_through_vault_body_parsers() {
    // Success criterion: a Distilled carrying tldr + enumeration + key-ideas
    // renders a body whose Summary and Claims still parse back cleanly (the new
    // sections are additive and back-compat: parsers scan until the next `## `).
    let distilled = Distilled {
        summary: "Round-trip with the new sections.".to_string(),
        tldr: Some("The hook.".to_string()),
        enumeration: Some(Enumeration {
            lead_in: Some("Two things:".to_string()),
            declared_count: Some(2),
            items: vec![
                EnumeratedItem {
                    name: "Alpha".to_string(),
                    text: "First.".to_string(),
                    anchor: Some("00:10".to_string()),
                },
                EnumeratedItem {
                    name: "Beta".to_string(),
                    text: "Second.".to_string(),
                    anchor: None,
                },
            ],
        }),
        key_ideas: vec!["Idea one.".to_string(), "Idea two.".to_string()],
        claims: vec![Claim {
            text: "A surviving claim.".to_string(),
            anchor: Some("01:00".to_string()),
            ..Default::default()
        }],
        meta: base_meta("distill-video-v1"),
        ..Default::default()
    };
    let body = render(&distilled, NO_TRANSCRIPT).body_markdown;

    // All new sections present.
    assert!(body.contains("> [!tldr]"));
    assert!(body.contains("## Enumerated Points"));
    assert!(body.contains("## Key Ideas"));

    // Existing parsers still recover summary + claims through the added sections.
    assert_eq!(
        vault::search::parse_body_summary(&body).expect("summary parses"),
        "Round-trip with the new sections."
    );
    let claims = vault::search::parse_body_claims(&body);
    assert_eq!(claims.len(), 1, "claims must survive the added sections: {body}");
    assert_eq!(claims[0].text, "A surviving claim.");
    assert_eq!(claims[0].anchor.as_deref(), Some("01:00"));
}

// ---- Per-call-site transcript policy (one test per render call site) -------
//
// The six production render call sites and the policy each passes
// (2026-07-07 distillation-output-restore, Architecture policy table):
//   1. borg/src/pipeline.rs (URL: video/article/repo/thread) -> for_url_publish
//   2. borg/src/pipeline/text.rs   (text/idea)   -> include_transcript: true
//   3. borg/src/pipeline/text.rs   (vocabulary)  -> include_transcript: true
//   4. borg/src/pipeline/handlers.rs (image)     -> include_transcript: true
//   5. borg/src/pipeline/handlers.rs (audio)     -> include_transcript: true
//   6. cortex/src/summarize.rs (backfill)        -> include_transcript: true
//     (site 6 is pinned by cortex/src/summarize/tests.rs::backfill_render_keeps_transcript_section)

fn transcript_distilled(kind: Option<KindPayload>) -> Distilled {
    Distilled {
        summary: "A summary.".to_string(),
        kind_specific: kind,
        meta: base_meta("distill-x-v1"),
        transcript: Some("Verbatim source text.".to_string()),
        ..Default::default()
    }
}

#[test]
fn site1_url_publish_policy_per_kind() {
    // pipeline.rs URL site: Thread keeps the transcript; Video/Repo/Article drop it.
    let video = transcript_distilled(Some(KindPayload::Video(VideoPayload::default())));
    assert!(
        !RenderOptions::for_url_publish(&video).include_transcript,
        "video -> false"
    );
    assert!(
        !render(&video, RenderOptions::for_url_publish(&video))
            .body_markdown
            .contains("## Transcript"),
        "video publish body must have no transcript"
    );

    let repo = transcript_distilled(Some(KindPayload::Repo(RepoPayload::default())));
    assert!(
        !RenderOptions::for_url_publish(&repo).include_transcript,
        "repo -> false"
    );

    let article = transcript_distilled(None);
    assert!(
        !RenderOptions::for_url_publish(&article).include_transcript,
        "article (no payload) -> false"
    );
    assert!(
        !render(&article, RenderOptions::for_url_publish(&article))
            .body_markdown
            .contains("## Transcript"),
        "article publish body must have no transcript"
    );

    let thread = transcript_distilled(Some(KindPayload::Thread(ThreadPayload {
        author: None,
        post_count: 3,
        platform: "x".to_string(),
    })));
    assert!(
        RenderOptions::for_url_publish(&thread).include_transcript,
        "thread -> true (verbatim kind)"
    );
    assert!(
        render(&thread, RenderOptions::for_url_publish(&thread))
            .body_markdown
            .contains("## Transcript"),
        "thread publish body must keep its transcript"
    );
}

#[test]
fn site2_text_idea_publish_includes_transcript() {
    // text.rs (text/idea) passes include_transcript: true — verbatim kind.
    let d = transcript_distilled(None);
    let body = render(&d, WITH_TRANSCRIPT).body_markdown;
    assert!(
        body.contains("## Transcript"),
        "text/idea note must keep its transcript"
    );
}

#[test]
fn site3_vocabulary_publish_includes_transcript() {
    // text.rs (vocabulary) passes include_transcript: true — verbatim kind.
    let d = transcript_distilled(None);
    let body = render(&d, WITH_TRANSCRIPT).body_markdown;
    assert!(
        body.contains("## Transcript"),
        "vocabulary note must keep its transcript"
    );
}

#[test]
fn site4_image_publish_includes_transcript() {
    // handlers.rs (image) passes include_transcript: true — verbatim kind.
    let d = transcript_distilled(None);
    let body = render(&d, WITH_TRANSCRIPT).body_markdown;
    assert!(
        body.contains("## Transcript"),
        "image note must keep its OCR/vision text"
    );
}

#[test]
fn site5_audio_publish_includes_transcript() {
    // handlers.rs (audio/voicenote) passes include_transcript: true — verbatim kind.
    let d = transcript_distilled(None);
    let body = render(&d, WITH_TRANSCRIPT).body_markdown;
    assert!(body.contains("## Transcript"), "audio note must keep its transcript");
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
        kind_specific: Some(KindPayload::Video(VideoPayload {
            channel: Some(nasty.to_string()),
            duration_seconds: None,
            published_at: None,
            repos: Vec::new(),
        })),
        meta: base_meta("distill-video-v1"),
        ..Default::default()
    };

    let fm = render(&distilled, NO_TRANSCRIPT).frontmatter_additions;
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
