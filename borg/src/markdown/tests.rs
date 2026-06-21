use super::*;

fn test_config() -> FrontmatterConfig {
    FrontmatterConfig {
        default_tags: vec![],
        default_creator: String::new(),
        timezone: "UTC".to_string(),
    }
}

#[test]
fn test_distilled_body_replaces_legacy_summary_block() {
    let mut additions = BTreeMap::new();
    additions.insert("distilled".to_string(), serde_yaml::Value::Bool(true));
    additions.insert(
        "distilled-extractor".to_string(),
        serde_yaml::Value::String("distill-article-v1".to_string()),
    );
    additions.insert(
        "cortex-thread-platform".to_string(),
        serde_yaml::Value::String("x".to_string()),
    );
    let note = NoteContent {
        title: "T".to_string(),
        source_url: Some("https://x.com/u/status/1".to_string()),
        tags: vec!["thread".to_string()],
        summary: "Short concise summary.".to_string(),
        content_type: ContentType::Article { author: None },
        distilled_body: Some("## Summary\n\nShort concise summary.\n\n## Claims\n\n- One claim.\n\n".to_string()),
        frontmatter_additions: additions,
        ..Default::default()
    };
    let rendered = render_note(&note, &test_config());
    // Pre-rendered Distilled body lands in the note body.
    assert!(rendered.contains("## Summary\n\nShort concise summary."));
    assert!(rendered.contains("## Claims\n\n- One claim."));
    // Frontmatter additions are spliced in.
    assert!(rendered.contains("distilled: true"));
    // serde_yaml renders simple scalars bare (no quotes needed).
    assert!(rendered.contains("distilled-extractor: distill-article-v1"));
    assert!(rendered.contains("cortex-thread-platform: x"));
    // The legacy double-`## Summary` wrap is NOT applied on top of the
    // already-structured body.
    let count = rendered.matches("## Summary").count();
    assert_eq!(
        count, 1,
        "rendered body must not stack ## Summary headings:\n{rendered}"
    );
}

#[test]
fn test_legacy_summary_path_still_wraps_in_summary_section() {
    let note = NoteContent {
        title: "Legacy".to_string(),
        source_url: Some("https://example.com/".to_string()),
        tags: vec![],
        summary: "Plain prose summary.".to_string(),
        content_type: ContentType::Article { author: None },
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    // No distilled_body -> legacy ## Summary wrap.
    assert!(rendered.contains("## Summary\n\nPlain prose summary."));
}

#[test]
fn test_render_includes_ingested_field() {
    let note = NoteContent {
        title: "Note".to_string(),
        source_url: Some("https://example.com".to_string()),
        asset_path: None,
        tags: vec![],
        summary: "S".to_string(),
        content_type: ContentType::Article { author: None },
        description: None,
        embed_code: None,
        method: None,
        trace_id: None,
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(rendered.contains("date: "), "date field should be present");
    assert!(
        rendered.contains("ingested: "),
        "ingested field should be present on fresh ingest"
    );
}

#[test]
fn test_render_article_note() {
    let note = NoteContent {
        title: "Test Article".to_string(),
        source_url: Some("https://example.com/post".to_string()),
        asset_path: None,
        tags: vec!["rust".to_string(), "programming".to_string()],
        summary: "This is a summary.".to_string(),
        content_type: ContentType::Article { author: None },
        description: None,
        embed_code: None,
        method: None,
        trace_id: None,
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(rendered.contains("title: Test Article"));
    assert!(rendered.contains("type: article"));
    assert!(rendered.contains("origin: assisted"));
    assert!(rendered.contains("  - rust"));
    assert!(rendered.contains("## Summary"));
    assert!(rendered.contains("This is a summary."));
    assert!(rendered.contains("Source: [https://example.com/post]"));
    // An article with no byline and no default_creator emits no creator:
    // line (the `fabric -u` default path leaves it empty).
    assert!(!rendered.contains("creator:"));
}

#[test]
fn test_render_youtube_note() {
    let note = NoteContent {
            title: "Cool Video".to_string(),
            source_url: Some("https://youtube.com/watch?v=abc".to_string()),
            asset_path: None,
            tags: vec!["youtube".to_string()],
            summary: "Video summary here.".to_string(),
            content_type: ContentType::YouTube {
                uploader: "TechChannel".to_string(),
                duration_secs: 600.0,
            },
            description: None,
            embed_code: Some(r#"<iframe width="854" height="480" src="https://www.youtube.com/embed/abc" frameborder="0" allowfullscreen></iframe>"#.to_string()),
            method: Some(IngestMethod::Telegram),
            trace_id: None,
            slides: Vec::new(),
            ..NoteContent::default()
        };
    let rendered = render_note(&note, &test_config());
    assert!(rendered.contains("type: youtube"));
    assert!(rendered.contains("method: telegram"));
    assert!(rendered.contains("creator: TechChannel"));
    assert!(rendered.contains("duration: 10"));
    assert!(rendered.contains("iframe"));
    assert!(rendered.contains("## Summary"));
}

#[test]
fn test_render_with_default_tags() {
    let config = FrontmatterConfig {
        default_tags: vec!["obsidian-borg".to_string()],
        default_creator: "Scott".to_string(),
        timezone: "UTC".to_string(),
    };
    let note = NoteContent {
        title: "Test".to_string(),
        source_url: Some("https://example.com".to_string()),
        asset_path: None,
        tags: vec!["ai".to_string()],
        summary: String::new(),
        content_type: ContentType::Article { author: None },
        description: None,
        embed_code: None,
        method: None,
        trace_id: None,
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &config);
    assert!(rendered.contains("  - ai"));
    assert!(rendered.contains("  - obsidian-borg"));
    assert!(rendered.contains("creator: Scott"));
}

#[test]
fn test_render_note_without_source() {
    let note = NoteContent {
        title: "Quick Thought".to_string(),
        source_url: None,
        asset_path: None,
        tags: vec!["note".to_string()],
        summary: "Some quick note text.".to_string(),
        content_type: ContentType::Note,
        description: None,
        embed_code: None,
        method: Some(IngestMethod::Telegram),
        trace_id: None,
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(rendered.contains("type: note"));
    assert!(!rendered.contains("source:"));
    assert!(!rendered.contains("Source:"));
}

#[test]
fn test_render_image_note() {
    let note = NoteContent {
        title: "Whiteboard Photo".to_string(),
        source_url: None,
        asset_path: Some("system/attachments/images/2026-03/whiteboard-a1b2c3d4.png".to_string()),
        tags: vec!["image".to_string()],
        summary: "A whiteboard diagram.".to_string(),
        content_type: ContentType::Image {
            asset_path: "system/attachments/images/2026-03/whiteboard-a1b2c3d4.png".to_string(),
        },
        description: None,
        embed_code: None,
        method: Some(IngestMethod::Cli),
        trace_id: None,
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(rendered.contains("type: image"));
    assert!(rendered.contains("asset:"));
    assert!(rendered.contains("![[whiteboard-a1b2c3d4.png]]"));
}

#[test]
fn test_render_note_with_trace_id() {
    let note = NoteContent {
        title: "Trace Test".to_string(),
        source_url: Some("https://example.com".to_string()),
        asset_path: None,
        tags: vec!["test".to_string()],
        summary: "Summary.".to_string(),
        description: None,
        content_type: ContentType::Article { author: None },
        embed_code: None,
        method: Some(IngestMethod::Telegram),
        trace_id: Some("tg-7f3a2c".to_string()),
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(rendered.contains("trace: tg-7f3a2c"));
    assert!(rendered.contains("method: telegram"));
    // trace should appear after method
    let method_pos = rendered.find("method: telegram").expect("method line");
    let trace_pos = rendered.find("trace: tg-7f3a2c").expect("trace line");
    assert!(trace_pos > method_pos, "trace should come after method");
}

#[test]
fn test_render_note_without_trace_id() {
    let note = NoteContent {
        title: "No Trace".to_string(),
        source_url: None,
        asset_path: None,
        tags: vec![],
        summary: String::new(),
        content_type: ContentType::Note,
        description: None,
        embed_code: None,
        method: None,
        trace_id: None,
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(!rendered.contains("trace:"));
}

#[test]
fn test_render_github_note() {
    let note = NoteContent {
        title: "open-webui/open-terminal".to_string(),
        source_url: Some("https://github.com/open-webui/open-terminal".to_string()),
        asset_path: None,
        tags: vec!["github".to_string()],
        summary: "A terminal you can curl.".to_string(),
        content_type: ContentType::GitHub {
            owner: "open-webui".to_string(),
        },
        description: None,
        embed_code: None,
        method: Some(IngestMethod::Telegram),
        trace_id: None,
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(rendered.contains("type: github"));
    // creator is resolved from the repo owner.
    assert!(rendered.contains("creator: open-webui"));
}

#[test]
fn test_render_social_note() {
    let note = NoteContent {
        title: "Z.ai announcement".to_string(),
        source_url: Some("https://x.com/Zai_org/status/123".to_string()),
        asset_path: None,
        tags: vec!["ai".to_string()],
        summary: "A social post.".to_string(),
        content_type: ContentType::Social,
        description: None,
        embed_code: None,
        method: Some(IngestMethod::Telegram),
        trace_id: None,
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(rendered.contains("type: social"));
}

#[test]
fn test_render_reddit_note() {
    let note = NoteContent {
        title: "Understanding inside zone".to_string(),
        source_url: Some("https://www.reddit.com/r/footballstrategy/comments/abc/test/".to_string()),
        asset_path: None,
        tags: vec!["football".to_string()],
        summary: "A reddit discussion.".to_string(),
        content_type: ContentType::Reddit,
        description: None,
        embed_code: None,
        method: Some(IngestMethod::Telegram),
        trace_id: None,
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(rendered.contains("type: reddit"));
}

#[test]
fn test_render_youtube_note_with_description_callout() {
    let note = NoteContent {
            title: "Homelab Tour".to_string(),
            source_url: Some("https://youtube.com/watch?v=abc".to_string()),
            asset_path: None,
            tags: vec!["homelab".to_string()],
            summary: "A tour of my homelab.".to_string(),
            description: Some("My homelab after 3 years\n\nResources:\n- Talos: https://talos.dev\n- Cilium: https://cilium.io".to_string()),
            content_type: ContentType::YouTube {
                uploader: "TechChannel".to_string(),
                duration_secs: 1440.0,
            },
            embed_code: Some(r#"<iframe width="854" height="480" src="https://www.youtube.com/embed/abc" frameborder="0" allowfullscreen></iframe>"#.to_string()),
            method: Some(IngestMethod::Telegram),
            trace_id: None,
            slides: Vec::new(),
            ..NoteContent::default()
        };
    let rendered = render_note(&note, &test_config());
    // Callout header
    assert!(rendered.contains("> [!info]- Video Description"));
    // Content inside callout
    assert!(rendered.contains("> My homelab after 3 years"));
    assert!(rendered.contains("> - Talos: https://talos.dev"));
    // Blank line inside callout renders as bare >
    assert!(rendered.contains(">\n"));
    // Callout appears before Summary
    let callout_pos = rendered.find("> [!info]- Video Description").expect("callout header");
    let summary_pos = rendered.find("## Summary").expect("summary header");
    assert!(callout_pos < summary_pos, "callout should appear before summary");
    // Callout appears after iframe
    let iframe_pos = rendered.find("iframe").expect("iframe embed");
    assert!(callout_pos > iframe_pos, "callout should appear after iframe");
}

#[test]
fn test_render_note_without_description_has_no_callout() {
    let note = NoteContent {
        title: "Article".to_string(),
        source_url: Some("https://example.com".to_string()),
        asset_path: None,
        tags: vec![],
        summary: "Content.".to_string(),
        description: None,
        content_type: ContentType::Article { author: None },
        embed_code: None,
        method: None,
        trace_id: None,
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(!rendered.contains("[!info]"), "no callout when description is None");
}

#[test]
fn test_creator_for_per_variant() {
    assert_eq!(
        creator_for(&ContentType::YouTube {
            uploader: "TechChannel".to_string(),
            duration_secs: 600.0,
        }),
        Some("TechChannel".to_string())
    );
    assert_eq!(
        creator_for(&ContentType::GitHub {
            owner: "open-webui".to_string(),
        }),
        Some("open-webui".to_string())
    );
    assert_eq!(
        creator_for(&ContentType::Article {
            author: Some("Jane Doe".to_string()),
        }),
        Some("Jane Doe".to_string())
    );
    assert_eq!(creator_for(&ContentType::Article { author: None }), None);
    assert_eq!(creator_for(&ContentType::Social), None);
    assert_eq!(creator_for(&ContentType::Reddit), None);
    assert_eq!(creator_for(&ContentType::Note), None);
    // An empty/whitespace carried value resolves to None (never fabricate).
    assert_eq!(
        creator_for(&ContentType::YouTube {
            uploader: "   ".to_string(),
            duration_secs: 1.0,
        }),
        None
    );
}

#[test]
fn test_render_article_with_byline() {
    let note = NoteContent {
        title: "Bylined Post".to_string(),
        source_url: Some("https://blog.example.com/post".to_string()),
        tags: vec![],
        summary: "Body.".to_string(),
        content_type: ContentType::Article {
            author: Some("Jane Doe".to_string()),
        },
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(rendered.contains("creator: Jane Doe"));
}

#[test]
fn test_render_emits_exactly_one_creator_line() {
    // The historical double-write bug: a YouTube note with a non-empty
    // `default_creator` emitted `creator:` twice (the standalone
    // default-creator write AND the YouTube-arm write). With a single
    // `creator_for`-driven write, the uploader wins and only one line
    // is emitted.
    let config = FrontmatterConfig {
        default_tags: vec![],
        default_creator: "Scott".to_string(),
        timezone: "UTC".to_string(),
    };
    let note = NoteContent {
        title: "Cool Video".to_string(),
        source_url: Some("https://youtube.com/watch?v=abc".to_string()),
        tags: vec!["youtube".to_string()],
        summary: "Video summary.".to_string(),
        content_type: ContentType::YouTube {
            uploader: "TechChannel".to_string(),
            duration_secs: 600.0,
        },
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &config);
    assert_eq!(
        rendered.matches("creator:").count(),
        1,
        "exactly one creator: line expected:\n{rendered}"
    );
    // The per-kind author (uploader) wins over default_creator.
    assert!(rendered.contains("creator: TechChannel"));
    assert!(!rendered.contains("creator: Scott"));
}

#[test]
fn test_render_falls_back_to_default_creator_when_no_author() {
    // An Article with no byline falls back to default_creator.
    let config = FrontmatterConfig {
        default_tags: vec![],
        default_creator: "Scott".to_string(),
        timezone: "UTC".to_string(),
    };
    let note = NoteContent {
        title: "No byline".to_string(),
        source_url: Some("https://blog.example.com/x".to_string()),
        tags: vec![],
        summary: "Body.".to_string(),
        content_type: ContentType::Article { author: None },
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &config);
    assert_eq!(rendered.matches("creator:").count(), 1);
    // serde_yaml emits a simple scalar bare (no quotes needed).
    assert!(rendered.contains("creator: Scott"));
}

#[test]
fn yaml_scalar_round_trips_nasty_inputs() {
    // The old escape only handled `"`; a trailing `\`, embedded newline,
    // or colon corrupted the frontmatter. Every scalar must round-trip
    // back to the original string through serde_yaml.
    for input in [
        "He said \"hello\"",
        "trailing backslash\\",
        "embedded\nnewline",
        "colon: in value",
        "C:\\Windows\\path",
        "quote\"and\\backslash",
        "- leading dash",
        "#hash start",
        "",
    ] {
        let scalar = yaml_scalar(input);
        let doc = format!("k: {scalar}");
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&doc).unwrap_or_else(|e| panic!("invalid YAML for {input:?}: {e}\n{doc}"));
        let got = parsed.get("k").and_then(|v| v.as_str()).unwrap_or_default();
        assert_eq!(got, input, "round-trip mismatch for {input:?} -> {scalar:?}");
    }
}

#[test]
fn render_note_frontmatter_parses_with_nasty_title() {
    let config = FrontmatterConfig {
        default_tags: vec![],
        default_creator: String::new(),
        timezone: "UTC".to_string(),
    };
    let note = NoteContent {
        title: "Weird: title with \"quotes\" and a trailing backslash\\".to_string(),
        source_url: None,
        tags: vec!["test".to_string()],
        summary: "Body.".to_string(),
        content_type: ContentType::Note,
        ..NoteContent::default()
    };
    let rendered = render_note(&note, &config);
    let fm = rendered
        .strip_prefix("---\n")
        .and_then(|r| r.split("\n---").next())
        .expect("frontmatter block");
    let parsed: serde_yaml::Value =
        serde_yaml::from_str(fm).unwrap_or_else(|e| panic!("frontmatter did not parse: {e}\n{fm}"));
    assert_eq!(parsed.get("title").and_then(|v| v.as_str()), Some(note.title.as_str()));
}

#[test]
fn render_note_emits_trace_expires_from_frontmatter_additions() {
    // Phase 3: the pipeline injects `trace-expires` via frontmatter_additions;
    // render_note must splice it into the YAML alongside `trace`/`ingested`.
    let mut additions = BTreeMap::new();
    additions.insert(
        "trace-expires".to_string(),
        serde_yaml::Value::String("2026-08-19".to_string()),
    );
    let note = NoteContent {
        title: "T".to_string(),
        source_url: Some("https://example.com/a".to_string()),
        summary: "S.".to_string(),
        content_type: ContentType::Article { author: None },
        trace_id: Some("ht-95aa4e".to_string()),
        frontmatter_additions: additions,
        ..Default::default()
    };
    let rendered = render_note(&note, &test_config());
    assert!(
        rendered.contains("trace: ht-95aa4e"),
        "missing trace handle:\n{rendered}"
    );
    assert!(
        rendered.contains("trace-expires: 2026-08-19"),
        "missing trace-expires:\n{rendered}"
    );
    // And it round-trips back through the shared frontmatter parser as a named
    // field (the Phase-1 promotion), proving the stamp is consumable.
    let (fm, _) = vault::frontmatter::parse_frontmatter(&rendered).expect("parse");
    assert_eq!(fm.trace_expires.as_deref(), Some("2026-08-19"));
    assert_eq!(fm.trace.as_deref(), Some("ht-95aa4e"));
}
