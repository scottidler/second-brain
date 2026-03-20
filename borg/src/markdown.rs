use chrono::Utc;
use chrono_tz::Tz;

use crate::config::FrontmatterConfig;
use crate::types::IngestMethod;

pub struct NoteContent {
    pub title: String,
    pub source_url: Option<String>,
    pub asset_path: Option<String>,
    pub tags: Vec<String>,
    pub summary: String,
    pub content_type: ContentType,
    pub embed_code: Option<String>,
    pub method: Option<IngestMethod>,
    pub trace_id: Option<String>,
    pub domain: String,
}

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

    let mut fm = format!("---\ntitle: \"{}\"\ndate: {date}\n", escape_yaml_string(&note.title),);

    if let Some(source) = &note.source_url {
        fm.push_str(&format!("source: \"{source}\"\n"));
    }
    if let Some(asset) = &note.asset_path {
        fm.push_str(&format!("asset: \"{asset}\"\n"));
    }
    fm.push_str(&format!("type: {type_field}\n"));
    fm.push_str(&format!("domain: {}\n", note.domain));
    fm.push_str("origin: assisted\n");

    if let Some(method) = &note.method {
        fm.push_str(&format!("method: {method}\n"));
    }

    if let Some(ref tid) = note.trace_id {
        fm.push_str(&format!("trace: {tid}\n"));
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

    // Summary section
    if !note.summary.is_empty() {
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
    fn test_render_article_note() {
        let note = NoteContent {
            title: "Test Article".to_string(),
            source_url: Some("https://example.com/post".to_string()),
            asset_path: None,
            tags: vec!["rust".to_string(), "programming".to_string()],
            summary: "This is a summary.".to_string(),
            content_type: ContentType::Article,
            embed_code: None,
            method: None,
            trace_id: None,
            domain: "tech".to_string(),
        };
        let rendered = render_note(&note, &test_config());
        assert!(rendered.contains("title: \"Test Article\""));
        assert!(rendered.contains("type: article"));
        assert!(rendered.contains("domain: tech"));
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
            embed_code: Some(r#"<iframe width="854" height="480" src="https://www.youtube.com/embed/abc" frameborder="0" allowfullscreen></iframe>"#.to_string()),
            method: Some(IngestMethod::Telegram),
            trace_id: None,
            domain: "ai".to_string(),
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
            embed_code: None,
            method: None,
            trace_id: None,
            domain: "ai".to_string(),
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
            embed_code: None,
            method: Some(IngestMethod::Telegram),
            trace_id: None,
            domain: "inbox".to_string(),
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
            embed_code: None,
            method: Some(IngestMethod::Cli),
            trace_id: None,
            domain: "inbox".to_string(),
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
            content_type: ContentType::Article,
            embed_code: None,
            method: Some(IngestMethod::Telegram),
            trace_id: Some("tg-7f3a2c".to_string()),
            domain: "tech".to_string(),
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
            embed_code: None,
            method: None,
            trace_id: None,
            domain: "inbox".to_string(),
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
            embed_code: None,
            method: Some(IngestMethod::Telegram),
            trace_id: None,
            domain: "tech".to_string(),
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
            embed_code: None,
            method: Some(IngestMethod::Telegram),
            trace_id: None,
            domain: "ai".to_string(),
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
            embed_code: None,
            method: Some(IngestMethod::Telegram),
            trace_id: None,
            domain: "football".to_string(),
        };
        let rendered = render_note(&note, &test_config());
        assert!(rendered.contains("type: reddit"));
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
