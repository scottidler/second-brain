//! Per-row extract. Reads the JSONL slice bounded by a
//! `cluster_assignments` row's `first_turn_uuid`/`last_turn_uuid`,
//! invokes the extract LLM (splitting at a turn boundary if the
//! digest exceeds `extract.max_input_tokens`), and persists the
//! resulting judgment moments. On LLM failure, the row stays
//! `extracted=0` and no moment rows land.

use chrono::Utc;
use eyre::{Context, Result};

use super::{ExtractOutput, ExtractedMoment};
use crate::config::Config;
use crate::fabric::{FabricCaller, request};
use crate::jsonl::{ContentBlock, Turn};
use crate::ledger::Ledger;
use crate::ledger::clusters::ClusterAssignmentRow;
use crate::ledger::moments::NewJudgmentMoment;
use crate::ledger::workitems::SessionContribution;

/// Mine judgment moments from a single cluster_assignment row.
///
/// `assignment` is the row to process. `turns` is the bounded slice of
/// the session's JSONL covering `[first_turn_uuid, last_turn_uuid]`
/// inclusive. `workitem_slug` and `workitem_title` are looked up by
/// the caller and threaded through into the prompt.
///
/// If the digest would exceed `extract.max_input_tokens`, the slice is
/// split at a turn boundary and each chunk gets its own LLM call.
/// Judgment moments are idempotent on `(workitem_id, turn_uuid, mode)`
/// so chunk overlap is harmless.
///
/// On success: inserts one `judgment_moments` row per mined moment,
/// advances `session_workitem.last_extract_turn_uuid`, flips
/// `cluster_assignments.extracted` to 1.
/// On failure (any chunk): returns Err; ledger writes for this row are
/// not persisted (the row stays `extracted=0` for the next tick).
pub async fn mine_moments(
    assignment: &ClusterAssignmentRow,
    turns: &[Turn],
    workitem_slug: &str,
    workitem_title: &str,
    repo_slug: Option<&str>,
    config: &Config,
    ledger: &Ledger,
    fabric: &dyn FabricCaller,
) -> Result<Vec<ExtractedMoment>> {
    log::debug!(
        "mine_moments: cluster_id={} workitem_id={} workitem_slug={} turns={}",
        assignment.id,
        assignment.workitem_id,
        workitem_slug,
        turns.len()
    );
    if turns.is_empty() {
        eyre::bail!(
            "mine_moments: empty turn slice for cluster_assignment id={}",
            assignment.id
        );
    }

    let chunks = split_turns_by_budget(workitem_slug, workitem_title, repo_slug, turns, config);
    log::debug!("mine_moments: split into {} chunk(s)", chunks.len());

    let mut all_moments: Vec<ExtractedMoment> = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let digest = build_digest(workitem_slug, workitem_title, repo_slug, chunk);
        log::debug!(
            "mine_moments: chunk {}/{} turns={} digest_chars={}",
            i + 1,
            chunks.len(),
            chunk.len(),
            digest.len()
        );
        let req = request(
            "facet-extract",
            digest,
            &config.llm.extract_model,
            config.llm.timeout_secs,
        );
        let raw = fabric.call(req).await.context("extract LLM call")?;
        let parsed: ExtractOutput = serde_yaml::from_str(&raw)
            .with_context(|| format!("parse extract YAML output (got {} bytes)", raw.len()))?;
        all_moments.extend(parsed.moments);
    }

    let now = Utc::now();
    let max_chars = config.extract.quote_max_chars;
    for m in &all_moments {
        let mut quote = m.quote_excerpt.clone();
        quote = quote.trim_start().to_string();
        if quote.chars().count() > max_chars {
            let mut end = max_chars;
            let mut indices = quote.char_indices();
            let last_idx = indices.nth(end).map(|(i, _)| i).unwrap_or(quote.len());
            end = last_idx;
            quote.truncate(end);
            quote.push('…');
        }
        ledger.insert_moment(NewJudgmentMoment {
            workitem_id: assignment.workitem_id,
            session_uuid: &assignment.session_uuid,
            turn_uuid: &m.turn_uuid,
            mode: &m.mode,
            ai_move: &m.ai_move,
            scott_move: &m.scott_move,
            quote_excerpt: &quote,
            why_it_matters: &m.why_it_matters,
            extractor_model: &config.llm.extract_model,
            extracted_at: now,
        })?;
    }
    ledger.record_contribution(SessionContribution {
        session_uuid: &assignment.session_uuid,
        workitem_id: assignment.workitem_id,
        at: now,
    })?;
    ledger.set_last_extract_turn_uuid(
        &assignment.session_uuid,
        assignment.workitem_id,
        &assignment.last_turn_uuid,
    )?;
    ledger.mark_extracted(assignment.id)?;
    Ok(all_moments)
}

/// Split a turn slice into contiguous chunks each below the
/// `extract.max_input_tokens` digest budget. The split happens between
/// turns, never inside one. A single turn that exceeds the budget on
/// its own still lands in a chunk (one oversized call beats zero
/// coverage).
///
/// The cap is `max_input_tokens * 4` characters, applying the standard
/// ~4-chars-per-token approximation. We floor the budget at 2_000 chars
/// so an absurdly small config value cannot defeat the splitter.
fn split_turns_by_budget(
    workitem_slug: &str,
    workitem_title: &str,
    repo_slug: Option<&str>,
    turns: &[Turn],
    config: &Config,
) -> Vec<Vec<Turn>> {
    let budget_chars = (config.extract.max_input_tokens.saturating_mul(4)).max(2_000);
    let header_chars = build_digest(workitem_slug, workitem_title, repo_slug, &[]).len();
    let mut out: Vec<Vec<Turn>> = Vec::new();
    let mut cur: Vec<Turn> = Vec::new();
    let mut cur_chars: usize = header_chars;
    for t in turns {
        let one_chars =
            build_digest(workitem_slug, workitem_title, repo_slug, std::slice::from_ref(t)).len() - header_chars;
        if !cur.is_empty() && cur_chars + one_chars > budget_chars {
            out.push(std::mem::take(&mut cur));
            cur_chars = header_chars;
        }
        cur.push(t.clone());
        cur_chars += one_chars;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(turns.to_vec());
    }
    out
}

fn build_digest(workitem_slug: &str, workitem_title: &str, repo_slug: Option<&str>, turns: &[Turn]) -> String {
    let mut s = String::new();
    s.push_str(&format!("workitem_slug: {}\n", yaml_str(workitem_slug)));
    s.push_str(&format!("workitem_title: {}\n", yaml_str(workitem_title)));
    s.push_str(&format!(
        "repo_slug: {}\n",
        match repo_slug {
            Some(r) => yaml_str(r),
            None => "null".to_string(),
        }
    ));
    s.push_str("turns:\n");
    for t in turns {
        s.push_str(&format!("  - uuid: {}\n", yaml_str(&t.uuid)));
        s.push_str(&format!(
            "    parent_uuid: {}\n",
            match &t.parent_uuid {
                Some(p) => yaml_str(p),
                None => "null".to_string(),
            }
        ));
        s.push_str(&format!("    role: {}\n", role_str(t)));
        s.push_str(&format!("    timestamp: {}\n", t.timestamp.to_rfc3339()));
        s.push_str(&format!("    text: {}\n", yaml_str(&turn_text(t))));
    }
    s
}

fn role_str(t: &Turn) -> &'static str {
    match t.role {
        crate::jsonl::Role::User => "user",
        crate::jsonl::Role::Assistant => "assistant",
    }
}

fn turn_text(t: &Turn) -> String {
    let mut buf = String::new();
    for b in &t.content {
        match b {
            ContentBlock::Text { text } => {
                buf.push_str(text);
                buf.push('\n');
            }
            ContentBlock::Thinking { text } => {
                buf.push_str("[thinking] ");
                buf.push_str(text);
                buf.push('\n');
            }
            ContentBlock::ToolUse { name, .. } => buf.push_str(&format!("[tool_use:{name}]\n")),
            ContentBlock::ToolResult { content, is_error, .. } => {
                let tag = if *is_error { "tool_result_err" } else { "tool_result" };
                buf.push_str(&format!("[{tag}] "));
                buf.push_str(content);
                buf.push('\n');
            }
            ContentBlock::Image { .. } => buf.push_str("[image]\n"),
            ContentBlock::Unknown { kind } => buf.push_str(&format!("[?{kind}]\n")),
        }
    }
    buf.trim().to_string()
}

fn yaml_str(s: &str) -> String {
    serde_yaml::to_string(s)
        .unwrap_or_else(|_| format!("{s:?}"))
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests;
