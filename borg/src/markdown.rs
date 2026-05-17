use chrono::Utc;
use chrono_tz::Tz;
use std::collections::BTreeMap;

use crate::config::FrontmatterConfig;
use crate::types::IngestMethod;

#[derive(Default)]
pub struct NoteContent {
    pub title: String,
    pub source_url: Option<String>,
    pub asset_path: Option<String>,
    pub tags: Vec<String>,
    pub summary: String,
    pub description: Option<String>,
    pub content_type: ContentType,
    pub embed_code: Option<String>,
    pub method: Option<IngestMethod>,
    pub trace_id: Option<String>,
    /// Vault-relative paths to slide JPEGs the note owns. Rendered into the
    /// `slides:` frontmatter list so cleanup on replay can find them.
    pub slides: Vec<String>,
    /// Post-Phase-6 cutover: pre-rendered structured body produced by
    /// `distillers::render`. When `Some`, replaces the legacy
    /// `## Summary\n\n{summary}` block - the rendered Distilled already
    /// carries `## Summary` / `## Claims` / `## Links` headings of its own.
    /// `None` for non-URL kinds (image, audio, vocab, idea) that still use
    /// the legacy prose-summary body.
    pub distilled_body: Option<String>,
    /// Additional frontmatter keys merged into the rendered YAML before the
    /// closing `---`. Populated by `distillers::render` with `distilled:
    /// true`, `distilled-extractor`, and per-kind `cortex-*` fields.
    pub frontmatter_additions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Default)]
pub enum ContentType {
    YouTube {
        uploader: String,
        duration_secs: f64,
    },
    Article,
    GitHub,
    Social,
    Reddit,
    Image {
        asset_path: String,
    },
    Pdf {
        asset_path: String,
    },
    Audio {
        asset_path: String,
        duration_secs: Option<f64>,
    },
    #[default]
    Note,
    VocabDefine {
        word: String,
        language: String,
    },
    VocabClarify {
        word_a: String,
        word_b: String,
        language: String,
    },
    Document {
        asset_path: String,
    },
    Code {
        language: String,
    },
}

pub fn render_note(note: &NoteContent, frontmatter_config: &FrontmatterConfig) -> String {
    let tz: Tz = frontmatter_config
        .timezone
        .parse()
        .unwrap_or(chrono_tz::America::Los_Angeles);
    let now = Utc::now().with_timezone(&tz);
    let date = now.format("%Y-%m-%d").to_string();

    let mut all_tags = frontmatter_config.default_tags.clone();
    all_tags.extend(note.tags.clone());
    // Deduplicate
    all_tags.sort();
    all_tags.dedup();

    let tags_yaml = all_tags
        .iter()
        .map(|t| format!("  - {t}"))
        .collect::<Vec<_>>()
        .join("\n");

    let type_field = match &note.content_type {
        ContentType::YouTube { .. } => "youtube",
        ContentType::Article => "article",
        ContentType::GitHub => "github",
        ContentType::Social => "social",
        ContentType::Reddit => "reddit",
        ContentType::Image { .. } => "image",
        ContentType::Pdf { .. } => "pdf",
        ContentType::Audio { .. } => "audio",
        ContentType::Note => "note",
        ContentType::VocabDefine { .. } | ContentType::VocabClarify { .. } => "vocab",
        ContentType::Document { .. } => "document",
        ContentType::Code { .. } => "code",
    };

    let mut fm = format!(
        "---\ntitle: \"{}\"\ndate: {date}\ningested: {date}\n",
        escape_yaml_string(&note.title),
    );

    if let Some(source) = &note.source_url {
        fm.push_str(&format!("source: \"{source}\"\n"));
    }
    if let Some(asset) = &note.asset_path {
        fm.push_str(&format!("asset: \"{asset}\"\n"));
    }
    fm.push_str(&format!("type: {type_field}\n"));
    fm.push_str("origin: assisted\n");

    if let Some(method) = &note.method {
        fm.push_str(&format!("method: {method}\n"));
    }

    if let Some(ref tid) = note.trace_id {
        fm.push_str(&format!("trace: {tid}\n"));
    }

    if !note.slides.is_empty() {
        fm.push_str("slides:\n");
        for s in &note.slides {
            fm.push_str(&format!("  - {s}\n"));
        }
    }

    fm.push_str(&format!("tags:\n{tags_yaml}\n"));

    if !frontmatter_config.default_creator.is_empty() {
        fm.push_str(&format!(
            "creator: \"{}\"\n",
            escape_yaml_string(&frontmatter_config.default_creator)
        ));
    }

    match &note.content_type {
        ContentType::YouTube {
            uploader,
            duration_secs,
        } => {
            let minutes = (*duration_secs / 60.0).round() as u32;
            fm.push_str(&format!(
                "creator: \"{}\"\nduration: {minutes}\n",
                escape_yaml_string(uploader)
            ));
        }
        ContentType::Audio {
            duration_secs: Some(secs),
            ..
        } => {
            let minutes = (*secs / 60.0).round() as u32;
            fm.push_str(&format!("duration: {minutes}\n"));
        }
        ContentType::Code { language } => {
            fm.push_str(&format!("language: \"{language}\"\n"));
        }
        _ => {}
    }

    // Post-Phase-6 cutover: merge any frontmatter additions produced by
    // `distillers::render` (distilled flag, extractor id, per-kind
    // `cortex-*` keys). Sorted alphabetically for stable diffs.
    for (key, value) in &note.frontmatter_additions {
        fm.push_str(&format!("{key}: {}\n", serialize_yaml_value(value)));
    }

    fm.push_str("---\n\n");

    // Heading
    let mut body = format!("# {}\n\n", note.title);

    // Embed code (YouTube iframe)
    if let Some(embed) = &note.embed_code {
        body.push_str(embed);
        body.push_str("\n\n");
    }

    // Asset embed for file-based content
    match &note.content_type {
        ContentType::Image { asset_path } | ContentType::Pdf { asset_path } | ContentType::Document { asset_path } => {
            if let Some(filename) = std::path::Path::new(asset_path).file_name().and_then(|f| f.to_str()) {
                body.push_str(&format!("![[{filename}]]\n\n"));
            }
        }
        _ => {}
    }

    // Description callout (YouTube only)
    if let Some(ref desc) = note.description {
        body.push_str("> [!info]- Video Description\n");
        for line in desc.lines() {
            if line.trim().is_empty() {
                body.push_str(">\n");
            } else {
                body.push_str(&format!("> {line}\n"));
            }
        }
        body.push('\n');
    }

    // Body: post-Phase-6 cutover prefers the pre-rendered structured body
    // produced by `distillers::render` (it already carries `## Summary` /
    // `## Claims` / `## Links` headings). The legacy `## Summary` wrapper
    // around `note.summary` is the fallback for non-URL kinds and for URL
    // kinds whose distillation produced no body (extreme fallback, never
    // expected in steady state because `fallback_distilled` always emits
    // a summary).
    if let Some(rendered) = &note.distilled_body
        && !rendered.trim().is_empty()
    {
        body.push_str(rendered);
        if !rendered.ends_with('\n') {
            body.push('\n');
        }
        if !rendered.ends_with("\n\n") {
            body.push('\n');
        }
    } else if !note.summary.is_empty() {
        body.push_str("## Summary\n\n");
        body.push_str(&note.summary);
        body.push_str("\n\n");
    }

    // Source footer
    if let Some(source) = &note.source_url {
        body.push_str(&format!("---\n\n*Source: [{source}]({source})*\n"));
    }

    format!("{fm}{body}")
}

fn escape_yaml_string(s: &str) -> String {
    s.replace('"', "\\\"")
}

/// Serialize a single `serde_yaml::Value` for inline insertion into the
/// hand-built frontmatter string. Scalars render bare; everything else
/// goes through `serde_yaml::to_string` and is reformatted to fit a
/// single key entry without disturbing the surrounding hand-built YAML.
fn serialize_yaml_value(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::String(s) => format!("\"{}\"", escape_yaml_string(s)),
        serde_yaml::Value::Null => "null".to_string(),
        // Sequences and mappings: serialize, drop the leading newline that
        // `serde_yaml::to_string` emits for non-scalar values, and indent
        // each subsequent line with two spaces so the YAML stays valid
        // under the `key:` prefix.
        other => {
            let raw = serde_yaml::to_string(other).unwrap_or_default();
            let trimmed = raw.trim_end();
            let mut out = String::from("\n");
            for line in trimmed.lines() {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
            out.pop();
            out
        }
    }
}

pub fn sanitize_filename(title: &str) -> String {
    crate::hygiene::sanitize_filename(title)
}

#[cfg(test)]
mod tests {
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
            content_type: ContentType::Article,
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
        assert!(rendered.contains("distilled-extractor: \"distill-article-v1\""));
        assert!(rendered.contains("cortex-thread-platform: \"x\""));
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
            content_type: ContentType::Article,
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
            content_type: ContentType::Article,
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
            content_type: ContentType::Article,
            description: None,
            embed_code: None,
            method: None,
            trace_id: None,
            slides: Vec::new(),
            ..NoteContent::default()
        };
        let rendered = render_note(&note, &test_config());
        assert!(rendered.contains("title: \"Test Article\""));
        assert!(rendered.contains("type: article"));
        assert!(rendered.contains("origin: assisted"));
        assert!(rendered.contains("  - rust"));
        assert!(rendered.contains("## Summary"));
        assert!(rendered.contains("This is a summary."));
        assert!(rendered.contains("Source: [https://example.com/post]"));
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
        assert!(rendered.contains("creator: \"TechChannel\""));
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
            content_type: ContentType::Article,
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
        assert!(rendered.contains("creator: \"Scott\""));
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
            content_type: ContentType::Article,
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
            content_type: ContentType::GitHub,
            description: None,
            embed_code: None,
            method: Some(IngestMethod::Telegram),
            trace_id: None,
            slides: Vec::new(),
            ..NoteContent::default()
        };
        let rendered = render_note(&note, &test_config());
        assert!(rendered.contains("type: github"));
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
            content_type: ContentType::Article,
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
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("Hello World!"), "hello-world");
        assert_eq!(sanitize_filename("Test: A/B \"quotes\""), "test-a-b-quotes");
        assert_eq!(sanitize_filename("normal-file_name"), "normal-file-name");
    }

    #[test]
    fn test_escape_yaml_string() {
        assert_eq!(escape_yaml_string("He said \"hello\""), "He said \\\"hello\\\"");
    }
}
