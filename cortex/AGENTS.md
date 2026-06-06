# cortex — Vault Governance Library

> Read before touching `cortex/`. Parent: `../CLAUDE.md`.

## Purpose

cortex governs an *existing* vault: it lints notes against rules (naming, frontmatter, tags, scope, broken links, duplicates, quality, auto-tag), classifies inbox notes into domains, sweeps/migrates tag vocabularies, embeds notes for hybrid search, and runs a tokio daemon that applies governance on a cadence / on file change. It does NOT ingest — borg writes the vault, cortex governs what is already there. lib-only; consumed by `sb`.

## Entry Points

Each command exposes `run(vault_root, config, opts)` returning typed reports/outcomes (`sb` formats):

- `classify::run` — Tier-1 tag/URL heuristics + Tier-2 LLM context to promote inbox notes.
- `sweep::run` — tag-vocabulary migration / cold-note scan (proposal-driven).
- `embed::run` — read/infer/write loop: pull stale rows → batch-embed (fastembed) → upsert `note_embeddings`.
- `summarize::run` — `--backfill`: walk vault, infer distill kind, invoke Fabric, rewrite legacy notes with rendered sections.
- `migrate::run` — planned file moves + frontmatter/value transforms.
- `daemon::run` — long-running watcher + scheduled sweep/intel/embed ticks; systemd install/uninstall.
- `lib::lint` / `lib::link` — dispatch enabled lint/linking rules.
- `intel::run` — weekly/daily summary generation.

## Contracts & Invariants

- **cortex is the ONLY writer to `note_embeddings`** in the oracle SQLite index. Other commands write vault markdown only; oracle's `VaultWatcher` reindexes on mtime change.
- **Embed write transaction stays <200ms** (batch=64). The three-phase loop — read (no txn) → CPU inference (no lock) → write-only txn — is load-bearing so oracle's index writes aren't starved.
- **Daemon wraps rayon work in `tokio::task::block_in_place`** (sweeps, migrations, linking, intel) so the async watcher/timers/embed ticks aren't starved.
- **Schema source of truth is `vault::schema`**; tag vocabulary is `canonical-tags.yml`, max **7 tags/note** (`canonical::filter_and_cap`).
- **`embedding_config` pins model + dim** (`index.set_active_embedding`) so cortex and oracle never drift on the embedding backend.

## Patterns

- **Add a lint rule:** write `lint_X(notes, config) -> Report` + `apply_X(...) -> Result<count>`; register in the `lib::lint` dispatch chain and (optionally) a daemon auto-apply tick; gate `apply_X` on `opts.apply`.
- **Add a command:** module with `pub fn run(vault_root, config, opts)`; branch scan → lint/apply on opts; wire into `sb` CLI and (optionally) a daemon tick.
- **Embed loop:** `stale_embedding_targets` (read) → `embed_batch` (outside txn) → `upsert_embeddings_batch` (single `BEGIN IMMEDIATE … COMMIT`).

## Anti-patterns

- Don't hold the embed write txn across `embed_batch` — it starves oracle's writes.
- Don't run rayon sweeps on a tokio worker thread — always `block_in_place`.
- Don't load the embedding model per daemon tick — load once at startup and hand a reference (avoids per-tick allocation leak).
- Don't fan unbounded chunks into candle's rayon pool — respect `max_chunks_per_call`.

## Module Map

- **Root/orchestration:** `lib.rs` (`lint`/`link` dispatch), `daemon.rs` (event loop, watcher, systemd, tick scheduling, cycle detection).
- **Classification & linking:** `classify.rs`, `autotag.rs`, `scope.rs`, `naming.rs`, `tags.rs`; `linking.rs`, `links.rs`, `intel.rs`.
- **Quality:** `quality.rs`, `duplicates.rs`, `hygiene.rs`, `frontmatter.rs`.
- **Embeddings:** `embed.rs` (+`embed/`).
- **Lifecycle:** `sweep.rs` (+`sweep/`), `summarize.rs` (+`summarize/`), `migrate.rs`, `state.rs`, `report.rs`.
- **Infra/config:** `config.rs` (schema source of truth), `vault.rs` (`scan_vault` adapter), `startup.rs` (`validate_canonical_assets` gate), `llm.rs`, `fabric.rs`, `opts.rs`, `testutil.rs`.

## Related Context

- Schema/embeddings/search primitives: `../vault/AGENTS.md` (+ `../vault/src/search/AGENTS.md`)
- The index oracle reads: `../oracle/AGENTS.md`
