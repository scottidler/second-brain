//! Cross-reference detector.

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
    should_xref: bool,
    confidence: f32,
    direction: Option<String>,
    reason: String,
}

pub fn run(ledger: &Ledger, config: &Config) -> Result<usize> {
    let items = ledger.all_work_items().context("load work_items")?;
    let dreams_dir = config.vault.root_path.join(&config.vault.dreams_dir);
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for i in 0..items.len() {
        for j in (i + 1)..items.len() {
            pairs.push((i, j));
        }
    }
    log::info!(
        "dream::xref::run: pairs={} parallelism={}",
        pairs.len(),
        config.daemon.dream_parallelism
    );
    let written = AtomicUsize::new(0);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(config.daemon.dream_parallelism.max(1))
        .build()
        .context("build rayon thread pool for dream::xref")?;
    pool.install(|| -> Result<()> {
        pairs.par_iter().try_for_each(|(i, j)| -> Result<()> {
            if let Some(proposal) = consider_pair(&items[*i], &items[*j], config)? {
                write_proposal(&dreams_dir, &proposal)?;
                written.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        })
    })?;
    Ok(written.load(Ordering::Relaxed))
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
        "glean-dream-xref",
        &input,
        &config.fabric.binary,
        &config.fabric.dream_model,
        config.fabric.max_input_chars,
        config.fabric.dream_timeout_secs,
    )
    .context("run glean-dream-xref pattern")?;
    let extracted = vault::fabric::extract_json(&raw);
    let parsed: Response = serde_json::from_str(&extracted)?;
    if !parsed.should_xref {
        return Ok(None);
    }
    Ok(Some(DreamProposal {
        kind: DreamKind::Xref,
        confidence: parsed.confidence,
        reason: parsed.reason,
        source_chunks: vec![a.content_hash.clone(), b.content_hash.clone()],
        suggested_title: None,
        direction: parsed.direction,
    }))
}
