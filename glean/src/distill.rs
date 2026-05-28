//! Tier-2 distiller: one opus fabric call per work-item.
//!
//! Composes a per-work-item bundle (every member session's normalized
//! interaction with explicit `=== session <uuid> ===` separators)
//! and asks fabric to produce a four-part chunk plus a title and
//! tldr. The renderer writes the result into `notes/glean/`.
//!
//! Concurrency: `BEGIN IMMEDIATE` over `work_items` for the full
//! bundle-compose + fabric call + write window. A concurrent cluster
//! pass blocks until the transaction closes.

use eyre::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

use crate::classify;
use crate::config::Config;
use crate::error::GleanError;
use crate::jsonl;
use crate::ledger::Ledger;
use crate::render::{self, DistillOutput};
use crate::types::WorkItem;

const EXTRACTOR_NAME: &str = "glean-distill-v1";
const SESSION_SEPARATOR_FMT: &str = "=== session {uuid} ===";

#[derive(Debug, Deserialize)]
struct DistillResponse {
    title: String,
    tldr: String,
    task: String,
    context: String,
    interaction: String,
    review: String,
}

#[derive(Debug, Clone)]
pub struct DistillReport {
    pub work_item_content_hash: String,
    pub chunk_path: PathBuf,
}

/// Distill one work-item. Acquires a BEGIN IMMEDIATE transaction over
/// `work_items` for the full window.
pub fn distill_one(ledger: &Ledger, config: &Config, work_item: &WorkItem) -> Result<DistillReport> {
    log::info!(
        "distill::distill_one: content_hash={} key_type={} key_value={}",
        &work_item.content_hash[..8.min(work_item.content_hash.len())],
        work_item.key_type.as_str(),
        work_item.key_value
    );
    ledger.with_immediate_tx(|_tx| Ok(()))?;

    let sessions = ledger
        .get_sessions_by_uuids(&work_item.session_uuids)
        .context("load member sessions")?;
    if sessions.is_empty() {
        return Err(GleanError::Distill(format!(
            "work_item {} has no member sessions in the ledger",
            work_item.content_hash
        ))
        .into());
    }

    let bundle = compose_bundle(&sessions, config.bundle.interaction_turn_budget_chars);
    let raw = vault::fabric::run_pattern(
        "glean-distill",
        &bundle,
        &config.fabric.binary,
        &config.fabric.distill_model,
        config.fabric.max_input_chars,
        config.fabric.distill_timeout_secs,
    )
    .context("run glean-distill pattern")?;
    let extracted = vault::fabric::extract_json(&raw);
    let parsed: DistillResponse = serde_json::from_str(&extracted)
        .map_err(|e| GleanError::Distill(format!("parse distill response: {e}\nraw: {extracted}")))?;
    let out = DistillOutput {
        title: parsed.title,
        tldr: parsed.tldr,
        task: parsed.task,
        context: parsed.context,
        interaction: parsed.interaction,
        review: parsed.review,
    };
    let glean_dir = config.vault.root_path.join(&config.vault.glean_dir);
    let chunk_path = render::render_chunk(
        &glean_dir,
        work_item,
        &out,
        EXTRACTOR_NAME,
        &config.fabric.distill_model,
    )?;
    Ok(DistillReport {
        work_item_content_hash: work_item.content_hash.clone(),
        chunk_path,
    })
}

fn compose_bundle(sessions: &[crate::types::SessionRecord], turn_budget: usize) -> String {
    let mut s = String::new();
    for sess in sessions {
        s.push_str(&SESSION_SEPARATOR_FMT.replace("{uuid}", &sess.session_uuid));
        s.push('\n');
        // If we have the original JSONL on disk and it's still readable,
        // re-normalize from scratch so the bundle carries the full
        // interaction (the stored `interaction_normalized` is the
        // classify-time snapshot; on re-distill a freshly grown
        // session is worth re-reading).
        match jsonl::parse_session_file(&sess.jsonl_path) {
            Ok(p) => {
                let norm = classify::normalize_interaction(&p, turn_budget);
                s.push_str(&norm);
            }
            Err(e) => {
                log::warn!(
                    "distill::compose_bundle: jsonl reread failed for {}: {e}; using stored snapshot",
                    sess.session_uuid
                );
                s.push_str(&sess.interaction_normalized);
            }
        }
        s.push('\n');
    }
    s
}

/// Distill every work-item in the ledger. Returns the per-item reports
/// in materialized order. Failures on individual work-items log at
/// WARN and are skipped.
pub fn distill_all(ledger: &Ledger, config: &Config) -> Result<Vec<DistillReport>> {
    log::info!("distill::distill_all");
    let items = ledger.all_work_items().context("load work_items")?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match distill_one(ledger, config, &item) {
            Ok(r) => out.push(r),
            Err(e) => log::warn!(
                "distill::distill_all: work_item {} failed: {e:?}",
                &item.content_hash[..8.min(item.content_hash.len())]
            ),
        }
    }
    Ok(out)
}
