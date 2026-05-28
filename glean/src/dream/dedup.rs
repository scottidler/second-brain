//! Dedup detector: pairs of work-items that read as the same.

use eyre::{Context, Result};
use serde::Deserialize;

use crate::config::Config;
use crate::ledger::Ledger;
use crate::types::WorkItem;

use super::render::{DreamKind, DreamProposal, write_proposal};

#[derive(Debug, Deserialize)]
struct Response {
    should_consolidate: bool,
    confidence: f32,
    reason: String,
    suggested_title: Option<String>,
}

/// Run the dedup detector. Returns the number of proposals written.
pub fn run(ledger: &Ledger, config: &Config) -> Result<usize> {
    log::info!("dream::dedup::run");
    let items = ledger.all_work_items().context("load work_items")?;
    let dreams_dir = config.vault.root_path.join(&config.vault.dreams_dir);
    let mut n = 0;
    for i in 0..items.len() {
        for j in (i + 1)..items.len() {
            let Some(proposal) = consider_pair(&items[i], &items[j], config)? else {
                continue;
            };
            write_proposal(&dreams_dir, &proposal)?;
            n += 1;
        }
    }
    Ok(n)
}

fn consider_pair(a: &WorkItem, b: &WorkItem, config: &Config) -> Result<Option<DreamProposal>> {
    let chunks_dir = config.vault.root_path.join(&config.vault.glean_dir);
    let Some(a_path) = crate::render::find_existing_by_content_hash(&chunks_dir, &a.content_hash)? else {
        return Ok(None);
    };
    let Some(b_path) = crate::render::find_existing_by_content_hash(&chunks_dir, &b.content_hash)? else {
        return Ok(None);
    };
    let a_body = std::fs::read_to_string(&a_path)?;
    let b_body = std::fs::read_to_string(&b_path)?;
    let input = format!("{a_body}\n=== chunk 2 ===\n{b_body}");
    let raw = vault::fabric::run_pattern(
        "glean-dream-dedup",
        &input,
        &config.fabric.binary,
        &config.fabric.dream_model,
        config.fabric.max_input_chars,
        config.fabric.dream_timeout_secs,
    )
    .context("run glean-dream-dedup pattern")?;
    let extracted = vault::fabric::extract_json(&raw);
    let parsed: Response = serde_json::from_str(&extracted)?;
    if !parsed.should_consolidate {
        return Ok(None);
    }
    Ok(Some(DreamProposal {
        kind: DreamKind::Dedup,
        confidence: parsed.confidence,
        reason: parsed.reason,
        source_chunks: vec![a.content_hash.clone(), b.content_hash.clone()],
        suggested_title: parsed.suggested_title,
        direction: None,
    }))
}
