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
/// Note -> repo hub membership (harvest-clyde-sessions design, Phase 10).
const KIND_REPO_MEMBER: &str = "repo-member";
/// Note -> creator hub membership (entity-hub-two-vector-synthesis, Phase 1).
const KIND_CREATOR_MEMBER: &str = "creator-member";
/// Note -> source-host hub membership (entity-hub-two-vector-synthesis, Phase 1).
const KIND_SOURCE_MEMBER: &str = "source-member";
/// Hub membership is a strong deterministic signal (unlike a rarity-weighted
/// shared tag), so every `*-member` kind rides at full weight.
const MEMBER_WEIGHT: f32 = 1.0;

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
    /// note -> `entities/repos/<org>/<repo>.md` membership edges.
    pub repo_member: usize,
    /// note -> `entities/<creator-slug>.md` membership edges.
    pub creator_member: usize,
    /// note -> `entities/<source-host-slug>.md` membership edges.
    pub source_member: usize,
    pub skipped: usize,
}

/// Outcome of the typed-`fact` layer pass (triple extraction + consolidation).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FactLayerStats {
    pub facts_written: usize,
    pub noise_removed: usize,
    pub contradictions: usize,
    pub bridges_added: usize,
}

/// Run the graph pass against the oracle index DB. Opens its own connection
/// (cortex commands do not share oracle's `Mutex<SearchIndex>`), takes the
/// shared embed lock, and writes edges in per-note bounded transactions.
pub fn run(vault_root: &Path, config: &Config, opts: &GraphOpts) -> Result<GraphStats> {
    log::debug!("cortex::graph::run: backfill={}", opts.backfill);

    let db_path = config.oracle_db_path();
    let mut index = SearchIndex::open(&db_path)
        .wrap_err_with(|| format!("failed to open search index at {}", db_path.display()))?;

    // Serialize against any concurrent `cortex embed` write: the pass reads
    // `note_embeddings` and must not interleave with an embed batch.
    let lock = crate::embed::acquire_lock()?;
    log::debug!("cortex::graph: acquired embed file lock");

    let stats = build(&mut index, &config.graph, opts.backfill)?;

    // Phase 5: --backfill also extracts typed `fact` edges (bounded, LLM) and
    // runs the consolidation agents. Deterministic edges above stand alone; the
    // factual layer is layered on top only on an explicit backfill. The embed
    // lock is already held, so call the lock-agnostic helper directly.
    if opts.backfill {
        extract_fact_layer(&mut index, vault_root, config)?;
    }

    drop(lock);
    log::info!(
        "graph complete: full_rebuild={} notes={} semantic={} wikilink={} shared_tag={} metadata={} repo_member={} creator_member={} source_member={} skipped={}",
        stats.full_rebuild,
        stats.notes_processed,
        stats.semantic,
        stats.wikilink,
        stats.shared_tag,
        stats.metadata,
        stats.repo_member,
        stats.creator_member,
        stats.source_member,
        stats.skipped,
    );
    Ok(stats)
}

/// Extract typed `fact` edges (bounded LLM triple extraction) and run the
/// consolidation agents (noise removal / contradiction flagging / cluster
/// bridging) against an already-open index. The caller MUST already hold the
/// shared embed lock — both `run` (under `--backfill`) and `fact_backfill` take
/// it before calling — so this helper does not manage the lock and is therefore
/// safe to reuse without re-entrant locking.
fn extract_fact_layer(index: &mut SearchIndex, vault_root: &Path, config: &Config) -> Result<FactLayerStats> {
    log::debug!(
        "cortex::graph::extract_fact_layer: fact_max_per_run={} fact_pattern={}",
        config.graph.fact_max_per_run,
        config.graph.fact_pattern,
    );
    let notes = crate::vault::scan_vault(vault_root, &config.vault)?;
    let extractor = crate::memgraph::FabricTripleExtractor {
        fabric: &config.fabric,
        pattern: &config.graph.fact_pattern,
        max_input_tokens: config.graph.fact_max_input_tokens,
        timeout_secs: config.graph.fact_timeout_secs,
    };
    let facts =
        crate::memgraph::extract_facts(index, &notes, &extractor, &config.graph, config.graph.fact_max_per_run)?;
    let consolidation = crate::memgraph::consolidate(index, &config.graph)?;
    let stats = FactLayerStats {
        facts_written: facts.facts_written,
        noise_removed: consolidation.noise_removed,
        contradictions: consolidation.contradictions.len(),
        bridges_added: consolidation.bridges_added,
    };
    log::info!(
        "memgraph fact layer: facts_written={} noise_removed={} contradictions={} bridges_added={}",
        stats.facts_written,
        stats.noise_removed,
        stats.contradictions,
        stats.bridges_added,
    );
    Ok(stats)
}

/// Daemon entry point for the scheduled fact-backfill tick: refresh ONLY the
/// typed `fact` layer. The deterministic edges are maintained incrementally by
/// the separate `graph_interval` tick (`daemon_tick`), so this pass skips the
/// deterministic rebuild and runs just triple extraction + consolidation. Opens
/// its own index connection and takes the shared embed lock IN-PROCESS, so it
/// serializes against the daemon's embed/graph ticks via the same lock rather
/// than colliding the way a separate-process systemd timer would.
pub fn fact_backfill(vault_root: &Path, config: &Config) -> Result<FactLayerStats> {
    log::debug!("cortex::graph::fact_backfill: vault_root={}", vault_root.display());
    let db_path = config.oracle_db_path();
    let mut index = SearchIndex::open(&db_path)
        .wrap_err_with(|| format!("failed to open search index at {}", db_path.display()))?;
    let lock = crate::embed::acquire_lock()?;
    log::debug!("cortex::graph::fact_backfill: acquired embed file lock");
    let stats = extract_fact_layer(&mut index, vault_root, config)?;
    drop(lock);
    Ok(stats)
}

/// Core edge-build loop, factored out of `run` so tests can drive an
/// in-memory index without the file lock / config-path machinery.
pub fn build(index: &mut SearchIndex, cfg: &crate::config::GraphConfig, force_full: bool) -> Result<GraphStats> {
    let full_rebuild = force_full || index.graph_state_get(KEY_LAST_RUN_AT)?.is_none();
    log::debug!("cortex::graph::build: full_rebuild={full_rebuild}");

    let rows = index.graph_note_rows()?;
    let by_path: HashMap<String, GraphNoteRow> = rows.iter().map(|r| (r.path.clone(), r.clone())).collect();

    // The shared vocabulary, built once per pass. Same list the auto-linker
    // is gated on (`crate::stopwords`), so a target that can never be
    // written can never mint an edge either.
    let stopwords = crate::stopwords::Stopwords::new(&cfg.wikilink_stopwords);

    // Inverted indexes over the whole corpus (cheap; needed even for
    // incremental targets to find their pair partners).
    let tag_buckets = invert(&rows, |r| r.tags.clone());
    let creator_buckets = invert(&rows, |r| single(&r.creator));
    let source_buckets = invert(&rows, |r| single(&source_bucket_key(&r.source)));
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
            &stopwords,
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
    stopwords: &crate::stopwords::Stopwords,
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

    // --- wikilink (resolved targets only, stopwords dropped) ---
    //
    // The stopword is consulted HERE, on the RAW slug straight out of
    // `extract_wikilinks`, BEFORE `resolve_note_path`. Checking after resolve
    // would be wrong twice over: `resolve_wikilink`'s last fallback is a bare
    // `LIKE '%target%'`, so a stoplisted word can resolve to an arbitrary note,
    // and the resolved PATH no longer carries the word that has to be judged.
    // Case-insensitive so `[[Every]]` cannot slip past a lowercase entry.
    for slug in vault::search::extract_wikilinks(&row.body) {
        if stopwords.contains(&slug) {
            log::trace!("wikilink: dropping stoplisted target {slug:?} in {src}");
            continue;
        }
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
        &source_bucket_key(&row.source),
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

    // --- repo-member (Phase 10 single-repo + Phase 4 multi-repo): note -> repo
    // hub edges. Unlike the shared-* buckets above (note<->note within a bucket,
    // fan-out capped), this is genuinely new routing: EVERY well-formed repo the
    // note anchors to joins that repo's hub via the shared `repo_hub_path`. The
    // set is the note's `repo:` (harvest-clyde Phase 9) UNION every element of
    // `repos-touched` (harvest-completion Phase 4). A malformed slug is skipped +
    // logged (the note is still indexed). Each edge resolves once the hub pass
    // has stubbed `entities/repos/<org>/<repo>.md`; until then insert_edges skips
    // it (resolve-endpoint-or-skip) and the next sweep re-adds it - monotonic.
    // The dst MUST match the hub note's actual nested path (same
    // `repo_hub_path`), or resolve-or-skip drops the edge and the hub synthesizes
    // memberless.
    //
    // Deduped on the resolved hub path (BTreeSet: deterministic + collision-safe)
    // so a repo listed in BOTH `repo` and `repos_touched`, or two `repos_touched`
    // entries that slug-collide, yields exactly one edge - no secondary-repo edge
    // is dropped and no duplicate is emitted. A note carrying `repos-touched
    // [X,Y]` joins hub `repo-<slug(X)>` AND `repo-<slug(Y)>` on every sweep.
    let mut repo_hub_paths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for repo in std::iter::once(&row.repo).chain(row.repos_touched.iter()) {
        if repo.is_empty() {
            continue;
        }
        if vault::schema::validate_repo_slug(repo) {
            repo_hub_paths.insert(crate::hub::repo_hub_path(repo));
        } else {
            log::warn!("graph: note {src} has malformed repo slug {repo:?} - skipping repo-member edge");
        }
    }
    for hub_path in repo_hub_paths {
        edges.push(Edge::deterministic(
            src.clone(),
            hub_path,
            KIND_REPO_MEMBER,
            MEMBER_WEIGHT,
        ));
    }

    // --- creator-member: note -> creator hub (entity-hub-two-vector-synthesis,
    // Phase 1). Same shape as repo-member: linear note->hub routing, NOT a
    // note<->note bucket, so `fanout_cap` deliberately does NOT apply. The cap
    // exists to stop quadratic pairwise blow-up in `metadata_edges`; copying it
    // here would emit NOTHING for exactly the largest creator hubs, which is the
    // opposite of what a membership primitive is for. The over-cap hub routing
    // at the shared-tag block above is the precedent this follows.
    //
    // The dst is byte-identical to `HubStub::hub_path()` for a Creator stub
    // because that stub's slug IS `slugify(creator)` (`hub.rs` `collect_stubs`).
    // A creator whose slug is empty (punctuation-only) mints no hub, so it emits
    // no edge rather than pointing at `entities/.md`.
    if !row.creator.is_empty() {
        let slug = crate::hub::slugify(&row.creator);
        if slug.is_empty() {
            log::trace!(
                "creator-member: creator {:?} on {src} slugifies empty; no edge",
                row.creator
            );
        } else {
            edges.push(Edge::deterministic(
                src.clone(),
                format!("{}/{}.md", crate::hub::HUB_DIR, slug),
                KIND_CREATOR_MEMBER,
                MEMBER_WEIGHT,
            ));
        }
    }

    // --- source-member: note -> source-host hub (same phase, same no-cap
    // reasoning; `www.youtube.com` alone holds >1000 notes and is the single
    // host over the cap, so a copied cap would zero the largest source hub).
    //
    // `hub::source_hub_path` is the ONE function that turns a `source:` value
    // into a hub path — the stub side reads the same host through
    // `hub::source_host` — so this dst can never drift from the minted hub.
    // `None` (schemeless: the 261 `clyde://` sessions and 21 provenance markers)
    // emits no edge: `collect_stubs` cannot mint those hubs, so the edge would be
    // dropped forever by resolve-or-skip. Sessions get membership via
    // `repo-member`.
    if let Some(hub_path) = crate::hub::source_hub_path(&row.source) {
        edges.push(Edge::deterministic(
            src.clone(),
            hub_path,
            KIND_SOURCE_MEMBER,
            MEMBER_WEIGHT,
        ));
    }

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

/// Bucket key for the note<->note `shared-source` layer: the HOST when the
/// source is URL-shaped, else the raw value lowercased (a non-URL source still
/// buckets by exact value, which is what makes co-provenance markers group).
///
/// Host extraction itself is delegated to `vault::search::extract_host` — the
/// single host implementation, also behind `hub::source_hub_path` — so this
/// layer and the `source-member` hub layer can never disagree on what a host is.
/// The URL-shaped/schemeless SPLIT stays here because it is this layer's own
/// policy, not a fact about hosts: the hub layer must skip schemeless input, the
/// bucket layer must keep it.
fn source_bucket_key(source: &str) -> String {
    match source
        .strip_prefix("https://")
        .or_else(|| source.strip_prefix("http://"))
    {
        // URL-shaped: the host, or empty (no bucket) when there is no host.
        Some(_) => vault::search::extract_host(source).unwrap_or_default(),
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
            // Explicit arms, not the catch-all: the `_ => {}` below hid
            // `repo-member` from every run report since Phase 10 shipped.
            KIND_REPO_MEMBER => stats.repo_member += 1,
            KIND_CREATOR_MEMBER => stats.creator_member += 1,
            KIND_SOURCE_MEMBER => stats.source_member += 1,
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
