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
- `lib::unlink` — retract stoplisted wikilink markup the linker already landed.
- `intel::run` — weekly/daily summary generation.

## Contracts & Invariants

- **cortex is the ONLY writer to `note_embeddings`** in the oracle SQLite index. Other commands write vault markdown only; oracle's `VaultWatcher` reindexes on mtime change.
- **Embed write transaction stays <200ms** (batch=64). The three-phase loop — read (no txn) → CPU inference (no lock) → write-only txn — is load-bearing so oracle's index writes aren't starved.
- **Embed inference runs in its OWN rayon pool, never the global one.** `embed::in_inference_pool` (sized by `embed.workers`) wraps every `embed_batch` call at the single choke point (`embed_in_sub_batches`) all three kind-batches funnel through. Candle's matmul fans out on rayon; on the global pool it consumes the workers `classify`/`quality`/`autotag`/lint run their `par_iter` on. On 2026-08-16 that starved governance for **two days** (one embed tick, 4 pegged workers, 50 harvested notes stuck in `inbox/`, systemd still reporting `active (running)`). `desk` is AVX-only (no AVX2/FMA), so gemm has no vectorized f32 microkernel and inference is permanently slow there — slow is fine, slow-and-shared is not. Do NOT "simplify" this back onto the global pool, and do not shrink `daemon.rayon-threads` to defend against embed: that throttles governance to guard against what the isolation already prevents.
- **Every embed tick is bounded in chunks, not just notes.** `batch_size` counts NOTES; `embed.max-chunks-per-tick` (`cap_work_by_chunks`) caps the flattened chunk count. Truncation happens on a NOTE boundary only — the write phase replaces a note's entire chunk set, so a half-embedded note would land truncated and then read as complete. A single note that alone exceeds the cap is the deliberate exception (embedded whole, warned) or it would defer forever and never converge.
- **Daemon wraps rayon work in `tokio::task::block_in_place`** (sweeps, migrations, linking, intel) so the async watcher/timers/embed ticks aren't starved.
- **Schema source of truth is `vault::schema`**; tag vocabulary is `canonical-tags.yml`, max **7 tags/note** (`canonical::filter_and_cap`).
- **`embedding_config` pins model + dim** (`index.set_active_embedding`) so cortex and oracle never drift on the embedding backend.
- **One wikilink stopword vocabulary, three consumers.** `graph.wikilink-stopwords` is read ONLY at a composition root (`lib::link`, `lib::unlink`, `graph::build`) and threaded in as a `stopwords::Stopwords`; `linking` refuses to write the markup, `graph` refuses to mint the edge, `unlink` retracts what already landed. Never re-derive the list or the predicate in a second place — the writer and the edge builder must judge the same raw `[[target]]`, before path resolution, or they drift. Defaults EMPTY: code never silently suppresses a link.
- **`unlink` is the only phase that retracts landed markup**, and only on explicit invocation (`--apply`). The linker and graph passes are add-only/ignore-only by design; `graph::tests::stoplisted_wikilink_leaves_the_note_body_byte_identical` pins that. `unlink` retracts only what the linker could have written (skips authored notes, hub bodies, code, transclusions), so it stays a true inverse rather than a vault-wide edit.
- **HTTP stack split is deliberate, not drift:** cortex uses blocking `ureq` for its one synchronous LLM POST (`llm::complete`); borg uses async `reqwest`. cortex's LLM call sits inside a synchronous sweep/intel loop (already `block_in_place`-wrapped), so a blocking client is the lighter, simpler fit — no tokio reactor needed for a single request. Don't "unify" them on `reqwest`.

## Patterns

- **Add a lint rule:** write `lint_X(notes, config) -> Report` + `apply_X(...) -> Result<count>`; register in the `lib::lint` dispatch chain and (optionally) a daemon auto-apply tick; gate `apply_X` on `opts.apply`.
- **Add a command:** module with `pub fn run(vault_root, config, opts)`; branch scan → lint/apply on opts; wire into `sb` CLI and (optionally) a daemon tick.
- **Embed loop:** `stale_embedding_targets` (read) → `embed_batch` (outside txn) → `upsert_embeddings_batch` (single `BEGIN IMMEDIATE … COMMIT`).

## Anti-patterns

- Don't hold the embed write txn across `embed_batch` — it starves oracle's writes.
- Don't call `model.embed_batch` outside `in_inference_pool` — that puts inference back on the governance pool.
- Don't add a long-running loop that logs only at start and finish. The two-day embed tick was invisible because of exactly that; `embed_in_sub_batches` logs per sub-batch on purpose.
- Don't run rayon sweeps on a tokio worker thread — always `block_in_place`.
- Don't load the embedding model per daemon tick — load once at startup and hand a reference (avoids per-tick allocation leak).
- Don't fan unbounded chunks into candle's rayon pool — respect `max_chunks_per_call`.

## Module Map

- **Root/orchestration:** `lib.rs` (`lint`/`link` dispatch), `daemon.rs` (event loop, watcher, systemd, tick scheduling, cycle detection).
- **Classification & linking:** `classify.rs`, `autotag.rs`, `scope.rs`, `naming.rs`, `tags.rs`; `linking.rs`, `links.rs`, `unlink.rs`, `stopwords.rs`, `intel.rs`.
- **Quality:** `quality.rs`, `duplicates.rs`, `frontmatter.rs`.
- **Embeddings:** `embed.rs` (+`embed/`).
- **Knowledge graph (graph-augmented-memory):** `graph.rs` (builds the materialized `edges` table oracle's graph retrieval reads), `entities.rs` (`sb cortex entities --discover`: proposes new glossary entries into `entity-proposals.yml`), `hub.rs` (+`hub/`: `sb cortex hub` stubs/refreshes `entities/*.md` hub notes; `hub/render.rs` is the deterministic claims-by-vector body assembly, `hub/asymmetry.rs` is the read-vs-applied classifier), `association.rs` (`cortex associate`: groups+merges harvest session notes by slug/similarity), `bridge.rs` (`sb cortex bridge-backfill`/`bridge-apply`: one-time historical multi-repo hub backfill), `memgraph.rs` (typed `fact` edges + consolidation agents, Phase 5 MemGraphRAG).
- **Lifecycle:** `sweep.rs` (+`sweep/`), `summarize.rs` (+`summarize/`), `migrate.rs`, `state.rs`, `report.rs`, `schema_docs.rs` (+`schema_docs/`: `sb cortex schema` renders `system/schemas/{domain,type,origin,status}-values.md` from `vault::schema`; snapshot fixtures under `schema_docs/fixtures/`).
- **Infra/config:** `config.rs` (schema source of truth), `vault.rs` (`scan_vault` adapter), `startup.rs` (`validate_canonical_assets` gate), `llm.rs`, `fabric.rs`, `opts.rs`, `testutil.rs`.

## Related Context

- Schema/embeddings/search primitives: `../vault/AGENTS.md` (+ `../vault/src/search/AGENTS.md`)
- The index oracle reads: `../oracle/AGENTS.md`
