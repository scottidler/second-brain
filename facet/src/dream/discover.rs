//! Dream-finding discovery. Produces [`Dream`] variants from the gem
//! and narrative corpus without ANY mutation to canonical state.
//!
//! Per the design doc: dreams are derived, regenerable artifacts. The
//! dream pass queries the ledger in-memory and renders proposals to
//! markdown; the operator confirms (or not).

use std::collections::{BTreeMap, BTreeSet};

use eyre::Result;

use crate::Ledger;
use crate::dream::Dream;
use crate::gems::Gem;
use crate::narrative::Narrative;

#[cfg(test)]
mod tests;

/// Minimum gem set size to surface a NarrativeCandidate. Mirrors
/// `narrative::discover::MIN_CLUSTER_SIZE` but lives here so the
/// thresholds can diverge later without entangling the two passes.
pub const NARRATIVE_CANDIDATE_MIN_GEMS: usize = 3;

/// Find all [`Dream`] kinds the ledger currently exposes. Pure
/// function over [`Ledger`]; the orchestrator decides which kinds
/// to surface based on flags.
pub fn find_all_dreams(ledger: &Ledger) -> Result<Vec<Dream>> {
    log::debug!("dream::discover::find_all_dreams");
    let workitem_ids = ledger.workitem_ids_with_gems()?;
    let mut all_gems: Vec<Gem> = Vec::new();
    for id in workitem_ids {
        all_gems.extend(ledger.gems_for_workitem(id)?);
    }
    log::debug!("dream::discover: total_gems={}", all_gems.len());

    let mut dreams = Vec::new();
    dreams.extend(find_semantic_duplicate_groups(&all_gems));
    dreams.extend(find_cross_references(&all_gems));
    dreams.extend(find_narrative_candidates(&all_gems, ledger)?);
    dreams.extend(find_stale_spectra(&all_gems, ledger)?);
    log::info!("dream::discover: produced {} dream(s)", dreams.len());
    Ok(dreams)
}

/// Surface gems whose `task` summary collides across workitems. Hint
/// of cross-workitem semantic duplication (the operator may want to
/// merge workitems). The "canonical" choice picks the lowest gem id.
pub fn find_semantic_duplicate_groups(gems: &[Gem]) -> Vec<Dream> {
    let mut by_task: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for g in gems {
        let key = normalise_task(&g.task);
        if key.is_empty() {
            continue;
        }
        by_task.entry(key).or_default().push(g.id);
    }
    let mut out = Vec::new();
    for (_task, mut ids) in by_task {
        if ids.len() < 2 {
            continue;
        }
        ids.sort_unstable();
        let canonical = ids[0];
        out.push(Dream::SemanticDuplicateGroup {
            gem_ids: ids,
            canonical,
        });
    }
    out
}

fn normalise_task(task: &str) -> String {
    task.trim().to_lowercase()
}

/// Surface gems whose `review.accepted` or `review.rejected` text
/// names another gem's task by substring (a heuristic precursor /
/// follow-up link). The relation is "precursor" if the review text
/// mentions the earlier gem; we do not attempt deeper relation
/// classification here.
pub fn find_cross_references(gems: &[Gem]) -> Vec<Dream> {
    let mut out = Vec::new();
    if gems.len() < 2 {
        return out;
    }
    // Sort by extracted_at ascending for stable precursor direction.
    let mut sorted: Vec<&Gem> = gems.iter().collect();
    sorted.sort_by(|a, b| a.extracted_at.cmp(&b.extracted_at).then(a.id.cmp(&b.id)));
    for (i, later) in sorted.iter().enumerate() {
        let review_text = collect_review_text(later);
        if review_text.is_empty() {
            continue;
        }
        let lower = review_text.to_lowercase();
        for earlier in &sorted[..i] {
            if earlier.task.trim().is_empty() {
                continue;
            }
            let needle = earlier.task.trim().to_lowercase();
            if needle.len() < 12 {
                continue;
            }
            if lower.contains(&needle) {
                out.push(Dream::CrossReference {
                    from_gem: later.id,
                    to_gem: earlier.id,
                    relation: "precursor".to_string(),
                });
            }
        }
    }
    out
}

fn collect_review_text(gem: &Gem) -> String {
    let mut parts = Vec::new();
    for s in [
        &gem.review.accepted,
        &gem.review.rejected,
        &gem.review.verified_manually,
    ]
    .into_iter()
    .flatten()
    {
        parts.push(s.as_str());
    }
    parts.join(" ")
}

/// Surface candidate narratives the narrate pass has not produced.
/// Heuristic: any session with `>= NARRATIVE_CANDIDATE_MIN_GEMS` gems
/// that has no corresponding `narratives.cluster_key = session_uuid`
/// row.
pub fn find_narrative_candidates(gems: &[Gem], ledger: &Ledger) -> Result<Vec<Dream>> {
    let mut by_session: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for g in gems {
        by_session.entry(g.session_uuid.clone()).or_default().push(g.id);
    }
    let mut out = Vec::new();
    for (session_uuid, ids) in by_session {
        if ids.len() < NARRATIVE_CANDIDATE_MIN_GEMS {
            continue;
        }
        if ledger.narrative_by_cluster_key(&session_uuid)?.is_some() {
            continue;
        }
        let title = title_from_first_gem(gems, &ids);
        let thesis = format!("{} gems in session `{session_uuid}` may form a narrative.", ids.len());
        out.push(Dream::NarrativeCandidate {
            gem_ids: ids,
            proposed_title: title,
            proposed_thesis: thesis,
        });
    }
    Ok(out)
}

fn title_from_first_gem(gems: &[Gem], ids: &[i64]) -> String {
    let first = ids.first().and_then(|first_id| gems.iter().find(|g| g.id == *first_id));
    match first {
        Some(g) => {
            let raw = g.task.trim();
            let one_line = raw.split('\n').next().unwrap_or(raw).trim();
            if one_line.chars().count() > 60 {
                one_line.chars().take(57).collect::<String>() + "..."
            } else {
                one_line.to_string()
            }
        }
        None => "untitled candidate".to_string(),
    }
}

/// Surface narratives whose cluster has grown since the last
/// synthesis. Heuristic for Session Arcs: count current gems for
/// `cluster_key = session_uuid` and compare to the narrative's
/// citation set; new ids since synthesis = stale.
pub fn find_stale_spectra(gems: &[Gem], ledger: &Ledger) -> Result<Vec<Dream>> {
    let mut by_session: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    for g in gems {
        by_session.entry(g.session_uuid.clone()).or_default().insert(g.id);
    }
    let mut out = Vec::new();
    for (session_uuid, current_ids) in by_session {
        let Some(narr) = ledger.narrative_by_cluster_key(&session_uuid)? else {
            continue;
        };
        let cited: BTreeSet<i64> = narr.gem_ids.iter().copied().collect();
        let new_ids: Vec<i64> = current_ids.difference(&cited).copied().collect();
        if new_ids.is_empty() {
            continue;
        }
        out.push(Dream::StaleSpectrum {
            narrative_id: narr.id,
            new_gem_ids_since: new_ids,
        });
    }
    Ok(out)
}

// Suppress dead_code on the rarely-used Narrative import path that
// `find_stale_spectra` pulls in indirectly via the trait surface.
#[allow(dead_code)]
fn _narrative_marker(_: &Narrative) {}
