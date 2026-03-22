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

/// Search notes by tag, or list all tags with counts when no tag is specified.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TagSearchRequest {
    /// Specific tag to search for. Exact match by default; append * for prefix match (e.g. "rust*"). Omit to list all tags with counts.
    #[schemars(
        description = "Tag to search for (exact match, or prefix match with trailing *). Omit to list all tags with counts."
    )]
    pub tag: Option<String>,

    /// Filter to tags within a domain
    #[schemars(description = "Filter to a specific domain")]
    pub domain: Option<Domain>,

    /// How much content to return per note
    #[schemars(description = "Detail level for returned notes. Default: metadata")]
    pub detail: Option<DetailLevel>,

    /// Maximum number of results
    #[schemars(description = "Maximum number of results (default: 20)")]
    pub limit: Option<u32>,
}

/// Find notes similar to given content or another note.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindSimilarRequest {
    /// Text to find similar notes for
    #[schemars(description = "Text content to find similar notes for. Provide either this or path, not both.")]
    pub content: Option<String>,

    /// Path to a note (uses its body as the comparison content)
    #[schemars(
        description = "Vault-relative path to a note. Uses its body to find similar notes. Provide either this or content."
    )]
    pub path: Option<String>,

    /// Filter by domain
    #[schemars(description = "Restrict results to a specific domain")]
    pub domain: Option<Domain>,

    /// How much content to return per note
    #[schemars(description = "Detail level for returned notes. Default: tldr")]
    pub detail: Option<DetailLevel>,

    /// Maximum number of results
    #[schemars(description = "Maximum number of similar notes to return (default: 5)")]
    pub limit: Option<u32>,
}

/// Cross-domain timeline of recent vault activity.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecentActivityRequest {
    /// How many days back to look
    #[schemars(description = "Number of days back to search (default: 7)")]
    pub days: Option<u32>,

    /// Filter by domain
    #[schemars(description = "Filter to a specific domain")]
    pub domain: Option<Domain>,

    /// Filter by note type
    #[schemars(description = "Filter to a specific note type")]
    pub note_type: Option<NoteType>,

    /// How much content to return per note
    #[schemars(description = "Detail level for returned notes. Default: tldr")]
    pub detail: Option<DetailLevel>,

    /// Maximum number of results
    #[schemars(description = "Maximum number of results (default: 20)")]
    pub limit: Option<u32>,
}

/// Wikilink graph traversal for a note - outbound and inbound links.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindLinksRequest {
    /// Note path to inspect
    #[schemars(description = "Vault-relative path to the note to inspect links for")]
    pub path: String,

    /// Direction: "outbound", "inbound", or "both"
    #[schemars(description = "Link direction: outbound, inbound, or both (default: both)")]
    pub direction: Option<String>,

    /// Detail level for inbound notes
    #[schemars(description = "Detail level for inbound note results. Default: metadata")]
    pub detail: Option<DetailLevel>,
}

/// Browse notes by creator/channel.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreatorBrowseRequest {
    /// Filter to specific creator (substring match). Omit to list all creators with counts.
    #[schemars(description = "Creator name to filter (substring match). Omit to list all creators with counts.")]
    pub creator: Option<String>,

    /// Filter by domain
    #[schemars(description = "Filter to a specific domain")]
    pub domain: Option<Domain>,

    /// How much content to return per note
    #[schemars(description = "Detail level for returned notes. Default: metadata")]
    pub detail: Option<DetailLevel>,

    /// Maximum number of results
    #[schemars(description = "Maximum number of results (default: 20)")]
    pub limit: Option<u32>,
}

/// Browse notes by source URL domain.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SourceBrowseRequest {
    /// Source domain to filter (e.g., "youtube.com"). Omit to list all source domains with counts.
    #[schemars(
        description = "Source domain to filter (e.g., 'youtube.com'). Omit to list all source domains with counts."
    )]
    pub host: Option<String>,

    /// Filter by vault domain
    #[schemars(description = "Filter to a specific vault domain")]
    pub domain: Option<Domain>,

    /// How much content to return per note
    #[schemars(description = "Detail level for returned notes. Default: metadata")]
    pub detail: Option<DetailLevel>,

    /// Maximum number of results
    #[schemars(description = "Maximum number of results (default: 20)")]
    pub limit: Option<u32>,
}

/// List all valid schema values (domains, note types, origins, statuses, methods).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SchemaInfoRequest {}

/// Trigger a reindex of the vault into the SQLite database.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReindexRequest {}
