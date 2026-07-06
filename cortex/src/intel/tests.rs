use super::*;
use crate::testutil::{NoteBuilder, TestVault};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Fake `IntelLlm` that counts `complete` calls and returns a fixed, non-empty
/// synthesis. Lets the idempotency test assert that an unchanged-input second
/// run makes ZERO LLM calls (the core Phase 2 contract).
struct CountingLlm {
    calls: AtomicUsize,
    reply: String,
}

impl CountingLlm {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            reply: "## Themes\nSynthetic reply.\n\n## Highlights\n- x\n\n## Breadcrumbs\n- y".to_string(),
        }
    }

    fn count(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl IntelLlm for CountingLlm {
    fn complete(
        &self,
        _system: &str,
        _user: &str,
        _model: &str,
        _max_tokens: u32,
        _timeout_secs: u64,
        _api_key: &str,
    ) -> Result<String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(self.reply.clone())
    }
}

/// Design doc `2026-07-05-cortex-daemon-oscillation-loop.md`, Phase 2 success
/// criterion (a): a second `generate` on UNCHANGED inputs makes ZERO LLM calls
/// and writes ZERO files. The input-side idempotency key (hash of input notes +
/// model + prompt) is persisted as `intel-input-hash` frontmatter and read back
/// before the LLM call; when it matches, generation is skipped entirely.
#[test]
fn daily_digest_second_run_on_unchanged_inputs_makes_zero_llm_calls_and_zero_writes() {
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
    let llm_config = v.config().llm;
    let fabric = FabricConfig::default();
    let opts = IntelOpts {
        mode: IntelMode::Daily,
        output: None,
        as_of: None,
    };
    let llm = CountingLlm::new();

    // First run: notes present -> exactly one LLM call, digest written.
    let report = generate(v.root(), &notes, &config, &llm_config, &fabric, &opts, &llm).expect("generate 1");
    assert_eq!(llm.count(), 1, "first run must make exactly one LLM call");
    let digest_path = report.output_path.clone();
    assert!(digest_path.exists(), "first run must write the digest");
    let after_1 = std::fs::read_to_string(&digest_path).expect("read after run 1");
    assert!(
        after_1.contains(&format!("{INTEL_INPUT_HASH_KEY}:")),
        "digest must persist the input hash: {after_1}"
    );
    assert!(
        !after_1.contains("tags:"),
        "digest must emit NO tags (digest is a NoteType, not a tag): {after_1}"
    );
    let mtime_1 = std::fs::metadata(&digest_path)
        .expect("meta 1")
        .modified()
        .expect("mtime 1");

    // Ensure any rewrite would be observable as a distinct mtime.
    std::thread::sleep(std::time::Duration::from_millis(20));

    // Second run: identical inputs -> ZERO additional LLM calls, ZERO writes.
    generate(v.root(), &notes, &config, &llm_config, &fabric, &opts, &llm).expect("generate 2");
    assert_eq!(
        llm.count(),
        1,
        "second run on unchanged inputs must make ZERO additional LLM calls"
    );
    let mtime_2 = std::fs::metadata(&digest_path)
        .expect("meta 2")
        .modified()
        .expect("mtime 2");
    assert_eq!(
        mtime_1, mtime_2,
        "second run on unchanged inputs must NOT rewrite the digest file"
    );
}

/// Complements criterion (a): when the input note set changes, the digest DOES
/// regenerate (a new LLM call fires). Proves the idempotency key is
/// input-sensitive, not a blanket skip.
#[test]
fn daily_digest_regenerates_when_inputs_change() {
    let v = TestVault::new();
    let yesterday = Local::now().date_naive() - chrono::Duration::days(1);
    let yesterday_str = yesterday.format("%Y-%m-%d").to_string();
    v.add_note(
        "yesterday-note.md",
        &format!(
            "---\ntitle: Yesterday Note\ndate: {yesterday_str}\ntype: note\ndomain: tech\norigin: authored\ntags: [rust]\n---\nSome content from yesterday.\n"
        ),
    );
    let config = v.config().actions.intel;
    let llm_config = v.config().llm;
    let fabric = FabricConfig::default();
    let opts = IntelOpts {
        mode: IntelMode::Daily,
        output: None,
        as_of: None,
    };
    let llm = CountingLlm::new();

    let notes1 = v.scan();
    generate(v.root(), &notes1, &config, &llm_config, &fabric, &opts, &llm).expect("generate 1");
    assert_eq!(llm.count(), 1);

    // Change the input set: add another note dated yesterday.
    v.add_note(
        "another-yesterday-note.md",
        &format!(
            "---\ntitle: Another Note\ndate: {yesterday_str}\ntype: note\ndomain: tech\norigin: authored\ntags: [python]\n---\nDifferent content.\n"
        ),
    );
    let notes2 = v.scan();
    generate(v.root(), &notes2, &config, &llm_config, &fabric, &opts, &llm).expect("generate 2");
    assert_eq!(
        llm.count(),
        2,
        "changed inputs must trigger regeneration (a fresh LLM call)"
    );
}

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
    generate(v.root(), &notes, &config, &llm_config, &fabric, &opts, &AnthropicLlm).expect("generate");

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
    generate(v.root(), &notes, &config, &llm_config, &fabric, &opts, &AnthropicLlm).expect("generate");

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
    generate(v.root(), &notes, &config, &llm_config, &fabric, &opts, &AnthropicLlm).expect("generate");

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
