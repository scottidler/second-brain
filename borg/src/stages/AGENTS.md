# borg::stages — Staged Artifact Capture

> Local node for the artifact model. Parent: `../../AGENTS.md`. Orchestration that drives these stages: `../pipeline/AGENTS.md`.

## Purpose

Defines the `ArtifactStore` trait (disk-layout abstraction) and the per-stage artifact model. Each trace is a tree of on-disk files (envelope, body, fetched bytes, transcript, summary, rejection). Stages write their outputs; later stages read prior outputs from disk. Pure bytes-on-disk — no long-lived DB, no cross-trace in-memory cache.

## What a "stage" is

A named step that reads a prior stage's artifacts and writes new ones:

- **Stage 0 (raw)** — fetch URL (or accept binary); writes `envelope.yml`, `body.txt`, `attachments/*`, `fetched.html`, `fetched.yml`.
- **Stage 1 (transcript)** — extract text/OCR/transcription; writes `transcript.md`, `transcript.yml`.
- **Stage 2 (summary)** — LLM summarize/distill; writes `summary.md`, `summary.yml`.
- **Gate** — any stage may write `rejection.yml`; a rejected trace skips all downstream stages.
- **Stage 3 (publish)** — render the distilled shape, publish to vault, record ledger row.

## Disk Layout

Config-selectable via `StagingLayout`:

- **PerTrace (default):** one dir per trace containing `envelope.yml`, `body.txt`, `attachments/`, `fetched.{html,yml}`, `transcript.{md,yml}`, `summary.{md,yml}`, `rejection.yml`.
- **PerStage:** artifacts grouped by stage (`raw/<trace>/…`, `transcripts/<trace>.{md,yml}`, `summaries/<trace>.{md,yml}`).

## ArtifactStore Contract (selected)

`write_envelope`/`read_envelope`, `write_body`, `write_attachment`, `write_fetched`/`read_fetched`, `read_raw` (assembles envelope+body+attachments+fetched into the `RawCapture` Stage 1 consumes — never hits the network), `write_transcript`/`read_transcript`, `write_summary`/`read_summary`, `write_rejection`/`read_rejection`, `list_traces(filter)`, `delete_trace`.

**Implementations:** `FsArtifactStore` (filesystem; atomic temp-then-rename for `fetched.*`), `MemArtifactStore` (in-memory, tests).

## Invariants

1. **Append-only per trace.** Stage 0 writes once; later stages append. No read-modify-write of an existing artifact.
2. **Envelope is the schema anchor.** Every trace starts with `write_envelope()`; its `kind` (`IngestKind`) and `received_at` are canonical for classification and age.
3. **Fetched bytes are atomic.** `write_fetched` is temp-then-rename — no partial `fetched.html`/`fetched.yml`.
4. **Rejection records are terminal markers.** If `read_rejection` returns `Some`, the trace is rejected and downstream stages skip it. One rejection per trace.
5. **Retention-windowed.** `retention.rs` deletes traces older than the configured window (by `received_at`), best-effort.

## Module Map

- `artifact.rs` — `ArtifactStore` trait + `FsArtifactStore`/`MemArtifactStore`; trace listing/filtering.
- `raw.rs` — Stage 0: blocklist gate, fetch routing, attachment acceptance, fetched persist.
- `classify.rs` — Gate 1 block-page detection on fetched bytes.
- `extract.rs` — Stage 1 extractor dispatch (text / OCR / transcription).
- `summarize.rs` — Stage 2 LLM summarize/distill + Gate 2 paraphrase detection.
- `distill.rs` — per-type distillers → `vault::distilled::Distilled`.
- `fetcher.rs` — `Fetcher` trait + Jina / browser-UA / fabric / caching / multi implementations.
