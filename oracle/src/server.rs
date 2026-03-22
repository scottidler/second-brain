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

    fn err(e: impl std::fmt::Display) -> McpError {
        warn!("Tool error: {}", e);
        McpError::internal_error(e.to_string(), None)
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
    /// Search the vault's ingested knowledge using full-text search
    #[tool(
        description = "Search the vault's ingested knowledge using full-text search. Filter by domain, note type, or status. Control content verbosity with the detail parameter: metadata, tldr, summary, full."
    )]
    async fn knowledge_search(&self, params: Parameters<KnowledgeSearchRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let detail_level = req.detail.unwrap_or(DetailLevel::Summary);
        let limit = req.limit.unwrap_or(10);

        let db = self.db.lock().map_err(Self::err)?;
        let notes = db
            .search(
                &req.query,
                req.domain.as_ref().map(|d| d.as_str()),
                req.note_type.as_ref().map(|t| t.as_str()),
                req.status.as_ref().map(|s| s.as_str()),
                Some(limit),
            )
            .map_err(Self::err)?;

        let results: Vec<serde_json::Value> = notes.iter().map(|n| Self::format_note(n, &detail_level)).collect();

        Ok(CallToolResult::success(vec![Content::json(json!({
            "count": results.len(),
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

        let recent: Vec<serde_json::Value> = brief
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
            "recent": recent,
        }))?]))
    }

    /// Query the borg ingest ledger
    #[tool(
        description = "Query the borg ingest ledger for ingestion history. Filter by source URL, domain, or date range."
    )]
    async fn ingest_history(&self, params: Parameters<IngestHistoryRequest>) -> Result<CallToolResult, McpError> {
        let req = params.0;
        let vault_root = self.config.vault_root();
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
                    "title": e.title,
                    "path": e.path,
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
        let vault_root = self.config.vault_root();
        let db = self.db.lock().map_err(Self::err)?;
        let stats = db.index_vault(&vault_root).map_err(Self::err)?;
        Ok(CallToolResult::success(vec![Content::json(&stats)?]))
    }
}

#[tool_handler]
impl ServerHandler for OracleMcpServer {
    fn get_info(&self) -> ServerInfo {
        info!("MCP client requested server info");
        ServerInfo {
            instructions: Some(
                "Oracle - knowledge retrieval MCP for a second-brain Obsidian vault. \
                 Search ingested knowledge by domain, type, or full-text query. \
                 Control content verbosity with the 'detail' parameter: \
                 metadata (fields only), tldr (one-liner), summary (summary section), full (complete body). \
                 Use vault_overview for the big picture, domain_brief for domain-specific intelligence, \
                 and knowledge_search for targeted queries."
                    .to_string(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
