//! MCP tool request types
//!
//! Filter parameters use vault schema enums for compile-time correctness.
//! Invalid values fail deserialization with a clear error listing valid options.

use rmcp::schemars;
use schemars::JsonSchema;
use serde::Deserialize;
use vault::detail::DetailLevel;
use vault::schema::{Domain, NoteType, Status};

/// Search the vault's ingested knowledge using full-text search with optional filters.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct KnowledgeSearchRequest {
    /// The search query (full-text search across titles, bodies, tags, and summaries)
    #[schemars(description = "Search query - searches across note titles, bodies, tags, and summaries")]
    pub query: String,

    /// Filter by domain
    #[schemars(description = "Filter by domain")]
    pub domain: Option<Domain>,

    /// Filter by note type
    #[schemars(description = "Filter by note type")]
    pub note_type: Option<NoteType>,

    /// Filter by status
    #[schemars(description = "Filter by status")]
    pub status: Option<Status>,

    /// How much content to return per note
    #[schemars(
        description = "Detail level: metadata (just fields), tldr (title + first sentence), summary (summary section), full (complete body). Default: summary"
    )]
    pub detail: Option<DetailLevel>,

    /// Maximum number of results
    #[schemars(description = "Maximum number of results to return (default: 10)")]
    pub limit: Option<u32>,
}

/// Read a specific note by its vault-relative path.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoteReadRequest {
    /// The vault-relative path to the note (e.g., 'ai/some-article.md')
    #[schemars(description = "Vault-relative path to the note (e.g., 'ai/some-article.md')")]
    pub path: String,

    /// How much content to return
    #[schemars(description = "Detail level: metadata, tldr, summary, full. Default: full")]
    pub detail: Option<DetailLevel>,
}

/// Get an overview of the vault - total notes, distribution by domain, type, and status, plus schema gaps.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VaultOverviewRequest {}

/// Get a briefing on a specific knowledge domain - stats, recent ingests, unread count.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DomainBriefRequest {
    /// The domain to get a briefing on
    #[schemars(description = "Domain to brief on")]
    pub domain: Domain,

    /// How much content to return per note in the recent list
    #[schemars(description = "Detail level for recent notes. Default: tldr")]
    pub detail: Option<DetailLevel>,

    /// Number of recent notes to include
    #[schemars(description = "Number of recent notes to include (default: 10)")]
    pub limit: Option<u32>,
}

/// List notes with optional filters, without requiring a search query.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListNotesRequest {
    /// Filter by domain
    #[schemars(description = "Filter by domain")]
    pub domain: Option<Domain>,

    /// Filter by note type
    #[schemars(description = "Filter by note type")]
    pub note_type: Option<NoteType>,

    /// Filter by status
    #[schemars(description = "Filter by status")]
    pub status: Option<Status>,

    /// Only notes on or after this date (YYYY-MM-DD)
    #[schemars(description = "Only notes on or after this date (YYYY-MM-DD)")]
    pub after: Option<String>,

    /// Only notes on or before this date (YYYY-MM-DD)
    #[schemars(description = "Only notes on or before this date (YYYY-MM-DD)")]
    pub before: Option<String>,

    /// How much content to return per note
    #[schemars(description = "Detail level: metadata, tldr, summary, full. Default: metadata")]
    pub detail: Option<DetailLevel>,

    /// Maximum number of results
    #[schemars(description = "Maximum number of results to return (default: 20)")]
    pub limit: Option<u32>,
}

/// Query the borg ingest ledger for ingestion history.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct IngestHistoryRequest {
    /// Filter by source URL substring
    #[schemars(description = "Filter by source URL (substring match)")]
    pub source: Option<String>,

    /// Filter by domain
    #[schemars(description = "Filter by domain")]
    pub domain: Option<Domain>,

    /// Only entries after this date (YYYY-MM-DD)
    #[schemars(description = "Only entries after this date (YYYY-MM-DD)")]
    pub after: Option<String>,

    /// Only entries before this date (YYYY-MM-DD)
    #[schemars(description = "Only entries before this date (YYYY-MM-DD)")]
    pub before: Option<String>,
}

/// List all valid schema values (domains, note types, origins, statuses, methods).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SchemaInfoRequest {}

/// Trigger a reindex of the vault into the SQLite database.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReindexRequest {}
