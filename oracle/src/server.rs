//! MCP server implementation for oracle

use crate::config::Config;
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
use vault::search::{NoteRow, SearchIndex};

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
        let mode = req.mode.unwrap_or(SearchMode::Hybrid);

        let db = self.db.lock().map_err(Self::err)?;
        let domain = req.domain.as_ref().map(|d| d.as_str());
        let note_type = req.note_type.as_ref().map(|t| t.as_str());
        let status = req.status.as_ref().map(|s| s.as_str());

        let notes = match mode {
            SearchMode::Bm25 => db
                .search(&req.query, domain, note_type, status, Some(limit))
                .map_err(Self::err)?,
            SearchMode::Vector => {
                let active_model = db.active_embedding_model().map_err(Self::err)?;
                let q_vec = vault::embedding::embed_query(&req.query, &active_model).map_err(Self::err)?;
                let hits = db
                    .search_vector(&q_vec, limit, domain, note_type, status)
                    .map_err(Self::err)?;
                self.warn_if_no_embeddings(&db, &hits)?;
                Self::resolve_note_paths(&db, hits.iter().map(|h| h.note_path.as_str()))?
            }
            SearchMode::Hybrid => {
                let active_model = db.active_embedding_model().map_err(Self::err)?;
                let q_vec = vault::embedding::embed_query(&req.query, &active_model).map_err(Self::err)?;
                let bm25 = db
                    .search(&req.query, domain, note_type, status, Some(vault::search::K_RRF_INPUT))
                    .map_err(Self::err)?;
                let vec_hits = db
                    .search_vector(&q_vec, vault::search::K_RRF_INPUT, domain, note_type, status)
                    .map_err(Self::err)?;
                self.warn_if_no_embeddings(&db, &vec_hits)?;

                let bm25_paths: Vec<String> = bm25.iter().map(|n| n.path.clone()).collect();
                let vec_paths: Vec<String> = vec_hits.iter().map(|h| h.note_path.clone()).collect();
                let fused = vault::search::reciprocal_rank_fusion(
                    &bm25_paths,
                    &vec_paths,
                    vault::search::RRF_K,
                    limit as usize,
                );
                Self::resolve_note_paths(&db, fused.iter().map(|h| h.note_path.as_str()))?
            }
        };

        let results: Vec<serde_json::Value> = notes.iter().map(|n| Self::format_note(n, &detail_level)).collect();

        Ok(CallToolResult::success(vec![Content::json(json!({
            "count": results.len(),
            "mode": match mode { SearchMode::Bm25 => "bm25", SearchMode::Vector => "vector", SearchMode::Hybrid => "hybrid" },
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
            None => Ok(CallToolResult::success(vec![Content::text(format!(
                "Note not found: {}",
                req.path
            ))])),
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
        let vault_root = self.config.vault_root().map_err(Self::err)?;
        let ledger_path = ledger::ledger_path(&vault_root);

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
            "entries": results,
        }))?]))
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
                    return Ok(CallToolResult::success(vec![Content::text(format!(
                        "Note not found: {p}"
                    ))]));
                }
            },
            (None, None) => {
                return Ok(CallToolResult::success(vec![Content::text(
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
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Note not found: {}",
                    req.path
                ))]));
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
            "notes": inbox_results,
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
                    None => Ok(CallToolResult::success(vec![Content::text(format!(
                        "Duplicate group not found: {gid}"
                    ))])),
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
}
