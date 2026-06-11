use super::*;
use crate::testutil::{NoteBuilder, TestVault};

#[test]
fn test_daily_digest_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.intel;
    let llm_config = v.config().llm;
    let opts = IntelOpts {
        mode: IntelMode::Daily,
        output: None,
        as_of: None,
    };

    let fabric = FabricConfig::default();
    generate(v.root(), &notes, &config, &llm_config, &fabric, &opts).expect("generate");

    let today = Local::now().format("%Y-%m-%d").to_string();
    let digest_path = v.root().join("notes/ai/daily").join(format!("{today}.md"));
    assert!(digest_path.exists());
    let content = std::fs::read_to_string(&digest_path).expect("read");
    assert!(content.contains("Daily Digest"));
    // With no API key set, LLM will fail gracefully
    assert!(
        content.contains("LLM synthesis unavailable") || content.contains("No notes ingested"),
        "should have fallback message or empty day message"
    );
}

#[test]
fn test_weekly_review_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.intel;
    let llm_config = v.config().llm;
    let opts = IntelOpts {
        mode: IntelMode::Weekly,
        output: None,
        as_of: None,
    };

    let fabric = FabricConfig::default();
    generate(v.root(), &notes, &config, &llm_config, &fabric, &opts).expect("generate");

    let output_dir = v.root().join("notes/ai/weekly");
    assert!(output_dir.exists());
    let files: Vec<_> = std::fs::read_dir(&output_dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".md"))
        .collect();
    assert!(!files.is_empty());
}

#[test]
fn test_resolve_output_path_explicit() {
    let config = IntelConfig::default();
    let opts = IntelOpts {
        mode: IntelMode::Daily,
        output: Some(PathBuf::from("/custom/path.md")),
        as_of: None,
    };

    let path = resolve_output_path(Path::new("/vault"), &config, &opts, "daily.md");
    assert_eq!(path, PathBuf::from("/custom/path.md"));
}

#[test]
fn test_resolve_output_path_default() {
    let config = IntelConfig::default();
    let opts = IntelOpts {
        mode: IntelMode::Daily,
        output: None,
        as_of: None,
    };

    let path = resolve_output_path(Path::new("/vault"), &config, &opts, "daily/2026-03-16.md");
    assert_eq!(path, PathBuf::from("/vault/notes/ai/daily/2026-03-16.md"));
}

#[test]
fn test_build_daily_prompt_includes_wikilinks() {
    let note = NoteBuilder::new("cool-video.md")
        .title("Cool Video")
        .body("This is about cool stuff.")
        .build();
    let notes = vec![&note];
    let prompt = build_daily_prompt(&notes, 50000);
    assert!(prompt.contains("ref=\"cool-video\""));
    assert!(prompt.contains("title=\"Cool Video\""));
    assert!(prompt.contains("<note "));
    assert!(prompt.contains("This is about cool stuff."));
}

#[test]
fn test_strip_markdown_headers() {
    let body = "## Summary\nSome text.\n### Claims\n- a claim\n#notaheader stays\nplain line";
    let out = strip_markdown_headers(body);
    assert_eq!(
        out,
        "Summary\nSome text.\nClaims\n- a claim\n#notaheader stays\nplain line"
    );
    assert!(!out.contains("## "));
    assert!(!out.contains("### "));
}

#[test]
fn test_link_target_folder_qualifies_intel_notes() {
    let daily = NoteBuilder::new("notes/ai/daily/2026-05-18.md").build();
    let weekly = NoteBuilder::new("notes/ai/weekly/2026-05-18.md").build();
    let content = NoteBuilder::new("notes/ai/cool-video.md").build();
    assert_eq!(link_target(&daily), "notes/ai/daily/2026-05-18");
    assert_eq!(link_target(&weekly), "notes/ai/weekly/2026-05-18");
    assert_eq!(link_target(&content), "cool-video");
}

#[test]
fn test_build_note_callout_format() {
    let note1 = NoteBuilder::new("note-one.md").title("Note One").build();
    let note2 = NoteBuilder::new("note-two.md").title("Note Two").build();
    let notes = vec![&note1, &note2];
    let callout = build_note_callout(&notes);
    assert!(callout.starts_with("> [!notes]- Yesterday's Notes (2)"));
    assert!(callout.contains("> - [[note-one|Note One]]"));
    assert!(callout.contains("> - [[note-two|Note Two]]"));
}

#[test]
fn test_daily_digest_fallback_on_llm_failure() {
    // Use a bogus API key env var to force LLM failure
    let v = TestVault::new();
    let yesterday = Local::now().date_naive() - chrono::Duration::days(1);
    let yesterday_str = yesterday.format("%Y-%m-%d").to_string();
    v.add_note(
            "yesterday-note.md",
            &format!(
                "---\ntitle: Yesterday Note\ndate: {yesterday_str}\ntype: note\ndomain: tech\norigin: authored\ntags: [rust]\n---\nSome content from yesterday.\n"
            ),
        );
    let notes = v.scan();
    let config = v.config().actions.intel;
    // Use a nonexistent env var to guarantee LLM failure
    let llm_config = LlmConfig {
        api_key: "NONEXISTENT_TEST_KEY_99999".to_string(),
        ..Default::default()
    };
    let opts = IntelOpts {
        mode: IntelMode::Daily,
        output: None,
        as_of: None,
    };

    let fabric = FabricConfig::default();
    generate(v.root(), &notes, &config, &llm_config, &fabric, &opts).expect("generate");

    let today = Local::now().format("%Y-%m-%d").to_string();
    let digest_path = v.root().join("notes/ai/daily").join(format!("{today}.md"));
    let content = std::fs::read_to_string(&digest_path).expect("read");
    assert!(content.contains("LLM synthesis unavailable"), "should show fallback");
    assert!(content.contains("[!notes]-"), "should have collapsed callout");
    assert!(
        content.contains("[[yesterday-note|Yesterday Note]]"),
        "should list the note"
    );
}
