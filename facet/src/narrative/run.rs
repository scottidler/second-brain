//! Narrate-pass orchestrator. Ties discovery + narrate + render + upsert
//! into one `sb facet narrate` invocation.
//!
//! Order of operations:
//!   1. Read every existing spectrum note from `<vault>/<spectra_dir>/`
//!      and extract `facet-spectrum-status` + `facet-spectrum-gem-ids`
//!      for rejection-overlap suppression.
//!   2. Load all gems from the ledger, ordered chronologically.
//!   3. Run all three discovery archetypes (or just the one requested).
//!   4. Filter candidates whose gem-id set overlaps >= 80% with a
//!      rejected spectrum (per Architect Round 2: "rejection is a
//!      one-edit operation in Obsidian").
//!   5. For each remaining candidate, narrate via fabric; on Accepted,
//!      upsert into the ledger and render the spectrum note.
//!   6. Return a [`NarrateReport`] for the operator surface.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use eyre::{Context, Result};

use crate::config::Config;
use crate::fabric::{FabricCaller, FabricShell};
use crate::gems::Gem;
use crate::ledger::Ledger;
use crate::ledger::narratives::NewNarrative;
use crate::narrative::discover::{
    self, ClusterCandidate, MIN_CLUSTER_SIZE, discover_cross_session_arcs, discover_session_arcs,
};
use crate::narrative::narrate::{NarrateOutcome, narrate};
use crate::narrative::render::{SpectrumMeta, read_spectrum_meta, render_spectrum_note};
use crate::narrative::{Archetype, SpectrumStatus};

#[cfg(test)]
mod tests;

/// Minimum overlap fraction between a candidate's gem-id set and a
/// rejected spectrum's gem-id set that triggers suppression.
pub const REJECTION_OVERLAP_THRESHOLD: f32 = 0.80;

/// What archetypes to run. `None` runs all three; `Some(x)` runs only
/// that archetype (debug mode).
#[derive(Debug, Clone, Copy)]
pub enum ArchetypeFilter {
    All,
    Only(Archetype),
}

#[derive(Debug, Clone, Default)]
pub struct NarrateReport {
    pub candidates_considered: usize,
    pub candidates_suppressed_by_rejection: usize,
    pub narratives_synthesised: usize,
    pub narratives_skipped_by_gate: usize,
    pub render_failures: usize,
}

/// Production entry point. Defers to [`run_with_fabric`] with a real
/// fabric shell.
pub async fn run(
    config: &Config,
    ledger: &Ledger,
    vault_root: &Path,
    filter: ArchetypeFilter,
) -> Result<NarrateReport> {
    log::info!(
        "facet::narrative::run: vault_root={} filter={:?}",
        vault_root.display(),
        filter
    );
    let fabric: Arc<dyn FabricCaller> = Arc::new(FabricShell::new(config.llm.fabric_binary.clone()));
    let embedder = ProductionEmbedder;
    run_with_fabric(config, ledger, vault_root, filter, fabric.as_ref(), &embedder).await
}

/// Trait over the embedding backend so tests inject deterministic
/// vectors. Production uses [`ProductionEmbedder`] which delegates to
/// `vault::embedding`.
pub trait Embedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

/// Production embedder: lazy-load the active model on first call.
pub struct ProductionEmbedder;

impl Embedder for ProductionEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        vault::embedding::embed_query(text, vault::embedding::ACTIVE_MODEL_VERSION)
            .context("vault::embedding::embed_query")
    }
}

/// Generic-over-fabric-and-embedder driver. Used by tests to inject
/// deterministic dependencies.
pub async fn run_with_fabric<E: Embedder>(
    config: &Config,
    ledger: &Ledger,
    vault_root: &Path,
    filter: ArchetypeFilter,
    fabric: &dyn FabricCaller,
    embedder: &E,
) -> Result<NarrateReport> {
    let mut report = NarrateReport::default();

    // 1. Read existing spectrum notes for rejection-suppression.
    let spectra_dir = vault_root.join(&config.vault.spectra_dir);
    let existing_metas = read_existing_spectrum_metas(&spectra_dir)?;
    let rejected: Vec<&SpectrumMeta> = existing_metas
        .iter()
        .filter(|m| m.status == SpectrumStatus::Rejected)
        .collect();
    log::debug!(
        "narrative::run: existing_spectra={} rejected={}",
        existing_metas.len(),
        rejected.len()
    );

    // 2. Load all gems.
    let all_gems = load_all_gems(ledger)?;
    log::debug!("narrative::run: total_gems={}", all_gems.len());
    if all_gems.len() < MIN_CLUSTER_SIZE {
        log::info!(
            "narrative::run: only {} gems; below MIN_CLUSTER_SIZE={MIN_CLUSTER_SIZE}; nothing to narrate",
            all_gems.len()
        );
        return Ok(report);
    }

    // 3. Discovery (filter by archetype if requested).
    let mut candidates: Vec<ClusterCandidate> = Vec::new();
    if matches!(filter, ArchetypeFilter::All | ArchetypeFilter::Only(Archetype::Session)) {
        candidates.extend(discover_session_arcs(&all_gems));
    }
    if matches!(
        filter,
        ArchetypeFilter::All | ArchetypeFilter::Only(Archetype::CrossSession)
    ) {
        let xs = discover_cross_session_arcs(&all_gems, |g| embedder.embed(&discover::embedding_text(g)))?;
        candidates.extend(xs);
    }
    report.candidates_considered = candidates.len();

    // 4. Filter by rejection overlap.
    let mut to_narrate: Vec<ClusterCandidate> = Vec::new();
    for c in candidates {
        if is_suppressed_by_rejection(&c, &rejected) {
            log::info!(
                "narrative::run: suppress candidate cluster_key={} archetype={} (>= {:.0}% overlap with rejected spectrum)",
                c.cluster_key,
                c.archetype.as_str(),
                REJECTION_OVERLAP_THRESHOLD * 100.0,
            );
            report.candidates_suppressed_by_rejection += 1;
            continue;
        }
        to_narrate.push(c);
    }

    // 5. Narrate + upsert + render.
    for c in &to_narrate {
        let outcome = match narrate(c, config, fabric).await {
            Ok(o) => o,
            Err(e) => {
                log::warn!(
                    "narrative::run: narrate failed for cluster_key={}: {e:#}",
                    c.cluster_key
                );
                report.render_failures += 1;
                continue;
            }
        };
        match outcome {
            NarrateOutcome::Skipped { reason } => {
                log::info!(
                    "narrative::run: skipped cluster_key={} reason={}",
                    c.cluster_key,
                    reason
                );
                report.narratives_skipped_by_gate += 1;
            }
            NarrateOutcome::Accepted(mut n) => {
                let new = NewNarrative {
                    cluster_key: &c.cluster_key,
                    archetype: c.archetype,
                    slug: &n.slug,
                    title: &n.title,
                    thesis: &n.thesis,
                    body_md: &n.body_md,
                    gem_ids: &n.gem_ids,
                    axes: &n.axes,
                    synthesised_at: n.synthesised_at,
                    synthesiser_model: &n.synthesiser_model,
                };
                match ledger.upsert_narrative(new) {
                    Ok(id) => {
                        n.id = id;
                    }
                    Err(e) => {
                        log::warn!("narrative::run: upsert failed for cluster_key={}: {e:#}", c.cluster_key);
                        report.render_failures += 1;
                        continue;
                    }
                }
                let target = spectra_dir.join(format!("{}.md", n.slug));
                if let Some(parent) = target.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    log::warn!("narrative::run: mkdir {} failed: {e:#}", parent.display());
                    report.render_failures += 1;
                    continue;
                }
                if let Err(e) = render_spectrum_note(&target, &n, c.archetype, &c.cluster_key) {
                    log::warn!("narrative::run: render failed for cluster_key={}: {e:#}", c.cluster_key);
                    report.render_failures += 1;
                } else {
                    report.narratives_synthesised += 1;
                }
            }
        }
    }

    log::info!(
        "narrative::run complete: considered={} suppressed={} synthesised={} skipped_by_gate={} render_failures={}",
        report.candidates_considered,
        report.candidates_suppressed_by_rejection,
        report.narratives_synthesised,
        report.narratives_skipped_by_gate,
        report.render_failures,
    );
    Ok(report)
}

fn read_existing_spectrum_metas(spectra_dir: &Path) -> Result<Vec<SpectrumMeta>> {
    let mut out = Vec::new();
    if !spectra_dir.exists() {
        return Ok(out);
    }
    let entries =
        std::fs::read_dir(spectra_dir).with_context(|| format!("read spectra dir {}", spectra_dir.display()))?;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                log::warn!("narrative::run: bad dir entry: {e:#}");
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        match read_spectrum_meta(&path) {
            Ok(Some(meta)) => out.push(meta),
            Ok(None) => {}
            Err(e) => {
                log::warn!("narrative::run: read_spectrum_meta {}: {e:#}", path.display());
            }
        }
    }
    Ok(out)
}

fn load_all_gems(ledger: &Ledger) -> Result<Vec<Gem>> {
    // Pull a bounded set of workitem ids (those that have gems), then
    // fold their gem lists. Cheaper than a single SELECT JOIN once the
    // corpus grows large, and reuses existing pagination shape.
    let workitem_ids = ledger.workitem_ids_with_gems()?;
    let mut all = Vec::new();
    for id in workitem_ids {
        all.extend(ledger.gems_for_workitem(id)?);
    }
    // Order globally by extracted_at; downstream archetypes expect this.
    all.sort_by(|a, b| a.extracted_at.cmp(&b.extracted_at).then(a.id.cmp(&b.id)));
    Ok(all)
}

fn is_suppressed_by_rejection(candidate: &ClusterCandidate, rejected: &[&SpectrumMeta]) -> bool {
    let candidate_ids: HashSet<i64> = candidate.gems.iter().map(|g| g.id).collect();
    if candidate_ids.is_empty() {
        return false;
    }
    for r in rejected {
        let rejected_ids: HashSet<i64> = r.gem_ids.iter().copied().collect();
        if rejected_ids.is_empty() {
            continue;
        }
        let intersection = candidate_ids.intersection(&rejected_ids).count();
        let overlap_of_candidate = intersection as f32 / candidate_ids.len() as f32;
        let overlap_of_rejected = intersection as f32 / rejected_ids.len() as f32;
        // Symmetric: suppress if EITHER set is >= 80% covered by the
        // intersection. This catches both "candidate is mostly a
        // rejected spectrum" and "rejected spectrum is mostly a
        // sub-cluster of the candidate."
        if overlap_of_candidate >= REJECTION_OVERLAP_THRESHOLD || overlap_of_rejected >= REJECTION_OVERLAP_THRESHOLD {
            return true;
        }
    }
    false
}
