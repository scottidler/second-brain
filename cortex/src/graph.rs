//! `cortex graph`: build the materialized deterministic `edges` table that
//! oracle's graph-expansion retrieval reads.
//!
//! Phase 1 of the graph-augmented-memory design
//! (`docs/design/2026-06-05-graph-augmented-memory.md`). cortex is the writer;
//! oracle only reads. The pass runs AFTER `cortex embed` (so semantic edges
//! see fresh vectors) and serializes against any concurrent embed write via
//! the shared embed file lock.
//!
//! Four deterministic edge kinds, all derived from data already in the index:
//! - **semantic** — each note's top-`k` cosine neighbors over
//!   `note_embeddings` (the primary discriminating edge). Keyed on embedding
//!   freshness (`note_embeddings.produced_at`), NOT `notes.modified_at`, so a
//!   note whose embedding lands after it was skipped is never stranded.
//! - **wikilink** — resolved body wikilinks (`weight = 1.0`); danglers skipped.
//! - **shared-tag** — rarity-weighted (`Σ 1/ln(1+df_t)`) so blanket tags
//!   contribute ~nothing; built via a tag→notes inverted index with a fan-out
//!   cap on blanket buckets.
//! - **shared-creator / shared-source / shared-domain** — low fixed weight,
//!   read straight from frontmatter columns.
//!
//! Every edge insert obeys the universal resolve-`dst`-or-skip rule (enforced
//! in `vault::search::SearchIndex::insert_edges`): an edge whose `dst` is
//! absent from `notes` is skipped, never inserted, so the `dst` FK can never
//! abort the batch.

use std::collections::HashMap;
use std::path::Path;

use eyre::{Result, WrapErr};
use vault::search::{Edge, GraphNoteRow, SearchIndex};

use crate::config::Config;
use crate::opts::GraphOpts;

/// `graph_state` key whose presence flips the next pass from full-rebuild to
/// incremental (its absence after a restart forces one safe full rebuild).
const KEY_LAST_RUN_AT: &str = "last_run_at";

const KIND_SEMANTIC: &str = "semantic";
const KIND_WIKILINK: &str = "wikilink";
const KIND_SHARED_TAG: &str = "shared-tag";
const KIND_SHARED_CREATOR: &str = "shared-creator";
const KIND_SHARED_SOURCE: &str = "shared-source";
const KIND_SHARED_DOMAIN: &str = "shared-domain";

const WIKILINK_WEIGHT: f32 = 1.0;

/// Outcome of one graph pass.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct GraphStats {
    pub full_rebuild: bool,
    pub notes_processed: usize,
    pub semantic: usize,
    pub wikilink: usize,
    pub shared_tag: usize,
    pub metadata: usize,
    pub skipped: usize,
}

/// Run the graph pass against the oracle index DB. Opens its own connection
/// (cortex commands do not share oracle's `Mutex<SearchIndex>`), takes the
/// shared embed lock, and writes edges in per-note bounded transactions.
pub fn run(_vault_root: &Path, config: &Config, opts: &GraphOpts) -> Result<GraphStats> {
    log::debug!("cortex::graph::run: backfill={}", opts.backfill);

    let db_path = config.oracle_db_path();
    let mut index = SearchIndex::open(&db_path)
        .wrap_err_with(|| format!("failed to open search index at {}", db_path.display()))?;

    // Serialize against any concurrent `cortex embed` write: the pass reads
    // `note_embeddings` and must not interleave with an embed batch.
    let lock = crate::embed::acquire_lock()?;
    log::debug!("cortex::graph: acquired embed file lock");

    let stats = build(&mut index, &config.graph, opts.backfill)?;

    drop(lock);
    log::info!(
        "graph complete: full_rebuild={} notes={} semantic={} wikilink={} shared_tag={} metadata={} skipped={}",
        stats.full_rebuild,
        stats.notes_processed,
        stats.semantic,
        stats.wikilink,
        stats.shared_tag,
        stats.metadata,
        stats.skipped,
    );
    Ok(stats)
}

/// Core edge-build loop, factored out of `run` so tests can drive an
/// in-memory index without the file lock / config-path machinery.
pub fn build(index: &mut SearchIndex, cfg: &crate::config::GraphConfig, force_full: bool) -> Result<GraphStats> {
    let full_rebuild = force_full || index.graph_state_get(KEY_LAST_RUN_AT)?.is_none();
    log::debug!("cortex::graph::build: full_rebuild={full_rebuild}");

    let rows = index.graph_note_rows()?;
    let by_path: HashMap<String, GraphNoteRow> = rows.iter().map(|r| (r.path.clone(), r.clone())).collect();

    // Inverted indexes over the whole corpus (cheap; needed even for
    // incremental targets to find their pair partners).
    let tag_buckets = invert(&rows, |r| r.tags.clone());
    let creator_buckets = invert(&rows, |r| single(&r.creator));
    let source_buckets = invert(&rows, |r| single(&source_host(&r.source)));
    let domain_buckets = invert(&rows, |r| single(&r.domain));

    // Determine which notes' edges to rebuild. Per-note staleness (mirroring
    // `stale_embedding_targets`) so an incremental pass touches only notes that
    // actually changed — content edges keyed on `modified_at`, semantic edges
    // keyed on embedding `produced_at` (never stranded).
    let targets: Vec<String> = if full_rebuild {
        index.clear_edges()?;
        rows.iter().map(|r| r.path.clone()).collect()
    } else {
        let mut set: std::collections::HashSet<String> = index.content_edge_targets()?.into_iter().collect();
        for p in index.semantic_edge_targets()? {
            set.insert(p);
        }
        set.into_iter().collect()
    };

    let mut stats = GraphStats {
        full_rebuild,
        ..Default::default()
    };

    for src in &targets {
        let Some(row) = by_path.get(src) else {
            continue;
        };
        if !full_rebuild {
            index.delete_edges_by_src(src)?;
        }
        let edges = build_edges_for(
            index,
            row,
            cfg,
            &tag_buckets,
            &creator_buckets,
            &source_buckets,
            &domain_buckets,
        )?;
        let (_inserted, skipped) = index.insert_edges(&edges)?;
        tally(&mut stats, &edges);
        stats.skipped += skipped;
        // Persist this note's build watermarks so it is not reprocessed until
        // its content or embedding changes again.
        let semantic_built_at = index.note_summary_produced_at(src).unwrap_or(0);
        index.record_edge_build(src, row.modified_at, semantic_built_at)?;
        stats.notes_processed += 1;
    }

    // last_run_at presence flips the next pass to incremental.
    index.graph_state_set(KEY_LAST_RUN_AT, &now_ts().to_string())?;

    Ok(stats)
}

/// Build every deterministic edge owned by `row.path`.
fn build_edges_for(
    index: &SearchIndex,
    row: &GraphNoteRow,
    cfg: &crate::config::GraphConfig,
    tag_buckets: &HashMap<String, Vec<String>>,
    creator_buckets: &HashMap<String, Vec<String>>,
    source_buckets: &HashMap<String, Vec<String>>,
    domain_buckets: &HashMap<String, Vec<String>>,
) -> Result<Vec<Edge>> {
    let mut edges: Vec<Edge> = Vec::new();
    let src = &row.path;

    // --- semantic ---
    for (neighbor, cosine) in index.semantic_neighbors(src, cfg.semantic_k, cfg.min_cosine)? {
        edges.push(Edge::deterministic(src.clone(), neighbor, KIND_SEMANTIC, cosine));
    }

    // --- wikilink (resolved targets only) ---
    for slug in vault::search::extract_wikilinks(&row.body) {
        if let Some(resolved) = index.resolve_note_path(&slug)? {
            edges.push(Edge::deterministic(
                src.clone(),
                resolved,
                KIND_WIKILINK,
                WIKILINK_WEIGHT,
            ));
        }
    }

    // --- shared-tag (rarity-weighted, fan-out capped) ---
    let mut tag_weight: HashMap<String, f32> = HashMap::new();
    for tag in &row.tags {
        let Some(bucket) = tag_buckets.get(tag) else {
            continue;
        };
        let df = bucket.len();
        // Rarity weight per the design: 1 / ln(1 + df). A blanket tag (large
        // df) contributes ~nothing; a rare shared tag is discriminating.
        let contrib = 1.0_f32 / (1.0 + df as f32).ln();
        if df > cfg.fanout_cap {
            // Over-cap blanket tag: route through the tag's hub note (Phase 3)
            // if one exists, instead of emitting df-1 pairwise edges. One edge
            // per note to the hub keeps the dense bucket from exploding.
            let hub_path = format!("{}/{}.md", crate::hub::HUB_DIR, tag);
            if index.note_path_exists(&hub_path)? {
                edges.push(Edge::deterministic(src.clone(), hub_path, KIND_SHARED_TAG, contrib));
            } else {
                log::trace!(
                    "shared-tag: blanket tag '{tag}' (df={df} > cap {}) has no hub; skipping",
                    cfg.fanout_cap
                );
            }
            continue;
        }
        for other in bucket {
            if other != src {
                *tag_weight.entry(other.clone()).or_insert(0.0) += contrib;
            }
        }
    }
    for (dst, weight) in tag_weight {
        edges.push(Edge::deterministic(src.clone(), dst, KIND_SHARED_TAG, weight));
    }

    // --- metadata (fixed weight, fan-out capped) ---
    metadata_edges(
        &mut edges,
        src,
        &row.creator,
        creator_buckets,
        KIND_SHARED_CREATOR,
        cfg.creator_weight,
        cfg.fanout_cap,
    );
    metadata_edges(
        &mut edges,
        src,
        &source_host(&row.source),
        source_buckets,
        KIND_SHARED_SOURCE,
        cfg.source_weight,
        cfg.fanout_cap,
    );
    metadata_edges(
        &mut edges,
        src,
        &row.domain,
        domain_buckets,
        KIND_SHARED_DOMAIN,
        cfg.domain_weight,
        cfg.fanout_cap,
    );

    Ok(edges)
}

/// Emit fixed-weight metadata edges from `src` to every other note sharing the
/// same `key` value, unless the bucket exceeds the fan-out cap.
fn metadata_edges(
    edges: &mut Vec<Edge>,
    src: &str,
    key: &str,
    buckets: &HashMap<String, Vec<String>>,
    kind: &str,
    weight: f32,
    fanout_cap: usize,
) {
    if key.is_empty() {
        return;
    }
    let Some(bucket) = buckets.get(key) else {
        return;
    };
    if bucket.len() > fanout_cap {
        log::trace!(
            "{kind}: skipping blanket value '{key}' (bucket={} > cap {fanout_cap})",
            bucket.len()
        );
        return;
    }
    for other in bucket {
        if other != src {
            edges.push(Edge::deterministic(src.to_string(), other.clone(), kind, weight));
        }
    }
}

/// Build a value→notes inverted index from a per-note key extractor.
fn invert<F>(rows: &[GraphNoteRow], key_fn: F) -> HashMap<String, Vec<String>>
where
    F: Fn(&GraphNoteRow) -> Vec<String>,
{
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        for key in key_fn(row) {
            if key.is_empty() {
                continue;
            }
            map.entry(key).or_default().push(row.path.clone());
        }
    }
    map
}

/// Wrap a single non-empty value in a Vec (empty Vec when blank).
fn single(value: &str) -> Vec<String> {
    if value.is_empty() { vec![] } else { vec![value.to_string()] }
}

/// Extract the host from a source URL (mirrors `vault::search`'s private
/// `extract_host`): strip scheme, drop path/query, drop `www.`. Returns the
/// input lowercased when it is not URL-shaped (so non-URL sources still
/// bucket by exact value).
fn source_host(source: &str) -> String {
    if source.is_empty() {
        return String::new();
    }
    let stripped = source
        .strip_prefix("https://")
        .or_else(|| source.strip_prefix("http://"));
    match stripped {
        Some(rest) => {
            let host = rest.split('/').next().unwrap_or(rest);
            let host = host.split('?').next().unwrap_or(host);
            host.strip_prefix("www.").unwrap_or(host).to_lowercase()
        }
        None => source.to_lowercase(),
    }
}

fn tally(stats: &mut GraphStats, edges: &[Edge]) {
    for e in edges {
        match e.kind.as_str() {
            KIND_SEMANTIC => stats.semantic += 1,
            KIND_WIKILINK => stats.wikilink += 1,
            KIND_SHARED_TAG => stats.shared_tag += 1,
            KIND_SHARED_CREATOR | KIND_SHARED_SOURCE | KIND_SHARED_DOMAIN => stats.metadata += 1,
            _ => {}
        }
    }
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Daemon tick: run an incremental graph pass. Mirrors the embed tick shape;
/// the daemon wraps it in `block_in_place`.
pub fn daemon_tick(vault_root: &Path, config: &Config) -> Result<GraphStats> {
    run(vault_root, config, &GraphOpts { backfill: false })
}

#[cfg(test)]
mod tests;
