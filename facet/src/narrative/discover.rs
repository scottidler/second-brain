//! Narrative discovery: turn the gems corpus into candidate clusters.
//!
//! Phase 5 implements two real archetypes plus an evergreen back-compat
//! shape (see the design doc):
//!
//! - **Session Arc**: gems within a single `session_uuid`, chronologically
//!   ordered. Eligible when `gem_count >= 3` AND the session contains at
//!   least one `name-the-failure` or `reject` gem. No HDBSCAN, no
//!   embedding step.
//! - **Cross-Session Arc**: gems clustered across sessions by semantic
//!   similarity. Eligible when the cluster has at least
//!   `MIN_CLUSTER_SIZE` gems. Uses [`vault::embedding`] for embeddings
//!   and a simple agglomerative (greedy single-link) cluster builder
//!   tuned for tightness by `CROSS_SESSION_SIMILARITY_THRESHOLD`.
//! - **Evergreen**: synthetic clusters keyed by primary tag (mode
//!   bucket). Back-compat with the v1 mode spectra.
//!
//! Each candidate is returned as a [`ClusterCandidate`] and consumed by
//! `narrate.rs`.

use std::collections::BTreeMap;

use eyre::Result;

use crate::gems::Gem;
use crate::narrative::Archetype;

#[cfg(test)]
mod tests;

/// Minimum gems per cluster before narrate is invoked. Architect Round
/// 2 says >= 3 with configurable; a 2-gem pair can still narrate when
/// both gems are name-the-failure (worth narrating because failures
/// recurred and were costly). Default 3.
pub const MIN_CLUSTER_SIZE: usize = 3;

/// Cosine-similarity threshold for the Cross-Session Arc greedy
/// agglomerative cluster builder. Higher = tighter clusters. Set
/// conservatively per Architect Round 2 ("tune for tightness; a
/// 100-gem cluster is a tuning signal that epsilon is too loose").
pub const CROSS_SESSION_SIMILARITY_THRESHOLD: f32 = 0.78;

/// A candidate cluster that has passed the eligibility filters and is
/// ready for narrate-pass.
#[derive(Debug, Clone)]
pub struct ClusterCandidate {
    pub archetype: Archetype,
    /// Stable key per archetype: a session_uuid for Session Arc, a
    /// hash of the gem-id set for Cross-Session Arc, the mode name
    /// for Evergreen.
    pub cluster_key: String,
    /// Gems in this cluster, ordered chronologically by `extracted_at`.
    pub gems: Vec<Gem>,
}

/// Discover Session Arc candidates from `all_gems`. `all_gems` is
/// expected to be ordered by `extracted_at` ascending so the
/// chronological-order guarantee holds.
pub fn discover_session_arcs(all_gems: &[Gem]) -> Vec<ClusterCandidate> {
    log::debug!("discover_session_arcs: total_gems={}", all_gems.len());

    let mut by_session: BTreeMap<String, Vec<Gem>> = BTreeMap::new();
    for g in all_gems {
        by_session.entry(g.session_uuid.clone()).or_default().push(g.clone());
    }

    let mut out = Vec::new();
    for (session_uuid, mut gems) in by_session {
        if gems.len() < MIN_CLUSTER_SIZE {
            continue;
        }
        if !has_obstacle_tag(&gems) {
            continue;
        }
        // Force chronological order even if the input was a different ordering.
        gems.sort_by(|a, b| a.extracted_at.cmp(&b.extracted_at).then(a.id.cmp(&b.id)));
        out.push(ClusterCandidate {
            archetype: Archetype::Session,
            cluster_key: session_uuid,
            gems,
        });
    }
    log::debug!("discover_session_arcs: produced {} candidate(s)", out.len());
    out
}

fn has_obstacle_tag(gems: &[Gem]) -> bool {
    gems.iter()
        .any(|g| g.tags.iter().any(|t| t == "name-the-failure" || t == "reject"))
}

/// Discover Evergreen candidates: synthetic clusters keyed by primary
/// tag (mode). One cluster per scaffolding mode, gems within ordered
/// chronologically. Skips clusters that fail `MIN_CLUSTER_SIZE`.
pub fn discover_evergreen_clusters(all_gems: &[Gem]) -> Vec<ClusterCandidate> {
    log::debug!("discover_evergreen_clusters: total_gems={}", all_gems.len());
    const SCAFFOLD_MODES: &[&str] = &["frame", "iterate", "reject", "push-for", "sequence", "name-the-failure"];
    let mut out = Vec::new();
    for mode in SCAFFOLD_MODES {
        let mut gems: Vec<Gem> = all_gems
            .iter()
            .filter(|g| g.tags.iter().any(|t| t == *mode))
            .cloned()
            .collect();
        if gems.len() < MIN_CLUSTER_SIZE {
            continue;
        }
        gems.sort_by(|a, b| a.extracted_at.cmp(&b.extracted_at).then(a.id.cmp(&b.id)));
        out.push(ClusterCandidate {
            archetype: Archetype::Evergreen,
            cluster_key: format!("mode-{mode}"),
            gems,
        });
    }
    log::debug!("discover_evergreen_clusters: produced {} candidate(s)", out.len());
    out
}

/// Discover Cross-Session Arc candidates by clustering gem embeddings.
///
/// Uses `embed_fn` to turn each gem into a vector; runs a simple greedy
/// single-link agglomerative cluster builder over those vectors with a
/// cosine-similarity threshold tuned for tightness; orders each cluster
/// chronologically; filters by `MIN_CLUSTER_SIZE`.
///
/// `embed_fn` is a closure so tests can inject deterministic vectors
/// (real embedding goes through `vault::embedding::embed_query` in
/// production; see `narrate::run` for the wiring).
pub fn discover_cross_session_arcs<F>(all_gems: &[Gem], mut embed_fn: F) -> Result<Vec<ClusterCandidate>>
where
    F: FnMut(&Gem) -> Result<Vec<f32>>,
{
    log::debug!(
        "discover_cross_session_arcs: total_gems={} threshold={}",
        all_gems.len(),
        CROSS_SESSION_SIMILARITY_THRESHOLD
    );
    if all_gems.len() < MIN_CLUSTER_SIZE {
        return Ok(Vec::new());
    }

    let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(all_gems.len());
    for g in all_gems {
        vectors.push(embed_fn(g)?);
    }

    let clusters = cluster_greedy_agglomerative(&vectors, CROSS_SESSION_SIMILARITY_THRESHOLD);
    let mut out = Vec::new();
    for indices in clusters {
        if indices.len() < MIN_CLUSTER_SIZE {
            continue;
        }
        let mut gems: Vec<Gem> = indices.iter().map(|&i| all_gems[i].clone()).collect();
        gems.sort_by(|a, b| a.extracted_at.cmp(&b.extracted_at).then(a.id.cmp(&b.id)));
        let cluster_key = cluster_key_for(&gems);
        out.push(ClusterCandidate {
            archetype: Archetype::CrossSession,
            cluster_key,
            gems,
        });
    }
    log::debug!("discover_cross_session_arcs: produced {} candidate(s)", out.len());
    Ok(out)
}

/// Greedy single-link agglomerative clustering. Each point starts as
/// its own cluster; merge any two clusters whose closest-points
/// cosine-similarity >= `threshold`. Repeat until no merges happen.
///
/// O(N^2) worst case; fine for the gem corpus sizes in scope
/// (Architect Round 2: "if 30 gems hit one cluster, Opus handles it;
/// 100 means epsilon is too loose, not a cap-at-runtime case").
fn cluster_greedy_agglomerative(vectors: &[Vec<f32>], threshold: f32) -> Vec<Vec<usize>> {
    let n = vectors.len();
    if n == 0 {
        return Vec::new();
    }
    let mut clusters: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();

    loop {
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..clusters.len() {
            for j in (i + 1)..clusters.len() {
                let s = single_link_similarity(&clusters[i], &clusters[j], vectors);
                if s >= threshold && best.is_none_or(|(_, _, b)| s > b) {
                    best = Some((i, j, s));
                }
            }
        }
        match best {
            None => break,
            Some((i, j, _)) => {
                // Merge j into i, then remove j. Indices stable thanks
                // to swap_remove-vs-remove choice (we use remove to
                // preserve order, accepting O(n) shift).
                let other = clusters.remove(j);
                clusters[i].extend(other);
            }
        }
    }
    clusters
}

fn single_link_similarity(a: &[usize], b: &[usize], vectors: &[Vec<f32>]) -> f32 {
    let mut best = f32::MIN;
    for &i in a {
        for &j in b {
            let s = cosine(&vectors[i], &vectors[j]);
            if s > best {
                best = s;
            }
        }
    }
    best
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Stable key for a Cross-Session cluster: sha256 of sorted gem ids,
/// hex-truncated to 12 chars. Used as the SQL `cluster_key` for the
/// narrative and as the cross-tick idempotency anchor.
fn cluster_key_for(gems: &[Gem]) -> String {
    use sha2::{Digest, Sha256};
    let mut ids: Vec<i64> = gems.iter().map(|g| g.id).collect();
    ids.sort_unstable();
    let mut hasher = Sha256::new();
    for (idx, id) in ids.iter().enumerate() {
        if idx > 0 {
            hasher.update(b",");
        }
        hasher.update(id.to_string().as_bytes());
    }
    let full = hex::encode(hasher.finalize());
    format!("xs-{}", &full[..12])
}

/// The text we embed for a gem: task + why_it_matters + first user
/// turn (truncated). Mirrors the design doc: "embed each gem's `task +
/// why_it_matters + interaction.user_says[0]`."
pub fn embedding_text(gem: &Gem) -> String {
    let first_user = gem.interaction.first().map(|t| t.user_says.as_str()).unwrap_or("");
    let truncated = first_user.chars().take(500).collect::<String>();
    format!("{}\n{}\n{truncated}", gem.task, gem.why_it_matters)
}
