//! Tier-1 classifier: one sonnet fabric call per session.
//!
//! Inputs: a `ParsedSession` from `jsonl::parse_session_file` and a
//! `Config`. Outputs: a `SessionRecord` ready for upsert into the
//! `sessions` table, OR a quarantine reason.

use chrono::Utc;
use eyre::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::GleanError;
use crate::jsonl::{ContentBlock, ParsedSession, Role, Turn};
use crate::repo;
use crate::types::{SessionRecord, quarantine_reason};

const TOOL_RESULT_PLACEHOLDER_FMT: &str = "<tool-result: {lines} lines, {tool}>";
const MIN_SUBSTANTIVE_TURNS: usize = 2;

/// Tier-1 classification verdict.
#[derive(Debug)]
pub enum ClassifyOutcome {
    /// Session was classified successfully.
    Ok(Box<SessionRecord>),
    /// Session was quarantined; reason is one of `quarantine_reason::*`
    /// or a free-form string.
    Quarantined { reason: String },
}

/// LLM-extracted fields parsed out of the classify call's JSON.
#[derive(Debug, Deserialize)]
struct ClassifyResponse {
    summary_one_line: String,
    theme_tags: Vec<String>,
    design_doc_focus: Option<String>,
    is_orphan: bool,
}

/// Classify one session.
///
/// 1. Detect repo/cwd, walk turns to extract design-doc touches and
///    skill invocations.
/// 2. Build the normalized interaction text (truncating large
///    tool-result blobs to placeholders) up to the bundle budget.
/// 3. Run the sonnet fabric call against `glean-classify`.
/// 4. Parse the JSON response and assemble a `SessionRecord`.
pub fn classify(session: &ParsedSession, config: &Config) -> Result<ClassifyOutcome> {
    log::debug!(
        "classify::classify: session_uuid={} jsonl_path={}",
        session.session_uuid,
        session.jsonl_path.display()
    );
    if session.turns.is_empty() {
        return Ok(ClassifyOutcome::Quarantined {
            reason: quarantine_reason::EMPTY_INTERACTION.to_string(),
        });
    }
    let substantive = session.turns.iter().filter(|t| has_text(t)).count();
    if substantive < MIN_SUBSTANTIVE_TURNS {
        return Ok(ClassifyOutcome::Quarantined {
            reason: quarantine_reason::EMPTY_INTERACTION.to_string(),
        });
    }

    let (repo_path, repo_slug) = match session.cwd.as_deref() {
        Some(cwd) => repo::resolve(cwd),
        None => (None, None),
    };

    let design_doc_files = extract_design_doc_paths(session, repo_path.as_deref());
    let skill_invocations = extract_skill_invocations(session);
    let interaction_normalized = normalize_interaction(session, config.bundle.interaction_turn_budget_chars);

    let classifier_model = config.fabric.classify_model.clone();
    let response = run_classify(
        &interaction_normalized,
        &config.fabric.binary,
        &classifier_model,
        config.fabric.max_input_chars,
        config.fabric.classify_timeout_secs,
    );
    let response = match response {
        Ok(r) => r,
        Err(e) => {
            log::warn!("classify::classify: fabric call failed: {e:?}");
            return Ok(ClassifyOutcome::Quarantined {
                reason: format!("{}: {e}", quarantine_reason::CLASSIFY_CALL_FAILED),
            });
        }
    };

    let design_doc_focus = response.design_doc_focus.filter(|s| !s.is_empty()).map(PathBuf::from);

    if response.theme_tags.iter().any(|t| t.eq_ignore_ascii_case("redacted")) {
        return Ok(ClassifyOutcome::Quarantined {
            reason: quarantine_reason::REDACTED.to_string(),
        });
    }

    let started_at = session.started_at().unwrap_or_else(Utc::now);
    let ended_at = session.ended_at().unwrap_or(started_at);

    Ok(ClassifyOutcome::Ok(Box::new(SessionRecord {
        session_uuid: session.session_uuid.clone(),
        jsonl_path: session.jsonl_path.clone(),
        jsonl_sha256: session.jsonl_sha256.clone(),
        repo_slug,
        repo_path,
        cwd: session.cwd.clone(),
        started_at,
        ended_at,
        design_doc_files,
        skill_invocations,
        interaction_normalized,
        summary_one_line: response.summary_one_line,
        theme_tags: response.theme_tags,
        design_doc_focus,
        is_orphan: response.is_orphan,
        classified_at: Utc::now(),
        classifier_model,
    })))
}

fn has_text(turn: &Turn) -> bool {
    turn.content.iter().any(|b| match b {
        ContentBlock::Text { text } => !text.trim().is_empty(),
        ContentBlock::ToolUse { .. } => true,
        _ => false,
    })
}

fn extract_design_doc_paths(session: &ParsedSession, repo_path: Option<&Path>) -> Vec<PathBuf> {
    let mut found = std::collections::BTreeSet::new();
    let re = regex::Regex::new(r#"docs/design/[^\s"'`]+\.md"#).expect("compile design-doc regex");
    for turn in &session.turns {
        for block in &turn.content {
            let text = match block {
                ContentBlock::Text { text } => text.clone(),
                ContentBlock::ToolUse { input, .. } => input.to_string(),
                ContentBlock::ToolResult { content, .. } => content.clone(),
                _ => continue,
            };
            for cap in re.find_iter(&text) {
                let raw = cap.as_str().to_string();
                let path = PathBuf::from(&raw);
                if let Some(root) = repo_path {
                    let abs = root.join(&raw);
                    found.insert(abs);
                } else {
                    found.insert(path);
                }
            }
        }
    }
    found.into_iter().collect()
}

fn extract_skill_invocations(session: &ParsedSession) -> Vec<String> {
    let mut found = std::collections::BTreeSet::new();
    let re = regex::Regex::new(r"^/([a-z][a-z0-9_-]+)\b").expect("compile slash-cmd regex");
    for turn in &session.turns {
        if !matches!(turn.role, Role::User) {
            continue;
        }
        for block in &turn.content {
            if let ContentBlock::Text { text } = block {
                let first = text.trim_start();
                if let Some(cap) = re.captures(first)
                    && let Some(name) = cap.get(1)
                {
                    found.insert(name.as_str().to_string());
                }
            }
        }
    }
    found.into_iter().collect()
}

/// Compose a turn-by-turn rendering with tool-result truncation. The
/// borg pipeline uses the same `<tool-result: N lines, $tool>`
/// placeholder; mirroring it keeps both subsystems' LLM input shapes
/// consistent.
pub fn normalize_interaction(session: &ParsedSession, turn_budget_chars: usize) -> String {
    let mut out = String::new();
    out.push_str(&format!("=== session {} ===\n", session.session_uuid));
    for turn in &session.turns {
        let prefix = match turn.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
        };
        for block in &turn.content {
            match block {
                ContentBlock::Text { text } => {
                    out.push_str(&format!("{prefix}: {text}\n"));
                }
                ContentBlock::Thinking { text: _ } => {
                    out.push_str(&format!("{prefix} (thinking): <thinking elided>\n"));
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    let serialized = input.to_string();
                    let preview = preview_blob(&serialized, turn_budget_chars);
                    out.push_str(&format!("{prefix} (tool_use:{name}): {preview}\n"));
                }
                ContentBlock::ToolResult { content, is_error, .. } => {
                    let label = if *is_error { "tool_result_err" } else { "tool_result" };
                    let preview = if content.len() > turn_budget_chars {
                        TOOL_RESULT_PLACEHOLDER_FMT
                            .replace("{lines}", &content.lines().count().to_string())
                            .replace("{tool}", label)
                    } else {
                        content.clone()
                    };
                    out.push_str(&format!("{prefix} ({label}): {preview}\n"));
                }
                ContentBlock::Image { marker } => {
                    out.push_str(&format!("{prefix}: {marker}\n"));
                }
                ContentBlock::Unknown { kind } => {
                    out.push_str(&format!("{prefix} (unknown:{kind})\n"));
                }
            }
        }
    }
    out
}

fn preview_blob(s: &str, budget: usize) -> String {
    if s.len() <= budget {
        return s.to_string();
    }
    format!("{}... <{} chars elided>", &s[..budget.min(s.len())], s.len() - budget)
}

fn run_classify(
    interaction: &str,
    binary: &str,
    model: &str,
    max_chars: usize,
    timeout_secs: u64,
) -> Result<ClassifyResponse> {
    log::debug!(
        "classify::run_classify: model={model} input_chars={} timeout_secs={timeout_secs}",
        interaction.len()
    );
    let raw = vault::fabric::run_pattern("glean-classify", interaction, binary, model, max_chars, timeout_secs)
        .context("run glean-classify pattern")?;
    let extracted = vault::fabric::extract_json(&raw);
    let parsed: ClassifyResponse = serde_json::from_str(&extracted)
        .map_err(|e| GleanError::Classify(format!("parse classify response: {e}\nraw: {extracted}")))?;
    Ok(parsed)
}

#[cfg(test)]
mod tests;
