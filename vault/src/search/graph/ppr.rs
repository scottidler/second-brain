//! Personalized PageRank over the `edges` graph, by forward push
//! (Andersen, Chung, Lang, "Local Graph Partitioning using PageRank
//! Vectors", FOCS 2006).
//!
//! The walk is the non-lazy one: from node `u` it teleports back to the seed
//! distribution with probability `alpha`, otherwise it steps to neighbor `v`
//! with probability `w(u,v) / d(u)`, where `d(u)` is `u`'s weighted degree over
//! the traversed edges. Edges are undirected, as in
//! [`SearchIndex::expand_graph`]. A node with no traversed edge keeps its own
//! mass (a self-loop), so no probability leaks out of the graph.
//!
//! Forward push keeps an estimate `p` and a residual `r` (seed mass not yet
//! spread). A push at `u` moves `alpha * r(u)` into `p(u)` and spreads the rest
//! over `u`'s neighbors. It pushes while some `r(u) > epsilon * d(u)`, so the
//! true vector exceeds the estimate by less than `epsilon * d(v)` at every `v`.
//! Only touched nodes are ever loaded, so the cost is local to the seeds.

use std::collections::{HashMap, HashSet, VecDeque};

use eyre::{Result, bail};

use super::SearchIndex;

/// Forward-push tuning, from `retrieval.methods.graph` in `oracle.yml`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PprParams {
    /// Teleport probability back to the seeds, in `(0, 1)`.
    pub alpha: f64,
    /// Residual threshold per unit of weighted degree, `> 0`.
    pub epsilon: f64,
    /// Work cap. Reaching it returns the current estimate with a WARN.
    pub max_pushes: usize,
}

impl PprParams {
    /// Reject parameters the push cannot run with: an `alpha` outside `(0, 1)`
    /// never converges or never spreads, a non-positive `epsilon` never stops,
    /// and a zero cap returns the empty estimate for every query.
    pub fn validate(&self) -> Result<()> {
        if !(self.alpha.is_finite() && self.alpha > 0.0 && self.alpha < 1.0) {
            bail!("ppr-alpha must be in (0, 1), got {}", self.alpha);
        }
        if !(self.epsilon.is_finite() && self.epsilon > 0.0) {
            bail!("ppr-epsilon must be a finite value > 0, got {}", self.epsilon);
        }
        if self.max_pushes == 0 {
            bail!("ppr-max-pushes must be >= 1");
        }
        Ok(())
    }
}

/// The PPR estimate for one query.
#[derive(Debug, Clone, PartialEq)]
pub struct PprScores {
    /// Every node with a non-zero estimate, seeds included, by score
    /// descending then path ascending.
    pub scores: Vec<(String, f64)>,
    /// Pushes performed.
    pub pushes: usize,
    /// True when the push cap stopped the run before the residual bound held.
    pub capped: bool,
}

/// One node's loaded adjacency: `(neighbor, weight)` per traversed edge, and
/// the weighted degree.
struct Adjacency {
    neighbors: Vec<(String, f64)>,
    degree: f64,
}

/// Forward-push PPR from `seeds` (`(path, weight)`; weights are normalized to
/// sum to 1). `neighbors(u)` returns every traversed edge incident to `u` as
/// `(other endpoint, weight)`; it is called at most once per node. An edge
/// weight that is not finite and positive is an error: it has no meaning as a
/// transition probability.
pub fn forward_push<F>(seeds: &[(String, f64)], params: &PprParams, mut neighbors: F) -> Result<PprScores>
where
    F: FnMut(&str) -> Result<Vec<(String, f64)>>,
{
    params.validate()?;
    let total: f64 = seeds.iter().map(|(_, w)| *w).sum();
    if let Some((path, w)) = seeds.iter().find(|(_, w)| !(w.is_finite() && *w > 0.0)) {
        bail!("ppr seed {path} has weight {w}; seed weights must be finite and > 0");
    }
    if seeds.is_empty() {
        return Ok(PprScores {
            scores: Vec::new(),
            pushes: 0,
            capped: false,
        });
    }

    let mut residual: HashMap<String, f64> = HashMap::new();
    let mut estimate: HashMap<String, f64> = HashMap::new();
    let mut adjacency: HashMap<String, Adjacency> = HashMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut queued: HashSet<String> = HashSet::new();
    for (path, w) in seeds {
        *residual.entry(path.clone()).or_insert(0.0) += w / total;
        if queued.insert(path.clone()) {
            queue.push_back(path.clone());
        }
    }

    let mut pushes = 0usize;
    let mut capped = false;
    while let Some(u) = queue.pop_front() {
        queued.remove(&u);
        let r_u = residual.get(&u).copied().unwrap_or(0.0);
        if !adjacency.contains_key(&u) {
            let neighbors = neighbors(&u)?;
            if let Some((v, w)) = neighbors.iter().find(|(_, w)| !(w.is_finite() && *w > 0.0)) {
                bail!("ppr edge {u} -- {v} has weight {w}; traversed edge weights must be finite and > 0");
            }
            let degree = neighbors.iter().map(|(_, w)| *w).sum();
            adjacency.insert(u.clone(), Adjacency { neighbors, degree });
        }
        let adj = &adjacency[&u];
        let dangling = adj.neighbors.is_empty();
        if r_u <= 0.0 || (!dangling && r_u <= params.epsilon * adj.degree) {
            continue;
        }
        if pushes >= params.max_pushes {
            capped = true;
            break;
        }
        pushes += 1;
        residual.insert(u.clone(), 0.0);
        if dangling {
            // A self-loop node returns every step to itself: all its residual
            // ends up in its own estimate.
            *estimate.entry(u.clone()).or_insert(0.0) += r_u;
            continue;
        }
        *estimate.entry(u.clone()).or_insert(0.0) += params.alpha * r_u;
        let spread = (1.0 - params.alpha) * r_u / adj.degree;
        for (v, w) in &adj.neighbors {
            *residual.entry(v.clone()).or_insert(0.0) += spread * w;
            if queued.insert(v.clone()) {
                queue.push_back(v.clone());
            }
        }
    }

    if capped {
        let left: f64 = residual.values().sum();
        log::warn!(
            "ppr: hit ppr-max-pushes={} with residual mass {left:.6} unspread; returning the current estimate",
            params.max_pushes
        );
    }

    let mut scores: Vec<(String, f64)> = estimate.into_iter().filter(|(_, p)| *p > 0.0).collect();
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    log::debug!(
        "ppr: seeds={} pushes={pushes} capped={capped} loaded={} scored={}",
        seeds.len(),
        adjacency.len(),
        scores.len()
    );
    Ok(PprScores { scores, pushes, capped })
}

impl SearchIndex {
    /// Personalized PageRank from `seeds` over the `edges` graph, by
    /// [`forward_push`]. `edge_kinds` and `min_weight` select the traversed
    /// edges exactly as in [`expand_graph`](Self::expand_graph).
    ///
    /// Adjacency is loaded lazily into a request-local map, one node at a time,
    /// through ONE prepared statement inside ONE read transaction: the whole
    /// walk sees one snapshot of `edges` even while cortex rewrites them, and
    /// nothing is cached across queries.
    pub fn personalized_pagerank(
        &self,
        seeds: &[(String, f64)],
        edge_kinds: Option<&[String]>,
        min_weight: f32,
        params: &PprParams,
    ) -> Result<PprScores> {
        log::debug!(
            "personalized_pagerank: seeds={} edge_kinds={edge_kinds:?} min_weight={min_weight} params={params:?}",
            seeds.len()
        );
        params.validate()?;
        let tx = self.conn.unchecked_transaction()?;
        let scores = {
            let (sql, kinds) = super::neighbor_query(edge_kinds);
            let mut stmt = tx.prepare(&sql)?;
            let min_weight = min_weight as f64;
            forward_push(seeds, params, |node| {
                let mut bound: Vec<&dyn rusqlite::types::ToSql> = vec![&node, &min_weight];
                for k in &kinds {
                    bound.push(k);
                }
                for k in &kinds {
                    bound.push(k);
                }
                let rows = stmt.query_map(bound.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, f64>(3)?,
                    ))
                })?;
                let mut out = Vec::new();
                for r in rows {
                    let (src, dst, weight) = r?;
                    out.push((if src == node { dst } else { src }, weight));
                }
                Ok(out)
            })?
        };
        tx.rollback()?;
        Ok(scores)
    }
}

#[cfg(all(test, feature = "vec"))]
mod tests;
