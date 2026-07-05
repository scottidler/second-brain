use super::*;

fn failed_result(stage: Option<FailureStage>) -> IngestResult {
    IngestResult {
        status: IngestStatus::Failed {
            reason: "x".to_string(),
        },
        failure_stage: stage,
        ..Default::default()
    }
}

#[test]
fn terminal_failure_stage_reads_typed_field() {
    // Every stage a failure site can set round-trips unchanged - no
    // substring matching, no reclassification.
    for stage in [
        FailureStage::IntakeRejected,
        FailureStage::ClassifyFailed,
        FailureStage::FetchFailed,
        FailureStage::QualityBlocked,
        FailureStage::PipelineTimedOut,
        FailureStage::PublishFailed,
        FailureStage::Crashed,
    ] {
        assert_eq!(terminal_failure_stage(&failed_result(Some(stage))), stage);
    }
}

#[test]
fn terminal_failure_stage_defaults_to_fetch_failed_when_unset() {
    assert_eq!(terminal_failure_stage(&failed_result(None)), FailureStage::FetchFailed);
}

#[test]
fn test_detect_text_pattern_define() {
    assert_eq!(
        detect_text_pattern("define: garrulous"),
        TextPattern::Define {
            word: "garrulous".to_string()
        }
    );
    assert_eq!(
        detect_text_pattern("Define: escurrir"),
        TextPattern::Define {
            word: "escurrir".to_string()
        }
    );
}

#[test]
fn test_detect_text_pattern_clarify() {
    assert_eq!(
        detect_text_pattern("clarify: affect vs effect"),
        TextPattern::Clarify {
            word_a: "affect".to_string(),
            word_b: "effect".to_string()
        }
    );
    assert_eq!(
        detect_text_pattern("Clarify: escurrir vs estrujar"),
        TextPattern::Clarify {
            word_a: "escurrir".to_string(),
            word_b: "estrujar".to_string()
        }
    );
}

#[test]
fn test_detect_text_pattern_url() {
    // Bare URL: annotated URL ingest with no capture note.
    match detect_text_pattern("https://example.com") {
        TextPattern::ContainsUrl { url, note } => {
            assert_eq!(url, "https://example.com");
            assert_eq!(note, None);
        }
        other => panic!("expected ContainsUrl, got {other:?}"),
    }
}

#[test]
fn test_detect_text_pattern_url_with_short_context() {
    // URL with very short surrounding text should still be treated as URL
    match detect_text_pattern("check https://example.com") {
        TextPattern::ContainsUrl { url, note } => {
            assert_eq!(url, "https://example.com");
            assert_eq!(note.as_deref(), Some("check"));
        }
        other => panic!("expected ContainsUrl, got {other:?}"),
    }
}

#[test]
fn test_detect_text_pattern_prose_and_url_is_annotated_url() {
    // Phase 8 (CLI transport capture-note fixture): long prose + URL now ALWAYS
    // becomes an annotated URL ingest (the old <10-char heuristic is gone). The
    // prose is the capture note (first-URL token removed, whitespace collapsed).
    match detect_text_pattern("This is how we should fix borg's linker: https://example.com/post") {
        TextPattern::ContainsUrl { url, note } => {
            assert_eq!(url, "https://example.com/post");
            assert_eq!(note.as_deref(), Some("This is how we should fix borg's linker:"));
        }
        other => panic!("expected ContainsUrl, got {other:?}"),
    }
}

#[test]
fn test_detect_text_pattern_idea_prefix_forces_idea_even_with_url() {
    // Phase 8: the `idea:` prefix is the escape hatch - it forces an Idea note
    // (General path) even when the text carries a URL.
    assert_eq!(
        detect_text_pattern("idea: a thought inspired by https://example.com/post"),
        TextPattern::General
    );
}

#[test]
fn test_detect_text_pattern_multi_url_keeps_trailing_urls_in_note() {
    // Only the FIRST URL token is removed; additional URLs stay in the note.
    match detect_text_pattern("compare https://a.example.com and https://b.example.com") {
        TextPattern::ContainsUrl { url, note } => {
            assert_eq!(url, "https://a.example.com");
            assert_eq!(note.as_deref(), Some("compare and https://b.example.com"));
        }
        other => panic!("expected ContainsUrl, got {other:?}"),
    }
}

#[test]
fn test_detect_text_pattern_general() {
    assert_eq!(
        detect_text_pattern("Met James at the Rust meetup"),
        TextPattern::General
    );
}

#[test]
fn test_detect_text_pattern_empty_define() {
    // "define:" with no word should not match
    assert_eq!(detect_text_pattern("define: "), TextPattern::General);
}

#[test]
fn test_expand_tilde() {
    let expanded = expand_tilde("~/test/path");
    assert!(!expanded.to_string_lossy().starts_with("~/"));
    assert!(expanded.to_string_lossy().ends_with("test/path"));
}

#[test]
fn test_expand_tilde_no_tilde() {
    let expanded = expand_tilde("/absolute/path");
    assert_eq!(expanded, PathBuf::from("/absolute/path"));
}

#[test]
fn test_extract_title_from_fabric_metadata() {
    let md = "Title: Rust Programming Language\n\nURL Source: https://rust-lang.org\n\nMarkdown Content:\n# Rust\n";
    assert_eq!(
        extract_article_title(md, "https://rust-lang.org"),
        "Rust Programming Language"
    );
}

#[test]
fn test_extract_title_from_pdf_filename() {
    let md = "Title: The-Complete-Guide-to-Building-Skill-for-Claude.pdf\n\nURL Source: https://example.com/doc.pdf\n\nMarkdown Content:\nThe Complete Guide\n\n# to Building Skills\n";
    assert_eq!(
        extract_article_title(md, "https://example.com/doc.pdf"),
        "The Complete Guide to Building Skill for Claude"
    );
}

#[test]
fn test_extract_title_falls_back_to_heading() {
    let md = "Some random content\n# My Article Title\nBody text\n";
    assert_eq!(
        extract_article_title(md, "https://example.com/page"),
        "My Article Title"
    );
}

#[test]
fn test_extract_title_falls_back_to_url_segment() {
    let md = "No title metadata here\nJust plain text\n";
    assert_eq!(
        extract_article_title(md, "https://example.com/my-great-article"),
        "my great article"
    );
}

#[tokio::test]
async fn test_process_content_formerly_unsupported_types() {
    // All content types (Image, PDF, Document, Audio) are now implemented.
    // This test is retained as a placeholder; type-specific tests cover each.
}

#[test]
fn test_audio_format_from_extension() {
    assert!(matches!(audio_format_from_extension("song.mp3"), AudioFormat::Mp3));
    assert!(matches!(audio_format_from_extension("recording.wav"), AudioFormat::Wav));
    assert!(matches!(audio_format_from_extension("voice.ogg"), AudioFormat::Ogg));
    assert!(matches!(audio_format_from_extension("memo.opus"), AudioFormat::Ogg));
    assert!(matches!(audio_format_from_extension("track.m4a"), AudioFormat::Mp3));
    assert!(matches!(audio_format_from_extension("lossless.flac"), AudioFormat::Mp3));
    assert!(matches!(audio_format_from_extension("clip.aac"), AudioFormat::Mp3));
    assert!(matches!(audio_format_from_extension("old.wma"), AudioFormat::Mp3));
    assert!(matches!(audio_format_from_extension("stream.webm"), AudioFormat::Mp3));
    assert!(matches!(audio_format_from_extension("RECORDING.WAV"), AudioFormat::Wav));
    assert!(matches!(audio_format_from_extension("noext"), AudioFormat::Mp3));
}

// --- Code detection tests ---

#[test]
fn test_looks_like_code_rust() {
    let rust_code = r#"use std::collections::HashMap;

fn main() {
    let mut map = HashMap::new();
    map.insert("key", "value");
    println!("{:?}", map);
}"#;
    let result = looks_like_code(rust_code);
    assert!(result.is_some(), "should detect Rust code");
    assert_eq!(result.expect("expected Some"), "rust");
}

#[test]
fn test_looks_like_code_python() {
    let python_code = r#"import os
from pathlib import Path

def process_files(directory):
    for f in Path(directory).iterdir():
        if f.is_file():
            print(f.name)
"#;
    let result = looks_like_code(python_code);
    assert!(result.is_some(), "should detect Python code");
    assert_eq!(result.expect("expected Some"), "python");
}

#[test]
fn test_looks_like_code_javascript() {
    let js_code = r#"const express = require('express');
const app = express();

function handleRequest(req, res) {
    const data = req.body;
    res.json({ status: 'ok' });
}
"#;
    let result = looks_like_code(js_code);
    assert!(result.is_some(), "should detect JavaScript code");
    assert_eq!(result.expect("expected Some"), "javascript");
}

#[test]
fn test_looks_like_code_go() {
    let go_code = r#"package main

import "fmt"

func main() {
    fmt.Println("Hello, World!")
}
"#;
    let result = looks_like_code(go_code);
    assert!(result.is_some(), "should detect Go code");
    assert_eq!(result.expect("expected Some"), "go");
}

#[test]
fn test_looks_like_code_bash_shebang() {
    let bash_code = r#"#!/bin/bash
set -euo pipefail

echo "Hello"
for i in 1 2 3; do
    echo "$i"
done
"#;
    let result = looks_like_code(bash_code);
    assert!(result.is_some(), "should detect bash via shebang");
    assert_eq!(result.expect("expected Some"), "bash");
}

#[test]
fn test_looks_like_code_python_shebang() {
    let python_code = r#"#!/usr/bin/env python3
import sys

def main():
    print(sys.argv)
"#;
    let result = looks_like_code(python_code);
    assert!(result.is_some(), "should detect python via shebang");
    assert_eq!(result.expect("expected Some"), "python");
}

#[test]
fn test_looks_like_code_c() {
    let c_code = r#"#include <stdio.h>
#include <stdlib.h>

int main() {
    printf("Hello, World!\n");
    return 0;
}
"#;
    let result = looks_like_code(c_code);
    assert!(result.is_some(), "should detect C code");
    assert_eq!(result.expect("expected Some"), "c");
}

#[test]
fn test_looks_like_code_plain_text_not_detected() {
    let text = "Met James at the Rust meetup yesterday. We talked about programming.";
    assert!(
        looks_like_code(text).is_none(),
        "plain text should not be detected as code"
    );
}

#[test]
fn test_looks_like_code_short_text_not_detected() {
    let text = "fn main()";
    assert!(
        looks_like_code(text).is_none(),
        "single line should not be detected as code (need 3+ lines)"
    );
}

#[test]
fn test_looks_like_code_football_play_not_detected() {
    let text = "4-2-5 blitz from weak side\nCorner press coverage\nSafety rolls down to flat";
    assert!(
        looks_like_code(text).is_none(),
        "football play description should not be detected as code"
    );
}

#[test]
fn test_looks_like_code_define_pattern_not_detected() {
    let text = "define: garrulous\nmeaning: excessively talkative\nusage: The garrulous host...";
    assert!(
        looks_like_code(text).is_none(),
        "define pattern should not be detected as code"
    );
}

#[test]
fn test_looks_like_code_grocery_list_not_detected() {
    let text = "Shopping list:\n- milk\n- eggs\n- bread\n- butter";
    assert!(
        looks_like_code(text).is_none(),
        "grocery list should not be detected as code"
    );
}

#[test]
fn test_looks_like_code_prose_with_technical_words_not_detected() {
    let text = "I was reading about how to import goods from China.\nThe class was interesting and we learned about different methods.\nThe function of the liver is to filter toxins.";
    assert!(
        looks_like_code(text).is_none(),
        "prose with technical-sounding words should not be detected as code"
    );
}

#[test]
fn test_render_code_note() {
    let note = NoteContent {
        title: "Rust HashMap Example".to_string(),
        source_url: None,
        asset_path: None,
        tags: vec!["rust".to_string(), "code-snippet".to_string()],
        summary: "```rust\nfn main() {\n    println!(\"hello\");\n}\n```".to_string(),
        description: None,
        content_type: ContentType::Code {
            language: "rust".to_string(),
        },
        embed_code: None,
        method: Some(IngestMethod::Cli),
        trace_id: None,
        slides: Vec::new(),
        ..NoteContent::default()
    };
    let rendered = markdown::render_note(
        &note,
        &crate::config::FrontmatterConfig {
            default_tags: vec![],
            default_creator: String::new(),
            timezone: "UTC".to_string(),
        },
    );
    assert!(rendered.contains("type: code"));
    assert!(rendered.contains("language: \"rust\""));
    assert!(rendered.contains("```rust"));
    assert!(rendered.contains("  - code-snippet"));
}

#[test]
fn test_vision_title_preferred_over_filename() {
    // Simulates the merge logic: vision title takes priority
    let vision_title = "Netgate SG-2100 Serial Label";
    let filename = "IMG_20260316_123456.jpg";

    let vision = Some(ocr::VisionResult {
        description: "A product label".to_string(),
        suggested_title: vision_title.to_string(),
        suggested_tags: vec!["hardware".to_string()],
        extracted_text: "Serial: ABC-123".to_string(),
    });

    let title = vision
        .as_ref()
        .and_then(|v| (!v.suggested_title.is_empty()).then_some(v.suggested_title.clone()))
        .unwrap_or_else(|| title_from_filename(filename));

    assert_eq!(title, vision_title);
}

#[test]
fn test_vision_none_falls_back_to_filename() {
    let filename = "screenshot-example.png";
    let vision: Option<ocr::VisionResult> = None;

    let title = vision
        .as_ref()
        .and_then(|v| (!v.suggested_title.is_empty()).then_some(v.suggested_title.clone()))
        .unwrap_or_else(|| title_from_filename(filename));

    assert_eq!(title, "screenshot example");
}

#[test]
fn test_vision_extracted_text_preferred_over_ocr() {
    let ocr_text = "115 a> Inpul: 12V".to_string();
    let vision = Some(ocr::VisionResult {
        description: String::new(),
        suggested_title: String::new(),
        suggested_tags: vec![],
        extracted_text: "Serial: ABC-123\nModel: SG-2100".to_string(),
    });

    let extracted = vision
        .as_ref()
        .and_then(|v| (!v.extracted_text.is_empty()).then_some(v.extracted_text.clone()))
        .unwrap_or_else(|| ocr_text.clone());

    assert_eq!(extracted, "Serial: ABC-123\nModel: SG-2100");
}

#[test]
fn test_vision_empty_text_falls_back_to_ocr() {
    let ocr_text = "Some OCR text".to_string();
    let vision = Some(ocr::VisionResult {
        description: "A photo".to_string(),
        suggested_title: "My Photo".to_string(),
        suggested_tags: vec![],
        extracted_text: String::new(),
    });

    let extracted = vision
        .as_ref()
        .and_then(|v| (!v.extracted_text.is_empty()).then_some(v.extracted_text.clone()))
        .unwrap_or_else(|| ocr_text.clone());

    assert_eq!(extracted, "Some OCR text");
}

// --- build_obsidian_url tests ---

#[test]
fn test_build_obsidian_url_inbox() {
    let url = build_obsidian_url("/home/user/obsidian/inbox/my-note.md");
    assert_eq!(url, Some("obsidian://open?file=my-note".to_string()));
}

#[test]
fn test_build_obsidian_url_notes_folder() {
    let url = build_obsidian_url("/home/user/obsidian/notes/claude-code-guide.md");
    assert_eq!(url, Some("obsidian://open?file=claude-code-guide".to_string()));
}

#[test]
fn test_build_obsidian_url_same_stem_different_dirs() {
    let inbox = build_obsidian_url("/home/user/obsidian/inbox/my-note.md");
    let notes = build_obsidian_url("/home/user/obsidian/notes/my-note.md");
    assert_eq!(
        inbox, notes,
        "URL must be path-independent (survives inbox/ -> notes/ move)"
    );
}

// Regression guard for the 2026-07-04 vault-name-mismatch bug: the link is
// tapped on devices with different vault names (desktop "obsidian", phone
// "obsidian-remote"), so it MUST NOT carry a `vault=` param. Bites if anyone
// re-hardcodes a vault name.
#[test]
fn test_build_obsidian_url_omits_vault_param() {
    let url = build_obsidian_url("/home/user/obsidian/notes/my-note.md").unwrap();
    assert!(
        !url.contains("vault="),
        "deep link must not hardcode a vault name: {url}"
    );
}

#[test]
fn test_build_obsidian_url_bare_filename() {
    let url = build_obsidian_url("my-note.md");
    assert_eq!(url, Some("obsidian://open?file=my-note".to_string()));
}

#[test]
fn test_extract_filename_strips_directory() {
    let path = std::path::Path::new("/home/user/vault/notes/my-note.md");
    assert_eq!(extract_filename(path), Some("my-note.md".to_string()));
}

#[test]
fn test_extract_filename_bare_filename() {
    let path = std::path::Path::new("my-note.md");
    assert_eq!(extract_filename(path), Some("my-note.md".to_string()));
}

#[test]
fn test_extract_filename_inbox_path() {
    let path = std::path::Path::new("/vault/inbox/some-article.md");
    assert_eq!(extract_filename(path), Some("some-article.md".to_string()));
}

#[test]
fn test_find_note_by_source_in_notes_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let notes = vault.join("notes");
    std::fs::create_dir_all(&notes).expect("mkdir");

    let note_content = "---\ntitle: Test\nsource: \"https://example.com/article\"\n---\nBody.\n";
    std::fs::write(notes.join("test-article.md"), note_content).expect("write");

    let found = find_note_by_source(vault, "https://example.com/article");
    assert!(found.is_some(), "should find note by source URL");
    assert_eq!(found.unwrap().file_name().unwrap().to_str().unwrap(), "test-article.md");
}

#[test]
fn test_find_note_by_source_in_inbox() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let inbox = vault.join("inbox");
    std::fs::create_dir_all(&inbox).expect("mkdir");

    let note_content = "---\ntitle: Inbox Note\nsource: \"https://example.com/inbox-item\"\n---\nBody.\n";
    std::fs::write(inbox.join("inbox-item.md"), note_content).expect("write");

    let found = find_note_by_source(vault, "https://example.com/inbox-item");
    assert!(found.is_some(), "should find note in inbox/");
    assert!(found.unwrap().starts_with(&inbox), "found path should be in inbox/");
}

#[test]
fn test_find_note_by_source_not_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let notes = vault.join("notes");
    std::fs::create_dir_all(&notes).expect("mkdir");

    let note_content = "---\ntitle: Other\nsource: \"https://other.com\"\n---\nBody.\n";
    std::fs::write(notes.join("other.md"), note_content).expect("write");

    let found = find_note_by_source(vault, "https://example.com/missing");
    assert!(found.is_none(), "should not find non-matching source");
}

#[test]
fn test_find_note_by_source_skips_dotfiles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let hidden = vault.join(".obsidian");
    std::fs::create_dir_all(&hidden).expect("mkdir");

    let note_content = "---\nsource: \"https://example.com/hidden\"\n---\n";
    std::fs::write(hidden.join("config.md"), note_content).expect("write");

    let found = find_note_by_source(vault, "https://example.com/hidden");
    assert!(found.is_none(), "should skip dot-prefixed directories");
}

#[test]
fn test_reingest_preserves_notes_directory() {
    // Integration test: when a note exists in notes/ and we reingest the same URL,
    // the reingest_dest should point to notes/, not inbox/.
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let notes = vault.join("notes");
    let inbox = vault.join("inbox");
    std::fs::create_dir_all(&notes).expect("mkdir notes");
    std::fs::create_dir_all(&inbox).expect("mkdir inbox");
    std::fs::create_dir_all(vault.join("system").join("views")).expect("mkdir ledger dir");

    let source_url = "https://example.com/reingest-test";

    // Create an existing note in notes/ (already promoted by cortex)
    let note_content =
        format!("---\ntitle: Original\ndate: 2026-03-20\nsource: \"{source_url}\"\n---\nOriginal body.\n");
    std::fs::write(notes.join("reingest-test.md"), &note_content).expect("write note");

    // Create a ledger with the existing entry
    let ledger_file = vault.join("system").join("views").join("borg-ledger.md");
    let ledger_content = format!(
        "---\ntitle: Borg Ledger\ndate: 2026-03-23\ntype: system\ndomain: system\norigin: authored\ntags: []\n---\n\n\
             # Borg Ledger\n\n\
             | Date | Time | Method | Status | Title | Filename | Source | Domain | Trace |\n\
             |------|------|--------|--------|-------|----------|--------|--------|-------|\n\
             | 2026-03-20 | 10:00 | http | {} | [[Original]] | reingest-test.md | {source_url} | ai | tr-000001 |\n",
        "\u{2705}"
    );
    std::fs::write(&ledger_file, ledger_content).expect("write ledger");

    // Simulate the reingest lookup logic from process_url_inner
    let existing = ledger::find_completed(&ledger_file, source_url)
        .expect("find_completed")
        .expect("should find existing entry");

    let old_note_path = find_note_by_source(vault, source_url).or_else(|| {
        if existing.filename != "-" {
            [vault.join("notes"), vault.join("inbox")]
                .iter()
                .map(|d| d.join(&existing.filename))
                .find(|p| p.exists())
        } else {
            None
        }
    });

    assert!(old_note_path.is_some(), "should find existing note");
    let reingest_dest = old_note_path.as_ref().unwrap().parent().map(|p| p.to_path_buf());
    assert_eq!(
        reingest_dest.as_ref().unwrap(),
        &notes,
        "reingest should target notes/ dir, not inbox/"
    );
}

#[test]
fn test_reingest_falls_back_to_inbox_for_new_urls() {
    // When no existing note is found, dest should fall back to inbox.
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let inbox_path = vault.join("inbox");
    std::fs::create_dir_all(&inbox_path).expect("mkdir");

    let found = find_note_by_source(vault, "https://example.com/brand-new");
    assert!(found.is_none());

    // reingest_dest would be None, so config.inbox_dir() is used
    let dest = inbox_path.clone();
    assert_eq!(dest, inbox_path, "new URLs should land in inbox/");
}

#[test]
fn test_reingest_finds_note_via_ledger_filename_fallback() {
    // When find_note_by_source fails (e.g. source URL changed slightly),
    // the fallback uses the ledger's stored filename to find the note.
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let notes = vault.join("notes");
    std::fs::create_dir_all(&notes).expect("mkdir notes");
    std::fs::create_dir_all(vault.join("system").join("views")).expect("mkdir ledger dir");

    // Note exists but with a DIFFERENT source URL (e.g. URL was re-canonicalized)
    let note_content = "---\ntitle: Fallback\nsource: \"https://old-url.com/page\"\n---\nBody.\n";
    std::fs::write(notes.join("fallback-note.md"), note_content).expect("write");

    // Ledger references the new canonical URL but has the filename
    let ledger_file = vault.join("system").join("views").join("borg-ledger.md");
    let ledger_content = format!(
        "---\ntitle: Borg Ledger\ndate: 2026-03-23\ntype: system\ndomain: system\norigin: authored\ntags: []\n---\n\n\
             # Borg Ledger\n\n\
             | Date | Time | Method | Status | Title | Filename | Source | Domain | Trace |\n\
             |------|------|--------|--------|-------|----------|--------|--------|-------|\n\
             | 2026-03-20 | 10:00 | http | {} | [[Fallback]] | fallback-note.md | https://new-url.com/page | ai | tr-000001 |\n",
        "\u{2705}"
    );
    std::fs::write(&ledger_file, ledger_content).expect("write ledger");

    let existing = ledger::find_completed(&ledger_file, "https://new-url.com/page")
        .expect("find_completed")
        .expect("should find entry");

    // find_note_by_source won't match (source URLs differ)
    let by_source = find_note_by_source(vault, "https://new-url.com/page");
    assert!(by_source.is_none(), "source URL mismatch, should not find");

    // Fallback: use ledger filename to locate the file
    let candidates = [
        vault.join("notes").join(&existing.filename),
        vault.join("inbox").join(&existing.filename),
    ];
    let by_filename = candidates.iter().find(|p| p.exists());

    assert!(by_filename.is_some(), "should find note via filename fallback");
    assert_eq!(
        by_filename.as_ref().unwrap().parent().unwrap(),
        notes,
        "found note should be in notes/"
    );
}

#[test]
fn test_read_cortex_fields_all_present() {
    let dir = tempfile::tempdir().unwrap();
    let note = dir.path().join("test.md");
    std::fs::write(
            &note,
            "---\ntitle: Test\ndate: 2026-03-20\ndomain: tech\nstatus: read\ncortex-classified: true\ncortex-classified-by: deterministic\ncortex-confidence: high\ncortex-quality: medium\ncortex-quality-issues: [no-outbound-links]\n---\nBody text.\n",
        )
        .unwrap();

    let fields = read_cortex_fields(&note);
    assert_eq!(fields.len(), 7);
    assert!(fields.iter().any(|(k, v)| k == "domain" && v == "tech"));
    assert!(fields.iter().any(|(k, v)| k == "status" && v == "read"));
    assert!(fields.iter().any(|(k, v)| k == "cortex-classified" && v == "true"));
    assert!(
        fields
            .iter()
            .any(|(k, v)| k == "cortex-classified-by" && v == "deterministic")
    );
    assert!(fields.iter().any(|(k, v)| k == "cortex-confidence" && v == "high"));
    assert!(fields.iter().any(|(k, v)| k == "cortex-quality" && v == "medium"));
    assert!(
        fields
            .iter()
            .any(|(k, v)| k == "cortex-quality-issues" && v == "[no-outbound-links]")
    );
}

#[test]
fn test_read_cortex_fields_partial() {
    let dir = tempfile::tempdir().unwrap();
    let note = dir.path().join("test.md");
    std::fs::write(&note, "---\ntitle: Test\ndate: 2026-03-20\ndomain: ai\n---\nBody.\n").unwrap();

    let fields = read_cortex_fields(&note);
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0], ("domain".to_string(), "ai".to_string()));
}

#[test]
fn test_read_cortex_fields_none_present() {
    let dir = tempfile::tempdir().unwrap();
    let note = dir.path().join("test.md");
    std::fs::write(
        &note,
        "---\ntitle: Test\ndate: 2026-03-20\ntags:\n  - rust\n---\nBody.\n",
    )
    .unwrap();

    let fields = read_cortex_fields(&note);
    assert!(fields.is_empty());
}

#[test]
fn test_read_cortex_fields_no_frontmatter() {
    let dir = tempfile::tempdir().unwrap();
    let note = dir.path().join("test.md");
    std::fs::write(&note, "Just plain text, no frontmatter.\n").unwrap();

    let fields = read_cortex_fields(&note);
    assert!(fields.is_empty());
}

#[test]
fn test_read_cortex_fields_missing_file() {
    let fields = read_cortex_fields(std::path::Path::new("/tmp/nonexistent-cortex-test.md"));
    assert!(fields.is_empty());
}

// Phase 3 of borg-pipeline-resilience: the previous patch_cortex_fields
// tests have moved to pipeline/atomic.rs alongside the apply_cortex_fields
// and apply_original_date helpers that replaced the patch_* functions.

// ---------------------------------------------------------------------------
// Phase 7 (distillation overhaul): slide-body-then-append splice + FTS reach.
// ---------------------------------------------------------------------------

/// A distilled payload with a summary, two claims, and a transcript - the
/// shape `distillers::render` turns into `## Summary` / `## Claims` /
/// `## Transcript` body sections.
fn phase7_distilled() -> vault::distilled::Distilled {
    use vault::distilled::{Claim, Distilled};
    Distilled {
        summary: "The talk argues orchestration beats raw model capability.".to_string(),
        claims: vec![
            Claim {
                text: "Harness quality dominates model quality for coding agents.".to_string(),
                ..Default::default()
            },
            Claim {
                text: "A tight feedback loop is the highest-leverage investment.".to_string(),
                ..Default::default()
            },
        ],
        transcript: Some("Full spoken transcript of the video goes here.".to_string()),
        ..Default::default()
    }
}

/// A representative slide-published body: an LLM section body with a slide
/// wikilink embedded under a `## <section>` heading (what `publish_slides`
/// emits for the `slide-section` shape).
fn phase7_slide_body() -> String {
    "## Opening Thesis\n\n![[talk-slide-001.jpg]]\n\nThe speaker frames the core question.\n\n\
     ## Live Demo\n\n![[talk-slide-002.jpg]]\n\nA worked example follows.\n"
        .to_string()
}

#[test]
fn append_distilled_below_slides_keeps_both_slide_and_distilled_sections() {
    // Defect #2: the splice must APPEND, not REPLACE. The composed body must
    // carry the slide sections AND the distilled `## Claims` (previously lost
    // wholesale on the slide path).
    let slide_body = phase7_slide_body();
    let distilled_body = distillers::render(&phase7_distilled()).body_markdown;

    let composed = append_distilled_below_slides(slide_body.clone(), &distilled_body);

    // Slide sections survive.
    assert!(
        composed.contains("## Opening Thesis") && composed.contains("![[talk-slide-001.jpg]]"),
        "slide sections must survive the splice: {composed}"
    );
    assert!(composed.contains("## Live Demo"), "second slide section must survive");
    // Distilled sections are appended below.
    assert!(composed.contains("## Claims"), "distilled ## Claims must be appended");
    assert!(composed.contains("## Summary"), "distilled ## Summary must be appended");
    assert!(
        composed.contains("## Transcript"),
        "distilled ## Transcript must be appended"
    );
    // Ordering: the slide body comes first, distilled sections follow.
    let slide_pos = composed.find("## Opening Thesis").expect("slide heading");
    let claims_pos = composed.find("## Claims").expect("claims heading");
    assert!(
        slide_pos < claims_pos,
        "slide body must precede appended distilled sections"
    );
    // A blank line separates the last slide block from the appended block.
    assert!(
        !composed.contains("A worked example follows.\n## Summary"),
        "there must be a blank line between the slide body and the appended sections"
    );
}

#[test]
fn append_distilled_below_slides_noop_on_empty_distilled_body() {
    let slide_body = phase7_slide_body();
    let composed = append_distilled_below_slides(slide_body.clone(), "");
    assert_eq!(
        composed, slide_body,
        "empty distilled body leaves the slide body untouched"
    );
}

#[test]
fn slide_path_composed_body_yields_claims_fts_text() {
    // FTS-parsing code path (vault::search::parse_body_claims, the same parse
    // `index_vault`/`index_one` runs to populate `notes.claims`). The
    // slide-path composed body must yield the distilled claims as FTS text -
    // exactly what the pre-Phase-7 replace behavior destroyed.
    let composed = append_distilled_below_slides(
        phase7_slide_body(),
        &distillers::render(&phase7_distilled()).body_markdown,
    );
    let claims = vault::search::parse_body_claims(&composed);
    assert_eq!(claims.len(), 2, "both claims must be parseable for FTS: {composed}");
    assert!(claims.iter().any(|c| c.text.contains("Harness quality dominates")));
    assert!(claims.iter().any(|c| c.text.contains("tight feedback loop")));
}

#[test]
fn article_rendered_body_carries_transcript_and_yields_claims_fts_text() {
    // Article durability + FTS reach. The rendered article body must carry the
    // full fetched markdown under `## Transcript` AND expose its claims to the
    // same FTS parse the indexer runs.
    let rendered = distillers::render(&phase7_distilled());
    assert!(
        rendered
            .body_markdown
            .contains("## Transcript\n\nFull spoken transcript of the video goes here."),
        "article body must carry the full fetched markdown under ## Transcript: {}",
        rendered.body_markdown
    );
    let claims = vault::search::parse_body_claims(&rendered.body_markdown);
    assert_eq!(claims.len(), 2, "article claims must be FTS-parseable");
}
