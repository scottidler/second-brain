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
  borg/        -- ingestion library (Telegram, Discord, ntfy, HTTP, clipboard, CLI) -- lib-only, consumed by sb
  cortex/      -- governance library (lint, link, intel, sweep, daemon, migrate, summarize --backfill, embed) -- lib-only, consumed by sb
  oracle/      -- knowledge retrieval library (search [bm25/vector/hybrid], browse, domain briefs, ledger queries) -- lib-only, consumed by sb
  sb/          -- unified CLI binary: `sb borg ...`, `sb cortex ...`, `sb oracle ...`, plus `sb status/doctor/bootstrap`
  config/      -- shared config source of truth (canonical-tags.yml, tag-mapping.yml, tag-proposals.yml)
  config/templates/ -- starter configs that `sb bootstrap` drops into ~/.config/sb/
```

Systemd unit files are NOT in the repo. They are written into `~/.config/systemd/user/` by `sb borg daemon --install` and `sb cortex daemon --install`. Source of truth for unit content lives in `borg::install_systemd` (`borg/src/lib.rs`) and `cortex::install_systemd_service` (`cortex/src/daemon.rs`).

## Key Conventions

- **Edition:** 2024
- **Logging:** env_logger + log (unified; no tracing) for borg/cortex/distillers; tracing for oracle (rmcp compatibility)
- **Parallelism:** `vault::note::scan_vault` and the CPU-bound per-note loops in `cortex::autotag`, `cortex::quality`, `borg::backfill`, `borg::audit`, and `cortex::migrate` use `rayon::par_iter` for data-parallel work. Async/LLM-bound loops stay tokio-based. The cortex daemon wraps its sync sweep calls in `tokio::task::block_in_place` so rayon worker threads do not starve the tokio runtime.
- **Schema:** vault::schema is THE single source of truth for Domain, NoteType, Origin, Status, Method. vault enums have feature-gated `schemars::JsonSchema` derives for MCP tool schemas.
- **L2 Distilled contract:** vault::distilled defines the `Distilled { summary, claims, tags, links, kind_specific, meta }` type produced by Stage-2 distillers. Borg renders it into the note body (`## Summary` / `## Claims` / `## Links` headings) and frontmatter (`distilled: true`, `distilled-extractor`, per-kind `cortex-*` keys) at publish time; cortex's `summarize --backfill` does the same for legacy notes.
- **Config:** all three subsystems read from `~/.config/sb/`. Borg reads `~/.config/sb/borg.yml`; cortex reads `~/.config/sb/cortex.yml`; oracle reads `~/.config/sb/oracle.yml`. Shared paths are resolved through `vault::paths` (single source of truth). On legacy installs `sb bootstrap` auto-migrates from the old layout (`~/.config/{borg,cortex,obsidian-cortex,oracle,second-brain}`) into `~/.config/sb/`; the legacy directories are left in place (a future `sb bootstrap --prune-legacy-config` is the cleanup verb).
- **Shared config:** `~/.config/sb/` also holds the cross-subsystem catalogue files: `canonical-tags.yml`, `tag-mapping.yml`, `tag-proposals.yml` (source of truth in `config/`). Both borg and cortex read these from the same shared location.
- **Patterns:** borg's Fabric patterns live at `~/.config/sb/patterns/` (source of truth in `borg/patterns/`). The L2 patterns are `distill-article.md`, `distill-repo.md`, `distill-thread.md`, `distill-video.md`, `distill-video-chunk.md`, `distill-video-reduce.md`.
- **Vault root:** resolved by `vault::paths::resolve_vault_root` with explicit precedence: CLI override (`--vault`) > config (`vault.root-path`) > marker-gated CWD (a `.obsidian/` directory must be present). No silent CWD fallback; commands error with a clear message if none of the three are set.
- **Tags:** 110 canonical tags, max 7 per note. Borg post-filters Fabric output through the canonical vocabulary. Cortex `sweep` command migrates and governs tags.
- **One-way data flow:** Borg writes the vault filesystem (markdown files + staged artifacts) and its own SQLite receipts log at `~/.local/share/sb/borg/receipts.db` (status: received / succeeded / failed by stage). Oracle owns its own separate SQLite FTS5+vector index and refreshes it via VaultWatcher when the vault changes. The two SQLite files are different files with different writers; nothing in borg opens oracle's DB and nothing in oracle opens borg's writer path (oracle opens borg's receipts DB read-only for the `failure_history` MCP tool).
- **Notifications:** borg ships three notification sinks (`notify::Telegram`, `notify::Desktop`, and `notify::Signal`) wired in parallel from the daemon - the desktop sink replaces the dead path the Firefox extension used to render, Signal acks back through the same `signal-rs` `Client` that handles inbound. All three fire from the same producer points inside every spawned ingest task; future channels go side-by-side, not behind a trait. All three sinks consult `notify::real_notifications_disabled()` (tripped by `cfg!(test)`, `CARGO_TARGET_TMPDIR`, `NEXTEST_RUN_ID`, or the `BORG_DISABLE_DESKTOP_NOTIFY` override) so no test path can leak a real toast/message to the operator.
- **Signal transport** (`docs/design/2026-05-24-signal-as-borg-transport.md`): in-process via the `signal-rs` crate, peer to Telegram. Sole-machine ingest pinned by `signal.host` (Signal-Server fans Note-to-Self to every linked device, so unpinned multi-machine would silently double-ingest). State dir defaults to `~/.local/share/sb/borg/signal-state/`, distinct from `signal-rs`'s CLI default (`~/.local/share/signal-rs/`) - sharing the dir would corrupt the Double Ratchet. The privacy gate (`borg/src/signal.rs::accepted_envelope`) accepts only two patterns: `Envelope::SyncMessage(SyncMessage::Sent { destination: Some(SelfSync), group_id: None, .. })` (Note-to-Self) and `Envelope::DataMessage { source: Recipient::Aci(<allowed>), group_id: None, .. }` (allowlisted peer DM). Backed by a fail-closed Note-to-Self rate gate (`signal.notetoself_rate_threshold_per_hour`, default 100) that pauses ingest until daemon restart on overflow - active runtime backstop for an upstream `signal-rs` regression on the wire-ACI → `Recipient::SelfSync` mapping. Bootstrap is out-of-band: `signal-rs link --name borg --state-dir ~/.local/share/sb/borg/signal-state/` after stopping the borg daemon, then restart. `sb doctor signal` reports linked-status, state_dir health, and CLI-default collisions.
- **Firefox extension lifecycle:** owned by `sb borg extension {sign, install, uninstall, stage, show, version}` (originally specced at `docs/design/2026-05-21-extension-lifecycle.md`, reshaped by `docs/design/2026-05-22-extension-manifest-binary-versioned.md` which is the current source of truth). The .xpi manifest `version` field is sb's `env!("CARGO_PKG_VERSION")`, threaded into `extension::stage` and `extension::sign::run` at CLI entry; the borg library does not call `env!` for the manifest. Sign and stage materialise the manifest, schema, static assets, and `.amo-upload-uuid` sidecar into a `tempfile::TempDir` and run `web-ext sign` there; nothing is committed and there is no validate/regen ritual. Structural correctness is gated by unit tests in `borg/src/extension/manifest/tests.rs` and the integration test `borg/tests/stage_produces_valid_extension_dir.rs`. For inspection, run `sb borg extension show` (or `--schema`). `IngestRequest` evolves additively-only; required-field additions require a coordinated extension re-sign in the same PR (`borg/tests/extension_body_matches_ingest_request.rs` enforces this). Day-to-day shipping is `bump && otto deploy` (the deploy task's last step refreshes the extension via `--no-policy --if-installed`, a no-op on daemon-only machines). First-machine bootstrap is `sudo sb bootstrap --extension`. Dev workflow: `sb borg extension stage --to ./dev-ext`, then load that directory in `about:debugging`.
- **Build scripts:** `sb/build.rs` is the only build script in the workspace; it emits `GIT_DESCRIBE` per the scaffold pattern (`~/repos/scottidler/scaffold/build.rs`). Library crates (`borg`, `cortex`, `oracle`, `vault`, `distillers`) have no build script. sb threads its versions into the libraries through public APIs (`borg::serve_init(config, env!("GIT_DESCRIBE"))` for `/health`; `extension::{stage, sign, install, show}(..., env!("CARGO_PKG_VERSION"), ...)` for the .xpi).
- **Binary name:** `sb` (one binary, subcommands `borg`/`cortex`/`oracle`/`status`/`doctor`/`bootstrap`). The borg/cortex/oracle crates are lib-only.

## Hybrid retrieval (Doc 2)

Oracle's `knowledge_search` accepts a `mode` parameter:

- `bm25` (FTS5 keyword search; the legacy mode)
- `vector` (semantic - fastembed `bge-small-en-v1.5` embedded query against `note_embeddings` BLOB rows, brute-force cosine)
- `hybrid` (default; pulls 50 candidates from each list and fuses via reciprocal rank fusion, k=60)

Embeddings live in the same SQLite file oracle reads for FTS5. Cortex is the only writer: `cortex embed [--backfill]` runs a read/inference/write loop (the write transaction stays under 200 ms regardless of batch size because `embed_batch` runs outside the transaction). The cortex daemon picks up the same code path on a configurable cadence (default 10 min). Active model and dimension are pinned in `embedding_config` so oracle and cortex cannot drift apart.

## Borg durable-capture stores

Every input borg receives is durably recorded in the receipts SQLite DB at `~/.local/share/sb/borg/receipts.db` with `status=received` synchronously at the door (BEFORE any allowed-chat check, classifier, or pipeline dispatch). The row is mutated in place to `succeeded` (with the resulting note path) or `failed` (with one of seven `failure_stage` values: `intake-rejected`, `classify-failed`, `fetch-failed`, `quality-blocked`, `pipeline-timed-out`, `publish-failed`, `crashed`) at terminal time. The success subset is also appended to `system/views/borg-ledger.md` for in-Obsidian browsing; failures stay in the receipts DB only. Query the receipts log via `sb borg log [--status ...] [--method ...] [--stage ...] [--since ...] [--source <LIKE>] [--trace <id>]`. Notes carry `ingested: <date>` in frontmatter (distinct from `date:`, which preserves the original content date across reingest) so the dashboard counts reingests as activity. The legacy markdown bookkeeping (`borg-intake.md`, `borg-dlq.md`, `borg-dlq-archive.md`, `borg-orphans.md`) is dual-written for safety during the rollout window; `bin/migrate-receipts` transitions the data into the receipts DB and `bin/migrate-receipts --prune-legacy` removes the markdown files once verified.

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

The workspace ships a single binary `sb` that subsumes the old `borg`, `cortex`, and `oracle` CLIs. Subcommands are namespaced: `sb borg ingest`, `sb cortex sweep`, `sb oracle serve`. See `sb --help` for the full surface.

```bash
otto deploy
# First run only: prefetch the fastembed model (~100 MB) so the
# next oracle/cortex invocation does not need network.
sb cortex embed --prefetch-model
```

`otto deploy` builds the single `sb` bin, installs it to `~/.cargo/bin/`, syncs the fabric patterns and canonical tags to `~/.config/sb/`, and restarts any borg/cortex systemd units that already exist. Systemd unit content is owned by `sb borg daemon --install` and `sb cortex daemon --install` - run those on a fresh machine to write the units; the deploy task only restarts.

oracle is an MCP server launched on demand via `.mcp.json` -> `sb oracle serve`. No restart needed.

For first-time setup on a new machine: `sb bootstrap` drops starter config files into `~/.config/sb/` and prefetches the fastembed cache. On machines with the legacy `~/.config/{borg,cortex,obsidian-cortex,oracle,second-brain}` layout, `sb bootstrap` auto-detects and migrates them.
