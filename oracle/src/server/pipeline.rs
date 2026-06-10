//! Configured-retrieval-pipeline machinery for the oracle MCP server.
//!
//! Extracted from `server.rs` (which exceeded the 1500-line bloat gate): the
//! BM25 / vector / graph retrievers, the fixed-stage `run_pipeline`
//! (transform -> retrieve -> fuse -> rerank -> exclude -> truncate), the
//! legacy `run_search_mode` dispatch, and the rerank/exclude/graph helpers.
//! These are `impl OracleMcpServer` methods living in a child module; they
//! reach `server.rs`'s private consts and sibling helper methods directly.

use super::{GRAPH_HOP_DECAY, OracleMcpServer, RERANK_TEXT_MAX_CHARS};
use crate::config::{ExcludeConfig, RerankConfig, RetrievalConfig};
use crate::tools::SearchMode;
use rmcp::ErrorData as McpError;
use tracing::{debug, warn};
use vault::search::{NoteRow, Reranker, SearchIndex};

impl OracleMcpServer {
    fn bm25_paths(
        &self,
        db: &SearchIndex,
        query: &str,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
        k: u32,
    ) -> Result<Vec<String>, McpError> {
        let rows = db
            .search(query, domain, note_type, status, Some(k))
            .map_err(Self::err)?;
        Ok(rows.iter().map(|n| n.path.clone()).collect())
    }

    /// Vector (brute-force cosine) retriever primitive: a ranked list of note
    /// paths, top `k`. Calls `warn_if_no_embeddings` on the `VectorHit` slice
    /// **before** mapping to `Vec<String>` — the warning needs the hit structs,
    /// which are dropped at the path-map boundary, so it cannot be hoisted into
    /// the caller. Shared by `run_search_mode` and `run_pipeline`.
    fn vector_paths(
        &self,
        db: &SearchIndex,
        query: &str,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
        k: u32,
    ) -> Result<Vec<String>, McpError> {
        let active_model = db.active_embedding_model().map_err(Self::err)?;
        let q_vec = vault::embedding::embed_query(query, &active_model).map_err(Self::err)?;
        let hits = db
            .search_vector(&q_vec, k, domain, note_type, status)
            .map_err(Self::err)?;
        self.warn_if_no_embeddings(db, &hits)?;
        Ok(hits.iter().map(|h| h.note_path.clone()).collect())
    }

    /// Graph-expansion primitive: expand `seed_paths` along the materialized
    /// `edges` graph and return the neighbor paths ranked by expansion score
    /// (`Σ w_seed(origin) · edge_weight · hop_decay^(hop-1)`), filtered to the
    /// schema filters. The seed-building and final fusion stay with the caller
    /// (`graph_dispatch` for legacy modes, `run_pipeline` for the configured
    /// graph retriever) so both share the exact same scoring logic.
    #[allow(clippy::too_many_arguments)]
    fn expand_to_graph_paths(
        &self,
        db: &SearchIndex,
        seed_paths: &[String],
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
        hops: u8,
        edge_kinds: Option<&[String]>,
        min_weight: f32,
        hop_decay: f32,
    ) -> Result<Vec<String>, McpError> {
        let seed_rank: std::collections::HashMap<&str, usize> =
            seed_paths.iter().enumerate().map(|(i, p)| (p.as_str(), i)).collect();

        let reaches = db
            .expand_graph(seed_paths, hops, edge_kinds, min_weight)
            .map_err(Self::err)?;
        let mut scores: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
        for reach in &reaches {
            let rank = *seed_rank.get(reach.origin_seed.as_str()).unwrap_or(&0);
            // w_seed: better-ranked seeds (rank 0 = top) contribute more.
            let w_seed = 1.0_f32 / (rank as f32 + 1.0);
            let decay = hop_decay.powi(reach.hop as i32 - 1);
            *scores.entry(reach.path.clone()).or_insert(0.0) += w_seed * reach.weight * decay;
        }

        // Sort by expansion score desc, then path asc as a stable tiebreaker
        // (without it, tied neighbors fall back to random HashMap order).
        let mut scored: Vec<(String, f32)> = scores.into_iter().collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let mut graph_paths: Vec<String> = Vec::new();
        for (path, _) in scored {
            if Self::note_matches_filters(db, &path, domain, note_type, status)? {
                graph_paths.push(path);
            }
        }
        Ok(graph_paths)
    }

    /// Run one search mode against `db` and return resolved `NoteRow`s in rank
    /// order. This is the single dispatch shared by the `knowledge_search` MCP
    /// tool and the `eval` harness, so the eval measures the exact production
    /// retrieval path (no divergent re-implementation). `expand_hops` /
    /// `edge_kinds` / `min_edge_weight` are only consulted by the graph modes.
    #[allow(clippy::too_many_arguments)]
    pub fn run_search_mode(
        &self,
        db: &SearchIndex,
        mode: SearchMode,
        query: &str,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
        limit: u32,
        expand_hops: u8,
        edge_kinds: Option<&[String]>,
        min_edge_weight: f32,
    ) -> Result<Vec<NoteRow>, McpError> {
        match mode {
            SearchMode::Bm25 => db
                .search(query, domain, note_type, status, Some(limit))
                .map_err(Self::err),
            SearchMode::Vector => {
                let paths = self.vector_paths(db, query, domain, note_type, status, limit)?;
                Self::resolve_note_paths(db, paths.iter().map(|p| p.as_str()))
            }
            SearchMode::Hybrid => {
                let bm25_paths = self.bm25_paths(db, query, domain, note_type, status, vault::search::K_RRF_INPUT)?;
                let vec_paths = self.vector_paths(db, query, domain, note_type, status, vault::search::K_RRF_INPUT)?;
                let fused = vault::search::reciprocal_rank_fusion(
                    &[&bm25_paths, &vec_paths],
                    vault::search::RRF_K,
                    limit as usize,
                );
                Self::resolve_note_paths(db, fused.iter().map(|h| h.note_path.as_str()))
            }
            SearchMode::Graph | SearchMode::GraphHybrid => self.graph_dispatch(
                db,
                query,
                domain,
                note_type,
                status,
                limit,
                expand_hops,
                edge_kinds,
                min_edge_weight,
                matches!(mode, SearchMode::GraphHybrid),
            ),
        }
    }

    /// Graph-expansion retrieval shared by `mode=graph` and
    /// `mode=graph-hybrid`.
    ///
    /// 1. Seed via hybrid (BM25 ∪ vector, top `K_RRF_INPUT` each, fused).
    /// 2. Expand the seed set `hops` hops along the materialized `edges` graph
    ///    (`SearchIndex::expand_graph` — the edge read lives in vault; oracle
    ///    never builds edges).
    /// 3. Score each expanded neighbor by `Σ w_seed(origin) · edge_weight ·
    ///    decay^(hop-1)`, then convert to a rank list (RRF consumes order, not
    ///    raw scores). Schema filters apply to the neighbors before scoring.
    /// 4. Fuse: `graph` re-fuses the seed list with the graph list;
    ///    `graph-hybrid` carries the raw BM25 and vector lists in too.
    /// 5. `limit` truncates last.
    #[allow(clippy::too_many_arguments)]
    fn graph_dispatch(
        &self,
        db: &SearchIndex,
        query: &str,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
        limit: u32,
        hops: u8,
        edge_kinds: Option<&[String]>,
        min_weight: f32,
        include_base_lists: bool,
    ) -> Result<Vec<NoteRow>, McpError> {
        let bm25_paths = self.bm25_paths(db, query, domain, note_type, status, vault::search::K_RRF_INPUT)?;
        let vec_paths = self.vector_paths(db, query, domain, note_type, status, vault::search::K_RRF_INPUT)?;

        // Seed list = the hybrid-fused order; seed rank feeds w_seed.
        let seed_fused = vault::search::reciprocal_rank_fusion(
            &[&bm25_paths, &vec_paths],
            vault::search::RRF_K,
            vault::search::K_RRF_INPUT as usize,
        );
        let seed_paths: Vec<String> = seed_fused.iter().map(|h| h.note_path.clone()).collect();

        // Expand and score (edge read lives in vault). Legacy graph modes use
        // the built-in GRAPH_HOP_DECAY; the configured pipeline passes its own.
        let graph_paths = self.expand_to_graph_paths(
            db,
            &seed_paths,
            domain,
            note_type,
            status,
            hops,
            edge_kinds,
            min_weight,
            GRAPH_HOP_DECAY,
        )?;

        // Fuse. graph: seed ⊕ graph. graph-hybrid: bm25 ⊕ vector ⊕ graph.
        let fused = if include_base_lists {
            vault::search::reciprocal_rank_fusion(
                &[&bm25_paths, &vec_paths, &graph_paths],
                vault::search::RRF_K,
                limit as usize,
            )
        } else {
            vault::search::reciprocal_rank_fusion(&[&seed_paths, &graph_paths], vault::search::RRF_K, limit as usize)
        };
        Self::resolve_note_paths(db, fused.iter().map(|h| h.note_path.as_str()))
    }

    /// Run the operator-configured pipeline (`self.config.retrieval`). The
    /// `configured` target of `sb oracle eval` calls this so the eval scores the
    /// exact live pipeline `knowledge_search` runs for a no-`mode` query.
    pub fn run_configured_pipeline(
        &self,
        db: &SearchIndex,
        query: &str,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
        limit: u32,
    ) -> Result<Vec<NoteRow>, McpError> {
        let queries = self.transform_queries(&self.config.retrieval, query);
        self.run_pipeline(
            db,
            &self.config.retrieval,
            query,
            &queries,
            domain,
            note_type,
            status,
            limit,
        )
    }

    /// Stage 1 (query transform) in isolation, with NO DB access. This is the
    /// LLM-bearing step (it shells to `vault::fabric`); it must run BEFORE the
    /// global `SearchIndex` mutex is taken so a slow/flaky transform can't
    /// freeze every other MCP tool call and the watcher task behind the lock.
    /// Fails open to the original query.
    pub fn transform_queries(&self, cfg: &RetrievalConfig, query: &str) -> Vec<String> {
        if cfg.query_transform.enabled {
            match crate::transform::fabric_transform(&cfg.query_transform, query) {
                Ok(qs) if !qs.is_empty() => qs,
                Ok(_) => vec![query.to_string()],
                Err(e) => {
                    warn!("query transform failed; falling back to original query: {e}");
                    vec![query.to_string()]
                }
            }
        } else {
            vec![query.to_string()]
        }
    }

    /// Compose the configured retrieval pipeline for a query that arrived with
    /// no explicit `mode`. Stage order is fixed:
    /// `transform -> retrieve -> fuse -> rerank -> exclude -> truncate`. Each
    /// method/stage is gated by its `enabled` flag in `cfg`. Shares the BM25 /
    /// vector / graph primitives with `run_search_mode`, so the legacy modes and
    /// the configured pipeline never diverge.
    pub fn run_pipeline(
        &self,
        db: &SearchIndex,
        cfg: &RetrievalConfig,
        query: &str,
        queries: &[String],
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
        limit: u32,
    ) -> Result<Vec<NoteRow>, McpError> {
        debug!(
            query = %query,
            vector = cfg.methods.vector.enabled,
            bm25 = cfg.methods.bm25.enabled,
            graph = cfg.methods.graph.enabled,
            rerank = cfg.rerank.enabled,
            transform = cfg.query_transform.enabled,
            limit,
            "run_pipeline"
        );

        // Stage 1 (query transform) already ran in `transform_queries` BEFORE
        // the DB lock was taken; `queries` is its output (the original query
        // verbatim when transform is disabled). The original `query` is kept
        // separately for the rerank stage, which scores against it.

        // Stage 2 - retrieve: one ranked list per enabled method, paired with
        // the fusion weight it contributes (vector has no `weight` field, so it
        // always contributes at weight 1.0). Each method retrieves with every
        // query variant and unions the lists before fusion - the identity for
        // the common single-query case, the multi-query union otherwise.
        let mut lists: Vec<(Vec<String>, f32)> = Vec::new();
        if cfg.methods.vector.enabled {
            let per_variant = queries
                .iter()
                .map(|q| self.vector_paths(db, q, domain, note_type, status, cfg.methods.vector.top_k))
                .collect::<Result<Vec<_>, _>>()?;
            lists.push((crate::transform::union_lists(per_variant), 1.0));
        }
        if cfg.methods.bm25.enabled {
            let per_variant = queries
                .iter()
                .map(|q| self.bm25_paths(db, q, domain, note_type, status, cfg.methods.bm25.top_k))
                .collect::<Result<Vec<_>, _>>()?;
            lists.push((crate::transform::union_lists(per_variant), cfg.methods.bm25.weight));
        }
        if cfg.methods.graph.enabled {
            let per_variant = queries
                .iter()
                .map(|q| self.pipeline_graph_paths(db, cfg, q, domain, note_type, status))
                .collect::<Result<Vec<_>, _>>()?;
            lists.push((crate::transform::union_lists(per_variant), cfg.methods.graph.weight));
        }

        if lists.is_empty() {
            warn!("run_pipeline: no retrieval methods enabled; returning no results");
            return Ok(Vec::new());
        }

        // Candidate pool kept ahead of the final truncate so the later exclude
        // (Phase 3) and rerank (Phase 4) stages have headroom and the result
        // still fills `limit`.
        let candidate_limit = (limit as usize)
            .max(vault::search::K_RRF_INPUT as usize)
            .max(if cfg.rerank.enabled { cfg.rerank.input_k as usize } else { 0 });

        // Stage 3 - fuse. A single enabled method passes through in its own
        // order; more than one fuses via weighted RRF (a zero-weight list
        // contributes nothing, so a demoted retriever stays out of the result).
        let fused_paths: Vec<String> = if lists.len() == 1 {
            lists[0].0.iter().take(candidate_limit).cloned().collect()
        } else {
            let weighted: Vec<(&[String], f32)> = lists.iter().map(|(p, w)| (p.as_slice(), *w)).collect();
            vault::search::reciprocal_rank_fusion_weighted(&weighted, cfg.fusion.k, candidate_limit)
                .into_iter()
                .map(|h| h.note_path)
                .collect()
        };

        // Stage 4 - rerank (cross-encoder): reorder the top `input_k` fused
        // candidates, latency-budgeted with a fail-open probe.
        let fused_paths = if cfg.rerank.enabled {
            self.maybe_rerank(db, &cfg.rerank, query, fused_paths)?
        } else {
            fused_paths
        };

        // Stage 5 - exclude filters: drop stub (quality=low) and short-body
        // notes from the fused candidates, in rank order.
        let kept_paths = self.apply_exclude_filters(db, &fused_paths, &cfg.exclude)?;

        // Stage 6 - truncate to `limit`, resolve to NoteRows in rank order.
        let final_paths: Vec<String> = kept_paths.into_iter().take(limit as usize).collect();
        Self::resolve_note_paths(db, final_paths.iter().map(|p| p.as_str()))
    }

    /// Apply the post-fusion exclude filters to a ranked candidate list,
    /// preserving rank order. `stub` drops notes whose cortex `quality` column
    /// is `low` (the only stub signal in the `notes` table - the richer
    /// `[stub-body]` marker lives in frontmatter, not a queryable column).
    /// `min_body_chars` drops notes whose retrieved body is shorter than the
    /// threshold. With both off the list passes through untouched. A path that
    /// no longer resolves is left in place; `resolve_note_paths` skips it later.
    fn apply_exclude_filters(
        &self,
        db: &SearchIndex,
        paths: &[String],
        cfg: &ExcludeConfig,
    ) -> Result<Vec<String>, McpError> {
        if !cfg.stub && cfg.min_body_chars == 0 {
            return Ok(paths.to_vec());
        }
        let mut kept = Vec::with_capacity(paths.len());
        for path in paths {
            if cfg.stub
                && let Some(q) = db.note_quality(path).map_err(Self::err)?
                && q.eq_ignore_ascii_case("low")
            {
                continue;
            }
            if cfg.min_body_chars > 0
                && let Some(note) = db.get_note(path).map_err(Self::err)?
                && note.body.chars().count() < cfg.min_body_chars
            {
                continue;
            }
            kept.push(path.clone());
        }
        Ok(kept)
    }

    /// Build the graph retriever's ranked list for the configured pipeline:
    /// seed from the hybrid (bm25 + vector) fused order - mirroring the legacy
    /// graph modes - then expand with the operator-configured graph params and
    /// cap at the method's `top-k`. Off by default, so this runs only when the
    /// operator explicitly enables graph; it re-runs bm25/vector for the seed.
    fn pipeline_graph_paths(
        &self,
        db: &SearchIndex,
        cfg: &RetrievalConfig,
        query: &str,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<String>, McpError> {
        let bm25 = self.bm25_paths(db, query, domain, note_type, status, vault::search::K_RRF_INPUT)?;
        let vec = self.vector_paths(db, query, domain, note_type, status, vault::search::K_RRF_INPUT)?;
        let seed =
            vault::search::reciprocal_rank_fusion(&[&bm25, &vec], cfg.fusion.k, vault::search::K_RRF_INPUT as usize);
        let seed_paths: Vec<String> = seed.iter().map(|h| h.note_path.clone()).collect();
        let mut g = self.expand_to_graph_paths(
            db,
            &seed_paths,
            domain,
            note_type,
            status,
            cfg.methods.graph.hops,
            Some(&cfg.methods.graph.edge_kinds),
            cfg.methods.graph.min_edge_weight,
            cfg.methods.graph.hop_decay,
        )?;
        g.truncate(cfg.methods.graph.top_k as usize);
        Ok(g)
    }

    /// Cross-encoder rerank of the top `input_k` fused candidates (stage 4).
    ///
    /// Latency-budgeted with a warmup probe: time one `(query, doc)` pair,
    /// project the batch cost over the available parallelism, and if it exceeds
    /// `latency_budget_ms` no-op the stage **for the process** (fail-open to the
    /// fused order) with a WARN. On the AVX-only daemon host the probe is
    /// expected to trip frequently - that is the documented baseline, not an
    /// error (the cross-encoder is genuinely too slow there). Reranks only the
    /// head; the tail beyond `input_k` keeps its fused order.
    fn maybe_rerank(
        &self,
        db: &SearchIndex,
        cfg: &RerankConfig,
        query: &str,
        fused_paths: Vec<String>,
    ) -> Result<Vec<String>, McpError> {
        use std::sync::atomic::{AtomicBool, Ordering};
        // Once the probe trips (or the model fails to load) in a process, stay
        // off so every subsequent query does not re-pay the probe.
        static RERANK_DISABLED: AtomicBool = AtomicBool::new(false);
        if RERANK_DISABLED.load(Ordering::Relaxed) {
            return Ok(fused_paths);
        }
        let input_k = cfg.input_k as usize;
        if fused_paths.len() <= 1 || input_k == 0 {
            return Ok(fused_paths);
        }

        // Head (reranked) / tail (kept in fused order). Cloned up front so the
        // fail-open branches can return `fused_paths` without borrow conflicts.
        let head_len = input_k.min(fused_paths.len());
        let head_paths: Vec<String> = fused_paths.iter().take(head_len).cloned().collect();
        let tail_paths: Vec<String> = fused_paths.iter().skip(head_len).cloned().collect();

        // Resolve candidate texts (summary preferred, body fallback), truncated.
        let mut items: Vec<(String, String)> = Vec::with_capacity(head_paths.len());
        for path in &head_paths {
            if let Some(note) = db.get_note(path).map_err(Self::err)? {
                let text = if !note.summary.is_empty() { note.summary } else { note.body };
                let text: String = text.chars().take(RERANK_TEXT_MAX_CHARS).collect();
                items.push((path.clone(), text));
            }
        }
        if items.is_empty() {
            return Ok(fused_paths);
        }

        let reranker = match vault::search::get_or_load_reranker(&cfg.model) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    "rerank disabled for this process: failed to load model {}: {e}",
                    cfg.model
                );
                RERANK_DISABLED.store(true, Ordering::Relaxed);
                return Ok(fused_paths);
            }
        };

        // Warmup probe: time one pair, project the full batch over the threads.
        let probe_doc = [items[0].1.as_str()];
        let start = std::time::Instant::now();
        if let Err(e) = reranker.score(query, &probe_doc) {
            warn!("rerank disabled for this process: probe scoring failed: {e}");
            RERANK_DISABLED.store(true, Ordering::Relaxed);
            return Ok(fused_paths);
        }
        let per_pair_ms = start.elapsed().as_secs_f64() * 1000.0;
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        let projected = vault::search::project_batch_ms(per_pair_ms, items.len(), threads);
        if projected > cfg.latency_budget_ms as f64 {
            warn!(
                "rerank disabled for this process: projected {projected:.0} ms for {} pairs \
                 ({per_pair_ms:.0} ms/pair, {threads} threads) exceeds budget {} ms; \
                 falling back to fused order",
                items.len(),
                cfg.latency_budget_ms
            );
            RERANK_DISABLED.store(true, Ordering::Relaxed);
            return Ok(fused_paths);
        }

        // Within budget: rerank the head, then append the untouched tail.
        let mut out = vault::search::rerank_paths(reranker.as_ref(), query, &items).map_err(Self::err)?;
        out.extend(tail_paths);
        Ok(out)
    }

    /// True when the note at `path` matches the (optional) schema filters.
    /// A missing note fails the check (it cannot be a valid result).
    fn note_matches_filters(
        db: &SearchIndex,
        path: &str,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
    ) -> Result<bool, McpError> {
        let Some(note) = db.get_note(path).map_err(Self::err)? else {
            return Ok(false);
        };
        let ok = domain.is_none_or(|d| note.domain == d)
            && note_type.is_none_or(|t| note.note_type == t)
            && status.is_none_or(|s| note.status == s);
        Ok(ok)
    }
}
