//! Stale detector: chunks whose member sessions have grown since the
//! chunk was last distilled.

use eyre::{Context, Result};
use rayon::prelude::*;
use serde::Deserialize;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::config::Config;
use crate::ledger::Ledger;
use crate::types::WorkItem;

use super::render::{DreamKind, DreamProposal, write_proposal};

#[derive(Debug, Deserialize)]
struct Response {
    is_stale: bool,
    confidence: f32,
    reason: String,
}

pub fn run(ledger: &Ledger, config: &Config) -> Result<usize> {
    let items = ledger.all_work_items().context("load work_items")?;
    let dreams_dir = config.vault.root_path.join(&config.vault.dreams_dir);
    log::info!(
        "dream::stale::run: n={} parallelism={}",
        items.len(),
        config.daemon.dream_parallelism
    );
    let written = AtomicUsize::new(0);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(config.daemon.dream_parallelism.max(1))
        .build()
        .context("build rayon thread pool for dream::stale")?;
    pool.install(|| -> Result<()> {
        items.par_iter().try_for_each(|item| -> Result<()> {
            if let Some(proposal) = consider_one(item, ledger, config)? {
                write_proposal(&dreams_dir, &proposal)?;
                written.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        })
    })?;
    Ok(written.load(Ordering::Relaxed))
}

fn consider_one(work_item: &WorkItem, ledger: &Ledger, config: &Config) -> Result<Option<DreamProposal>> {
    let chunks_dir = config.vault.root_path.join(&config.vault.glean_dir);
    let Some(chunk_path) = crate::render::find_existing_by_content_hash(&chunks_dir, &work_item.content_hash)? else {
        return Ok(None);
    };
    let chunk_body = std::fs::read_to_string(&chunk_path)?;
    let sessions = ledger.get_sessions_by_uuids(&work_item.session_uuids)?;
    let mut summaries = String::new();
    for s in &sessions {
        summaries.push_str(&format!("- session {}: {}\n", s.session_uuid, s.summary_one_line));
    }
    let input = format!("{chunk_body}\n=== summaries ===\n{summaries}");
    let raw = vault::fabric::run_pattern(
        "glean-dream-stale",
        &input,
        &config.fabric.binary,
        &config.fabric.dream_model,
        config.fabric.max_input_chars,
        config.fabric.dream_timeout_secs,
    )
    .context("run glean-dream-stale pattern")?;
    let extracted = vault::fabric::extract_json(&raw);
    let parsed: Response = serde_json::from_str(&extracted)?;
    if !parsed.is_stale {
        return Ok(None);
    }
    Ok(Some(DreamProposal {
        kind: DreamKind::Stale,
        confidence: parsed.confidence,
        reason: parsed.reason,
        source_chunks: vec![work_item.content_hash.clone()],
        suggested_title: None,
        direction: None,
    }))
}
