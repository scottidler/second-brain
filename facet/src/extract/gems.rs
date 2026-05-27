//! Gem extractor. Reads the JSONL slice bounded by a
//! `cluster_assignments` row, runs the `facet-extract.md` Fabric
//! pattern (one call per chunk produced by [`super::chunker`]), parses
//! the JSON output into `Vec<Gem>`, and persists each gem via the
//! `(workitem_id, content_hash)` idempotency key.
//!
//! On LLM/parse failure for any chunk, the cluster_assignment row
//! stays `extracted=0` and no gem rows land (the next tick retries).

use chrono::Utc;
use eyre::{Context, Result};
use serde::{Deserialize, Serialize};

use super::chunker;
use crate::config::Config;
use crate::fabric::{FabricCaller, request};
use crate::gems::{Gem, InteractionTurn, Review};
use crate::jsonl::{ContentBlock, Role, Turn};
use crate::ledger::Ledger;
use crate::ledger::clusters::ClusterAssignmentRow;
use crate::ledger::gems::NewGem;
use crate::ledger::workitems::SessionContribution;

#[cfg(test)]
mod tests;

/// LLM-output shape for one gem (matches `facet-extract.md`'s
/// SCHEMA section). Server-side fields (`workitem_id`, `session_uuid`,
/// `extractor_model`, `extracted_at`) are filled in by the extract
/// pipeline, not the LLM.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExtractedGem {
    pub task: String,
    #[serde(default)]
    pub context_loaded: Vec<String>,
    #[serde(default)]
    pub context_missing: Vec<String>,
    pub interaction: Vec<InteractionTurn>,
    #[serde(default)]
    pub review: Review,
    #[serde(default)]
    pub tags: Vec<String>,
    pub why_it_matters: String,
}

/// Raw LLM output wrapper. `{"gems": [...]}`. An empty list is a
/// valid result (the LLM correctly recognised the chunk had no gems).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtractV2Output {
    #[serde(default)]
    pub gems: Vec<ExtractedGem>,
}

/// Mine gems from a single cluster_assignment row.
///
/// Takes the
/// cluster_assignment, the JSONL turn slice for its range, the
/// work-item identifying info, the config, ledger, and a Fabric
/// caller. Returns the list of [`Gem`]s persisted (each with its
/// ledger-assigned id).
///
/// On success: upserts gem rows via
/// [`Ledger::upsert_gem`], advances
/// `session_workitem.last_extract_turn_uuid`, and flips
/// `cluster_assignments.extracted` to 1. On failure (any chunk):
/// returns Err; the cluster_assignment row stays `extracted=0` for
/// the next tick.
pub async fn mine_gems(
    assignment: &ClusterAssignmentRow,
    turns: &[Turn],
    workitem_slug: &str,
    workitem_title: &str,
    repo_slug: Option<&str>,
    config: &Config,
    ledger: &Ledger,
    fabric: &dyn FabricCaller,
) -> Result<Vec<Gem>> {
    log::debug!(
        "mine_gems: cluster_id={} workitem_id={} workitem_slug={} turns={}",
        assignment.id,
        assignment.workitem_id,
        workitem_slug,
        turns.len(),
    );
    if turns.is_empty() {
        eyre::bail!(
            "mine_gems: empty turn slice for cluster_assignment id={}",
            assignment.id
        );
    }

    let chunks = chunker::chunk_turns(
        turns,
        chunker::DEFAULT_MAX_TURNS_PER_CHUNK,
        chunker::DEFAULT_OVERLAP_TURNS,
    );
    log::debug!("mine_gems: split into {} chunk(s)", chunks.len());

    let mut all_extracted: Vec<ExtractedGem> = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let digest = build_digest(workitem_slug, workitem_title, repo_slug, chunk);
        log::debug!(
            "mine_gems: chunk {}/{} turns={} digest_chars={}",
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
        let body = crate::yaml_out::strip_fences(&raw);
        let parsed: ExtractV2Output = serde_json::from_str(body).with_context(|| {
            let preview: String = body.chars().take(240).collect();
            format!(
                "parse extract JSON output (got {} bytes); preview: {preview:?}",
                raw.len()
            )
        })?;
        log::debug!(
            "mine_gems: chunk {}/{} yielded {} gem(s)",
            i + 1,
            chunks.len(),
            parsed.gems.len()
        );
        all_extracted.extend(parsed.gems);
    }

    let now = Utc::now();
    let mut persisted: Vec<Gem> = Vec::with_capacity(all_extracted.len());
    for eg in &all_extracted {
        if eg.interaction.is_empty() {
            log::warn!(
                "mine_gems: dropping ExtractedGem with empty interaction (task={:?})",
                eg.task
            );
            continue;
        }
        let gem_id = ledger.upsert_gem(NewGem {
            workitem_id: assignment.workitem_id,
            session_uuid: &assignment.session_uuid,
            task: &eg.task,
            context_loaded: &eg.context_loaded,
            context_missing: &eg.context_missing,
            interaction: &eg.interaction,
            review: &eg.review,
            tags: &eg.tags,
            why_it_matters: &eg.why_it_matters,
            extractor_model: &config.llm.extract_model,
            extracted_at: now,
        })?;
        let gem = ledger
            .gem_by_id(gem_id)?
            .ok_or_else(|| eyre::eyre!("upserted gem id={gem_id} not found on readback"))?;
        persisted.push(gem);
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

    log::info!(
        "mine_gems: cluster_id={} workitem_id={} persisted_gems={}",
        assignment.id,
        assignment.workitem_id,
        persisted.len()
    );
    Ok(persisted)
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
        s.push_str(&format!("    role: {}\n", role_str(t)));
        s.push_str(&format!("    timestamp: {}\n", t.timestamp.to_rfc3339()));
        s.push_str(&format!("    text: {}\n", yaml_str(&turn_text(t))));
    }
    s
}

fn role_str(t: &Turn) -> &'static str {
    match t.role {
        Role::User => "user",
        Role::Assistant => "assistant",
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
