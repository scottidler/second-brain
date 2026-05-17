# second-brain - Claude Code Instructions

## Project Overview

Cargo workspace consolidating obsidian-borg (ingestion daemon), obsidian-cortex (vault governance), and oracle (knowledge retrieval MCP) with a shared `vault` library crate. All tools operate on the same Obsidian vault with the same schema.

- **Repo:** `~/repos/scottidler/second-brain/`
- **Vault:** `~/repos/scottidler/obsidian/`
- **Design docs:** `docs/design/` (workspace consolidation, oracle MCP, classify pipeline, tag sweeper, etc.)

## Architecture

```
second-brain/
  vault/       -- shared library crate (schema, frontmatter, note, ledger, hygiene, canonical, config, logging, fabric, trace, distilled, embedding)
  distillers/  -- per-kind Stage-2 distillers (article, repo, video, thread, idea, passthrough) + Fabric port + dispatcher + render
  borg/        -- ingestion binary (Telegram, Discord, ntfy, HTTP, clipboard, CLI)
  cortex/      -- governance binary (lint, link, intel, sweep, daemon, migrate, summarize --backfill, embed)
  oracle/      -- knowledge retrieval MCP server (search [bm25/vector/hybrid], browse, domain briefs, ledger queries)
  config/      -- shared config source of truth (canonical-tags.yml, tag-mapping.yml, tag-proposals.yml)
```

## Key Conventions

- **Edition:** 2024
- **Logging:** env_logger + log (unified; no tracing) for borg/cortex/distillers; tracing for oracle (rmcp compatibility)
- **Parallelism:** `vault::note::scan_vault` and the CPU-bound per-note loops in `cortex::autotag`, `cortex::quality`, `borg::backfill`, `borg::audit`, and `cortex::migrate` use `rayon::par_iter` for data-parallel work. Async/LLM-bound loops stay tokio-based. The cortex daemon wraps its sync sweep calls in `tokio::task::block_in_place` so rayon worker threads do not starve the tokio runtime.
- **Schema:** vault::schema is THE single source of truth for Domain, NoteType, Origin, Status, Method. vault enums have feature-gated `schemars::JsonSchema` derives for MCP tool schemas.
- **L2 Distilled contract:** vault::distilled defines the `Distilled { summary, claims, tags, links, kind_specific, meta }` type produced by Stage-2 distillers. Borg renders it into the note body (`## Summary` / `## Claims` / `## Links` headings) and frontmatter (`distilled: true`, `distilled-extractor`, per-kind `cortex-*` keys) at publish time; cortex's `summarize --backfill` does the same for legacy notes.
- **Config:** borg reads ~/.config/borg/borg.yml; cortex reads ~/.config/obsidian-cortex/obsidian-cortex.yml; oracle reads ~/.config/oracle/oracle.yml
- **Shared config:** ~/.config/second-brain/ has canonical-tags.yml, tag-mapping.yml, tag-proposals.yml (source of truth in `config/`). Both borg and cortex read from this shared directory.
- **Patterns:** borg's Fabric patterns live at `~/.config/borg/patterns/` (source of truth in `borg/patterns/`). The L2 patterns are `distill-article.md`, `distill-repo.md`, `distill-thread.md`, `distill-video.md`, `distill-video-chunk.md`, `distill-video-reduce.md`.
- **Tags:** 110 canonical tags, max 7 per note. Borg post-filters Fabric output through the canonical vocabulary. Cortex `sweep` command migrates and governs tags.
- **One-way data flow:** Borg writes only to the vault filesystem (markdown files + staged artifacts). Oracle owns the SQLite FTS5 index and refreshes it via VaultWatcher when the vault changes. Borg's `Cargo.toml` does NOT depend on `rusqlite`.
- **Binary names:** `borg`, `cortex`, and `oracle` (no obsidian- prefix)

## Hybrid retrieval (Doc 2)

Oracle's `knowledge_search` accepts a `mode` parameter:

- `bm25` (FTS5 keyword search; the legacy mode)
- `vector` (semantic - fastembed `bge-small-en-v1.5` embedded query against `note_embeddings` BLOB rows, brute-force cosine)
- `hybrid` (default; pulls 50 candidates from each list and fuses via reciprocal rank fusion, k=60)

Embeddings live in the same SQLite file oracle reads for FTS5. Cortex is the only writer: `cortex embed [--backfill]` runs a read/inference/write loop (the write transaction stays under 200 ms regardless of batch size because `embed_batch` runs outside the transaction). The cortex daemon picks up the same code path on a configurable cadence (default 10 min). Active model and dimension are pinned in `embedding_config` so oracle and cortex cannot drift apart.

## Borg durable-capture stores

Every input borg receives is recorded in `system/views/borg-intake.md` synchronously at the door (BEFORE any allowed-chat check, classifier, or pipeline dispatch). Failed or rejected inputs are mirrored to `system/views/borg-dlq.md`. The invariant: every `trace_id` in `borg-intake.md` must also appear in `borg-ledger.md` (success path) or `borg-dlq.md` (failure path); `borg audit` walks that. Notes carry `ingested: <date>` in frontmatter (distinct from `date:`, which preserves the original content date across reingest) so the dashboard counts reingests as activity.

## Testing

```
cargo test --workspace
```

## Building

```
otto ci          # full CI pipeline
otto install     # build and install binaries
```

## Install (for /shipit)

```bash
cargo install --path borg && systemctl --user restart borg
cargo install --path cortex && systemctl --user restart cortex
cargo install --path oracle
cp borg/patterns/*.md ~/.config/borg/patterns/
mkdir -p ~/.config/second-brain && cp config/canonical-tags.yml config/tag-mapping.yml config/tag-proposals.yml ~/.config/second-brain/
# First run only: prefetch the fastembed model (~100 MB to the fastembed cache) so
# the next oracle/cortex invocation does not need network.
cortex embed --prefetch-model
```

borg and cortex run as systemd user daemons and must be restarted after install.
oracle is an MCP server launched on demand, no restart needed.
