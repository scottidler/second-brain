# oracle — Knowledge-Retrieval MCP Server

> Read before touching `oracle/`. Parent: `../CLAUDE.md`. Retrieval engine it drives: `../vault/src/search/AGENTS.md`.

## Purpose

oracle owns knowledge retrieval from the ingested vault, exposed as MCP tools over stdio (`sb oracle serve`) or direct dispatch (`sb oracle call <tool>`). It owns its own SQLite FTS5+vector index (separate file from borg's receipts DB), reindexed automatically via `VaultWatcher`; inbound-link counts recompute on a background cadence (~10 min). It opens borg's receipts DB **read-only** for the `failure_history` tool. lib-only; consumed by `sb`.

## Entry Points

- `lib.rs`: `serve()` (stdio MCP bootstrap), `call()` (single-tool dispatch, no transport), `index()` (reindex), `stats()`, `tools()` (list).
- `server.rs`: `OracleMcpServer::new()`, `OracleMcpServer::dispatch()` (tool name → method; used by `sb oracle call`).

## MCP Tool Surface

Defined as `#[tool]` methods on `OracleMcpServer` (`server.rs`); request types + `SearchMode` in `tools.rs`. Tools: `knowledge_search`, `note_read`, `list_notes`, `vault_overview`, `tag_brief`, `ingest_history`, `failure_history`, `schema_info`, `reindex`, `tag_search`, `find_similar`, `recent_activity`, `find_links`, `creator_browse`, `source_browse`, `inbox_status`, `quality_report`, `duplicate_groups`, `classify_status`, `ingest_queue`.

- **`tags` is THE classification filter** (`domain` was deleted in P11) on `knowledge_search`, `list_notes`, `tag_search`, `find_similar`, `recent_activity`, `creator_browse`, `source_browse`, `classify_status`: `Option<Vec<String>>` plus a `tags_mode` sibling (`any` = OR, the default; `all` = AND) that threads to `vault::search`'s `tags_all`. One exception by design: `ingest_history` (the ledger never carried tags). Two schema-walking tests enforce the shape over the router's schemars output rather than a hand-maintained list: `no_mcp_tool_schema_carries_a_domain_parameter` and `every_tags_parameter_has_a_tags_mode_sibling` (`server/tests.rs`). `vault_overview` exposes `by_tag` (top 20) plus `distinct_tags` (the true count), and `schema_info.tags` comes from the canonical vocabulary, loaded live from borg.yml's `tags.canonical-path` (`oracle::vocab`, through `BorgView`; unset or borg.yml absent -> `vault::paths::canonical_tags()`). A vocabulary that will not load is `tags: null` / `known: null` plus `vocabulary-error`, never an empty list or `known: false`.
- **Every request struct is `#[serde(deny_unknown_fields)]`** (`tools.rs`), so a stale client sending a retired facet or a misspelled parameter gets a loud `unknown field` error instead of a silently unfiltered result. This also publishes `additionalProperties: false` in the tool schema.

## Contracts & Invariants

- **`tracing` only** — no `log!` / `println!` / `env_logger`. Load-bearing for MCP stdio compatibility (rmcp).
- **Search modes** (`knowledge_search`): explicit per-call `mode` (`bm25` / `vector` / `hybrid` / `graph` / `graph-hybrid`) is the legacy single-path override. **No `mode` → the configurable pipeline** (`run_pipeline`, config in `oracle.yml` `retrieval:`), reported as `mode: "configured"`. Default is vector-first (eval-best), NOT hybrid. Stages: `query-transform → retrieve → fuse → rerank → exclude → truncate`, each `enabled`-gated. rerank (`vault::search::rerank`, cross-encoder, latency-budgeted fail-open) and query-transform (`oracle::transform`, HyDE/multi-query via `vault::fabric`) are off by default. Detail levels: `metadata` / `tldr` / `summary` / `full`. See `docs/design/2026-06-06-configurable-retrieval-pipeline.md`.
- **Only `note_read` bumps access** (`search_hit_count`, `last_accessed_at`). `knowledge_search` does NOT — regression-tested (`knowledge_search_does_not_bump_access`). Load-bearing for decay-based pruning (avoids a high-BM25 immortality loop).
- **Not-found vs. error:** a deleted-between-search-and-read note returns `{found:false,…}` with `is_error:false`; only protocol/arg failures set `is_error:true`.
- **Receipts DB opened read-only** — never with write flags (borg owns it).
- **`ingest_queue` asks the daemon, never a DB.** It GETs borg's `/queue` (design: `docs/design/2026-10-04-ingest-queue-status.md`) so it answers the same on lappy, which has no receipts DB. Address = `hotkey.host/port`, token = `server.auth-token` via `vault::daemon::client_auth_token`, both read from `borg.yml` through `vault::config::load_config` into a view struct (`queue::BorgView`). Every failure (connect, 10s timeout, non-2xx, bad body) is an MCP error naming `host:port`; an unreachable daemon must never read as `{"state":"idle"}`. Its description points at `sb borg wait` for blocking.

## Patterns

- **`Arc<Mutex<SearchIndex>>`** shared by all tool handlers + background tasks (watcher, inbound recompute); locked minimally.
- **Add an MCP tool:** add a request type in `tools.rs`, a `#[tool]` method on `OracleMcpServer` in `server.rs`, and a `dispatch()` arm. List-shaped tools normalize to `{count, results}`; detail extraction via `format_note()` + `DetailLevel`.

## Anti-patterns

- Don't bump access in `knowledge_search`.
- Don't open the receipts DB writable.
- Don't serialize `CallToolResult` as raw JSON — use `Content::json()` for rmcp.

## Module Map

- `lib.rs` — public API (serve/call/index/stats/tools) + tracing enforcement.
- `server.rs` — `OracleMcpServer`, tool implementations, `ServerHandler` (capabilities). `run_search_mode` (legacy modes) + `run_pipeline` (configured) share the bm25/vector/graph primitives; `maybe_rerank` is stage 4.
- `tools.rs` — request types; `SearchMode` (Bm25/Vector/Hybrid/Graph/GraphHybrid); `DetailLevel` (from vault).
- `vocab.rs` (+`vocab/`) — the canonical vocabulary: `vocabulary_path` (borg.yml `tags.canonical-path`, tilde-expanded; absent borg.yml or unset key -> `vault::paths::canonical_tags()`; present-but-unreadable/unparseable -> error, via `fs::metadata` kind), `load_canonical_tags(path) -> Result`, `resolve`.
- `queue.rs` (+`queue/`) — `ingest_queue` client: `BorgView` (daemon address, auth-token and `tags.canonical-path` view of `borg.yml`), `load_view`, `fetch` (GET `/queue`, explicit timeout, errors name the address).
- `transform.rs` — query-transform stage (HyDE / multi-query); shells to `vault::fabric` (oracle owns the LLM call, `vault` stays LLM-free).
- `config.rs` — vault root, db path, watcher + inbound-recompute config, and `RetrievalConfig` (the `retrieval:` pipeline); YAML load.
- `eval.rs` (+`eval/`) — `sb oracle eval`: relevance-lift harness measuring whether graph-augmented retrieval beats the `hybrid` baseline via a pooled, blind LLM judge.
