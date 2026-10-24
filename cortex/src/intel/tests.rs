use super::*;
use crate::testutil::{NoteBuilder, TestVault};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Fake `IntelLlm` that counts `complete` calls and returns a fixed, non-empty
/// synthesis. Lets the idempotency test assert that an unchanged-input second
/// run makes ZERO LLM calls (the core idempotency contract).
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

/// Design doc `2026-07-05-cortex-daemon-oscillation-loop.md` success
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
            "---\ntitle: Yesterday Note\ndate: {yesterday_str}\ntype: note\norigin: authored\ntags: [rust]\n---\nSome content from yesterday.\n"
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
    let report = generate(
        v.root(),
        &notes,
        &config,
        &llm_config,
        &ShellFabric(&fabric),
        &opts,
        &llm,
        &crate::shutdown::StopHandle::never(),
    )
    .expect("generate 1");
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
    generate(
        v.root(),
        &notes,
        &config,
        &llm_config,
        &ShellFabric(&fabric),
        &opts,
        &llm,
        &crate::shutdown::StopHandle::never(),
    )
    .expect("generate 2");
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
            "---\ntitle: Yesterday Note\ndate: {yesterday_str}\ntype: note\norigin: authored\ntags: [rust]\n---\nSome content from yesterday.\n"
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
    generate(
        v.root(),
        &notes1,
        &config,
        &llm_config,
        &ShellFabric(&fabric),
        &opts,
        &llm,
        &crate::shutdown::StopHandle::never(),
    )
    .expect("generate 1");
    assert_eq!(llm.count(), 1);

    // Change the input set: add another note dated yesterday.
    v.add_note(
        "another-yesterday-note.md",
        &format!(
            "---\ntitle: Another Note\ndate: {yesterday_str}\ntype: note\norigin: authored\ntags: [python]\n---\nDifferent content.\n"
        ),
    );
    let notes2 = v.scan();
    generate(
        v.root(),
        &notes2,
        &config,
        &llm_config,
        &ShellFabric(&fabric),
        &opts,
        &llm,
        &crate::shutdown::StopHandle::never(),
    )
    .expect("generate 2");
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
    generate(
        v.root(),
        &notes,
        &config,
        &llm_config,
        &ShellFabric(&fabric),
        &opts,
        &AnthropicLlm,
        &crate::shutdown::StopHandle::never(),
    )
    .expect("generate");

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
    generate(
        v.root(),
        &notes,
        &config,
        &llm_config,
        &ShellFabric(&fabric),
        &opts,
        &AnthropicLlm,
        &crate::shutdown::StopHandle::never(),
    )
    .expect("generate");

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
                "---\ntitle: Yesterday Note\ndate: {yesterday_str}\ntype: note\norigin: authored\ntags: [rust]\n---\nSome content from yesterday.\n"
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
    generate(
        v.root(),
        &notes,
        &config,
        &llm_config,
        &ShellFabric(&fabric),
        &opts,
        &AnthropicLlm,
        &crate::shutdown::StopHandle::never(),
    )
    .expect("generate");

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

// ---- stop check point before the Fabric fallback ----

/// An LLM double that fails every call, setting the stop flag first when
/// `stop` is given: the call the stop interrupted.
struct FailingLlm {
    stop: Option<crate::shutdown::StopHandle>,
}

impl IntelLlm for FailingLlm {
    fn complete(&self, _: &str, _: &str, _: &str, _: u32, _: u64, _: &str) -> Result<String> {
        if let Some(stop) = &self.stop {
            stop.stop();
        }
        Err(eyre::eyre!("stub LLM failure"))
    }
}

/// A Fabric double that is always available and counts its calls.
struct CountingFabric {
    calls: AtomicUsize,
}

impl IntelFabric for CountingFabric {
    fn is_available(&self) -> bool {
        true
    }

    fn run_pattern(&self, _pattern: &str, _input: &str, _timeout_secs: u64) -> Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("fabric wisdom".to_string())
    }
}

fn weekly_fixture() -> (TestVault, Vec<Note>, IntelOpts) {
    let v = TestVault::new();
    v.add_note(
        "this-week.md",
        "---\ntitle: This Week\ndate: 2026-01-06\ntype: note\norigin: authored\ntags: [rust]\n---\nA note from this week.\n",
    );
    let notes = v.scan();
    let opts = IntelOpts {
        mode: IntelMode::Weekly,
        output: None,
        as_of: chrono::NaiveDate::from_ymd_opt(2026, 1, 7),
    };
    (v, notes, opts)
}

/// The vault's intel config with the weekly Fabric fallback configured, so
/// only the stop can skip it.
fn with_weekly_fallback(v: &TestVault) -> IntelConfig {
    IntelConfig {
        batch_weekly: Some(crate::config::DEFAULT_WEEKLY_PATTERN.to_string()),
        ..v.config().actions.intel
    }
}

/// LLM fails after the flag is set: the Fabric fallback is never called and
/// the review is not written.
#[test]
fn weekly_llm_failure_after_the_stop_skips_the_fabric_fallback_and_writes_nothing() {
    let (v, notes, opts) = weekly_fixture();
    let config = with_weekly_fallback(&v);
    let stop = crate::shutdown::StopHandle::never();
    let llm = FailingLlm {
        stop: Some(stop.clone()),
    };
    let fabric = CountingFabric {
        calls: AtomicUsize::new(0),
    };

    let report = generate(v.root(), &notes, &config, &v.config().llm, &fabric, &opts, &llm, &stop).expect("generate");

    assert_eq!(fabric.calls.load(Ordering::SeqCst), 0, "no Fabric call after the stop");
    assert!(report.stopped);
    assert!(!report.output_path.exists(), "a stopped review writes nothing");
}

/// Without a stop the same LLM failure falls back to Fabric, as before.
#[test]
fn weekly_llm_failure_without_a_stop_falls_back_to_fabric() {
    let (v, notes, opts) = weekly_fixture();
    let config = with_weekly_fallback(&v);
    let llm = FailingLlm { stop: None };
    let fabric = CountingFabric {
        calls: AtomicUsize::new(0),
    };

    let report = generate(
        v.root(),
        &notes,
        &config,
        &v.config().llm,
        &fabric,
        &opts,
        &llm,
        &crate::shutdown::StopHandle::never(),
    )
    .expect("generate");

    assert_eq!(fabric.calls.load(Ordering::SeqCst), 1);
    assert!(!report.stopped);
    let content = std::fs::read_to_string(&report.output_path).expect("review written");
    assert!(content.contains("fabric wisdom"), "{content}");
}

/// The model's daily reply keeps a wikilink only when its target is a `ref`
/// the prompt supplied: a truncated ref and an invented one are unwrapped to
/// plain text instead of landing in the vault as broken links.
#[test]
fn daily_digest_unlinks_model_refs_that_name_no_supplied_note() {
    let v = TestVault::new();
    let yesterday = Local::now().date_naive() - chrono::Duration::days(1);
    v.add_note(
        "i-spent-200-hours-teaching-ai-writing-here-are-6-principles.md",
        &format!(
            "---\ntitle: I Spent 200 Hours\ndate: {}\ntype: note\norigin: authored\ntags: [ai]\n---\nWriting.\n",
            yesterday.format("%Y-%m-%d")
        ),
    );
    let notes = v.scan();
    let llm = CountingLlm {
        calls: AtomicUsize::new(0),
        reply: "## Themes\nWriting.\n\n## Highlights\n\
                - [[i-spent-200-hours-teaching-ai-writing-here-are-6-principles|I Spent 200 Hours]] - kept\n\
                - [[i-spent-200-hours-teaching-ai-writing|truncated ref]] - unwrapped\n\n\
                ## Breadcrumbs\n- What links [[invented-note]] to it?"
            .to_string(),
    };
    let opts = IntelOpts {
        mode: IntelMode::Daily,
        output: None,
        as_of: None,
    };

    let report = generate(
        v.root(),
        &notes,
        &v.config().actions.intel,
        &v.config().llm,
        &ShellFabric(&FabricConfig::default()),
        &opts,
        &llm,
        &crate::shutdown::StopHandle::never(),
    )
    .expect("generate");

    let digest = std::fs::read_to_string(&report.output_path).expect("digest written");
    assert!(
        digest.contains("- [[i-spent-200-hours-teaching-ai-writing-here-are-6-principles|I Spent 200 Hours]] - kept")
    );
    assert!(digest.contains("- truncated ref - unwrapped"), "{digest}");
    assert!(digest.contains("What links invented-note to it?"), "{digest}");
    assert!(!digest.contains("[[i-spent-200-hours-teaching-ai-writing|"), "{digest}");
    assert!(!digest.contains("[[invented-note]]"), "{digest}");
}

/// Same rule for the weekly review's insights, whose prompt supplies no refs
/// at all: a guessed stem that names a week note survives, a wrong one does not.
#[test]
fn weekly_review_unlinks_model_refs_that_name_no_week_note() {
    let (v, notes, opts) = weekly_fixture();
    let llm = CountingLlm {
        calls: AtomicUsize::new(0),
        reply: "You read [[this-week|one note]] and guessed at [[this-week-summary]].".to_string(),
    };

    let report = generate(
        v.root(),
        &notes,
        &v.config().actions.intel,
        &v.config().llm,
        &ShellFabric(&FabricConfig::default()),
        &opts,
        &llm,
        &crate::shutdown::StopHandle::never(),
    )
    .expect("generate");

    let review = std::fs::read_to_string(&report.output_path).expect("review written");
    assert!(
        review.contains("You read [[this-week|one note]] and guessed at this-week-summary."),
        "{review}"
    );
}

/// A fallback digest must not record its input hash, or the unchanged-input
/// skip keeps the fallback for the rest of the day (2026-10 digests).
#[test]
fn a_fallback_daily_digest_is_retried_on_the_next_run() {
    let v = TestVault::new();
    let yesterday = Local::now().date_naive() - chrono::Duration::days(1);
    let yesterday_str = yesterday.format("%Y-%m-%d").to_string();
    v.add_note(
        "yesterday-note.md",
        &format!(
            "---\ntitle: Yesterday Note\ndate: {yesterday_str}\ntype: note\norigin: authored\ntags: [rust]\n---\nSome content from yesterday.\n"
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
    let stop = crate::shutdown::StopHandle::never();

    generate(
        v.root(),
        &notes,
        &config,
        &llm_config,
        &ShellFabric(&fabric),
        &opts,
        &FailingLlm { stop: None },
        &stop,
    )
    .expect("fallback run");
    let fallback = std::fs::read_to_string(
        v.root()
            .join("notes/ai/daily")
            .join(format!("{}.md", Local::now().format("%Y-%m-%d"))),
    )
    .expect("read fallback");
    assert!(fallback.contains("LLM synthesis unavailable"));
    assert!(
        !fallback.contains(INTEL_INPUT_HASH_KEY),
        "fallback must not record the input hash"
    );

    let llm = CountingLlm::new();
    generate(
        v.root(),
        &notes,
        &config,
        &llm_config,
        &ShellFabric(&fabric),
        &opts,
        &llm,
        &stop,
    )
    .expect("retry run");
    assert_eq!(llm.count(), 1, "same inputs after a fallback must call the LLM again");
    let retried = std::fs::read_to_string(
        v.root()
            .join("notes/ai/daily")
            .join(format!("{}.md", Local::now().format("%Y-%m-%d"))),
    )
    .expect("read retried");
    assert!(retried.contains("Synthetic reply."));
    assert!(retried.contains(INTEL_INPUT_HASH_KEY));
}
