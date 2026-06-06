# oracle — Knowledge-Retrieval MCP Server

> Read before touching `oracle/`. Parent: `../CLAUDE.md`. Retrieval engine it drives: `../vault/src/search/AGENTS.md`.

## Purpose

oracle owns knowledge retrieval from the ingested vault, exposed as MCP tools over stdio (`sb oracle serve`) or direct dispatch (`sb oracle call <tool>`). It owns its own SQLite FTS5+vector index (separate file from borg's receipts DB), reindexed automatically via `VaultWatcher`; inbound-link counts recompute on a background cadence (~10 min). It opens borg's receipts DB **read-only** for the `failure_history` tool. lib-only; consumed by `sb`.

## Entry Points

- `lib.rs`: `serve()` (stdio MCP bootstrap), `call()` (single-tool dispatch, no transport), `index()` (reindex), `stats()`, `tools()` (list).
- `server.rs`: `OracleMcpServer::new()`, `OracleMcpServer::dispatch()` (tool name → method; used by `sb oracle call`).

## MCP Tool Surface

Defined as `#[tool]` methods on `OracleMcpServer` (`server.rs`); request types + `SearchMode` in `tools.rs`. Tools: `knowledge_search`, `note_read`, `list_notes`, `vault_overview`, `domain_brief`, `ingest_history`, `failure_history`, `schema_info`, `reindex`, `tag_search`, `find_similar`, `recent_activity`, `find_links`, `creator_browse`, `source_browse`, `inbox_status`, `quality_report`, `duplicate_groups`, `classify_status`.

## Contracts & Invariants

- **`tracing` only** — no `log!` / `println!` / `env_logger`. Load-bearing for MCP stdio compatibility (rmcp).
- **Search modes** (`knowledge_search`): `bm25` (FTS5), `vector` (cosine), `hybrid` (RRF, default). Detail levels: `metadata` / `tldr` / `summary` / `full`.
- **Only `note_read` bumps access** (`search_hit_count`, `last_accessed_at`). `knowledge_search` does NOT — regression-tested (`knowledge_search_does_not_bump_access`). Load-bearing for decay-based pruning (avoids a high-BM25 immortality loop).
- **Not-found vs. error:** a deleted-between-search-and-read note returns `{found:false,…}` with `is_error:false`; only protocol/arg failures set `is_error:true`.
- **Receipts DB opened read-only** — never with write flags (borg owns it).

## Patterns

- **`Arc<Mutex<SearchIndex>>`** shared by all tool handlers + background tasks (watcher, inbound recompute); locked minimally.
- **Add an MCP tool:** add a request type in `tools.rs`, a `#[tool]` method on `OracleMcpServer` in `server.rs`, and a `dispatch()` arm. List-shaped tools normalize to `{count, results}`; detail extraction via `format_note()` + `DetailLevel`.

## Anti-patterns

- Don't bump access in `knowledge_search`.
- Don't open the receipts DB writable.
- Don't serialize `CallToolResult` as raw JSON — use `Content::json()` for rmcp.

## Module Map

- `lib.rs` — public API (serve/call/index/stats/tools) + tracing enforcement.
- `server.rs` — `OracleMcpServer`, tool implementations, `ServerHandler` (capabilities).
- `tools.rs` — request types; `SearchMode` (Bm25/Vector/Hybrid); `DetailLevel` (from vault).
- `config.rs` — vault root, db path, watcher + inbound-recompute config; YAML load.
