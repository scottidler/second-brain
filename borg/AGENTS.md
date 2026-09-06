# borg — Ingestion Library

> Read this before touching `borg/`. Deeper nodes: `src/pipeline/AGENTS.md` (orchestration), `src/stages/AGENTS.md` (staged artifacts), `clients/AGENTS.md` (browser/hotkey clients).

## Purpose

borg owns durable capture, multi-channel ingest, and the staged pipeline that publishes notes into the Obsidian vault. It does NOT own the vault schema or frontmatter contracts (delegated to `vault`), nor CLI orchestration (owned by `sb`). lib-only; consumed by `sb`.

## Entry Points

- `serve_init(config, version) -> (ServerStartup, ServerHandle)` (`lib.rs`) — bootstraps Telegram/Discord/ntfy/Signal transports, watchdog, and the HTTP listener (port 8181).
- HTTP endpoints (`routes.rs`): `POST /ingest` (JSON), `POST /ingest/file` (multipart), `POST /note` (JSON), `GET /trace/{trace_id}` (`routes.rs:211`, a receipts row's terminal state — replay/reingest poll this because the receipts DB is per-host), `GET /health`, `GET /health/audit`. `/ingest`, `/ingest/file`, `/note`, and `/trace/{trace_id}` sit behind `routes::require_auth` (`routes.rs:49`), wired as a `route_layer` over that subrouter in `build_router` (`lib.rs:104`); `/health` and `/health/audit` stay open for probes/dashboard.
- CLI helpers: `note(config, text, tags)`, `ingest_file(config, file_path, tags, force)` → `IngestOutcome` (`lib.rs`).
- Pipeline dispatch: `pipeline::process_content(content, tags, method, force, config, trace_id) -> IngestResult`.

## Contracts & Invariants

1. **Durable capture at the door, before dispatch.** Every door calls `intake::record_received_with_sidecar()` synchronously before any classify/pipeline work — writes the raw-input sidecar + receipts `received` row. Both must succeed or the caller returns `Failed`. No accepted input is ever silently dropped.
2. **Receipts DB is the sole durable failure store.** Outcomes (success/duplicate/failure) are written at the pipeline's terminal chokepoint (`record_terminal_to_receipts`). State machine: `received → succeeded | failed(stage) | crashed`. A `succeeded` row also carries a `degraded` flag: the note landed but via a distill fallback (e.g. a fabric API error), so its body is impoverished. Because degraded notes read as `succeeded`, they hide in the status counts — `audit_health_stats` exposes `degraded_24h` (alongside `failed_24h`/`crashed_24h`) as the silent-quality signal, queryable via `count_degraded_since`/`sb borg log --degraded` and Warned by `sb doctor`. Legacy markdown intake/DLQ tables were excised.
3. **Notifications are detached from the HTTP response.** Sinks fire inside `tokio::spawn` off the response path (`routes.rs`); the response returns in milliseconds and survives client cancellation.
4. **Test notifications are disabled** when `notify::real_notifications_disabled()` is true (tripped by `cfg!(test)`, nextest, `CARGO_TARGET_TMPDIR`, or `BORG_DISABLE_DESKTOP_NOTIFY`). No test path leaks a real toast/message.
5. **`IngestRequest` is additive-only** (`types.rs`). Required: `url`. Optional: `tags`, `priority`, `force`, `method`. Never remove/rename a field — clients (extension, bookmarklet, hotkey) depend on it; a required-field addition needs a coordinated extension re-sign in the same PR.
6. **Signal privacy gate is load-bearing** (`signal.rs::accepted_envelope`): accepts only Note-to-Self sync and allowlisted-peer DMs, behind a fail-closed rate gate.
7. **Audit duplicate detection is content-hash, never `source:`-string** (`audit.rs`). `build_dup_index` groups notes by the SHA-256 of their normalized body; the `source:` field does NOT decide duplication (a shared batch label like `pais-migration` must never collapse distinct notes). Empty/short bodies (< `MIN_DUP_BODY_LEN`) never group. Body-identity is authority to REPORT only: `--fix duplicate` and blanket `--fix` move nothing. Destructive quarantine is the explicit `--fix duplicate-quarantine` opt-in, gated by `quarantine_eligible` (identical normalized bodies AND a shared non-empty `source:`), keeping the lexicographically-first note. See `docs/design/2026-06-19-content-hash-dedup.md`.

## Patterns

- **Add an ingest source/channel:** write an async `run(config, ...)` that generates a trace (`trace::generate`), calls `intake::record_received_with_sidecar()`, then detaches to `pipeline::process_content()`; route results to `notify::*`. Mirror `telegram.rs` / `discord.rs` / `ntfy.rs` / `signal.rs`. New channels go side-by-side, not behind a trait.
- **Stage flow for a URL:** Stage 0 fetch → Stage 1 extract → Stage 2 summarize/distill → Stage 3 publish (vault + ledger + receipts). See `src/stages/AGENTS.md`.

## Anti-patterns

- Don't call `process_content` synchronously from an HTTP handler — it blocks the response and exposes the pipeline to cancellation.
- Don't write to ledger/receipts from inside `process_content`; use the terminal dual-write chokepoint so all outcomes are captured consistently.
- Don't inline the `real_notifications_disabled` check at call sites — call the sink unconditionally; the sink decides.

## Module Map

**Sources (transports):** `telegram.rs`, `discord.rs`, `ntfy.rs`, `github.rs` (+`github/`), `youtube.rs`, `slides.rs`, `jina.rs`, `signal.rs` (+`signal/`).

**Core pipeline:** `pipeline.rs` (+`pipeline/`), `stages.rs` (+`stages/`), `intake.rs` (+`intake/`), `receipts.rs` (+`receipts/`), `router.rs`, `routes.rs`, `dispatch.rs` (per-transport pipeline-run + notify boilerplate shared by telegram/ntfy/routes), `triage.rs`, `replay.rs` (+`replay/`), `backfill.rs` (+`backfill/`), `harvest.rs` (+`harvest/`, `sb borg harvest`: pull-based ingestion from clyde session exports).

**Infrastructure:** `notify.rs` (+`notify/`), `watchdog.rs` (+`watchdog/`), `migrate.rs`, `config.rs`, `health.rs`, `startup.rs`, `retention.rs` (+`retention/`), `blocklist.rs` (+`blocklist/`), `rkvr.rs` (+`rkvr/`), `service.rs` (systemd unit install/uninstall + render, `install_systemd`/`render_systemd_unit`), `backoff.rs` (`ExponentialBackoff` for transport reconnect).

**Content/extract:** `markdown.rs`, `quality.rs`, `description.rs`, `hygiene.rs`, `assets.rs`, `ocr.rs`, `transcription.rs` (+`transcription/`), `extraction.rs`, `fabric.rs`, `audit.rs`, `byline.rs` (deterministic author extraction from raw HTML), `readability.rs` (in-process clean-article extraction: `dom_smoothie` + `htmd`, replaces the `defuddle` shell-out), `thread.rs` (LLM-free thread/social-post title construction), `dedupe.rs` (`sb borg dedupe-sessions`: retire surplus harvest-session-note forks by trace identity), `eval.rs` (+`eval/`, `sb borg eval`: distillation-quality eval harness against golden fixtures).

**Extension lifecycle:** `extension.rs` + `extension/{manifest,schema,sign,install}` — Firefox .xpi manifest/schema/signing (see root CLAUDE.md for the full lifecycle contract).

**Types/glue:** `types.rs` (`ContentKind`, `IngestRequest`, `IngestResult`, `IngestStatus`, `Envelope`, …), `trace.rs`, `error.rs`, `ledger.rs`, `opts.rs`.

## Related Context

- Schema/frontmatter/paths: `../vault/AGENTS.md`
- Stage-2 distillers borg invokes: `../distillers/AGENTS.md`
- Root invariants: `../CLAUDE.md`
