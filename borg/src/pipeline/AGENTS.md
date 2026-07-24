# borg::pipeline — Ingest Orchestration

> Local node for the pipeline. Parent: `../../AGENTS.md`. Artifact model: `../stages/AGENTS.md`.

## Purpose

Orchestrates ingestion: receives content (URL/image/PDF/audio/text), routes to type-specific handlers, stages through fetch → extract → summarize → distill → publish, and dual-writes the outcome to the receipts DB. Owns permit gating, hard-timeout enforcement, deduplication, and the terminal state transition that closes a trace.

## Entry Point

- `process_content(content: ContentKind, tags, method, force, config, trace_id?) -> IngestResult` (`pipeline.rs`). Returns once the (async) work completes; all work runs on a detached task spawned by the caller.

## Stage Flow

```
Stage 0 (raw)  classify URL + fetch / accept binary   → envelope + fetched.*
Stage 1 (tx)   extract text / transcript / OCR         → transcript.*
Stage 2 (sum)  summarize / distill (LLM)               → summary.*
Gate 2         paraphrase-detect on summary            → rejection.yml if blocked
Stage 3 (pub)  render note + publish to vault          → ledger row + receipts row
```

Each type handler (`process_url`, `process_image`, `process_audio`, `process_document_file`, `process_text`) wraps its logic in a hard timeout and calls `record_terminal_to_receipts()` at the chokepoint.

## Invariants

1. **Permit before work.** `GENERAL_PERMITS` (cap `pipeline.max-concurrent-traces`) is acquired at the top of `process_content`. No async work runs before the permit is held.
2. **HEAVY_PERMITS at the subprocess call site, not at dispatch.** yt-dlp / fabric / ffmpeg / Groq-OCR handlers acquire `HEAVY_PERMITS` immediately before the subprocess — closing the gap where a URL classified as "article" still fans out to ffmpeg.
3. **TraceLeaseGuard before the permit wait.** Each trace writes a renewable lease (`lease_owner_pid` + `lease_until`) onto its shared receipts row before the permit, renews it at permit grant, and clears it on Drop UNLESS `cancel()`led after the terminal write (which NULLs the lease in the same UPDATE). Fail CLOSED: a failed initial lease write aborts the trace to a terminal failure. The watchdog reads liveness from that shared row (absent-or-expired lease = reap-eligible, checked atomically in the promotion UPDATE), so it never falsely reaps a trace a SEPARATE `sb borg harvest` process is still working. See `docs/design/2026-07-24-harvest-watchdog-cross-process-reaping.md`.
4. **Hard timeout bounds every handler** (`tokio::time::timeout(hard_timeout_secs)`). On timeout the future drops, guards release, and the outer dispatch records `Failed { reason: "timeout" }`.
5. **Terminal status is final and atomic.** `record_terminal_to_receipts()` records exactly one of `Completed | Duplicate | Failed`. The UPDATE carries `WHERE status='received'` so concurrent writes can't stomp; SQLite serializes via `busy_timeout`. `Queued` is never terminal.
6. **Inflight guard dedupes concurrent duplicates.** On non-force runs, `InflightGuard::try_acquire(canonical_url)` is held for the rest of the URL handler; a concurrent second attempt returns `Duplicate{original_date:"inflight"}` (a success for receipts). Force runs skip the guard.
7. **Cross-restart dedup** checks the ledger for a prior success of the canonical URL after the inflight guard passes.
8. **Atomic publish.** Stage 3 writes to a temp file, applies frontmatter, then renames — no half-written note ever lands in the vault.

## Patterns

- **Add a content type:** add a `ContentKind` variant + a `process_<type>` handler that wraps logic in timeout + (for non-force) InflightGuard, calls extractors/distillers, returns `IngestResult`; wire it into the `process_content` match.
- **Add a gate:** a function returning `Result<(), RejectionRecord>`; on failure write the rejection and skip downstream stages.

## Anti-patterns

- Don't add subprocess calls outside the HEAVY_PERMITS sites — the watchdog won't account for their concurrency.
- Don't write to receipts before the terminal chokepoint — state transitions must stay atomic.
- Don't leave any handler un-timeout-wrapped — unbounded futures starve the permit pool.
