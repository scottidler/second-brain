//! Narrate-pass: synthesise one [`crate::narrative::Narrative`] per
//! cluster candidate by invoking the `facet-narrate.md` Fabric pattern.
//!
//! Implements the strict rejection gate per Architect Round 2: when
//! Opus returns `title: ""`, the cluster is recorded as "skipped" and
//! NO narrative row lands.

use chrono::Utc;
use eyre::{Context, Result};
use serde::Deserialize;

use crate::config::Config;
use crate::fabric::{FabricCaller, request};
use crate::gems::Gem;
use crate::narrative::Narrative;
use crate::narrative::discover::ClusterCandidate;
use crate::workitem::derive_slug;

#[cfg(test)]
mod tests;

/// Raw LLM output. Empty `title` means the rejection gate fired.
#[derive(Debug, Clone, Deserialize)]
struct NarrateOutput {
    title: String,
    thesis: String,
    #[serde(default)]
    body_md: String,
    #[serde(default)]
    gem_ids: Vec<i64>,
    #[serde(default)]
    #[allow(dead_code)]
    chronologically_ordered: bool,
}

/// The outcome of a narrate call: either a synthesised narrative
/// ready for upsert, or a rejection (Opus returned empty title).
///
/// The `Accepted` variant is large (`Narrative` carries the full
/// `body_md`) but the variants are not constructed in tight loops
/// where size matters; the `Box` would just add an allocation.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum NarrateOutcome {
    Accepted(Narrative),
    Skipped { reason: String },
}

/// Run one narrate call on one cluster candidate. Pure async function;
/// no ledger writes (the caller upserts on `Accepted`).
pub async fn narrate(
    candidate: &ClusterCandidate,
    config: &Config,
    fabric: &dyn FabricCaller,
) -> Result<NarrateOutcome> {
    log::debug!(
        "narrate: archetype={} cluster_key={} gem_count={}",
        candidate.archetype.as_str(),
        candidate.cluster_key,
        candidate.gems.len(),
    );

    let digest = build_digest(candidate);
    let req = request(
        "facet-narrate",
        digest,
        &config.llm.spectra_model,
        config.llm.timeout_secs,
    );
    let raw = fabric.call(req).await.context("facet-narrate LLM call")?;
    let body = crate::yaml_out::strip_fences(&raw);
    let parsed: NarrateOutput = serde_json::from_str(body).with_context(|| {
        let preview: String = body.chars().take(240).collect();
        format!(
            "parse facet-narrate JSON output (got {} bytes); preview: {preview:?}",
            raw.len()
        )
    })?;

    if parsed.title.trim().is_empty() || parsed.thesis.trim().is_empty() {
        log::info!(
            "narrate: rejection gate fired (empty title/thesis) for cluster_key={} archetype={}",
            candidate.cluster_key,
            candidate.archetype.as_str()
        );
        return Ok(NarrateOutcome::Skipped {
            reason: "empty title/thesis (rejection gate)".to_string(),
        });
    }

    let now = Utc::now();
    let cited_ids = if parsed.gem_ids.is_empty() {
        candidate.gems.iter().map(|g| g.id).collect()
    } else {
        parsed.gem_ids.clone()
    };
    let slug = build_slug(candidate, &parsed.title);
    let axes = crate::narrative::NarrativeAxes::default();
    let narrative = Narrative {
        id: 0,
        slug,
        title: parsed.title,
        thesis: parsed.thesis,
        body_md: parsed.body_md,
        gem_ids: cited_ids,
        axes: build_axes(candidate),
        synthesised_at: now,
        synthesiser_model: config.llm.spectra_model.clone(),
        revision: 1,
    };
    let _ = axes;
    Ok(NarrateOutcome::Accepted(narrative))
}

fn build_axes(candidate: &ClusterCandidate) -> crate::narrative::NarrativeAxes {
    use std::collections::BTreeMap;
    let mut mode_counts: BTreeMap<String, u32> = BTreeMap::new();
    for g in &candidate.gems {
        for t in &g.tags {
            *mode_counts.entry(t.clone()).or_default() += 1;
        }
    }
    let mode_mix: Vec<(String, u32)> = {
        let mut v: Vec<_> = mode_counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    };
    let time_window = match (candidate.gems.first(), candidate.gems.last()) {
        (Some(first), Some(last)) => Some((first.extracted_at, last.extracted_at)),
        _ => None,
    };
    let workitem_ids: Vec<i64> = {
        let mut s: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
        for g in &candidate.gems {
            s.insert(g.workitem_id);
        }
        s.into_iter().collect()
    };
    crate::narrative::NarrativeAxes {
        semantic_cluster_id: None,
        mode_mix,
        time_window,
        repos: Vec::new(),
        workitem_ids,
    }
}

fn build_slug(candidate: &ClusterCandidate, title: &str) -> String {
    let base = derive_slug(title);
    let key_short = candidate
        .cluster_key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>();
    if key_short.is_empty() { base } else { format!("{base}-{key_short}") }
}

fn build_digest(candidate: &ClusterCandidate) -> String {
    let mut s = String::new();
    s.push_str(&format!("archetype: {}\n", yaml_str(candidate.archetype.as_str())));
    s.push_str(&format!("cluster_key: {}\n", yaml_str(&candidate.cluster_key)));
    s.push_str("gems:\n");
    for g in &candidate.gems {
        s.push_str(&format!("  - id: {}\n", g.id));
        s.push_str(&format!("    extracted_at: {}\n", g.extracted_at.to_rfc3339()));
        s.push_str(&format!("    task: {}\n", yaml_str(&g.task)));
        s.push_str(&format!("    why_it_matters: {}\n", yaml_str(&g.why_it_matters)));
        s.push_str("    tags:\n");
        if g.tags.is_empty() {
            s.push_str("      []\n");
        } else {
            for t in &g.tags {
                s.push_str(&format!("      - {}\n", yaml_str(t)));
            }
        }
        s.push_str(&format!(
            "    accepted: {}\n",
            yaml_str_opt(g.review.accepted.as_deref())
        ));
        s.push_str(&format!(
            "    rejected: {}\n",
            yaml_str_opt(g.review.rejected.as_deref())
        ));
        s.push_str(&format!(
            "    verified_manually: {}\n",
            yaml_str_opt(g.review.verified_manually.as_deref())
        ));
        s.push_str(&format!(
            "    rewrote_by_hand: {}\n",
            yaml_str_opt(g.review.rewrote_by_hand.as_deref())
        ));
        let first_user = g.interaction.first().map(|t| t.user_says.as_str()).unwrap_or("");
        let truncated: String = first_user.chars().take(500).collect();
        s.push_str(&format!("    first_user_says: {}\n", yaml_str(&truncated)));
    }
    s
}

fn yaml_str(s: &str) -> String {
    serde_yaml::to_string(s)
        .unwrap_or_else(|_| format!("{s:?}"))
        .trim_end()
        .to_string()
}

fn yaml_str_opt(s: Option<&str>) -> String {
    match s {
        Some(v) => yaml_str(v),
        None => "null".to_string(),
    }
}

#[allow(dead_code)]
fn count_gems_in_outcome(_outcome: &NarrateOutcome, _gems: &[Gem]) -> usize {
    // placeholder so the dispatcher can quickly report counts without
    // matching on the enum variant at every call site
    0
}
