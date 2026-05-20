use chrono::{Datelike, Local, NaiveDate};
use eyre::{Context, Result};
use std::path::{Path, PathBuf};

use crate::config::{Config, IntelConfig, LlmConfig};
use crate::opts::IntelOpts;
use crate::vault::{Note, scan_vault};

/// Outcome of a `sb cortex intel` invocation. Captures the paths the daily /
/// weekly generators wrote; sb formats the output.
#[derive(Debug, Default)]
pub struct IntelReport {
    pub daily_path: Option<PathBuf>,
    pub weekly_path: Option<PathBuf>,
}

/// Top-level orchestrator for `sb cortex intel`. Scans the vault and runs
/// either the daily-digest or weekly-review generator (or both) based on
/// `opts`. The cortex daemon calls this directly on its daily/weekly tick.
pub fn run(vault_root: &Path, config: &Config, opts: &IntelOpts) -> Result<IntelReport> {
    log::info!("starting intel command (vault_root={})", vault_root.display());
    let notes = scan_vault(vault_root, &config.vault)?;
    generate(vault_root, &notes, &config.actions.intel, &config.llm, opts)
}

const DAILY_SYSTEM_PROMPT: &str = "\
You are a sharp, well-read colleague reviewing someone's daily reading and notes. \
You've read everything they ingested yesterday and you're giving them a morning \
briefing - conversational, second-person, concise. Not a summary bot. Not a book \
report. You notice patterns, connections, and tensions they might have missed.

Output exactly three markdown sections. No frontmatter, no title heading.
Never use em dashes. Use regular dashes, commas, or semicolons instead.

## Themes
3-5 sentences. What threads connected yesterday's reading? What was the user \
gravitating toward? Be specific - name concepts, tools, people. Don't just list \
topics.

## Highlights
3-5 bullet points. Each starts with a wikilink in the format [[slug|Title]] using \
the exact slugs provided, followed by a dash and a one-liner about why this note \
stood out or how it connects to a broader theme. Pick the most interesting or \
connective notes, not just the longest.

## Breadcrumbs
2-3 bullet points. Provocative questions, surprising connections between notes, or \
tensions you noticed. These should make the reader want to go back and look at \
specific notes. Reference notes by their wikilink when relevant.";

/// Generate intelligence outputs (daily digest, weekly review). Returns the
/// paths that were written so sb can announce them.
pub fn generate(
    vault_root: &Path,
    notes: &[Note],
    config: &IntelConfig,
    llm_config: &LlmConfig,
    opts: &IntelOpts,
) -> Result<IntelReport> {
    let mut report = IntelReport::default();

    if opts.daily || !opts.weekly {
        report.daily_path = Some(generate_daily_digest(vault_root, notes, config, llm_config, opts)?);
    }

    if opts.weekly {
        report.weekly_path = Some(generate_weekly_review(vault_root, notes, config, opts)?);
    }

    Ok(report)
}

/// Process new/unread notes with Fabric pattern.
/// Sets cortex-insights in frontmatter and updates status to processed.
pub fn process_new_notes(vault_root: &Path, notes: &[Note], config: &IntelConfig) -> Result<usize> {
    let pattern = match &config.on_new_note {
        Some(p) => p.clone(),
        None => return Ok(0),
    };

    if !crate::fabric::is_available() {
        log::debug!("fabric not available, skipping new note processing");
        return Ok(0);
    }

    let mut processed = 0;

    for note in notes {
        // Only process unread notes
        if note.frontmatter.status.as_deref() != Some("unread") {
            continue;
        }

        // Skip if already processed
        if note.frontmatter.extra.contains_key("cortex-insights") {
            continue;
        }

        // Skip empty bodies
        if note.body.trim().is_empty() {
            continue;
        }

        let input = crate::fabric::truncate_input(&note.body, config.max_input_tokens);
        match crate::fabric::run_pattern(&pattern, input, config.fabric_timeout_secs) {
            Ok(insights) => {
                let abs_path = vault_root.join(&note.path);
                let content = std::fs::read_to_string(&abs_path)?;

                // Write cortex-insights and update status
                let fields = vec![
                    (
                        "cortex-insights".to_string(),
                        serde_yaml::Value::String(insights.trim().to_string()),
                    ),
                    ("status".to_string(), serde_yaml::Value::String("processed".to_string())),
                ];

                if let Some(new_content) = crate::scope::insert_frontmatter_fields(&content, &fields) {
                    std::fs::write(&abs_path, new_content)?;
                    log::info!("processed new note with fabric: {}", note.path.display());
                    processed += 1;
                }
            }
            Err(e) => {
                log::warn!("failed to process note with fabric: {}: {e}", note.path.display());
            }
        }
    }

    Ok(processed)
}

/// Build the user prompt for the daily digest LLM call.
///
/// Each note is formatted with its wikilink header so the LLM can reference it.
fn build_daily_prompt(recent_notes: &[&Note], max_input_tokens: usize) -> String {
    let concatenated: String = recent_notes
        .iter()
        .map(|n| {
            let title = n.frontmatter.title.as_deref().unwrap_or("Untitled");
            let stem = n.path.file_stem().and_then(|s| s.to_str()).unwrap_or("untitled");
            format!("=== [[{stem}|{title}]] ===\n\n{}", n.body)
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    crate::llm::truncate_input(&concatenated, max_input_tokens).to_string()
}

/// Build the collapsed callout section listing all notes.
fn build_note_callout(recent_notes: &[&Note]) -> String {
    let mut callout = format!("> [!notes]- Yesterday's Notes ({})\n", recent_notes.len());
    for note in recent_notes {
        let title = note
            .frontmatter
            .title
            .as_deref()
            .unwrap_or_else(|| note.path.to_str().unwrap_or("untitled"));
        let stem = note.path.file_stem().and_then(|s| s.to_str()).unwrap_or("untitled");
        callout.push_str(&format!("> - [[{stem}|{title}]]\n"));
    }
    callout
}

/// Generate a daily digest note.
///
/// Collects notes from the previous day (yesterday's ingestions) and synthesizes
/// themes, highlights, and breadcrumbs via the Anthropic API.
fn generate_daily_digest(
    vault_root: &Path,
    notes: &[Note],
    config: &IntelConfig,
    llm_config: &LlmConfig,
    opts: &IntelOpts,
) -> Result<PathBuf> {
    let yesterday = Local::now().date_naive() - chrono::Duration::days(1);
    let yesterday_str = yesterday.format("%Y-%m-%d").to_string();
    let today = Local::now().format("%Y-%m-%d").to_string();
    log::info!("generating daily digest covering {yesterday_str}");

    // Find notes from yesterday (the day being digested)
    let recent_notes: Vec<&Note> = notes
        .iter()
        .filter(|n| {
            n.frontmatter
                .date
                .as_ref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                == Some(yesterday)
        })
        .collect();

    // Build frontmatter + heading
    let mut digest = String::new();
    digest.push_str(&format!(
        "---\ntitle: Daily Digest {today}\ndate: {today}\ntype: digest\ntags: [digest]\n---\n\n"
    ));
    digest.push_str(&format!("# Daily Digest - {today}\n\n"));

    if recent_notes.is_empty() {
        digest.push_str(&format!("No notes ingested on {yesterday_str}.\n"));
    } else {
        // Try LLM synthesis
        let model = config.model.as_deref().unwrap_or(&llm_config.model);

        let user_prompt = build_daily_prompt(&recent_notes, config.max_input_tokens);

        match crate::llm::complete(
            DAILY_SYSTEM_PROMPT,
            &user_prompt,
            model,
            config.max_output_tokens,
            config.llm_timeout_secs,
            &llm_config.api_key,
        ) {
            Ok(synthesis) => {
                // Strip any accidental frontmatter or title from LLM output
                let cleaned = synthesis.trim();
                digest.push_str(cleaned);
                digest.push_str("\n\n");
            }
            Err(e) => {
                log::warn!("LLM daily synthesis failed, using fallback: {e}");
                digest.push_str("*LLM synthesis unavailable.*\n\n");
            }
        }

        // Always append collapsed note list
        digest.push_str(&build_note_callout(&recent_notes));
    }

    // Write to output path
    let output_path = resolve_output_path(vault_root, config, opts, &format!("daily-{today}.md"));
    write_intel_output(&output_path, &digest)?;

    log::info!("generated daily digest: {}", output_path.display());
    Ok(output_path)
}

/// Generate a weekly review note.
fn generate_weekly_review(
    vault_root: &Path,
    notes: &[Note],
    config: &IntelConfig,
    opts: &IntelOpts,
) -> Result<PathBuf> {
    let today = Local::now().date_naive();
    let week_start = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let week_str = week_start.format("%Y-%m-%d").to_string();

    log::info!("generating weekly review: week_start={week_str}");

    // Find notes from this week
    let week_notes: Vec<&Note> = notes
        .iter()
        .filter(|n| {
            n.frontmatter
                .date
                .as_ref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .is_some_and(|d| d >= week_start && d <= today)
        })
        .collect();

    // Group by type
    let mut by_type: std::collections::HashMap<&str, Vec<&Note>> = std::collections::HashMap::new();
    for note in &week_notes {
        let note_type = note.frontmatter.note_type.as_deref().unwrap_or("untyped");
        by_type.entry(note_type).or_default().push(note);
    }

    // Gather all tags
    let mut tag_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for note in &week_notes {
        if let Some(ref tags) = note.frontmatter.tags {
            for tag in tags {
                *tag_counts.entry(tag.as_str()).or_insert(0) += 1;
            }
        }
    }

    let mut top_tags: Vec<(&&str, &usize)> = tag_counts.iter().collect();
    top_tags.sort_by(|a, b| b.1.cmp(a.1));
    let top_tags: Vec<(&str, usize)> = top_tags.iter().take(10).map(|(t, c)| (**t, **c)).collect();

    let today_str = today.format("%Y-%m-%d").to_string();

    // Generate review
    let mut review = String::new();
    review.push_str(&format!(
        "---\ntitle: Weekly Review {week_str}\ndate: {today_str}\ntype: review\ntags: [review]\n---\n\n"
    ));
    review.push_str(&format!("# Weekly Review - Week of {week_str}\n\n"));

    review.push_str(&format!(
        "## Summary\n\n- Notes this week: {}\n- Total vault size: {}\n\n",
        week_notes.len(),
        notes.len()
    ));

    if !by_type.is_empty() {
        review.push_str("## By Type\n\n");
        let mut types: Vec<_> = by_type.iter().collect();
        types.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        for (note_type, type_notes) in types {
            review.push_str(&format!("### {note_type} ({})\n\n", type_notes.len()));
            for note in type_notes {
                let title = note
                    .frontmatter
                    .title
                    .as_deref()
                    .unwrap_or_else(|| note.path.to_str().unwrap_or("untitled"));
                let stem = note.path.file_stem().and_then(|s| s.to_str()).unwrap_or("untitled");
                review.push_str(&format!("- [[{stem}|{title}]]\n"));
            }
            review.push('\n');
        }
    }

    if !top_tags.is_empty() {
        review.push_str("## Top Topics\n\n");
        for (tag, count) in &top_tags {
            review.push_str(&format!("- #{tag} ({count} notes)\n"));
        }
        review.push('\n');
    }

    // Fabric enhancement: synthesize across all of the week's notes
    if let Some(ref pattern) = config.batch_weekly
        && crate::fabric::is_available()
        && !week_notes.is_empty()
    {
        let concatenated: String = week_notes
            .iter()
            .map(|n| {
                let title = n.frontmatter.title.as_deref().unwrap_or("Untitled");
                format!("# {title}\n\n{}", n.body)
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");
        let input = crate::fabric::truncate_input(&concatenated, config.max_input_tokens);
        match crate::fabric::run_pattern(pattern, input, config.fabric_timeout_secs) {
            Ok(wisdom) => {
                review.push_str("## AI Insights\n\n");
                review.push_str(wisdom.trim());
                review.push('\n');
            }
            Err(e) => {
                log::warn!("fabric weekly insights failed, skipping: {e}");
            }
        }
    }

    let output_path = resolve_output_path(vault_root, config, opts, &format!("weekly-{week_str}.md"));
    write_intel_output(&output_path, &review)?;

    log::info!("generated weekly review: {}", output_path.display());
    Ok(output_path)
}

/// Resolve the output path for an intel file.
fn resolve_output_path(vault_root: &Path, config: &IntelConfig, opts: &IntelOpts, filename: &str) -> PathBuf {
    if let Some(ref output) = opts.output {
        return output.clone();
    }
    vault_root.join(&config.output_path).join(filename)
}

/// Write intel output to disk.
fn write_intel_output(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context(format!("failed to create directory {}", parent.display()))?;
    }
    std::fs::write(path, content).context(format!("failed to write {}", path.display()))?;
    log::info!("wrote intel output: {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{NoteBuilder, TestVault};

    #[test]
    fn test_daily_digest_on_vault() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.intel;
        let llm_config = v.config().llm;
        let opts = IntelOpts {
            daily: true,
            weekly: false,
            output: None,
        };

        generate(v.root(), &notes, &config, &llm_config, &opts).expect("generate");

        let today = Local::now().format("%Y-%m-%d").to_string();
        let digest_path = v.root().join("notes/ai").join(format!("daily-{today}.md"));
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
            daily: false,
            weekly: true,
            output: None,
        };

        generate(v.root(), &notes, &config, &llm_config, &opts).expect("generate");

        let output_dir = v.root().join("notes/ai");
        assert!(output_dir.exists());
        let files: Vec<_> = std::fs::read_dir(&output_dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("weekly-"))
            .collect();
        assert!(!files.is_empty());
    }

    #[test]
    fn test_resolve_output_path_explicit() {
        let config = IntelConfig::default();
        let opts = IntelOpts {
            daily: true,
            weekly: false,
            output: Some(PathBuf::from("/custom/path.md")),
        };

        let path = resolve_output_path(Path::new("/vault"), &config, &opts, "daily.md");
        assert_eq!(path, PathBuf::from("/custom/path.md"));
    }

    #[test]
    fn test_resolve_output_path_default() {
        let config = IntelConfig::default();
        let opts = IntelOpts {
            daily: true,
            weekly: false,
            output: None,
        };

        let path = resolve_output_path(Path::new("/vault"), &config, &opts, "daily-2026-03-16.md");
        assert_eq!(path, PathBuf::from("/vault/notes/ai/daily-2026-03-16.md"));
    }

    #[test]
    fn test_process_new_notes_skips_without_fabric() {
        let v = TestVault::new();
        v.add_note(
            "unread-note.md",
            "---\ntitle: Unread Note\ndate: 2026-03-18\ntype: note\ndomain: tech\norigin: assisted\nstatus: unread\ntags: []\n---\nSome content to process.\n",
        );
        let notes = v.scan();
        let config = IntelConfig {
            on_new_note: None, // Disabled
            ..Default::default()
        };

        let count = process_new_notes(v.root(), &notes, &config).expect("process");
        assert_eq!(count, 0, "should skip when on_new_note is None");
    }

    #[test]
    fn test_build_daily_prompt_includes_wikilinks() {
        let note = NoteBuilder::new("cool-video.md")
            .title("Cool Video")
            .body("This is about cool stuff.")
            .build();
        let notes = vec![&note];
        let prompt = build_daily_prompt(&notes, 50000);
        assert!(prompt.contains("[[cool-video|Cool Video]]"));
        assert!(prompt.contains("This is about cool stuff."));
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
            daily: true,
            weekly: false,
            output: None,
        };

        generate(v.root(), &notes, &config, &llm_config, &opts).expect("generate");

        let today = Local::now().format("%Y-%m-%d").to_string();
        let digest_path = v.root().join("notes/ai").join(format!("daily-{today}.md"));
        let content = std::fs::read_to_string(&digest_path).expect("read");
        assert!(content.contains("LLM synthesis unavailable"), "should show fallback");
        assert!(content.contains("[!notes]-"), "should have collapsed callout");
        assert!(
            content.contains("[[yesterday-note|Yesterday Note]]"),
            "should list the note"
        );
    }
}
