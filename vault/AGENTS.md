# vault — Shared Schema & Primitives

> Read before touching `vault/`. This is the workspace's source of truth. Hybrid-search engine: `src/search/AGENTS.md`. Parent: `../CLAUDE.md`.

## Purpose

The single source of truth for the Obsidian-vault domain schema (Domain / NoteType / Origin / Status / Method), YAML frontmatter, note parsing, path resolution, the search index, embeddings, and the L2 `Distilled` contract. Consumed by borg, cortex, oracle, distillers, and sb. If a primitive is shared across crates, it lives here — not in a consumer.

## Entry Points

- `vault::schema` — the enums (`.as_str()`, `.all()`, `FromStr`), with feature-gated `schemars::JsonSchema` derives for MCP tool schemas.
- `vault::note::scan_vault(vault_root, scan_config) -> Vec<Note>` (rayon `par_iter`, path-sorted, deterministic); `parse_note(vault_root, path)`.
- `vault::frontmatter::Frontmatter` — known fields extracted, unknown fields preserved in an extras map (distillers / `cortex-*` keys).
- `vault::paths::{expand_tilde, deserialize_tilde_pathbuf, config_root, resolve_vault_root}`.
- `vault::ledger` — borg's dedup log helpers; `vault::receipts::FailureStage` (the seven terminal stages).
- `vault::embedding::{EmbeddingModel, load_active_model, embed_query}` (Candle / fastembed, feature-gated).
- `vault::distilled::Distilled { summary, claims, tags, links, kind_specific, meta, transcript }`.
- `vault::watcher::VaultWatcher::start(vault_root, config, applying_flag)` — debounced change stream.

## Contracts & Invariants

- **Schema is law.** `vault::schema` enums are THE source of truth — never hardcode `"ai"`/`"article"`/`"authored"` strings in consumer crates; import the enums.
- **L2 Distilled contract** `{summary, claims, tags, links, kind_specific, meta, transcript}` is the finalized extractor output consumed by renderers (`distillers`) and borg's publish stage.
- **Tilde expansion at the boundary.** Any user-supplied path MUST pass through `expand_tilde` / `deserialize_tilde_pathbuf` before a filesystem call. For `PathBuf` config fields use `#[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]`.
- **Vault-root precedence:** CLI override > config (`vault.root-path`) > marker-gated CWD (a `.obsidian/` dir must exist). No silent CWD fallback.
- **`embedding_config` pins `active_model` + `active_dim`** (384 for bge-small-en-v1.5); both cortex and oracle read these on dispatch so they never drift.
- **`scan_vault` is deterministic** (par_iter + sort by path). **Pinned** is strict `Some(true)` — typos/nulls parse as not-pinned, never a parse error.

## Patterns

- **Add a schema variant:** add the enum arm + `.as_str()`/`FromStr`, add a roundtrip/serde test.
- **Add a frontmatter field:** add to `Frontmatter`, parse in `from_value` (extract or extras), emit in canonical order in `to_yaml`.
- **New path config field:** annotate with `deserialize_tilde_pathbuf` (or call `expand_tilde` at the boundary for `String`).

## Anti-patterns

- **Literal `~` directory bug:** `fs::create_dir_all("~/vault")` creates a literal `~` dir in CWD — always `expand_tilde` first.
- **Fabricated fallback path:** `dirs::*_dir().unwrap_or_else(|| PathBuf::from("~/.local/share"))` creates a literal `~`. Use `.expect("… set HOME/XDG_*")` — panic is correct when both are unset.
- **Schema duplication / hardcoded model string** in consumer crates — import the enum; read `active_model` from `embedding_config`.

## Module Map

- **Schema/notes:** `schema.rs`, `frontmatter.rs`, `note.rs`, `detail.rs`, `table.rs` (+`table/`).
- **Paths:** `paths.rs` (+`paths/`).
- **Capture/ledger:** `ledger.rs`, `receipts.rs` (+`receipts/`), `intake.rs` (+`intake/`), `trace.rs`.
- **Embeddings/distilled:** `embedding.rs` (+`embedding/`), `distilled.rs` (+`distilled/`).
- **Search:** `search.rs` (+`search/`) — see `src/search/AGENTS.md`.
- **Tags/hygiene:** `canonical.rs`, `hygiene.rs`.
- **Misc:** `watcher.rs`, `rss.rs` (+`rss/`), `logging.rs`, `config.rs`, `fabric.rs`.
