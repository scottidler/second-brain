//! Cluster stage: rematerialize work-items from the sessions table.
//!
//! Three-stage pipeline (per design doc):
//! 0. Force singletons for orphan sessions (`is_orphan = true`).
//! 1. Hard-cluster on `design_doc_focus` (LLM-judged primary anchor).
//! 2. Soft-cluster remaining sessions via embedding similarity.
//!
//! Re-cluster is always full recompute. The `work_items` table is
//! truncate-and-rebuild, not upsert-by-key. `content_hash =
//! sha256(sorted member session_uuids)` is the stable identity.

use chrono::Utc;
use eyre::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::config::ClusterConfig;
use crate::ledger::Ledger;
use crate::types::{SessionRecord, WorkItem, WorkItemKey};

const VEC_BATCH_LOG_INTERVAL: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linkage {
    CompleteLink,
    AverageLink,
    SingleLink,
}

impl Linkage {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "complete-link" => Some(Self::CompleteLink),
            "average-link" => Some(Self::AverageLink),
            "single-link" => Some(Self::SingleLink),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CompleteLink => "complete-link",
            Self::AverageLink => "average-link",
            Self::SingleLink => "single-link",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClusterReport {
    pub n_singletons: usize,
    pub n_design_doc: usize,
    pub n_theme: usize,
    pub n_total: usize,
}

/// Run the full cluster pipeline against the current `sessions` table
/// and replace the `work_items` table with the result. Returns counts
/// for the CLI surface.
pub fn run(ledger: &Ledger, config: &ClusterConfig) -> Result<ClusterReport> {
    log::info!(
        "cluster::run: algorithm={} threshold={:.3} min={}",
        config.algorithm,
        config.similarity_threshold,
        config.min_cluster_size
    );
    let linkage = Linkage::parse(&config.algorithm).ok_or_else(|| {
        eyre::eyre!(
            "unknown cluster algorithm {:?}; expected one of complete-link, average-link, single-link",
            config.algorithm
        )
    })?;
    let sessions = ledger.all_sessions().context("load sessions")?;
    let items = cluster_sessions(&sessions, linkage, config.similarity_threshold)?;
    let report = ClusterReport {
        n_singletons: items.iter().filter(|w| w.key_type == WorkItemKey::Singleton).count(),
        n_design_doc: items.iter().filter(|w| w.key_type == WorkItemKey::DesignDoc).count(),
        n_theme: items.iter().filter(|w| w.key_type == WorkItemKey::Theme).count(),
        n_total: items.len(),
    };
    ledger.replace_work_items(&items).context("replace work_items table")?;
    Ok(report)
}

/// Pure-function variant for tests: returns the work-items without
/// touching SQLite.
pub fn cluster_sessions(sessions: &[SessionRecord], linkage: Linkage, threshold: f32) -> Result<Vec<WorkItem>> {
    let mut work_items: Vec<WorkItem> = Vec::new();
    let mut consumed: BTreeSet<String> = BTreeSet::new();

    // Stage 0: orphans become singletons.
    for s in sessions {
        if s.is_orphan {
            work_items.push(make_singleton(s));
            consumed.insert(s.session_uuid.clone());
        }
    }

    // Stage 1: hard-cluster on design_doc_focus within the same repo.
    let mut hard_buckets: BTreeMap<(Option<String>, PathBuf), Vec<&SessionRecord>> = BTreeMap::new();
    for s in sessions {
        if consumed.contains(&s.session_uuid) {
            continue;
        }
        let Some(focus) = s.design_doc_focus.clone() else {
            continue;
        };
        hard_buckets.entry((s.repo_slug.clone(), focus)).or_default().push(s);
    }
    for ((repo_slug, focus), members) in hard_buckets {
        if members.is_empty() {
            continue;
        }
        for m in &members {
            consumed.insert(m.session_uuid.clone());
        }
        work_items.push(make_work_item(
            WorkItemKey::DesignDoc,
            focus.to_string_lossy().to_string(),
            repo_slug,
            &members,
        ));
    }

    // Stage 2: soft-cluster on remaining sessions via embedding similarity.
    let unconsumed: Vec<&SessionRecord> = sessions
        .iter()
        .filter(|s| !consumed.contains(&s.session_uuid))
        .collect();
    if !unconsumed.is_empty() {
        let embeddings = embed_sessions(&unconsumed)?;
        let groups = agglomerative_cluster(&embeddings, linkage, threshold);
        for group in groups {
            if group.is_empty() {
                continue;
            }
            if group.len() == 1 {
                work_items.push(make_singleton(unconsumed[group[0]]));
                continue;
            }
            let members: Vec<&SessionRecord> = group.iter().map(|&i| unconsumed[i]).collect();
            let theme_key = derive_theme_key(&members);
            let repo_slug = members.iter().filter_map(|m| m.repo_slug.clone()).next();
            work_items.push(make_work_item(WorkItemKey::Theme, theme_key, repo_slug, &members));
        }
    }

    work_items.sort_by(|a, b| a.time_start.cmp(&b.time_start));
    Ok(work_items)
}

fn make_singleton(s: &SessionRecord) -> WorkItem {
    let session_uuids = vec![s.session_uuid.clone()];
    WorkItem {
        id: 0,
        key_type: WorkItemKey::Singleton,
        key_value: s.session_uuid.clone(),
        repo_slug: s.repo_slug.clone(),
        content_hash: compute_content_hash(&session_uuids),
        session_uuids,
        time_start: s.started_at,
        time_end: s.ended_at,
        aggregated_tags: s.theme_tags.clone(),
        materialized_at: Utc::now(),
    }
}

fn make_work_item(
    key_type: WorkItemKey,
    key_value: String,
    repo_slug: Option<String>,
    members: &[&SessionRecord],
) -> WorkItem {
    let mut uuids: Vec<String> = members.iter().map(|m| m.session_uuid.clone()).collect();
    uuids.sort();
    let mut tags: BTreeSet<String> = BTreeSet::new();
    for m in members {
        for t in &m.theme_tags {
            tags.insert(t.clone());
        }
    }
    let time_start = members.iter().map(|m| m.started_at).min().unwrap_or_else(Utc::now);
    let time_end = members.iter().map(|m| m.ended_at).max().unwrap_or_else(Utc::now);
    WorkItem {
        id: 0,
        key_type,
        key_value,
        repo_slug,
        content_hash: compute_content_hash(&uuids),
        session_uuids: uuids,
        time_start,
        time_end,
        aggregated_tags: tags.into_iter().collect(),
        materialized_at: Utc::now(),
    }
}

pub fn compute_content_hash(session_uuids: &[String]) -> String {
    use sha2::Digest;
    let mut sorted = session_uuids.to_vec();
    sorted.sort();
    let mut hasher = sha2::Sha256::new();
    hasher.update(sorted.join("|").as_bytes());
    hex::encode(hasher.finalize())
}

fn derive_theme_key(members: &[&SessionRecord]) -> String {
    let mut tags: BTreeSet<String> = BTreeSet::new();
    for m in members {
        for t in &m.theme_tags {
            tags.insert(t.clone());
        }
    }
    let joined = tags.iter().take(3).cloned().collect::<Vec<_>>().join("-");
    if joined.is_empty() {
        let uuids: Vec<String> = members.iter().map(|m| m.session_uuid.clone()).collect();
        let h = compute_content_hash(&uuids);
        format!("theme-{}", &h[..12])
    } else {
        let uuids: Vec<String> = members.iter().map(|m| m.session_uuid.clone()).collect();
        let h = compute_content_hash(&uuids);
        let safe = joined
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>();
        format!("{}-{}", safe.trim_matches('-'), &h[..8])
    }
}

fn embed_sessions(sessions: &[&SessionRecord]) -> Result<Vec<Vec<f32>>> {
    let model_version = vault::embedding::ACTIVE_MODEL_VERSION;
    let mut out = Vec::with_capacity(sessions.len());
    for (i, s) in sessions.iter().enumerate() {
        if i % VEC_BATCH_LOG_INTERVAL == 0 {
            log::debug!("cluster::embed_sessions: progress {}/{} sessions", i, sessions.len());
        }
        let key = embed_key_for(s);
        let v = vault::embedding::embed_query(&key, model_version)
            .with_context(|| format!("embed session {}", s.session_uuid))?;
        out.push(v);
    }
    Ok(out)
}

fn embed_key_for(s: &SessionRecord) -> String {
    let mut docs = s
        .design_doc_files
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    docs.sort();
    format!("{}\n{}\n{}", s.summary_one_line, s.theme_tags.join(" "), docs.join(" "))
}

fn agglomerative_cluster(embeddings: &[Vec<f32>], linkage: Linkage, threshold: f32) -> Vec<Vec<usize>> {
    let n = embeddings.len();
    if n == 0 {
        return Vec::new();
    }
    let mut clusters: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    loop {
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..clusters.len() {
            for j in (i + 1)..clusters.len() {
                let sim = cluster_similarity(&clusters[i], &clusters[j], embeddings, linkage);
                if sim < threshold {
                    continue;
                }
                match best {
                    Some((_, _, b)) if sim <= b => {}
                    _ => best = Some((i, j, sim)),
                }
            }
        }
        let Some((i, j, _sim)) = best else { break };
        // merge j into i; remove j
        let merged_j = clusters.remove(j);
        clusters[i].extend(merged_j);
    }
    clusters
}

fn cluster_similarity(a: &[usize], b: &[usize], embeddings: &[Vec<f32>], linkage: Linkage) -> f32 {
    let mut pairs: Vec<f32> = Vec::with_capacity(a.len() * b.len());
    for &i in a {
        for &j in b {
            pairs.push(cosine(&embeddings[i], &embeddings[j]));
        }
    }
    match linkage {
        Linkage::CompleteLink => pairs.iter().cloned().fold(f32::INFINITY, f32::min),
        Linkage::SingleLink => pairs.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        Linkage::AverageLink => {
            if pairs.is_empty() {
                0.0
            } else {
                pairs.iter().sum::<f32>() / pairs.len() as f32
            }
        }
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    // bge-small-en-v1.5 vectors are L2-normalised, so this is a plain dot product.
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests;
