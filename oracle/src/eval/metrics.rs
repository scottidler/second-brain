//! Retrieval metrics: pooling, graded nDCG@K, Recall@K, MRR.
//!
//! All pure functions over `(ranked_list, judgments)` so they are deterministic
//! and unit-testable without a search index or an LLM. A `judgments` map holds
//! the graded relevance (`0..=MAX_SCORE`) of every note in a query's pool; a
//! note absent from the map is treated as relevance `0`.

use std::collections::{HashMap, HashSet};

/// Per-query graded relevance: vault-relative note path -> score `0..=3`.
pub type Judgments = HashMap<String, u8>;

/// Union of every mode's ranked list, deduped, sorted for determinism. This is
/// the set of `(query, note)` pairs that must be judged (TREC-style pooling).
pub fn pool(ranked_lists: &[Vec<String>]) -> Vec<String> {
    let mut set: HashSet<&str> = HashSet::new();
    for list in ranked_lists {
        for path in list {
            set.insert(path.as_str());
        }
    }
    let mut out: Vec<String> = set.into_iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

/// Graded gain for a relevance score: `2^rel - 1` (so a "3" is worth much more
/// than three "1"s — the standard exponential nDCG gain).
fn gain(rel: u8) -> f64 {
    (2u32.pow(rel as u32) - 1) as f64
}

/// Discount for a 1-based rank: `1 / log2(rank + 1)`.
fn discount(rank_1based: usize) -> f64 {
    1.0 / ((rank_1based + 1) as f64).log2()
}

/// DCG of `ranked`'s top `k`, scoring each note by its judged relevance (0 if
/// unjudged).
pub fn dcg_at_k(ranked: &[String], j: &Judgments, k: usize) -> f64 {
    ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, path)| gain(j.get(path).copied().unwrap_or(0)) * discount(i + 1))
        .sum()
}

/// Ideal DCG: the maximum achievable DCG@k given the judged scores in the pool
/// (sort all judged scores descending, take `k`). Invariant to how
/// equal-relevance notes are ordered.
pub fn idcg_at_k(j: &Judgments, k: usize) -> f64 {
    let mut scores: Vec<u8> = j.values().copied().collect();
    scores.sort_unstable_by(|a, b| b.cmp(a));
    scores
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, &rel)| gain(rel) * discount(i + 1))
        .sum()
}

/// nDCG@k = DCG/IDCG. `None` when IDCG is 0 (no relevant note in the pool) — the
/// query cannot discriminate modes and is excluded from the mean.
pub fn ndcg_at_k(ranked: &[String], j: &Judgments, k: usize) -> Option<f64> {
    let idcg = idcg_at_k(j, k);
    if idcg == 0.0 {
        return None;
    }
    Some(dcg_at_k(ranked, j, k) / idcg)
}

/// Total notes in the pool judged at/above `threshold` (the relevant set).
fn total_relevant(j: &Judgments, threshold: u8) -> usize {
    j.values().filter(|&&s| s >= threshold).count()
}

/// Recall@k = relevant-in-top-k / total-relevant-in-pool. `None` when the pool
/// has no relevant note (excluded from the mean).
pub fn recall_at_k(ranked: &[String], j: &Judgments, k: usize, threshold: u8) -> Option<f64> {
    let total = total_relevant(j, threshold);
    if total == 0 {
        return None;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|p| j.get(*p).copied().unwrap_or(0) >= threshold)
        .count();
    Some(hits as f64 / total as f64)
}

/// Reciprocal rank: `1 / rank` of the first relevant note in the top `k`.
/// `None` when the pool has no relevant note (excluded); `Some(0.0)` when
/// relevant notes exist but none appear in the top `k`.
pub fn reciprocal_rank(ranked: &[String], j: &Judgments, k: usize, threshold: u8) -> Option<f64> {
    if total_relevant(j, threshold) == 0 {
        return None;
    }
    for (i, path) in ranked.iter().take(k).enumerate() {
        if j.get(path).copied().unwrap_or(0) >= threshold {
            return Some(1.0 / (i + 1) as f64);
        }
    }
    Some(0.0)
}

/// The three metrics for one query/mode. `None` fields mean the query was
/// excluded for that metric (no relevant note in the pool).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueryScores {
    pub ndcg: Option<f64>,
    pub recall: Option<f64>,
    pub rr: Option<f64>,
}

/// Score one query's ranked list against its pool judgments.
pub fn score_query(ranked: &[String], j: &Judgments, k: usize, threshold: u8) -> QueryScores {
    QueryScores {
        ndcg: ndcg_at_k(ranked, j, k),
        recall: recall_at_k(ranked, j, k, threshold),
        rr: reciprocal_rank(ranked, j, k, threshold),
    }
}

/// Mean of each metric across queries, ignoring `None` (excluded) queries, with
/// the count of contributing queries per metric.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetricMeans {
    pub ndcg: f64,
    pub recall: f64,
    pub mrr: f64,
    pub n_ndcg: usize,
    pub n_recall: usize,
    pub n_mrr: usize,
}

fn mean_of(values: impl Iterator<Item = Option<f64>>) -> (f64, usize) {
    let mut sum = 0.0;
    let mut n = 0usize;
    for v in values.flatten() {
        sum += v;
        n += 1;
    }
    if n == 0 { (0.0, 0) } else { (sum / n as f64, n) }
}

/// Aggregate per-query scores into per-metric means + contributing counts.
pub fn aggregate(per_query: &[QueryScores]) -> MetricMeans {
    let (ndcg, n_ndcg) = mean_of(per_query.iter().map(|q| q.ndcg));
    let (recall, n_recall) = mean_of(per_query.iter().map(|q| q.recall));
    let (mrr, n_mrr) = mean_of(per_query.iter().map(|q| q.rr));
    MetricMeans {
        ndcg,
        recall,
        mrr,
        n_ndcg,
        n_recall,
        n_mrr,
    }
}

#[cfg(test)]
mod tests;
