//! MCP server implementation for oracle

use crate::config::{Config, ExcludeConfig, RerankConfig, RetrievalConfig};
use crate::tools::*;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use serde_json::json;
use std::sync::Mutex;
use tracing::{debug, info, warn};
use vault::detail::{self, DetailLevel};
use vault::ledger;
use vault::schema::{Domain, Method, NoteType, Origin, Status};
use vault::search::{NoteRow, Reranker, SearchIndex};

/// Default hops for graph-expansion modes when the caller omits `expand_hops`.
const DEFAULT_EXPAND_HOPS: u8 = 1;
/// Hard cap on graph-expansion hops (bounds traversal cost on the read path).
const MAX_EXPAND_HOPS: u8 = 2;
/// Per-hop decay applied to expansion scores so distant neighbors rank lower.
/// 0.5 ≈ one effective hop. Feeds the graph rank list (an ordering into RRF).
const GRAPH_HOP_DECAY: f32 = 0.5;
/// Char budget for the candidate text sent to the cross-encoder reranker. The
/// tokenizer truncates to 512 tokens anyway; this bounds memory before that.
const RERANK_TEXT_MAX_CHARS: usize = 2000;

/// Oracle MCP server - knowledge retrieval from an Obsidian vault
#[derive(Clone)]
pub struct OracleMcpServer {
    config: Config,
    db: std::sync::Arc<Mutex<SearchIndex>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl OracleMcpServer {
    pub fn new(config: Config, db: SearchIndex) -> Self {
        info!("Creating OracleMcpServer");
        let tool_router = Self::tool_router();
        debug!("Tool router created with {} tools", tool_router.list_all().len());
        Self {
            config,
            db: std::sync::Arc::new(Mutex::new(db)),
            tool_router,
        }
    }

    /// Get a clone of the database handle for use in background tasks.
    pub fn db_handle(&self) -> std::sync::Arc<Mutex<SearchIndex>> {
        std::sync::Arc::clone(&self.db)
    }

    /// List all registered tools (for `oracle call --list`).
    pub fn list_tools() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    /// Dispatch a tool call directly, bypassing MCP transport.
    pub async fn dispatch(&self, name: &str, args: serde_json::Value) -> Result<CallToolResult, McpError> {
        match name {
            "knowledge_search" => {
                let req: KnowledgeSearchRequest =
                    serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.knowledge_search(Parameters(req)).await
            }
            "note_read" => {
                let req: NoteReadRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.note_read(Parameters(req)).await
            }
            "list_notes" => {
                let req: ListNotesRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.list_notes(Parameters(req)).await
            }
            "vault_overview" => {
                let req: VaultOverviewRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.vault_overview(Parameters(req)).await
            }
            "domain_brief" => {
                let req: DomainBriefRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.domain_brief(Parameters(req)).await
            }
            "ingest_history" => {
                let req: IngestHistoryRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.ingest_history(Parameters(req)).await
            }
            "failure_history" => {
                let req: FailureHistoryRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.failure_history(Parameters(req)).await
            }
            "schema_info" => {
                let req: SchemaInfoRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.schema_info(Parameters(req)).await
            }
            "reindex" => {
                let req: ReindexRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.reindex(Parameters(req)).await
            }
            "tag_search" => {
                let req: TagSearchRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.tag_search(Parameters(req)).await
            }
            "find_similar" => {
                let req: FindSimilarRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.find_similar(Parameters(req)).await
            }
            "recent_activity" => {
                let req: RecentActivityRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.recent_activity(Parameters(req)).await
            }
            "find_links" => {
                let req: FindLinksRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.find_links(Parameters(req)).await
            }
            "creator_browse" => {
                let req: CreatorBrowseRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.creator_browse(Parameters(req)).await
            }
            "source_browse" => {
                let req: SourceBrowseRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.source_browse(Parameters(req)).await
            }
            "inbox_status" => {
                let req: InboxStatusRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.inbox_status(Parameters(req)).await
            }
            "quality_report" => {
                let req: QualityReportRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.quality_report(Parameters(req)).await
            }
            "duplicate_groups" => {
                let req: DuplicateGroupsRequest =
                    serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.duplicate_groups(Parameters(req)).await
            }
            "classify_status" => {
                let req: ClassifyStatusRequest = serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.classify_status(Parameters(req)).await
            }
            _ => Err(McpError::invalid_params(
                format!("unknown tool: {name} (use oracle call --list)"),
                None,
            )),
        }
    }

    fn deser_err(tool: &str, e: &serde_json::Error) -> McpError {
        McpError::invalid_params(format!("{tool}: {e}"), None)
    }

    fn err(e: impl std::fmt::Display) -> McpError {
        warn!("Tool error: {}", e);
        McpError::internal_error(e.to_string(), None)
    }

    /// Look up `NoteRow`s for an ordered list of paths. Preserves order
    /// (callers pass an RRF-ranked list) and silently skips paths that
    /// no longer resolve (a note may have been deleted between embed
    /// time and query time).
    fn resolve_note_paths<'a, I>(db: &::vault::search::SearchIndex, paths: I) -> Result<Vec<NoteRow>, McpError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut out = Vec::new();
        for path in paths {
            match db.get_note(path).map_err(Self::err)? {
                Some(note) => out.push(note),
                None => warn!("knowledge_search: vector hit references missing note: {path}"),
            }
        }
        Ok(out)
    }

    /// Emit a one-shot WARN the first time a vector / hybrid call comes
    /// back empty against a vault that has no embedded notes at all.
    /// The intent is to surface "cortex embed has not run on this DB"
    /// without flooding the log when the user genuinely searches for
    /// something with no hits.
    fn warn_if_no_embeddings<T>(&self, db: &::vault::search::SearchIndex, hits: &[T]) -> Result<(), McpError> {
        if !hits.is_empty() {
            return Ok(());
        }
        let count = db.count_embeddings(None).map_err(Self::err)?;
        if count == 0 {
            // log once per process via the AtomicBool below
            use std::sync::atomic::{AtomicBool, Ordering};
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                warn!(
                    "knowledge_search: vault has zero embeddings; run `cortex embed --backfill` to populate them. \
                     Pure-BM25 queries (mode=bm25) still work."
                );
            }
        }
        Ok(())
    }

    /// BM25 (FTS5) retriever primitive: a ranked list of note paths, top `k`.
    /// Shared by `run_search_mode` (legacy modes) and `run_pipeline` (configured
    /// pipeline) so there is exactly one BM25 query path.
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

        // Stage 1 - query transform (HyDE / multi-query). oracle owns this
        // LLM-bearing stage (it shells to `vault::fabric`); the rewritten
        // query/queries are passed *down* into the vault retriever primitives.
        // Fails open to the original query so a flaky LLM never breaks search.
        let queries: Vec<String> = if cfg.query_transform.enabled {
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
        };

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

    /// Format a NoteRow according to the requested detail level
    fn format_note(note: &NoteRow, detail_level: &DetailLevel) -> serde_json::Value {
        let metadata = json!({
            "path": note.path,
            "title": note.title,
            "domain": note.domain,
            "type": note.note_type,
            "origin": note.origin,
            "status": note.status,
            "date": note.date,
            "tags": serde_json::from_str::<Vec<String>>(&note.tags).unwrap_or_default(),
            "source": note.source,
            "creator": note.creator,
        });

        match detail_level {
            DetailLevel::Metadata => metadata,
            DetailLevel::Tldr => {
                let tldr = if !note.summary.is_empty() {
                    detail::first_sentence(&note.summary)
                } else {
                    detail::first_sentence(&note.body)
                };
                let mut obj = metadata;
                if let Some(map) = obj.as_object_mut() {
                    map.insert("tldr".to_string(), json!(tldr));
                }
                obj
            }
            DetailLevel::Summary => {
                let summary = if !note.summary.is_empty() {
                    note.summary.clone()
                } else {
                    detail::extract_summary(&note.body)
                };
                let mut obj = metadata;
                if let Some(map) = obj.as_object_mut() {
                    map.insert("summary".to_string(), json!(summary));
                }
                obj
            }
            DetailLevel::Full => {
                let mut obj = metadata;
                if let Some(map) = obj.as_object_mut() {
                    map.insert("body".to_string(), json!(note.body));
                }
                obj
            }
        }
    }
}

#[tool_router]
impl OracleMcpServer {
    /// Search the vault's ingested knowledge.
    #[tool(
        description = "Search the vault's ingested knowledge. Modes: bm25 (FTS5 keyword search), vector (semantic, brute-force cosine over embeddings), or hybrid (BM25 + vector fused via RRF; default). Filter by domain, note type, or status. Control content verbosity with the detail parameter: metadata, tldr, summary, full."
    )]
    async fn knowledge_search(&self, params: Parameters<KnowledgeSearchRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        if req.query.trim().is_empty() {
            return Err(Self::err("query is empty"));
        }
        let detail_level = req.detail.unwrap_or(DetailLevel::Summary);
        let limit = req.limit.unwrap_or(10);

        let db = self.db.lock().map_err(Self::err)?;
        let domain = req.domain.as_ref().map(|d| d.as_str());
        let note_type = req.note_type.as_ref().map(|t| t.as_str());
        let status = req.status.as_ref().map(|s| s.as_str());

        // Precedence: explicit per-call `mode` -> legacy single-mode path;
        // no `mode` -> the operator-configured pipeline (`run_pipeline`).
        // Before this change `None` defaulted to `Hybrid`; the configured
        // default is now vector-first (eval-best). `mode: hybrid` is still
        // available per-call for exact back-compat.
        let notes = match req.mode {
            Some(mode) => self.run_search_mode(
                &db,
                mode,
                &req.query,
                domain,
                note_type,
                status,
                limit,
                req.expand_hops.unwrap_or(DEFAULT_EXPAND_HOPS).min(MAX_EXPAND_HOPS),
                req.edge_kinds.as_deref(),
                req.min_edge_weight.unwrap_or(0.0),
            )?,
            None => self.run_pipeline(
                &db,
                &self.config.retrieval,
                &req.query,
                domain,
                note_type,
                status,
                limit,
            )?,
        };

        let mode_label = match req.mode {
            Some(SearchMode::Bm25) => "bm25",
            Some(SearchMode::Vector) => "vector",
            Some(SearchMode::Hybrid) => "hybrid",
            Some(SearchMode::Graph) => "graph",
            Some(SearchMode::GraphHybrid) => "graph-hybrid",
            None => "configured",
        };

        let results: Vec<serde_json::Value> = notes.iter().map(|n| Self::format_note(n, &detail_level)).collect();

        Ok(CallToolResult::success(vec![Content::json(json!({
            "count": results.len(),
            "mode": mode_label,
            "results": results,
        }))?]))
    }

    /// Read a specific note by its vault-relative path
    #[tool(
        description = "Read a specific note by its vault-relative path. Returns the note at the requested detail level: metadata, tldr, summary, or full."
    )]
    async fn note_read(&self, params: Parameters<NoteReadRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let detail_level = req.detail.unwrap_or(DetailLevel::Full);

        let db = self.db.lock().map_err(Self::err)?;
        match db.get_note(&req.path).map_err(Self::err)? {
            Some(note) => {
                // Explicit human-intent read - the only signal we count.
                // knowledge_search does NOT bump; see the load-bearing
                // regression test `knowledge_search_does_not_bump_access`.
                if let Err(e) = db.bump_access(&req.path) {
                    warn!(path = %req.path, error = %e, "bump_access failed");
                }
                let result = Self::format_note(&note, &detail_level);
                Ok(CallToolResult::success(vec![Content::json(&result)?]))
            }
            None => Ok(CallToolResult::success(vec![Content::json(json!({
                "found": false,
                "kind": "note",
                "path": req.path,
                "message": "Note not found",
            }))?])),
        }
    }

    /// List notes with optional filters
    #[tool(
        description = "List notes with optional filters by domain, note type, status, or date range. Unlike knowledge_search, this does not require a search query - use it to browse by category."
    )]
    async fn list_notes(&self, params: Parameters<ListNotesRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let detail_level = req.detail.unwrap_or(DetailLevel::Metadata);
        let limit = req.limit.unwrap_or(20);

        let db = self.db.lock().map_err(Self::err)?;
        let notes = db
            .list_notes(
                req.domain.as_ref().map(|d| d.as_str()),
                req.note_type.as_ref().map(|t| t.as_str()),
                req.status.as_ref().map(|s| s.as_str()),
                req.after.as_deref(),
                req.before.as_deref(),
                Some(limit),
            )
            .map_err(Self::err)?;

        let results: Vec<serde_json::Value> = notes.iter().map(|n| Self::format_note(n, &detail_level)).collect();

        Ok(CallToolResult::success(vec![Content::json(json!({
            "count": results.len(),
            "results": results,
        }))?]))
    }

    /// Get an overview of the vault
    #[tool(
        description = "Get an overview of the entire vault - total note count, distribution by domain, note type, and status, plus schema gaps showing notes with missing fields."
    )]
    async fn vault_overview(&self, _params: Parameters<VaultOverviewRequest>) -> Result<CallToolResult, McpError> {
        let db = self.db.lock().map_err(Self::err)?;
        let stats = db.stats().map_err(Self::err)?;
        Ok(CallToolResult::success(vec![Content::json(&stats)?]))
    }

    /// Get a briefing on a specific knowledge domain
    #[tool(
        description = "Get a briefing on a specific knowledge domain - total notes, unread count, starred count, type breakdown, and recent notes."
    )]
    async fn domain_brief(&self, params: Parameters<DomainBriefRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let detail_level = req.detail.unwrap_or(DetailLevel::Tldr);

        let db = self.db.lock().map_err(Self::err)?;
        let brief = db.domain_brief(req.domain.as_str(), req.limit).map_err(Self::err)?;

        let results: Vec<serde_json::Value> = brief
            .recent
            .iter()
            .map(|n| Self::format_note(n, &detail_level))
            .collect();

        Ok(CallToolResult::success(vec![Content::json(json!({
            "domain": brief.domain,
            "total_notes": brief.total_notes,
            "unread": brief.unread,
            "starred": brief.starred,
            "by_type": brief.by_type,
            "results": results,
        }))?]))
    }

    /// Query the borg ingest ledger
    #[tool(
        description = "Query the borg ingest ledger for ingestion history. Filter by source URL, domain, or date range."
    )]
    async fn ingest_history(&self, params: Parameters<IngestHistoryRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let ledger_path = ledger::ledger_path().map_err(Self::err)?;

        let filter = vault::ledger::EntryFilter {
            source: req.source,
            domain: req.domain.map(|d| d.as_str().to_string()),
            before: req.before,
            after: req.after,
        };

        let entries = ledger::query_entries(&ledger_path, &filter).map_err(Self::err)?;

        let results: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                json!({
                    "date": e.date,
                    "method": e.method,
                    "slug": e.slug,
                    "filename": e.filename,
                    "source": e.source,
                    "domain": e.domain,
                })
            })
            .collect();

        Ok(CallToolResult::success(vec![Content::json(json!({
            "count": results.len(),
            "results": results,
        }))?]))
    }

    /// Query the borg receipts log for failure history.
    ///
    /// Counterpart to `ingest_history`: that tool reads `borg-ledger.md`
    /// (success-only after Phase 4 of the receipts-log refactor); this tool
    /// reads the SQLite receipts DB and surfaces failures with their
    /// `failure_stage` taxonomy. Opens the DB read-only.
    #[tool(
        description = "Query the borg receipts log for failed ingests. Filter by failure_stage, method, source pattern, since timestamp."
    )]
    async fn failure_history(&self, params: Parameters<FailureHistoryRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let path = vault::receipts::receipts_db_path().map_err(Self::err)?;
        if !path.exists() {
            return Ok(CallToolResult::success(vec![
                Content::json(json!({
                    "count": 0,
                    "results": [],
                    "note": "receipts DB does not exist yet",
                }))
                .map_err(Self::err)?,
            ]));
        }
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|e| Self::err(eyre::eyre!("open receipts DB: {e}")))?;

        let mut sql = String::from(
            "SELECT trace_id, received_at, method, kind, raw_input, status, \
                    terminal_at, note_path, failure_stage, failure_reason, replay_of \
             FROM receipts WHERE status='failed'",
        );
        let mut bound: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(stage) = &req.stage {
            sql.push_str(" AND failure_stage=?");
            bound.push(stage.clone().into());
        }
        if let Some(method) = &req.method {
            sql.push_str(" AND method=?");
            bound.push(method.clone().into());
        }
        if let Some(pat) = &req.source {
            sql.push_str(" AND raw_input LIKE ?");
            bound.push(pat.clone().into());
        }
        if let Some(since) = &req.since {
            sql.push_str(" AND received_at >= ?");
            bound.push(since.clone().into());
        }
        sql.push_str(" ORDER BY received_at DESC LIMIT ?");
        let limit = req.limit.unwrap_or(50) as i64;
        bound.push(limit.into());

        let mut stmt = conn.prepare(&sql).map_err(|e| Self::err(eyre::eyre!("prepare: {e}")))?;
        let rows_iter = stmt
            .query_map(
                rusqlite::params_from_iter(bound.iter()),
                |row| -> rusqlite::Result<serde_json::Value> {
                    Ok(json!({
                        "trace_id":       row.get::<_, String>(0)?,
                        "received_at":    row.get::<_, String>(1)?,
                        "method":         row.get::<_, String>(2)?,
                        "kind":           row.get::<_, String>(3)?,
                        "raw_input":      row.get::<_, String>(4)?,
                        "status":         row.get::<_, String>(5)?,
                        "terminal_at":    row.get::<_, Option<String>>(6)?,
                        "note_path":      row.get::<_, Option<String>>(7)?,
                        "failure_stage":  row.get::<_, Option<String>>(8)?,
                        "failure_reason": row.get::<_, Option<String>>(9)?,
                        "replay_of":      row.get::<_, Option<String>>(10)?,
                    }))
                },
            )
            .map_err(|e| Self::err(eyre::eyre!("query_map: {e}")))?;

        let mut results: Vec<serde_json::Value> = Vec::new();
        for r in rows_iter {
            results.push(r.map_err(|e| Self::err(eyre::eyre!("row: {e}")))?);
        }

        Ok(CallToolResult::success(vec![
            Content::json(json!({
                "count": results.len(),
                "results": results,
            }))
            .map_err(Self::err)?,
        ]))
    }

    /// List all valid schema values
    #[tool(
        description = "List all valid schema values - domains, note types, origins, statuses, and ingest methods. Use this to understand what filter values are available."
    )]
    async fn schema_info(&self, _params: Parameters<SchemaInfoRequest>) -> Result<CallToolResult, McpError> {
        let domains: Vec<&str> = Domain::all().iter().map(|d| d.as_str()).collect();
        let note_types: Vec<&str> = NoteType::all().iter().map(|t| t.as_str()).collect();
        let origins: Vec<&str> = Origin::all().iter().map(|o| o.as_str()).collect();
        let statuses: Vec<&str> = Status::all().iter().map(|s| s.as_str()).collect();
        let methods: Vec<&str> = Method::all().iter().map(|m| m.as_str()).collect();

        Ok(CallToolResult::success(vec![Content::json(json!({
            "domains": domains,
            "note_types": note_types,
            "origins": origins,
            "statuses": statuses,
            "methods": methods,
        }))?]))
    }

    /// Trigger a reindex of the vault
    #[tool(
        description = "Trigger a reindex of the vault into the SQLite database. Only updates notes whose files have changed since the last index."
    )]
    async fn reindex(&self, _params: Parameters<ReindexRequest>) -> Result<CallToolResult, McpError> {
        let vault_root = self.config.vault_root().map_err(Self::err)?;
        let db = self.db.lock().map_err(Self::err)?;
        let stats = db.index_vault(&vault_root).map_err(Self::err)?;
        Ok(CallToolResult::success(vec![Content::json(&stats)?]))
    }

    /// Search notes by tag or list all tags
    #[tool(
        description = "Search notes by tag, or list all tags with counts when no tag is specified. Supports exact match and prefix match (append * to tag). Filter by domain."
    )]
    async fn tag_search(&self, params: Parameters<TagSearchRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let db = self.db.lock().map_err(Self::err)?;

        match req.tag {
            Some(tag) => {
                let detail_level = req.detail.unwrap_or(DetailLevel::Metadata);
                let notes = db
                    .tag_search(&tag, req.domain.as_ref().map(|d| d.as_str()), req.limit)
                    .map_err(Self::err)?;

                let results: Vec<serde_json::Value> =
                    notes.iter().map(|n| Self::format_note(n, &detail_level)).collect();

                Ok(CallToolResult::success(vec![Content::json(json!({
                    "tag": tag,
                    "count": results.len(),
                    "results": results,
                }))?]))
            }
            None => {
                let stats = db.tag_stats().map_err(Self::err)?;
                let limit = req.limit.unwrap_or(50) as usize;
                let stats: Vec<&vault::search::TagStat> = stats.iter().take(limit).collect();
                let tags: Vec<serde_json::Value> = stats
                    .iter()
                    .map(|s| {
                        json!({
                            "tag": s.tag,
                            "count": s.count,
                            "domains": s.domains,
                        })
                    })
                    .collect();

                Ok(CallToolResult::success(vec![Content::json(json!({
                    "count": tags.len(),
                    "results": tags,
                }))?]))
            }
        }
    }

    /// Find notes similar to given content or another note
    #[tool(
        description = "Find notes similar to given text content or another note. Provide either content text or a note path. Uses FTS5 term extraction for similarity matching."
    )]
    async fn find_similar(&self, params: Parameters<FindSimilarRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let detail_level = req.detail.unwrap_or(DetailLevel::Tldr);
        let limit = req.limit.unwrap_or(5);

        let db = self.db.lock().map_err(Self::err)?;

        let content = match (&req.content, &req.path) {
            (Some(c), _) => c.clone(),
            (None, Some(p)) => match db.get_note(p).map_err(Self::err)? {
                Some(note) => note.body,
                None => {
                    return Ok(CallToolResult::success(vec![Content::json(json!({
                        "found": false,
                        "kind": "note",
                        "path": p,
                        "message": "Note not found",
                    }))?]));
                }
            },
            (None, None) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    "Provide either 'content' or 'path' parameter",
                )]));
            }
        };

        let mut notes = db.find_similar(&content, limit as usize).map_err(Self::err)?;

        // Filter by domain if requested
        if let Some(ref domain) = req.domain {
            let d = domain.as_str();
            notes.retain(|n| n.domain == d);
        }

        // Exclude the source note if searching by path
        if let Some(ref path) = req.path {
            notes.retain(|n| n.path != *path);
        }

        let results: Vec<serde_json::Value> = notes.iter().map(|n| Self::format_note(n, &detail_level)).collect();

        Ok(CallToolResult::success(vec![Content::json(json!({
            "count": results.len(),
            "results": results,
        }))?]))
    }

    /// Get recent vault activity across domains
    #[tool(
        description = "Cross-domain timeline of recent vault activity. Shows notes added or modified in the last N days. Filter by domain or note type."
    )]
    async fn recent_activity(&self, params: Parameters<RecentActivityRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let detail_level = req.detail.unwrap_or(DetailLevel::Tldr);

        let db = self.db.lock().map_err(Self::err)?;
        let notes = db
            .recent_notes(
                req.days,
                req.domain.as_ref().map(|d| d.as_str()),
                req.note_type.as_ref().map(|t| t.as_str()),
                req.limit,
            )
            .map_err(Self::err)?;

        let days = req.days.unwrap_or(7);
        let results: Vec<serde_json::Value> = notes.iter().map(|n| Self::format_note(n, &detail_level)).collect();

        Ok(CallToolResult::success(vec![Content::json(json!({
            "days": days,
            "count": results.len(),
            "results": results,
        }))?]))
    }

    /// Wikilink graph traversal for a note
    #[tool(
        description = "Traverse the wikilink graph for a note. Shows outbound links (what this note links to), inbound links (what links to this note), and whether the note is an orphan."
    )]
    async fn find_links(&self, params: Parameters<FindLinksRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let detail_level = req.detail.unwrap_or(DetailLevel::Metadata);
        let direction = req.direction.as_deref().unwrap_or("both");

        let db = self.db.lock().map_err(Self::err)?;

        let note = db.get_note(&req.path).map_err(Self::err)?;
        let (title, path) = match note {
            Some(n) => (n.title.clone(), n.path.clone()),
            None => {
                return Ok(CallToolResult::success(vec![Content::json(json!({
                    "found": false,
                    "kind": "note",
                    "path": req.path,
                    "message": "Note not found",
                }))?]));
            }
        };

        let mut result = json!({
            "note": { "path": path, "title": title },
        });

        if direction == "outbound" || direction == "both" {
            let outbound = db.find_outbound_links(&req.path).map_err(Self::err)?;
            let outbound_json: Vec<serde_json::Value> = outbound
                .iter()
                .map(|l| {
                    json!({
                        "target": l.target,
                        "resolved_path": l.resolved_path,
                        "exists": l.exists,
                    })
                })
                .collect();
            result["outbound"] = json!(outbound_json);
        }

        if direction == "inbound" || direction == "both" {
            let inbound = db.find_inbound_links(&req.path).map_err(Self::err)?;
            let inbound_json: Vec<serde_json::Value> =
                inbound.iter().map(|n| Self::format_note(n, &detail_level)).collect();
            result["inbound"] = json!(inbound_json);
            result["orphan"] = json!(inbound.is_empty());
        }

        Ok(CallToolResult::success(vec![Content::json(&result)?]))
    }

    /// Browse notes by creator/channel
    #[tool(
        description = "Browse notes by creator or channel. When creator is provided, returns matching notes. When omitted, lists all creators with counts."
    )]
    async fn creator_browse(&self, params: Parameters<CreatorBrowseRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let db = self.db.lock().map_err(Self::err)?;

        match req.creator {
            Some(creator) => {
                let detail_level = req.detail.unwrap_or(DetailLevel::Metadata);
                let notes = db
                    .notes_by_creator(&creator, req.domain.as_ref().map(|d| d.as_str()), req.limit)
                    .map_err(Self::err)?;
                let results: Vec<serde_json::Value> =
                    notes.iter().map(|n| Self::format_note(n, &detail_level)).collect();

                Ok(CallToolResult::success(vec![Content::json(json!({
                    "creator": creator,
                    "count": results.len(),
                    "results": results,
                }))?]))
            }
            None => {
                let stats = db.creator_stats().map_err(Self::err)?;
                let limit = req.limit.unwrap_or(50) as usize;
                let creators: Vec<serde_json::Value> = stats
                    .iter()
                    .take(limit)
                    .map(|(name, count)| {
                        json!({
                            "name": name,
                            "count": count,
                        })
                    })
                    .collect();

                Ok(CallToolResult::success(vec![Content::json(json!({
                    "count": creators.len(),
                    "results": creators,
                }))?]))
            }
        }
    }

    /// Browse notes by source URL domain
    #[tool(
        description = "Browse notes by source URL domain. When host is provided, returns matching notes. When omitted, lists all source domains with counts."
    )]
    async fn source_browse(&self, params: Parameters<SourceBrowseRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let db = self.db.lock().map_err(Self::err)?;

        match req.host {
            Some(host) => {
                let detail_level = req.detail.unwrap_or(DetailLevel::Metadata);
                let notes = db
                    .notes_by_source_domain(&host, req.domain.as_ref().map(|d| d.as_str()), req.limit)
                    .map_err(Self::err)?;
                let results: Vec<serde_json::Value> =
                    notes.iter().map(|n| Self::format_note(n, &detail_level)).collect();

                Ok(CallToolResult::success(vec![Content::json(json!({
                    "host": host,
                    "count": results.len(),
                    "results": results,
                }))?]))
            }
            None => {
                let stats = db.source_domain_stats().map_err(Self::err)?;
                let limit = req.limit.unwrap_or(50) as usize;
                let sources: Vec<serde_json::Value> = stats
                    .iter()
                    .take(limit)
                    .map(|(host, count)| {
                        json!({
                            "host": host,
                            "count": count,
                        })
                    })
                    .collect();

                Ok(CallToolResult::success(vec![Content::json(json!({
                    "count": sources.len(),
                    "results": sources,
                }))?]))
            }
        }
    }

    /// View inbox contents and classification pipeline health
    #[tool(
        description = "View inbox contents, notes needing review, and classification pipeline health. Shows inbox count, review candidates, and classified notes."
    )]
    async fn inbox_status(&self, params: Parameters<InboxStatusRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let detail_level = req.detail.unwrap_or(DetailLevel::Tldr);

        let db = self.db.lock().map_err(Self::err)?;
        let inbox = db.inbox_notes(req.limit).map_err(Self::err)?;
        let review = db.notes_needing_review(req.limit).map_err(Self::err)?;

        let inbox_results: Vec<serde_json::Value> = inbox.iter().map(|n| Self::format_note(n, &detail_level)).collect();
        let review_results: Vec<serde_json::Value> =
            review.iter().map(|n| Self::format_note(n, &detail_level)).collect();

        let classified: u64 = inbox.iter().filter(|n| !n.domain.is_empty()).count() as u64;

        Ok(CallToolResult::success(vec![Content::json(json!({
            "inbox_count": inbox_results.len(),
            "needs_review": review_results.len(),
            "classified": classified,
            "results": inbox_results,
            "review_candidates": review_results,
        }))?]))
    }

    /// Notes by quality score and common issues
    #[tool(
        description = "View note quality distribution and browse notes by quality level. Shows quality score distribution and notes filtered by quality."
    )]
    async fn quality_report(&self, params: Parameters<QualityReportRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let detail_level = req.detail.unwrap_or(DetailLevel::Tldr);

        let db = self.db.lock().map_err(Self::err)?;
        let distribution = db.quality_distribution().map_err(Self::err)?;

        let results = if let Some(quality) = req.quality {
            let notes = db.notes_by_quality(&quality, req.limit).map_err(Self::err)?;
            notes.iter().map(|n| Self::format_note(n, &detail_level)).collect()
        } else {
            vec![]
        };

        Ok(CallToolResult::success(vec![Content::json(json!({
            "distribution": distribution.iter().map(|(q, c)| json!({"quality": q, "count": c})).collect::<Vec<_>>(),
            "results": results,
        }))?]))
    }

    /// Browse duplicate note clusters
    #[tool(
        description = "Browse duplicate note clusters identified by cortex. List all groups or inspect a specific group."
    )]
    async fn duplicate_groups(&self, params: Parameters<DuplicateGroupsRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let db = self.db.lock().map_err(Self::err)?;
        let groups = db.duplicate_groups().map_err(Self::err)?;

        match req.group_id {
            Some(gid) => {
                let group = groups.into_iter().find(|g| g.group_id == gid);
                match group {
                    Some(g) => Ok(CallToolResult::success(vec![Content::json(&g)?])),
                    None => Ok(CallToolResult::success(vec![Content::json(json!({
                        "found": false,
                        "kind": "duplicate_group",
                        "group_id": gid,
                        "message": "Duplicate group not found",
                    }))?])),
                }
            }
            None => {
                let limit = req.limit.unwrap_or(10) as usize;
                let groups: Vec<serde_json::Value> = groups
                    .iter()
                    .take(limit)
                    .map(|g| {
                        json!({
                            "group_id": g.group_id,
                            "note_count": g.note_count,
                            "titles": g.notes.iter().map(|n| &n.title).collect::<Vec<_>>(),
                        })
                    })
                    .collect();

                Ok(CallToolResult::success(vec![Content::json(json!({
                    "count": groups.len(),
                    "groups": groups,
                }))?]))
            }
        }
    }

    /// Classification pipeline health and metadata
    #[tool(
        description = "View classification pipeline statistics - total classified, method breakdown, confidence distribution, domain assignments, inbox count, and pending reviews."
    )]
    async fn classify_status(&self, params: Parameters<ClassifyStatusRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let db = self.db.lock().map_err(Self::err)?;
        let stats = db
            .classify_stats(req.domain.as_ref().map(|d| d.as_str()))
            .map_err(Self::err)?;

        Ok(CallToolResult::success(vec![Content::json(&stats)?]))
    }
}

#[tool_handler]
impl ServerHandler for OracleMcpServer {
    fn get_info(&self) -> ServerInfo {
        info!("MCP client requested server info");
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Oracle - knowledge retrieval MCP for a second-brain Obsidian vault. \
             Search ingested knowledge by domain, type, or full-text query. knowledge_search \
             defaults to hybrid mode (BM25 + vector embeddings fused via RRF); pass mode=bm25 \
             for pure keyword search or mode=vector for pure semantic similarity. \
             Control content verbosity with the 'detail' parameter: \
             metadata (fields only), tldr (one-liner), summary (summary section), full (complete body). \
             Use vault_overview for the big picture, domain_brief for domain-specific intelligence, \
             and knowledge_search for targeted queries."
                .to_string(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Bm25Method, ExcludeConfig, MethodsConfig, RetrievalConfig, VaultConfig, VectorMethod};
    use serde_json::json;
    use std::path::PathBuf;
    use vault::frontmatter::Frontmatter;
    use vault::note::Note;
    use vault::search::SearchIndex;

    fn seed_one_article(db: &SearchIndex, path: &str, title: &str, body: &str) {
        let fm = Frontmatter {
            title: Some(title.to_string()),
            note_type: Some("article".to_string()),
            origin: Some("assisted".to_string()),
            domain: Some("ai".to_string()),
            ..Frontmatter::default()
        };
        let note = Note {
            path: PathBuf::from(path),
            frontmatter: fm,
            body: body.to_string(),
            raw: format!("---\n---\n{body}"),
        };
        db.index_one(&note, 100).expect("seed note");
    }

    /// Load-bearing regression guard for the decay model.
    ///
    /// `knowledge_search` must NOT bump `search_hit_count` / `last_accessed_at`.
    /// Counting BM25 / hybrid matches as access creates a positive feedback
    /// loop where high-scoring notes become immortal and the entire decay
    /// premise collapses (parent roadmap: "high-BM25-scoring notes become
    /// immortal and the entire decay premise collapses"). Only an explicit
    /// `note_read` is a human-intent signal. If a future refactor adds a
    /// `bump_access` call into `knowledge_search`, this test must fail.
    #[tokio::test]
    async fn knowledge_search_does_not_bump_access() {
        let db = SearchIndex::open_memory().expect("open db");
        seed_one_article(
            &db,
            "notes/ai/transformer.md",
            "Transformer",
            "Transformer attention mechanism.",
        );
        let server = OracleMcpServer::new(Config::default(), db);

        // BM25 mode avoids the embedding-model lookup; the rule we are
        // guarding applies to every mode of knowledge_search.
        let search_args = json!({"query": "transformer", "mode": "bm25"});
        let result = server
            .dispatch("knowledge_search", search_args)
            .await
            .expect("knowledge_search dispatch");
        assert_ne!(result.is_error, Some(true), "knowledge_search returned an error");

        let signals_after_search = {
            let db = server.db.lock().expect("lock");
            db.note_signals("notes/ai/transformer.md")
                .expect("signals")
                .expect("present")
        };
        assert_eq!(
            signals_after_search.0, 0,
            "knowledge_search must not bump search_hit_count",
        );
        assert!(
            signals_after_search.1.is_none(),
            "knowledge_search must not stamp last_accessed_at",
        );

        // Now note_read MUST bump.
        let read_args = json!({"path": "notes/ai/transformer.md"});
        let result = server
            .dispatch("note_read", read_args)
            .await
            .expect("note_read dispatch");
        assert_ne!(result.is_error, Some(true), "note_read returned an error");

        let signals_after_read = {
            let db = server.db.lock().expect("lock");
            db.note_signals("notes/ai/transformer.md")
                .expect("signals")
                .expect("present")
        };
        assert_eq!(signals_after_read.0, 1, "note_read must bump search_hit_count");
        assert!(signals_after_read.1.is_some(), "note_read must stamp last_accessed_at",);
    }

    /// Decode a CallToolResult's first content item as a JSON value. All the
    /// list-tools we test below serialize their response via Content::json,
    /// which the rmcp `Content::json` constructor stores as RawContent::Text
    /// (per rmcp 1.x).
    fn first_content_as_json(result: &rmcp::model::CallToolResult) -> serde_json::Value {
        let text = result
            .content
            .first()
            .expect("response has at least one content item")
            .as_text()
            .expect("content[0] is text-shaped JSON")
            .text
            .clone();
        serde_json::from_str(&text).expect("content text is valid JSON")
    }

    /// Phase 2 invariant: every list-shaped tool's response is keyed on
    /// `results`. After the clean rename, the legacy keys (`tags`,
    /// `creators`, `sources`, `recent`) must be absent.
    #[tokio::test]
    async fn tag_search_no_arg_returns_results_key() {
        let db = SearchIndex::open_memory().expect("open db");
        seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
        let server = OracleMcpServer::new(Config::default(), db);

        let result = server.dispatch("tag_search", json!({})).await.expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert!(v.get("results").is_some(), "tag_search must expose `results`: {v}");
        assert!(v.get("count").is_some(), "tag_search must expose `count`: {v}");
        assert!(
            v.get("tags").is_none(),
            "legacy `tags` key must be gone (clean rename, no aliases): {v}"
        );
    }

    #[tokio::test]
    async fn creator_browse_no_arg_returns_results_key() {
        let db = SearchIndex::open_memory().expect("open db");
        seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
        let server = OracleMcpServer::new(Config::default(), db);

        let result = server.dispatch("creator_browse", json!({})).await.expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert!(v.get("results").is_some(), "creator_browse must expose `results`: {v}");
        assert!(v.get("count").is_some());
        assert!(v.get("creators").is_none(), "legacy `creators` key must be gone: {v}");
    }

    #[tokio::test]
    async fn source_browse_no_arg_returns_results_key() {
        let db = SearchIndex::open_memory().expect("open db");
        seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
        let server = OracleMcpServer::new(Config::default(), db);

        let result = server.dispatch("source_browse", json!({})).await.expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert!(v.get("results").is_some(), "source_browse must expose `results`: {v}");
        assert!(v.get("count").is_some());
        assert!(v.get("sources").is_none(), "legacy `sources` key must be gone: {v}");
    }

    #[tokio::test]
    async fn domain_brief_returns_results_key() {
        let db = SearchIndex::open_memory().expect("open db");
        seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
        let server = OracleMcpServer::new(Config::default(), db);

        let result = server
            .dispatch("domain_brief", json!({"domain": "ai"}))
            .await
            .expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert!(v.get("results").is_some(), "domain_brief must expose `results`: {v}");
        assert!(v.get("recent").is_none(), "legacy `recent` key must be gone: {v}");
        assert!(
            v.get("recent_notes").is_none(),
            "legacy `recent_notes` key (per design doc) must be gone: {v}"
        );
        // unread is u64, not Option<u64>, so it must serialize as a number, never null.
        assert!(
            v.get("unread").is_some_and(|u| u.is_number()),
            "domain_brief.unread must be a number, never null: {v}"
        );
    }

    /// D2: missing-note paths should return a structured `{found: false, ...}`
    /// payload, not a free-text string. The CallToolResult must NOT set
    /// `is_error: true` (MCP `isError` is reserved for protocol-level
    /// failures, not domain-level "no row matched").
    #[tokio::test]
    async fn note_read_missing_path_returns_found_false() {
        let db = SearchIndex::open_memory().expect("open db");
        let server = OracleMcpServer::new(Config::default(), db);

        let result = server
            .dispatch("note_read", json!({"path": "notes/does-not-exist.md"}))
            .await
            .expect("dispatch");
        assert_ne!(
            result.is_error,
            Some(true),
            "domain not-found must not set is_error: true",
        );
        let v = first_content_as_json(&result);
        assert_eq!(v.get("found").and_then(|f| f.as_bool()), Some(false), "{v}");
        assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("note"), "{v}");
        assert_eq!(
            v.get("path").and_then(|p| p.as_str()),
            Some("notes/does-not-exist.md"),
            "{v}"
        );
    }

    #[tokio::test]
    async fn find_similar_missing_path_returns_found_false() {
        let db = SearchIndex::open_memory().expect("open db");
        let server = OracleMcpServer::new(Config::default(), db);

        let result = server
            .dispatch("find_similar", json!({"path": "notes/does-not-exist.md"}))
            .await
            .expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert_eq!(v.get("found").and_then(|f| f.as_bool()), Some(false), "{v}");
        assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("note"), "{v}");
    }

    /// D2: invalid arguments (neither `content` nor `path`) IS a protocol-level
    /// failure - the tool can't execute. This branch should set
    /// `is_error: true` so MCP agents know the call itself failed.
    #[tokio::test]
    async fn find_similar_missing_args_returns_is_error() {
        let db = SearchIndex::open_memory().expect("open db");
        let server = OracleMcpServer::new(Config::default(), db);

        let result = server.dispatch("find_similar", json!({})).await.expect("dispatch");
        assert_eq!(
            result.is_error,
            Some(true),
            "invalid args must be a protocol-level error",
        );
    }

    #[tokio::test]
    async fn find_links_missing_path_returns_found_false() {
        let db = SearchIndex::open_memory().expect("open db");
        let server = OracleMcpServer::new(Config::default(), db);

        let result = server
            .dispatch("find_links", json!({"path": "notes/does-not-exist.md"}))
            .await
            .expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert_eq!(v.get("found").and_then(|f| f.as_bool()), Some(false), "{v}");
        assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("note"), "{v}");
    }

    /// D4: ingest_history should return its rows under the canonical
    /// `results` key, not the legacy `entries` key. The Phase 2 design
    /// classified ingest_history as "per-tool object - unchanged," but the
    /// shakedown showed it's really a list-of-things tool that should
    /// follow the same convention as tag_search/source_browse/etc.
    #[tokio::test]
    async fn ingest_history_returns_results_key() {
        // ingest_history needs a vault root to locate the ledger; query_entries
        // returns an empty Vec when the ledger file doesn't exist, so a bare
        // tempdir with a `.obsidian/` marker is sufficient.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".obsidian")).expect("mkdir .obsidian");
        let config = Config {
            vault: VaultConfig {
                root_path: Some(tmp.path().to_string_lossy().into_owned()),
            },
            ..Config::default()
        };
        let db = SearchIndex::open_memory().expect("open db");
        let server = OracleMcpServer::new(config, db);

        let result = server.dispatch("ingest_history", json!({})).await.expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert!(v.get("results").is_some(), "ingest_history must expose `results`: {v}");
        assert!(v.get("count").is_some(), "ingest_history must expose `count`: {v}");
        assert!(v.get("entries").is_none(), "legacy `entries` key must be gone: {v}");
    }

    /// D4: inbox_status should rename `notes` -> `results`. The other keys
    /// (inbox_count, needs_review, classified, review_candidates) stay -
    /// they're semantic counters and a secondary list, not "the list" the
    /// tool is named for.
    #[tokio::test]
    async fn inbox_status_returns_results_key() {
        let db = SearchIndex::open_memory().expect("open db");
        let server = OracleMcpServer::new(Config::default(), db);

        let result = server.dispatch("inbox_status", json!({})).await.expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert!(v.get("results").is_some(), "inbox_status must expose `results`: {v}");
        assert!(v.get("notes").is_none(), "legacy `notes` key must be gone: {v}");
        // Secondary keys must remain.
        assert!(v.get("inbox_count").is_some(), "{v}");
        assert!(v.get("needs_review").is_some(), "{v}");
        assert!(v.get("classified").is_some(), "{v}");
        assert!(v.get("review_candidates").is_some(), "{v}");
    }

    #[tokio::test]
    async fn duplicate_groups_missing_group_id_returns_found_false() {
        let db = SearchIndex::open_memory().expect("open db");
        let server = OracleMcpServer::new(Config::default(), db);

        let result = server
            .dispatch("duplicate_groups", json!({"group_id": "no-such-group"}))
            .await
            .expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert_eq!(v.get("found").and_then(|f| f.as_bool()), Some(false), "{v}");
        assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("duplicate_group"), "{v}");
    }

    /// A retrieval config with only BM25 enabled. Lets the pipeline tests run
    /// without loading the real embedding model (the vector path needs it).
    fn bm25_only_config() -> Config {
        let retrieval = RetrievalConfig {
            methods: MethodsConfig {
                vector: VectorMethod {
                    enabled: false,
                    ..Default::default()
                },
                bm25: Bm25Method {
                    enabled: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        Config {
            retrieval,
            ..Default::default()
        }
    }

    /// Phase 2: a `knowledge_search` with no `mode` routes to `run_pipeline`
    /// (reported as `mode: "configured"`) and returns the configured retrievers'
    /// results. Uses a bm25-only pipeline to avoid the embedding-model load.
    #[tokio::test]
    async fn knowledge_search_no_mode_routes_to_configured_pipeline() {
        let db = SearchIndex::open_memory().expect("open db");
        seed_one_article(
            &db,
            "notes/ai/transformer.md",
            "Transformer",
            "Transformer attention mechanism.",
        );
        let server = OracleMcpServer::new(bm25_only_config(), db);

        let result = server
            .dispatch("knowledge_search", json!({"query": "transformer"}))
            .await
            .expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert_eq!(
            v.get("mode").and_then(|m| m.as_str()),
            Some("configured"),
            "no-mode call must route to the configured pipeline: {v}"
        );
        assert!(
            v.get("count").and_then(|c| c.as_u64()).unwrap_or(0) >= 1,
            "bm25 pipeline must find the seeded note: {v}"
        );
    }

    /// Explicit `mode` still uses the legacy single-mode path and reports that
    /// mode's label (back-compat preserved).
    #[tokio::test]
    async fn knowledge_search_explicit_mode_reports_that_mode() {
        let db = SearchIndex::open_memory().expect("open db");
        seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
        let server = OracleMcpServer::new(Config::default(), db);

        let result = server
            .dispatch("knowledge_search", json!({"query": "transformer", "mode": "bm25"}))
            .await
            .expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert_eq!(v.get("mode").and_then(|m| m.as_str()), Some("bm25"), "{v}");
    }

    /// Seed an article with a cortex `quality` level (empty string = unscored)
    /// and a chosen body, so the exclude-filter tests can drive the `quality`
    /// column and body length without the embedding model.
    fn seed_with_quality(db: &SearchIndex, path: &str, title: &str, body: &str, quality: &str) {
        let mut extra = std::collections::HashMap::new();
        if !quality.is_empty() {
            extra.insert(
                "cortex-quality".to_string(),
                serde_yaml::Value::String(quality.to_string()),
            );
        }
        let fm = Frontmatter {
            title: Some(title.to_string()),
            note_type: Some("article".to_string()),
            origin: Some("assisted".to_string()),
            domain: Some("ai".to_string()),
            extra,
            ..Frontmatter::default()
        };
        let note = Note {
            path: PathBuf::from(path),
            frontmatter: fm,
            body: body.to_string(),
            raw: format!("---\n---\n{body}"),
        };
        db.index_one(&note, 100).expect("seed note");
    }

    fn bm25_config_with_exclude(exclude: ExcludeConfig) -> Config {
        let mut cfg = bm25_only_config();
        cfg.retrieval.exclude = exclude;
        cfg
    }

    /// Phase 3: the stub filter (on by default) drops a `quality=low` note from
    /// the results while keeping a `quality=high` note that matches the query.
    #[tokio::test]
    async fn pipeline_stub_filter_drops_low_quality() {
        let db = SearchIndex::open_memory().expect("open db");
        seed_with_quality(
            &db,
            "notes/ai/good.md",
            "Good transformer",
            "transformer attention good",
            "high",
        );
        seed_with_quality(
            &db,
            "notes/ai/stub.md",
            "Stub transformer",
            "transformer attention stub",
            "low",
        );
        // bm25_only_config keeps exclude at its default (stub = true).
        let server = OracleMcpServer::new(bm25_only_config(), db);

        let result = server
            .dispatch(
                "knowledge_search",
                json!({"query": "transformer", "detail": "metadata"}),
            )
            .await
            .expect("dispatch");
        let v = first_content_as_json(&result);
        let paths: Vec<String> = v["results"]
            .as_array()
            .expect("results array")
            .iter()
            .map(|r| r["path"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(
            paths.contains(&"notes/ai/good.md".to_string()),
            "high-quality kept: {paths:?}"
        );
        assert!(
            !paths.contains(&"notes/ai/stub.md".to_string()),
            "low-quality stub must be dropped: {paths:?}"
        );
    }

    /// Phase 3: with `exclude.stub = false`, the same low-quality note survives.
    #[tokio::test]
    async fn pipeline_stub_filter_disabled_keeps_low_quality() {
        let db = SearchIndex::open_memory().expect("open db");
        seed_with_quality(
            &db,
            "notes/ai/stub.md",
            "Stub transformer",
            "transformer attention stub",
            "low",
        );
        let cfg = bm25_config_with_exclude(ExcludeConfig {
            stub: false,
            min_body_chars: 0,
        });
        let server = OracleMcpServer::new(cfg, db);

        let result = server
            .dispatch(
                "knowledge_search",
                json!({"query": "transformer", "detail": "metadata"}),
            )
            .await
            .expect("dispatch");
        let v = first_content_as_json(&result);
        let paths: Vec<String> = v["results"]
            .as_array()
            .expect("results array")
            .iter()
            .map(|r| r["path"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(
            paths.contains(&"notes/ai/stub.md".to_string()),
            "stub filter off must keep low-quality: {paths:?}"
        );
    }

    /// Phase 3: `min_body_chars` drops a note whose body is shorter than the
    /// threshold and keeps a longer one.
    #[tokio::test]
    async fn pipeline_min_body_chars_drops_short_body() {
        let db = SearchIndex::open_memory().expect("open db");
        // Short body (< 50 chars) and a long one; both match the bm25 query.
        seed_with_quality(&db, "notes/ai/short.md", "Short", "transformer", "high");
        seed_with_quality(
            &db,
            "notes/ai/long.md",
            "Long",
            "transformer attention mechanism explained at length for retrieval testing purposes",
            "high",
        );
        let cfg = bm25_config_with_exclude(ExcludeConfig {
            stub: false,
            min_body_chars: 50,
        });
        let server = OracleMcpServer::new(cfg, db);

        let result = server
            .dispatch(
                "knowledge_search",
                json!({"query": "transformer", "detail": "metadata"}),
            )
            .await
            .expect("dispatch");
        let v = first_content_as_json(&result);
        let paths: Vec<String> = v["results"]
            .as_array()
            .expect("results array")
            .iter()
            .map(|r| r["path"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(
            paths.contains(&"notes/ai/long.md".to_string()),
            "long body kept: {paths:?}"
        );
        assert!(
            !paths.contains(&"notes/ai/short.md".to_string()),
            "short body must be dropped: {paths:?}"
        );
    }

    /// Phase 2: a pipeline with every retriever disabled returns no results
    /// (and does not error) - the degenerate operator config.
    #[tokio::test]
    async fn run_pipeline_no_methods_enabled_returns_empty() {
        let db = SearchIndex::open_memory().expect("open db");
        seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
        // vector off, bm25/graph already off by default => nothing enabled.
        let retrieval = RetrievalConfig {
            methods: MethodsConfig {
                vector: VectorMethod {
                    enabled: false,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let cfg = Config {
            retrieval,
            ..Default::default()
        };
        let server = OracleMcpServer::new(cfg, db);

        let result = server
            .dispatch("knowledge_search", json!({"query": "transformer"}))
            .await
            .expect("dispatch");
        assert_ne!(result.is_error, Some(true));
        let v = first_content_as_json(&result);
        assert_eq!(
            v.get("count").and_then(|c| c.as_u64()),
            Some(0),
            "no methods enabled must yield zero results: {v}"
        );
    }
}
