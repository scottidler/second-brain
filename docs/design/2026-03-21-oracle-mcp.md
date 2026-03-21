# Design Document: Oracle - Knowledge Retrieval MCP Server

**Author:** Scott Idler
**Date:** 2026-03-21
**Status:** In Review
**Review Passes Completed:** 5/5

## Summary

Add an `oracle` crate to the second-brain workspace - a Model Context Protocol (MCP) server that gives Claude agents schema-aware, queryable access to the vault's ingested knowledge. Unlike the generic Obsidian MCP (file-level CRUD), Oracle understands the vault's domain model, frontmatter schema, and borg ledger, enabling semantic queries like "what do I know about X in domain Y" with configurable content verbosity to manage token consumption.

## Problem Statement

### Background

The second-brain workspace has two tools that write to and govern the vault:

- **borg**: ingestion daemon - receives content via Telegram, Discord, HTTP, CLI, clipboard, ntfy; summarizes via Fabric/LLM; renders structured markdown notes with YAML frontmatter; logs to a dedup ledger.
- **cortex**: governance tool - lints naming, frontmatter, tags, quality, links; generates intelligence digests; watches vault for changes.

Both build on the shared `vault` crate which provides canonical schema enums (Domain, NoteType, Origin, Status, Method), frontmatter parsing, note scanning, ledger operations, and hygiene utilities.

Claude agents (via Claude Code) interact with the vault today through the generic Obsidian MCP server, which provides basic read/write/search/tag operations against markdown files. This works for simple file operations but has no understanding of:

- The vault's domain model (10 domains, 21 note types, statuses, origins)
- Note structure (YAML frontmatter, ## Summary sections, source links)
- The borg ledger (ingestion history, duplicate detection)
- Content types and their metadata (YouTube duration, code language, asset paths)

Scott has already built `multi-account-github-mcp` - a Rust MCP server using `rmcp 0.12` on stdio transport - proving the pattern works and establishing reusable architectural decisions.

### Problem

Claude agents cannot effectively leverage the vault's ingested knowledge because the generic Obsidian MCP treats all notes as plain markdown files. There is no way for an agent to:

1. Search across knowledge domains with schema-aware filtering
2. Get domain-specific intelligence briefings
3. Query ingestion history from the borg ledger
4. Control content verbosity to manage token budgets
5. Understand the shape of the knowledge base (what domains, how many notes, what's unread)

The vault contains 900+ notes across 10 domains - the knowledge is there, but it's not queryable in a structured way.

### Goals

- Read-only MCP server that exposes vault knowledge as structured, queryable data
- Full-text search with filtering by domain, note type, status, and date range
- Configurable detail levels (metadata, tldr, summary, full) so agents can dial up/down token consumption
- SQLite index with FTS5 for fast full-text search, incremental reindex by file mtime
- Domain briefings - stats, unread counts, recent ingests, type breakdown
- Ledger queries - ingestion history with filtering
- Schema introspection - list valid enum values for filtering
- Workspace member using `vault` crate - same compile-time schema guarantees as borg and cortex

### Non-Goals

- Write operations against the vault (marking notes as read, adding tags, triggering cortex sweeps) - future work. Note: oracle writes to its own SQLite index, but never modifies vault files or the borg ledger.
- Replacing the generic Obsidian MCP - Oracle complements it for knowledge retrieval; the generic MCP handles basic file CRUD
- Real-time vault watching / auto-reindex on file changes - reindex is on-demand (at serve startup and via tool)
- LLM-powered summarization or embedding-based semantic search - queries use SQLite FTS5
- Ingesting new content - that's borg's job

## Proposed Solution

### Overview

A fourth workspace member (`oracle/`) containing a Rust binary that:

1. On startup, indexes the vault into a local SQLite database with FTS5
2. Serves MCP tools over stdio transport using `rmcp`
3. Provides schema-aware search, note retrieval with detail levels, domain intelligence, and ledger queries

```
second-brain/
  vault/    -- shared library crate (unchanged)
  borg/     -- ingestion binary (unchanged)
  cortex/   -- governance binary (unchanged)
  oracle/   -- knowledge retrieval MCP server (new)
```

### Architecture

```
oracle/
  Cargo.toml
  build.rs          -- git describe version embedding
  src/
    main.rs         -- CLI entrypoint (serve, index, stats)
    lib.rs          -- module exports
    cli.rs          -- clap CLI definitions
    config.rs       -- config loading (YAML, defaults, tilde expansion)
    db.rs           -- SQLite schema, indexing, queries
    detail.rs       -- detail level enum + section extraction
    server.rs       -- OracleMcpServer, tool implementations, ServerHandler
    tools.rs        -- MCP tool request type definitions
```

**Key architectural decisions:**

- **Vault types at every boundary**: The whole point of the workspace consolidation was compile-time schema guarantees. Oracle enforces this at two boundaries: (1) **MCP tool input** - filter parameters use vault enums (`Domain`, `NoteType`, `Status`), so an agent passing `"footbal"` gets a clear deserialization error listing valid values, not a silent empty result; (2) **Indexing** - frontmatter strings are parsed through vault enums during index, normalizing canonical values and logging warnings for invalid ones. SQLite stores strings (that's what SQL does), but every string that enters or exits passes through a vault type. See [Type Safety](#type-safety) for details.
- **Flat module layout**: No nested `mcp/` or `tools/` directories. Oracle is simpler than multi-account-github-mcp (8 tools vs 38), so a flat layout is clearer.
- **SQLite with FTS5**: The vault has 900+ notes today. SQLite handles this trivially and gives us full-text search, indexes, and atomic operations. The DB lives at `~/.local/share/oracle/oracle.db`.
- **Incremental reindex**: Notes are indexed by file mtime. On reindex, only changed files are updated. Stale entries (deleted notes) are removed.
- **tracing (not env_logger)**: The `rmcp` crate emits tracing events. While the rest of the workspace uses `env_logger + log`, Oracle uses `tracing + tracing-subscriber` for rmcp compatibility, matching the proven pattern from multi-account-github-mcp.
- **Arc<Mutex<Database>>**: The MCP server must be `Clone` (rmcp requirement). The SQLite connection is wrapped in `Arc<Mutex<>>` for shared access across tool handlers.

### Data Model

**SQLite Schema:**

```sql
CREATE TABLE notes (
    path TEXT PRIMARY KEY,       -- vault-relative path (e.g., "ai/some-article.md")
    title TEXT,
    domain TEXT,                 -- from vault::schema::Domain
    note_type TEXT,              -- from vault::schema::NoteType
    origin TEXT,                 -- from vault::schema::Origin
    status TEXT,                 -- from vault::schema::Status
    date TEXT,                   -- YYYY-MM-DD
    tags TEXT,                   -- JSON array
    source TEXT,                 -- source URL
    creator TEXT,
    body TEXT,                   -- full note body (after frontmatter)
    summary TEXT,                -- extracted from ## Summary section
    modified_at INTEGER          -- file mtime for incremental reindex
);

-- Indexes for filtered queries
CREATE INDEX idx_notes_domain ON notes(domain);
CREATE INDEX idx_notes_note_type ON notes(note_type);
CREATE INDEX idx_notes_status ON notes(status);
CREATE INDEX idx_notes_date ON notes(date);

-- FTS5 for full-text search (synced via triggers)
CREATE VIRTUAL TABLE notes_fts USING fts5(
    title, body, tags, summary,
    content=notes, content_rowid=rowid
);
```

FTS5 sync is maintained via `AFTER INSERT/UPDATE/DELETE` triggers on the `notes` table. The `INSERT OR REPLACE` pattern used for upserts correctly fires both DELETE (for the old row) and INSERT (for the new row) triggers, keeping FTS5 in sync.

**Type-safe indexing:** During indexing, each frontmatter field is parsed through its vault enum:

- `domain` string -> `Domain::from_str()` -> stored as `Domain::as_str()` (canonical lowercase)
- `note_type` string -> `NoteType::from_str()` -> stored as `NoteType::as_str()`
- Same for `origin`, `status`

Notes with invalid enum values are indexed with an empty string for that field and a warning is logged. Notes with missing fields get empty strings. This means:
- Valid values are normalized (case-insensitive input -> canonical output)
- Invalid values are flagged during indexing, not silently propagated
- The 58 "Null"-domain notes from the vault are a known gap - cortex should migrate these, and oracle surfaces them via `list_notes` without filters or `vault_overview`

**Detail Levels:**

| Level | Returns | Use Case |
|-------|---------|----------|
| `metadata` | Frontmatter fields only (title, domain, type, status, date, source, tags) | Scanning/filtering, minimal tokens |
| `tldr` | Metadata + first sentence of summary | Quick orientation |
| `summary` | Metadata + full ## Summary section content | Standard retrieval |
| `full` | Metadata + complete note body | Deep reading |

Detail levels are implemented by parsing note bodies into H2 sections via a generic section parser (not hardcoded to "Summary"), so if the note template evolves to add new sections, they're automatically available.

**Fallback chain for content extraction:**
- `summary` level: returns the `## Summary` section if present; otherwise the first H2 section found; otherwise the first 500 characters of the body.
- `tldr` level: extracts the first sentence from the summary (using the same fallback chain above).
- This means detail levels degrade gracefully for notes that predate the current template or have non-standard structure.

**Example tool call and response:**

An agent calling `knowledge_search` with `query: "rust ownership"`, `domain: "tech"`, `detail: "tldr"` would receive:

```json
{
  "count": 2,
  "results": [
    {
      "path": "tech/understanding-rust-ownership.md",
      "title": "Understanding Rust Ownership",
      "domain": "tech",
      "type": "article",
      "origin": "assisted",
      "status": "reviewed",
      "date": "2026-03-15",
      "tags": ["rust", "programming"],
      "source": "https://example.com/rust-article",
      "creator": "",
      "tldr": "Rust's ownership system manages memory safety without garbage collection."
    }
  ]
}
```

The agent can then call `note_read` with `path: "tech/understanding-rust-ownership.md"`, `detail: "full"` to get the complete body when needed.

### API Design

**MCP Tools (8 total):**

| Tool | Description | Key Parameters |
|------|-------------|---------------|
| `knowledge_search` | Full-text search across vault | `query: String`, `domain?: Domain`, `note_type?: NoteType`, `status?: Status`, `detail?: DetailLevel`, `limit?: u32` |
| `note_read` | Read a specific note by path | `path: String`, `detail?: DetailLevel` |
| `list_notes` | Browse by category (no search query needed) | `domain?: Domain`, `note_type?: NoteType`, `status?: Status`, `after?: String`, `before?: String`, `detail?: DetailLevel`, `limit?: u32` |
| `vault_overview` | Vault-wide stats: counts by domain/type/status + schema gaps | (none) |
| `domain_brief` | Domain intelligence: stats + recent notes | `domain: Domain`, `detail?: DetailLevel`, `limit?: u32` |
| `ingest_history` | Query borg ledger | `source?: String`, `domain?: Domain`, `after?: String`, `before?: String` |
| `schema_info` | List all valid enum values | (none) |
| `reindex` | Trigger vault reindex | (none) |

Filter parameters that reference vault schema (`domain`, `note_type`, `status`) use the actual vault enum types. This means:
- MCP tool schemas advertise the valid values (via `JsonSchema` derive)
- Invalid values fail at deserialization with a clear error, before any query runs
- The `domain_brief` tool requires a valid `Domain`, not an arbitrary string

**Server Instructions** (returned to MCP clients):

> Oracle - knowledge retrieval MCP for a second-brain Obsidian vault. Search ingested knowledge by domain, type, or full-text query. Control content verbosity with the 'detail' parameter: metadata (fields only), tldr (one-liner), summary (summary section), full (complete body). Use vault_overview for the big picture, domain_brief for domain-specific intelligence, and knowledge_search for targeted queries.

### Configuration

Config file at `~/.config/oracle/oracle.yml` (optional - sensible defaults):

```yaml
vault_root: ~/repos/scottidler/obsidian
db_path: ~/.local/share/oracle/oracle.db
logging:
  level: info
  file: ~/logs/oracle.log
```

Config resolution: CLI `--config` flag > `~/.config/oracle/oracle.yml` > `./oracle.yml` > built-in defaults.

### Implementation Plan

**Phase 1: Scaffold and Core Infrastructure**
- Add `schemars` as a feature-gated optional dependency to `vault/Cargo.toml`, add `JsonSchema` derives to vault schema enums behind the feature
- Create `oracle/` directory and add to workspace `Cargo.toml` members
- `oracle/Cargo.toml` with dependencies (vault path dep with schemars feature, rmcp, rusqlite bundled, tokio, serde stack, clap, tracing)
- `build.rs` for git describe version embedding
- `cli.rs`: clap CLI with `serve`, `index`, `stats` subcommands
- `config.rs`: YAML config loading with defaults, tilde expansion, config file resolution chain
- `lib.rs`, `main.rs`: module structure and entrypoint
- **Done when:** `cargo check -p oracle` passes, `oracle --help` prints usage, vault enums have `JsonSchema` behind feature gate

**Phase 2: SQLite Indexing**
- `db.rs`: Database struct, `open()` with WAL mode, `ensure_schema()` creating notes table + indexes + FTS5 + sync triggers
- `detail.rs`: `DetailLevel` enum, `parse_sections()` for H2 extraction, `first_sentence()` helper
- `index_vault()`: scan via `vault::note::scan_vault()`, parse frontmatter fields through vault enums (normalize valid, warn on invalid), extract summary from sections, upsert into SQLite, skip unchanged (mtime check), remove stale entries
- Query methods: `search()` (FTS5 + filters), `list_notes()` (filters + date range), `get_note()` (by path), `stats()` (counts by domain/type/status + schema gaps), `domain_brief()` (domain stats + recent)
- `stats()` includes a `schema_gaps` section: counts of notes with empty domain, type, origin, status - surfaces data quality issues without enforcing them (that's cortex's job)
- **Done when:** `oracle index` indexes the vault, `oracle stats` shows correct counts, unit tests pass for section parsing

**Phase 3: MCP Server and Tools**
- `tools.rs`: Request structs for all 8 tools with `Deserialize + JsonSchema`, using vault enums (`Option<Domain>`, `Option<NoteType>`, `Option<Status>`) for filter params
- `server.rs`: `OracleMcpServer` struct with `Arc<Mutex<Database>>`, `#[tool_router]` impl with all 8 tools, `format_note()` for detail level rendering, `#[tool_handler]` ServerHandler with instructions
- **Done when:** `oracle serve` starts and responds to MCP tool calls, `cargo test -p oracle` passes

**Phase 4: Integration and Testing**
- Integration test: in-memory DB, index sample notes, verify search/list/stats roundtrip
- Smoke test: `oracle index` and `oracle stats` against real vault
- Wire into Claude Code MCP config (`~/.claude/settings.json` or project-level)
- Update `CLAUDE.md` architecture section to include oracle
- Update `.otto.yml` to include oracle in CI pipeline
- **Done when:** Oracle tools are visible and functional in Claude Code

## Alternatives Considered

### Alternative 1: Extend Generic Obsidian MCP
- **Description:** Add schema-aware search to the existing Obsidian MCP server (kepano's or the community one).
- **Pros:** No new crate to maintain.
- **Cons:** The generic MCP is TypeScript, not Rust - can't share the vault crate. Would require reimplementing schema knowledge outside the workspace. Schema drift becomes possible again.
- **Why not chosen:** The whole point of the workspace consolidation was compile-time schema guarantees. An external MCP would be outside that guarantee boundary.

### Alternative 2: CLI-Only (No MCP)
- **Description:** Add a `query` subcommand to cortex and call it from Claude Code via bash.
- **Pros:** No new crate. Simpler.
- **Cons:** No structured tool discovery (agents wouldn't know what queries are available). Parsing CLI output is fragile. No detail level control - would dump full content every time. No persistent index (scan-on-every-query).
- **Why not chosen:** MCP provides structured tool discovery, typed parameters, and JSON responses - exactly what agents need. The overhead of a separate crate is justified by the agent UX improvement.

### Alternative 3: HTTP API Instead of MCP
- **Description:** Build a REST API server instead of an MCP server.
- **Pros:** Accessible from any HTTP client, not just MCP-aware agents.
- **Cons:** MCP is the native protocol for Claude agent tool use. HTTP would require a separate MCP wrapper or manual curl calls. More infrastructure to run (port management, auth).
- **Why not chosen:** MCP stdio transport is simpler, more secure (no network exposure), and directly supported by Claude Code.

### Alternative 4: Embedding-Based Semantic Search
- **Description:** Use vector embeddings (via Claude API or local model) for semantic search instead of FTS5.
- **Pros:** Better semantic matching ("notes about machine learning" would match notes that don't literally contain that phrase).
- **Cons:** Requires embedding generation (API calls or local model), vector storage, significant complexity. FTS5 is good enough for a 900-note vault with consistent terminology.
- **Why not chosen:** Premature optimization. FTS5 covers the immediate need. Embeddings can be added later as a search strategy alongside FTS5 if needed.

## Technical Considerations

### Type Safety

Oracle enforces vault types at three boundaries:

**1. MCP tool input (agent-facing):**
Tool request structs use `Option<Domain>`, `Option<NoteType>`, `Option<Status>` - not `Option<String>`. The vault enums need `JsonSchema` derives so rmcp can generate MCP tool schemas advertising valid values. Two options:

- **Preferred:** Add `schemars` as a feature-gated optional dependency to the vault crate (`vault/Cargo.toml: schemars = { version = "1.2", optional = true }`). Oracle enables the feature. Vault enums get `#[cfg_attr(feature = "schemars", derive(JsonSchema))]`. This keeps the type real - an `Option<Domain>` in the request struct.
- **Fallback:** If we don't want to touch vault, oracle defines thin wrapper enums that mirror vault enums, derive `JsonSchema`, and impl `From<Wrapper>` for the vault type. More boilerplate, same safety.

Either way, an agent passing `"footbal"` gets: `Error: invalid value "footbal" for domain, expected one of: ai, tech, football, work, writing, music, spanish, knowledge, resources, system`.

**2. Indexing (vault -> SQLite):**
When indexing frontmatter into SQLite, oracle parses each schema field through its vault enum via `FromStr`. Valid values are stored as `enum.as_str()` (canonical lowercase). Invalid values are stored as empty string with a warning logged. This normalizes data on the way in - a note with `Domain: AI` or `domain: Ai` both become `ai` in the index.

**3. Query output (SQLite -> agent):**
The `format_note()` function returns canonical enum strings in structured JSON. The `stats()`, `domain_brief()`, and `schema_info()` responses all use vault enum values, never raw database strings.

**What this means in practice:**
- Vault types are the single source of truth, just like for borg and cortex
- If a new domain is added to `vault::schema::Domain`, oracle gets it automatically (recompile + reindex)
- If a domain is removed, oracle will reject it at the tool boundary and flag existing notes during reindex
- The 58 notes with missing domains are a data quality issue, not a type safety issue - oracle surfaces them, cortex should fix them

### Dependencies

**Internal:**
- `vault` crate (path dependency) - schema, frontmatter, note scanning, ledger, config

**External (new to workspace):**
- `rmcp 0.12` - MCP protocol (server, macros features)
- `rusqlite 0.34` - SQLite (bundled feature for self-contained builds)
- `tracing 0.1` + `tracing-subscriber 0.3` - logging (rmcp compatibility)
- `thiserror 2.0` - error types

**From workspace:**
- `tokio`, `serde`, `serde_json`, `serde_yaml`, `clap`, `eyre`, `dirs`, `shellexpand`, `chrono`

### Performance

- **Index build**: Full scan of 900 notes takes ~100ms. Incremental reindex (check mtime, skip unchanged) is near-instant for typical use.
- **FTS5 queries**: Sub-millisecond for a vault this size.
- **Memory**: SQLite in WAL mode, single connection. Minimal memory footprint.
- **Startup**: Auto-index on `serve` startup means first query is always against fresh data.

### Security

- **Read-only**: No write operations to vault or ledger. Oracle cannot modify knowledge.
- **Local only**: Stdio transport - no network exposure. Only accessible to the parent process (Claude Code).
- **No secrets**: Oracle doesn't handle API keys or tokens. Config only contains file paths and log levels.
- **SQL injection**: All queries use parameterized statements. The `count_by_column()` helper uses known column names from code, never user input.

### Testing Strategy

- **Unit tests**: Section parsing, first-sentence extraction, detail level formatting
- **Integration tests**: Database creation, index/reindex, search queries, FTS5 correctness
- **Smoke tests**: `oracle index` and `oracle stats` against the real vault
- **MCP integration**: Manual testing via Claude Code after wiring into MCP config

### Rollout Plan

1. Build and install: `otto install` (or `cargo install --path oracle`)
2. Add to Claude Code MCP config:
   ```json
   {
     "mcpServers": {
       "oracle": {
         "command": "oracle",
         "args": ["serve"]
       }
     }
   }
   ```
3. Verify tools appear in Claude Code
4. Test with real queries

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| FTS5 ranking is poor for domain-specific queries | Medium | Low | FTS5 ranking is best-effort; agents can also use `list_notes` with filters for precise browsing |
| SQLite DB grows large with full note bodies stored | Low | Low | 900 notes with full bodies is ~50MB. Negligible for disk storage. |
| Note template evolves, breaking section extraction | Medium | Low | Section parser is generic (splits on ## headings), not hardcoded to specific section names |
| rmcp crate has breaking changes | Low | Medium | Pin to 0.12.x. Same risk exists for multi-account-github-mcp and will be addressed workspace-wide. |
| Auto-index on serve startup is slow for large vaults | Low | Low | Incremental reindex only touches changed files. Even full reindex of 900 notes is ~100ms. |
| Concurrent access: borg writes while oracle indexes | Low | Low | Borg writes notes via temp-file-then-rename (atomic). Oracle reads via `scan_vault` which reads files sequentially. Worst case: oracle skips a note mid-write and picks it up on next reindex. |
| FTS5 query syntax errors from malformed input | Medium | Low | FTS5 returns an error for invalid syntax. Tool handler catches and returns the error as a text response. Agents can retry with simpler queries. |
| Binary name `oracle` collides with Oracle Database tools | Low | Low | Personal tooling, not distributed. No Oracle DB tools are installed. If collision arises, can rename the binary in Cargo.toml without changing the crate name. |

## Open Questions

- [ ] Should oracle support a `--watch` mode that reindexes on file changes (using `notify` crate like cortex)? Or is reindex-on-startup + on-demand sufficient?
- [ ] Should the Claude Code MCP config also include server instructions that reference the detail levels, or is the ServerHandler instructions field sufficient?
- [ ] Should we add a `similar_notes` tool that finds notes with overlapping tags or same domain/type? (Could be useful for "what else do I know about this topic?")

## References

- [Workspace consolidation design doc](2026-03-20-workspace-consolidation.md) - established the workspace pattern and vault crate
- [multi-account-github-mcp](https://github.com/scottidler/multi-account-github-mcp) - proven MCP server pattern in Rust with rmcp
- [MCP specification](https://modelcontextprotocol.io) - Model Context Protocol
- [rmcp crate](https://crates.io/crates/rmcp) - Rust MCP SDK
