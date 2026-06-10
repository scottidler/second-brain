//! MCP server implementation for oracle

use crate::config::Config;
use crate::tools::*;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use serde_json::json;
use std::sync::Mutex;
use tracing::{debug, info, warn};
use vault::detail::{self, DetailLevel};
use vault::ledger;
use vault::schema::{Domain, Method, NoteType, Origin, Status};
use vault::search::{NoteRow, SearchIndex};

mod pipeline;

/// Default hops for graph-expansion modes when the caller omits `expand_hops`.
const DEFAULT_EXPAND_HOPS: u8 = 1;
/// Hard cap on graph-expansion hops (bounds traversal cost on the read path).
const MAX_EXPAND_HOPS: u8 = 2;
/// Per-hop decay applied to expansion scores so distant neighbors rank lower.
/// 0.5 ≈ one effective hop. Feeds the graph rank list (an ordering into RRF).
const GRAPH_HOP_DECAY: f32 = 0.5;
/// `find_similar` over-fetch multiplier: when a post-filter (domain / self
/// exclusion) is active, fetch this many times `limit` candidates so filtering
/// can't shrink the result below `limit` (or to zero when matches exist).
const FIND_SIMILAR_OVERFETCH: usize = 5;
/// Char budget for the candidate text sent to the cross-encoder reranker. The
/// tokenizer truncates to 512 tokens anyway; this bounds memory before that.
const RERANK_TEXT_MAX_CHARS: usize = 2000;

/// Oracle MCP server - knowledge retrieval from an Obsidian vault
#[derive(Clone)]
pub struct OracleMcpServer {
    config: Config,
    db: std::sync::Arc<Mutex<SearchIndex>>,
}

impl OracleMcpServer {
    pub fn new(config: Config, db: SearchIndex) -> Self {
        info!("Creating OracleMcpServer");
        // `#[tool_handler]` resolves the router via `Self::tool_router()` (the
        // associated fn the `#[tool_router]` macro generates), not a stored
        // field - so no `tool_router` field is kept. Log the count for parity
        // with startup diagnostics.
        debug!(
            "Tool router created with {} tools",
            Self::tool_router().list_all().len()
        );
        Self {
            config,
            db: std::sync::Arc::new(Mutex::new(db)),
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

    /// Run CPU-bound, lock-holding work with `block_in_place` on a
    /// multi-thread runtime (production: `sb oracle serve` under
    /// `#[tokio::main]`), and inline on a current-thread runtime
    /// (`#[tokio::test]`, where `block_in_place` panics).
    fn block_in_place_compat<F, R>(f: F) -> R
    where
        F: FnOnce() -> R,
    {
        use tokio::runtime::{Handle, RuntimeFlavor};
        match Handle::try_current().map(|h| h.runtime_flavor()) {
            Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(f),
            _ => f(),
        }
    }

    fn err(e: impl std::fmt::Display) -> McpError {
        warn!("Tool error: {}", e);
        McpError::internal_error(e.to_string(), None)
    }

    /// Caller-fault error (bad argument value), distinct from `err`'s
    /// server-fault `internal_error`. An empty query is the caller's mistake.
    fn invalid(e: impl std::fmt::Display) -> McpError {
        warn!("Tool invalid params: {}", e);
        McpError::invalid_params(e.to_string(), None)
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
        description = "Search the vault's ingested knowledge. Omit mode (the common case) to run the operator-configured pipeline (vector-first by default, eval-best). Mode overrides force a single path: bm25 (FTS5 keyword search), vector (semantic, brute-force cosine over embeddings), or hybrid (BM25 + vector fused via RRF). Filter by domain, note type, or status. Control content verbosity with the detail parameter: metadata, tldr, summary, full."
    )]
    async fn knowledge_search(&self, params: Parameters<KnowledgeSearchRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        if req.query.trim().is_empty() {
            return Err(Self::invalid("query is empty"));
        }
        let detail_level = req.detail.unwrap_or(DetailLevel::Summary);
        let limit = req.limit.unwrap_or(10);

        let domain = req.domain.as_ref().map(|d| d.as_str());
        let note_type = req.note_type.as_ref().map(|t| t.as_str());
        let status = req.status.as_ref().map(|s| s.as_str());

        // Query transform (LLM, no DB) runs BEFORE the lock so a slow/flaky
        // transform can't freeze every other MCP tool behind the global
        // SearchIndex mutex. Only the configured (no-`mode`) path transforms.
        let pre_queries: Option<Vec<String>> = match req.mode {
            None => Some(self.transform_queries(&self.config.retrieval, &req.query)),
            Some(_) => None,
        };

        // Precedence: explicit per-call `mode` -> legacy single-mode path;
        // no `mode` -> the operator-configured pipeline (`run_pipeline`).
        // Before this change `None` defaulted to `Hybrid`; the configured
        // default is now vector-first (eval-best). `mode: hybrid` is still
        // available per-call for exact back-compat.
        //
        // The lock-holding retrieval (embedding inference, brute-force cosine,
        // rerank) is CPU-bound and synchronous; `block_in_place` tells tokio so
        // it can keep servicing other tasks on its worker pool instead of
        // wedging the whole runtime behind one slow query.
        let notes = Self::block_in_place_compat(|| -> Result<Vec<NoteRow>, McpError> {
            let db = self.db.lock().map_err(Self::err)?;
            match req.mode {
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
                ),
                None => self.run_pipeline(
                    &db,
                    &self.config.retrieval,
                    &req.query,
                    pre_queries.as_deref().unwrap_or_default(),
                    domain,
                    note_type,
                    status,
                    limit,
                ),
            }
        })?;

        let mode_label = req.mode.map(crate::eval::mode_label).unwrap_or("configured");

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

        // The ledger is chronological (oldest first), so the most-recent
        // `limit` rows are the tail. Bound the response: the full ledger can be
        // many thousand rows and would otherwise land in one MCP payload.
        let limit = req.limit.unwrap_or(50) as usize;
        let skip = entries.len().saturating_sub(limit);

        let results: Vec<serde_json::Value> = entries
            .iter()
            .skip(skip)
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

        // Over-fetch a candidate pool BEFORE the post-filters so a selective
        // domain filter or the self-note exclusion cannot shrink the result
        // below `limit` (the previous fetch-exactly-`limit` could return 0 with
        // matches present). With no post-filter active the pool is exactly `limit`.
        let filtering = req.domain.is_some() || req.path.is_some();
        let fetch = if filtering {
            (limit as usize)
                .saturating_mul(FIND_SIMILAR_OVERFETCH)
                .saturating_add(1)
        } else {
            limit as usize
        };
        let mut notes = db.find_similar(&content, fetch).map_err(Self::err)?;

        // Filter by domain if requested
        if let Some(ref domain) = req.domain {
            let d = domain.as_str();
            notes.retain(|n| n.domain == d);
        }

        // Exclude the source note if searching by path
        if let Some(ref path) = req.path {
            notes.retain(|n| n.path != *path);
        }

        // Truncate the over-fetched pool back down to the requested limit.
        notes.truncate(limit as usize);

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
        let direction = req.direction.unwrap_or(LinkDirection::Both);

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

        if matches!(direction, LinkDirection::Outbound | LinkDirection::Both) {
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

        if matches!(direction, LinkDirection::Inbound | LinkDirection::Both) {
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
            let notes = db.notes_by_quality(quality.as_str(), req.limit).map_err(Self::err)?;
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
             with no mode runs the operator-configured pipeline (vector-first by default, \
             eval-best); pass mode=bm25 for pure keyword search, mode=vector for pure semantic \
             similarity, or mode=hybrid to force BM25 + vector fused via RRF. \
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
mod tests;
