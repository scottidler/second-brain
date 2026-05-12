# Design Document: Borg Intake Log + Dead Letter Queue

**Author:** Scott Idler
**Date:** 2026-05-11
**Status:** Draft
**Review Passes Completed:** 5/5

## Summary

Every user input to borg must land in exactly one of two append-only stores: a new `borg-intake.md` (recorded the instant we receive the input) or a new `borg-dlq.md` (recorded when the pipeline fails or refuses to process the input). Silent drops are bugs. This doc adds those two files, threads a single `trace_id` through intake → ledger → dlq so the invariant is mechanically checkable, fixes the dashboard's reingest-blindness by introducing an `ingested:` frontmatter field distinct from `date:`, and resolves the existing image-note filter bug by switching dashboard queries from `WHERE source != null` to `WHERE origin = "assisted"`.

## Problem Statement

### Background

Borg has six intake paths: Telegram bot, Discord bot, ntfy subscription, HTTP `/ingest` and `/note` and `/ingest/multipart`, ntfy, and a CLI subcommand. Each ingest is expected to produce one row in `system/views/borg-ledger.md` and at most one note in the vault. Today's contract is "if we got an input, the ledger will have a row." Empirically that contract is broken in several places:

1. **Silent intake drops.** Telegram messages from chats not in `allowed-chat-ids` return `Ok(())` with zero log lines (`borg/src/telegram.rs:135-137`). Messages of unsupported types — stickers, video notes, animations, GIFs, polls, locations, contacts — fall through to a `DEBUG`-level "Empty message, ignoring" branch (`telegram.rs:391-393`). With production log-level at `info`, these never produce a single visible event.
2. **Silent pipeline hangs.** Three pipelines this month — `ht-fb0810` (2026-05-08 16:15), `ht-cf3881` (2026-05-08 16:22), `ht-b925a7` (2026-05-02 08:42) — logged "Starting ingest" and then never emitted another line. No ledger row was produced. `ht-fb0810` is the worst case: it ran the destructive replace path, removed the old note `why-5-million-people-are-obsessed-with-excalidraw.md`, marked its ledger row as replaced, and then hung. Old note gone, new note never written, ledger row pointing at nothing. The atomic-publish work in commit `b950995` (Phase 3 of `2026-05-08-borg-pipeline-resilience.md`) closes the data-loss window prospectively; it does not surface the failure when it does happen.
3. **Dashboard miscounts reingests.** The dashboard's dataview queries filter notes by `WHERE source != null AND date = date(today) - dur(N day)`. The frontmatter `date:` field is *preserved across reingest by design* — so a reingest on 2026-05-08 of a note originally ingested 2026-04-16 keeps `date: 2026-04-16` and lands in the dashboard's "Last Month" bucket, invisible as 2026-05-08 activity. Concrete evidence from 2026-05-11: ledger ✅ rows in the May 4-9 window = 32; dashboard "This Week (May 4-9)" = 31. The off-by-one is exactly the 2026-05-08 reingest of `youtube.com/watch?v=KRpZSvtMiTI`.
4. **Image notes excluded from the dashboard.** Image-only notes use `asset:` in frontmatter, not `source:`. The `WHERE source != null` gate silently drops them from every dashboard panel. Two such notes exist today; the 2026-05-10 lamp-spec image is one of them and is invisible on the dashboard despite being correctly recorded in the ledger.

### Problem

Borg cannot reliably answer two questions:

1. **"Did borg receive my input X?"** — currently answerable only by grepping the `borg.log` file (currently 110 MB, 96% hyper-pool noise) at `debug`, and only when the input survived to produce *some* log line. Telegram intake silent-drops do not produce any line at any level.
2. **"What did borg do in time window T?"** — answered today by the dashboard, which queries the wrong field and excludes image notes. The ledger has the correct answer; the dashboard does not derive from the ledger.

### Goals

- **Universal intake capture.** Every received input lands as a row in `borg-intake.md` synchronously, before any classification, dedup, allowed-chat check, media-kind dispatch, or pipeline scheduling. If the intake write fails, the intake fails (caller sees an error); we never accept silently.
- **Universal failure capture.** Every input that does not produce a successful (✅, ⏭️, 🔄) ledger row produces a row in `borg-dlq.md` carrying the trace_id, the stage at which it failed, the reason, and the retry state. Rejections, timeouts, fetch failures, quality-gate blocks, panics, and watchdog-killed pipelines all funnel here.
- **Mechanically checkable invariant.** For every trace_id in `borg-intake.md`, there exists at least one row in `borg-ledger.md` (success path) OR `borg-dlq.md` (failure path) with the same trace_id within a bounded time. `borg audit` walks the three tables and reports any violations.
- **Dashboard accurately reports activity.** A reingest is borg activity; the dashboard's daily/weekly buckets reflect when borg did the work, not when the content was originally learned. Image notes are not filtered out.
- **Production log level can return to `info`.** Once intake events are durable structured rows, the operator no longer needs DEBUG visibility to answer "did borg see X?"; the noise floor drops by ~96% of current volume.

### Non-Goals

- **Changing the ledger schema.** The most recent revision (`f81d865`, "collapse Title + Filename into one Note column") is the canonical layout; this doc adds new files and a new frontmatter field but does not touch ledger columns.
- **Replacing markdown-table stores with sqlite.** The `2026-04-20-sqlite-ledger-and-views.md` design covers that migration; the intake/dlq schemas in this doc are forward-compatible (same column shape projects cleanly from sqlite).
- **Cortex changes.** Cortex's classify/sweep/lint/intel actions read notes; they continue to do so. Cortex-managed frontmatter (`CORTEX_PRESERVE_KEYS`) is unchanged; the new `ingested:` field is explicitly *not* in that set.
- **Recovering pre-existing silent drops.** Whatever was dropped before this design ships stays dropped. The invariant is forward-looking.
- **Replaying inputs that pre-date intake.md.** The DLQ replay command only works for trace_ids that exist in the intake log.

## Proposed Solution

### Overview

Three new artifacts plus one fix to the existing dashboard:

```
                ┌─────────────────────────────────────────────────────────┐
                │                       INTAKE                            │
                │  telegram, http, ntfy, discord, cli (each entry path)   │
                └─────────────────────────────────────────────────────────┘
                                          │
                                  trace_id = trace::generate()
                                          │
                                          ▼
                ┌─────────────────────────────────────────────────────────┐
                │   append row to system/views/borg-intake.md            │
                │   write raw input to system/intake/{trace_id}.txt      │
                │   THIS WRITE IS SYNCHRONOUS AND ITS FAILURE FAILS       │
                │   THE INTAKE.                                           │
                └─────────────────────────────────────────────────────────┘
                                          │
                          ┌───────────────┴───────────────┐
                          │                               │
                          ▼                               ▼
              filtered/rejected/                  pipeline accepts;
              unsupported at intake               processes content
                          │                               │
                          ▼                               ▼
              append row to borg-dlq.md       success: append ✅/⏭️/🔄
              with stage=intake-reject        row to borg-ledger.md
                                              failure: append row to
                                                       borg-dlq.md
```

Invariant: every trace_id appearing in `borg-intake.md` must appear in `borg-ledger.md` or `borg-dlq.md` (or both — replace path has a 🔄 ledger row for the original *and* a ✅ row for the new ingest). The watchdog catches the case where neither row appears within a deadline.

### Architecture

#### Files and locations

| Path | Purpose | Lifetime |
|---|---|---|
| `system/views/borg-intake.md` | Markdown table, one row per received input. Newest first (insert-at-row-1, matching ledger). | Append-only; never compacted automatically. |
| `system/views/borg-dlq.md` | Markdown table, one row per failure. Newest first. | Append-only by code; `borg dlq archive` moves resolved rows to `system/views/borg-dlq-archive.md` when the table exceeds 1000 rows. |
| `system/intake/{trace_id}.txt` | Verbatim raw input as bytes. One file per trace_id, even if the input is just a URL. | Successful traces: deleted after 90 days. DLQ traces: persist until the DLQ row is archived. |
| Note frontmatter `ingested:` | Most-recent borg-activity date. Bumped on every ingest, including reingests. | Lives with the note. |

#### Trace_id flow

A trace_id is generated **exactly once** at the moment of intake (telegram receive, HTTP request, ntfy event, etc.). The same trace_id is:

1. Written into the `borg-intake.md` row.
2. Threaded into `pipeline::process_content(..., trace_id: Some(trace_id))`.
3. Recorded on the `LedgerEntry` produced (or `DlqEntry` on failure).
4. Stamped into the note's frontmatter as `trace:` (already done today).

The existing `vault::trace::generate(IngestMethod)` is unchanged.

#### Failure routing

| Failure point | Goes to | Stage value |
|---|---|---|
| Telegram disallowed chat | DLQ | `intake-reject` |
| Telegram unsupported media (sticker, animation, video, GIF, poll, location, contact) | DLQ | `intake-reject` |
| Telegram empty message (no text and no recognized media) | DLQ | `intake-reject` |
| HTTP malformed payload | DLQ | `intake-reject` |
| Pipeline classification error | DLQ | `classify-failed` |
| Pipeline fetch failed (fabric + jina both fail, blocklist hit, 4xx/5xx) | DLQ | `fetch-failed` |
| Quality gate rejected output | DLQ | `quality-blocked` |
| Pipeline hard-timeout (existing `PIPELINE_HARD_TIMEOUT_SECS`) | DLQ | `pipeline-timed-out` |
| Atomic publish failed (filesystem error) | DLQ | `publish-failed` |
| Watchdog: intake row older than N minutes with no ledger or DLQ row | DLQ (auto-injected) | `watchdog-orphan` |
| Replay re-attempt failure | DLQ (new trace, `replay_of:` references original) | as above |

#### Compatibility with the resilience design doc

This design composes cleanly on top of `2026-05-08-borg-pipeline-resilience.md`:

- The hard timeout (Phase 1, `8d061bf`) fires → produces a DLQ row with stage `pipeline-timed-out`, intake row stays.
- The inflight RAII guard (Phase 2, `618bbf3`) releases on Drop → on subsequent retries, a duplicate inflight produces a ledger ⏭️ row, no new DLQ entry needed.
- Atomic publish (Phase 3, `b950995`) → on a publish failure, the old note survives intact, a DLQ row is appended with stage `publish-failed`, intake row is preserved for replay.

### Data Model

#### IntakeEntry (`vault/src/intake.rs`)

```rust
pub struct IntakeEntry {
    pub date: String,           // YYYY-MM-DD of receipt
    pub time: String,           // HH:MM of receipt
    pub method: Method,         // reuse vault::schema::Method
    pub origin_ctx: String,     // chat_id (telegram), remote_addr (http), topic (ntfy), etc.
    pub kind: IntakeKind,
    pub preview: String,        // first 80 chars / "[image: filename.jpg]" / etc.
    pub trace_id: String,
}

pub enum IntakeKind {
    Url, Text, Photo, Voice, Audio, Document,
    Sticker, Video, Animation, Poll, Location, Contact,
    Empty, Unknown,
}
```

Markdown table layout:

```
| Date       | Time  | Method   | Origin       | Kind     | Preview                       | Trace      |
|------------|-------|----------|--------------|----------|-------------------------------|------------|
| 2026-05-11 | 19:07 | telegram | 8474692082   | url      | https://www.xda-developers... | tg-bd8893  |
| 2026-05-11 | 19:09 | http     | 192.168.0.42 | url      | https://youtube.com/watch?... | ht-4ddeef  |
| 2026-05-11 | 14:32 | telegram | 8474692082   | sticker  | [sticker: party-parrot]       | tg-9914a7  |
```

#### DlqEntry (`vault/src/dlq.rs`)

```rust
pub struct DlqEntry {
    pub date: String,
    pub time: String,
    pub method: Method,
    pub stage: DlqStage,
    pub reason: String,         // short failure description
    pub preview: String,
    pub retries: u32,
    pub status: DlqStatus,
    pub trace_id: String,
    pub replay_of: Option<String>,  // when this row is a replay attempt
}

pub enum DlqStage {
    IntakeReject, ClassifyFailed, FetchFailed,
    QualityBlocked, PipelineTimedOut, PublishFailed,
    WatchdogOrphan,
}

pub enum DlqStatus { Pending, Retried, Abandoned, Resolved }
```

Markdown table layout:

```
| Date       | Time  | Method   | Stage             | Reason                            | Preview                       | Retries | Status   | Trace      | Replay-Of  |
|------------|-------|----------|-------------------|-----------------------------------|-------------------------------|---------|----------|------------|------------|
| 2026-05-08 | 16:15 | http     | pipeline-timed-out | yt-dlp stalled                   | https://youtube.com/...       | 0       | pending  | ht-fb0810  | -          |
| 2026-05-11 | 14:32 | telegram | intake-reject     | unsupported media: sticker        | [sticker: party-parrot]       | 0       | abandoned| tg-9914a7  | -          |
```

#### Frontmatter changes

For every note where `origin: assisted`:

```yaml
date: 2026-04-16          # content date — preserved on reingest (UNCHANGED semantics)
ingested: 2026-05-08      # last borg activity — bumped on every ingest/reingest (NEW)
```

The renderer in `borg/src/markdown.rs` adds an `ingested:` line to the standard template. A new helper `apply_ingested_date(rendered, now_date)` in `pipeline/atomic.rs` parallels the existing `apply_original_date` and is invoked unconditionally on every publish (original + reingest). The field is *not* included in `CORTEX_PRESERVE_KEYS` — it is the one field that must be overwritten on each reingest.

### API Design

#### CLI additions

```
borg intake list [--method <m>] [--since <date>] [--limit <n>]
borg intake show <trace_id>                    # prints intake row + raw-input file contents
borg dlq list [--method <m>] [--stage <s>] [--status <s>] [--limit <n>]
borg dlq show <trace_id>                       # prints DLQ row + intake row + raw-input
borg dlq replay <trace_id>                     # re-inject input via original method; new trace_id with replay_of: <trace_id>
borg dlq archive <trace_id> [--reason <text>]  # mark resolved + move to archive file
borg audit [--bound-secs <n>]                  # walks intake/ledger/dlq; reports orphans
borg backfill-ingested [--dry-run]             # one-shot: set ingested:<date:> on every assisted note missing the field
```

#### HTTP additions (read-only health endpoint)

```
GET /health/audit         { orphan_count: N, oldest_orphan_secs: T, ... }
```

#### Dashboard query changes (`system/views/borg-dashboard.md`)

For all five existing panels, replace:

```dataview
WHERE source != null AND date = date(today) - dur(1 day)
```

with:

```dataview
WHERE origin = "assisted" AND ingested = date(today) - dur(1 day)
```

The `source != null` → `origin = "assisted"` swap captures image-only notes; the `date` → `ingested` swap counts reingests correctly.

Add two new panels:

```dataview
## Recently failed (last 7 days)
TABLE WITHOUT ID
  trace as "Trace",
  method as "Method",
  stage as "Stage",
  reason as "Reason",
  retries as "Retries"
FROM "system/views/borg-dlq"
WHERE status = "pending"
SORT date DESC
LIMIT 50
```

```dataview
## Intake without resolution (open ingests)
TABLE WITHOUT ID
  trace as "Trace",
  method as "Method",
  kind as "Kind",
  preview as "Preview"
FROM "system/views/borg-intake"
WHERE !exists-in("system/views/borg-ledger", trace)
  AND !exists-in("system/views/borg-dlq", trace)
SORT date DESC
LIMIT 20
```

(The `exists-in` join expressed in dataview-ish pseudocode; the actual implementation may rely on `borg audit` populating a derived `system/views/borg-orphans.md` since dataview does not natively cross-join tables. See Open Question.)

### Implementation Plan

#### Phase 1: vault::intake and vault::dlq modules
**Model:** opus

- New `vault/src/intake.rs` mirroring `vault::ledger`: `IntakeEntry`, `IntakeKind`, `append_intake_entry`, `find_by_trace`, `intake_path`. File creation with frontmatter template (matching the ledger's pattern) if absent.
- New `vault/src/dlq.rs`: `DlqEntry`, `DlqStage`, `DlqStatus`, `append_dlq_entry`, `find_by_trace`, `update_status`, `dlq_path`.
- New `system/intake/` directory for raw-input sidecar files.
- Atomic-append semantics: insert row at line 1 (after header), matching ledger. Hold a file lock during the read-modify-write to guarantee concurrent intakes don't collide.
- Add `IntakeKind` and `DlqStage` to `vault::schema` if other crates need them; otherwise keep them inside the new modules.
- Unit tests for append, find, parse, header drift, concurrent-append.

#### Phase 2: Wire intake into every intake path
**Model:** opus

- `telegram.rs`: at the very top of the `Update::filter_message` endpoint, BEFORE the allowed-chats check, generate the trace_id and append the intake row. Classify the message kind (`url | text | photo | voice | audio | document | sticker | video | animation | poll | location | contact | empty | unknown`) and record it. Pass `trace_id` forward into every downstream path.
- Append a DLQ row with `stage=intake-reject` for: disallowed chat, unsupported media, empty message. INFO-level log line accompanies the DLQ row.
- `routes.rs`: at the top of `ingest`, `note`, and `ingest_multipart`, append intake row before any payload validation. Bad payloads append a DLQ row with `stage=intake-reject` reason `bad-payload: <detail>`.
- `ntfy.rs`: same pattern in the event-loop branch.
- `discord.rs`: same pattern in the message handler.
- `cli.rs`: same pattern for the CLI ingest path.
- Raw input is written to `system/intake/{trace_id}.txt` synchronously alongside the intake-row append.
- All silent-drop sites now produce: (a) INFO log line, (b) intake row, (c) DLQ row. No path can exit without writing both.

#### Phase 3: `ingested:` frontmatter field
**Model:** opus

- New `apply_ingested_date(rendered: &str, date: &str) -> String` in `borg/src/pipeline/atomic.rs`, parallel to `apply_original_date`. Inserts an `ingested:` line if missing, replaces it if present.
- `pipeline::process_url_inner`: after composing `final_str` with `apply_original_date` and `apply_cortex_fields`, call `apply_ingested_date(&final_str, &now_date(config))` unconditionally (original ingest *and* reingest).
- `borg/src/markdown.rs`: add `ingested:` to the rendered frontmatter template so first-ingest notes already have the field.
- Unit tests in `pipeline/atomic.rs`: `apply_ingested_date` inserts on a note without the field, replaces on a note with it, preserves all other frontmatter.
- Integration test: reingest preserves `date:` AND bumps `ingested:` to today.

#### Phase 4: Dashboard rework
**Model:** sonnet

- Edit `system/views/borg-dashboard.md`. For each of the five existing dataview blocks, swap `source != null` → `origin = "assisted"` and `date` → `ingested`.
- Add two new panels: "Recently failed" (from DLQ) and "Intake without resolution" (orphans). The orphan panel reads from `system/views/borg-orphans.md` produced by `borg audit` (since dataview cannot natively cross-join two tables).

#### Phase 5: borg audit + DLQ CLI
**Model:** sonnet

- `borg audit`: read intake.md, ledger.md, dlq.md into in-memory maps keyed by trace_id. Compute set differences. Emit:
  - intake rows with no ledger/dlq match older than `--bound-secs` (default 1800) → category "orphan"
  - ledger rows with no intake match → category "asymmetric-ledger" (e.g., backfilled old rows; expected during transition)
  - dlq rows with no intake match → category "asymmetric-dlq"
- Write `system/views/borg-orphans.md` table for dataview consumption.
- `borg dlq list`, `show`, `replay`, `archive` as per API Design.
- Replay generates a new trace_id with `replay_of: <original>` recorded in the new DLQ row's `replay_of` column.

#### Phase 6: Watchdog
**Model:** opus

- New tokio task spawned at daemon startup. Every 60 seconds (configurable), scan intake rows from the last hour; for any trace_id without a matching ledger or DLQ row AND with an intake timestamp older than `PIPELINE_HARD_TIMEOUT_SECS + 60s`, append a DLQ row with `stage=watchdog-orphan` reason `no ledger or dlq row produced within timeout window`.
- This catches: OOM-killed processes that never get to write the timeout-Failed ledger row; panics outside the timeout-wrapped scope; bugs where a future return path skips the DLQ append.

#### Phase 7: Backfill + docs + cleanup
**Model:** sonnet

- `borg backfill-ingested`: walk every note where `origin == assisted` and `ingested` is absent. Use `write_atomic` to set `ingested: <date>`. Skip notes with mtime < 60s old (concurrent-write protection). Idempotent.
- Update `CLAUDE.md` to mention `system/views/borg-intake.md` and `borg-dlq.md`.
- Remove the stale `~/.local/share/obsidian-borg/` directory.
- Add a daemon-startup check: if intake.md or dlq.md is missing, create with the standard frontmatter template.
- Add a systemd user timer for `borg audit` (hourly); on non-empty output, write to a known location the user/operator can monitor.

## Alternatives Considered

### Alternative 1: Add `last-ingested:` only; skip intake.md and DLQ entirely
- **Description:** Just add `last-ingested:` to frontmatter and rework the dashboard. Don't introduce intake.md / dlq.md.
- **Pros:** Far smaller diff. No new files.
- **Cons:** Silent intake drops (sticker, disallowed-chat) still produce zero record. Failed pipelines still produce zero record. The "every input is captured" invariant is unmet. The DLQ requirement comes back the moment something hangs.
- **Why not chosen:** The user's stated invariant explicitly requires durable capture of every input on both success and failure paths. This option satisfies the dashboard half of the requirement but leaves the bigger half unaddressed.

### Alternative 2: sqlite-backed source of truth, markdown rendered from sqlite
- **Description:** Migrate ledger, intake, and DLQ to sqlite. Generate markdown views periodically for human/dataview consumption.
- **Pros:** Real query power. No markdown-table parsing fragility. Joins (intake ↔ ledger ↔ dlq) become trivial in SQL instead of cross-table dataview hacks.
- **Cons:** Bigger refactor. Depends on the `2026-04-20-sqlite-ledger-and-views.md` plan landing first. Dataview reads notes/markdown, not sqlite — view-regeneration cadence becomes a new failure surface.
- **Why not chosen:** Out of scope for the immediate observability fix. This design is forward-compatible with that migration: the proposed intake-log and DLQ are append-only markdown tables that can later be projected from sqlite without changing their schemas or callers.

### Alternative 3: ingest-history list in note frontmatter
- **Description:** Replace `ingested:` with `ingest-history:` — a growing YAML list of all ingest events.
- **Pros:** Full history in the note. Replay/audit becomes a per-note operation.
- **Cons:** Frontmatter bloat (a frequently reingested note grows unbounded). Dataview filter complexity. Duplicates the ledger which already has the chronology.
- **Why not chosen:** Cost too high relative to caching only the most-recent activity in a scalar field. The ledger remains the authoritative chronology.

### Alternative 4: Use file mtime as the activity timestamp; no new frontmatter field
- **Description:** Don't add `ingested:`. Have the dashboard query `file.mtime` instead.
- **Pros:** Zero schema change.
- **Cons:** Cortex's classify/lint/sweep actions bump mtime too; any cortex touch indistinguishably becomes "borg activity." A manual edit in Obsidian bumps mtime. The intent of "borg ingestion activity" is lost. Worse: `obsidian-sync` and `syncthing` can rewrite mtime on file propagation.
- **Why not chosen:** mtime is too noisy a signal for the question the dashboard is trying to answer.

## Technical Considerations

### Dependencies

No new external dependencies. `tempfile` already moved to deps in `b950995`. Markdown-table append logic exists in `vault::ledger` and is generalized into a shared helper (or duplicated cleanly across `vault::intake`, `vault::dlq`, `vault::ledger`).

### Performance

- Intake row append: single file lock + read header + insert at row 1 + write + fsync. Steady-state cost ~1ms. Synchronous on the hot path; failure fails the intake.
- Raw-input sidecar write: one small file create per intake. <1ms.
- Audit walk: linear scan of three markdown tables. At ~50 intake rows/day, the intake table is ~18k rows/year. Scan completes in <100ms with simple line-based parsing.
- Dashboard queries: dataview already handles 1000+ note vaults; adding two new tables and switching one field has negligible impact.
- Watchdog: hourly default; per-iteration cost = audit-walk cost. Configurable cadence.

### Security

- `system/views/` and `system/intake/` sit inside the vault, governed by the same `ReadWritePaths` granted to the systemd unit. No new exposure.
- Raw-input sidecar files contain the same user payload that already exists in the note's body. No new disclosure surface.
- DLQ rows include URLs and message previews; same data as existing log files. The DLQ may surface that an unsupported sticker was sent — operator should be aware that sticker IDs / animation IDs appear in plaintext in `system/views/borg-dlq.md`.

### Testing Strategy

- **Unit tests** for `vault::intake` and `vault::dlq` (mirror `vault::ledger`): append, find, parse, header drift, concurrent-append correctness.
- **Integration tests** in `borg`:
  - Telegram sticker → intake row with kind=sticker, DLQ row with stage=intake-reject reason="unsupported media: sticker", no ledger row.
  - Telegram URL happy path → intake row, ledger row ✅, no DLQ row, trace_id matches across both.
  - Reingest of existing URL → original ledger row marked 🔄, new ledger row ✅, note's `date:` preserved, note's `ingested:` bumped to today.
  - Simulated hang post-fetch → intake row, no ledger row, watchdog produces a DLQ row with stage=watchdog-orphan after the timeout window.
  - Replay of a DLQ entry → new intake row, new trace_id with `replay_of:` referencing original, new ledger row ✅ if the underlying URL now succeeds.
- **Audit regression test:** synthetic intake/ledger/dlq tables with known orphans → `borg audit` reports exactly those orphans.
- **Backfill test:** seed a temp vault with 100 notes (mix of origin=assisted and origin=authored); run backfill; assert ingested:<date:> set only on assisted notes, authored untouched, mtime not bumped beyond what `write_atomic` does.

### Rollout Plan

1. **Phases 1-3 + atomic.rs `apply_ingested_date` helper** ship together as a single coherent feature. Tests green. Reinstall borg + restart daemon.
2. **Phase 7 backfill** runs once against the existing vault (~1100 notes). Outputs a summary of notes touched. The vault is in git, so backfill is reversible.
3. **Phase 4 dashboard update** ships after backfill confirms `ingested:` is populated.
4. **Phase 5 (audit + DLQ CLI)** ships next. `borg audit` is now meaningful.
5. **Phase 6 (watchdog)** ships last so the audit baseline is clean before background DLQ injection starts.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Intake-row write fails (disk full, permission) → input dropped silently | Low | High | Intake append is synchronous; failure propagates as an error. HTTP returns 5xx. Telegram cannot refuse-by-not-acking under the long-poll model, so the handler logs ERROR and surfaces a Telegram reply ("borg failed to record your input"). Better to fail loudly than silently. |
| Backfill corrupts a note | Low | High | Backfill uses `write_atomic` (the Phase 3 helper). Dry-run mode emits the planned changes without writing. The vault is git-tracked. |
| Dashboard `ingested:` query returns empty during the window between backfill and panel changes | Med | Low | Backfill completes in seconds (~1100 notes). Run backfill *before* updating dashboard queries. |
| DLQ markdown file grows unbounded under a chronic failure | Med | Low | Once DLQ exceeds 1000 rows, `borg dlq archive --resolved` moves `status=resolved`/`status=abandoned` rows to `borg-dlq-archive.md`. |
| Concurrent intake writes from two intake paths collide on the file | Med | Med | File lock during read-modify-write. The ledger already runs this pattern under load; reuse the same helper. |
| Trace_id collision (random suffix collides between two intakes) | Negligible | Med | `trace::generate` uses 24-bit randomness per method-prefix; collision rate at peak ingest is well under 1-in-millions. If it ever happens, both rows simply share a trace_id — the audit walk will flag it. |
| Watchdog produces false positives (slow happy-path pipeline misclassified as orphan) | Med | Low | Watchdog window = `PIPELINE_HARD_TIMEOUT_SECS + 60s` (1860s default). Any pipeline that has not produced a ledger or DLQ row by then is genuinely lost. Hard-timeout already bounds happy-path duration. |
| Replay storms (someone scripts `borg dlq replay-all`) overwhelm the system | Low | Med | Replay generates a normal intake row + trace_id; existing inflight RAII guard deduplicates URLs in-flight. CLI subcommand intentionally has no `--all` flag in v1 (Open Question). |
| `borg audit` becomes too noisy because legacy ledger rows have no matching intake row | Med | Low | Audit accepts an `--ignore-before <date>` flag; default is the date intake.md was first written. Pre-intake-era rows are expected asymmetries. |

## Open Questions

- [ ] Field name: `ingested:` vs `last-ingested:` vs `ingested-at:`. Recommend `ingested:` (shortest, symmetric with `date:`). Confirm before Phase 3.
- [ ] DLQ row schema includes a `replay_of:` column. Should we also surface a `replays:` *list* on the original DLQ row pointing forward to its replay attempts? Saves a join during inspection but mutates a "row" that's otherwise append-only.
- [ ] Dashboard cross-table join: dataview cannot natively join two markdown tables. Option (a): write `system/views/borg-orphans.md` as a derived table from `borg audit`. Option (b): teach dataviewjs to read both tables. (a) is simpler; (b) is more responsive.
- [ ] DLQ retention: indefinite, time-bounded (e.g., archive after 90 days), or capped row count? Lean toward capped row count (1000) since DLQ volume is unpredictable.
- [ ] Should `borg dlq replay <trace>` re-use the original trace_id or generate a new one? **Tentative: new trace, with `replay_of: <original>` field.** This preserves the audit chain (every replay attempt is recorded) and matches how intake events are otherwise immutable.
- [ ] Watchdog cadence: 60s default; user-tunable via config?
- [ ] Backfill behavior: set `ingested: <date>` (the original) or skip and let future reingests populate it? **Tentative: set to `<date>` — for a never-reingested note, most-recent activity == original activity.**
- [ ] Should we expose `GET /health/audit` as a public HTTP endpoint, or restrict it to a unix socket? Live state of the audit invariant is operationally useful; same authentication story as the existing HTTP intake endpoints (i.e., none — bound to localhost via systemd).

## References

- `docs/design/2026-05-08-borg-pipeline-resilience.md` — Phases 1-3 (hard timeout, RAII inflight guard, atomic publish-or-revert).
- `docs/design/2026-04-19-staged-ingestion-pipeline.md` — staged replay/blocklist pipeline; informs intake/DLQ replay semantics.
- `docs/design/2026-04-20-sqlite-ledger-and-views.md` — future sqlite migration path; this design is forward-compatible.
- `docs/design/2026-05-03-active-vault-v1.md` — agent runtime + provenance; independent surface, no conflict.
- Commit `b950995` — atomic publish (Phase 3 of resilience).
- Commit `fe6c451` — `CORTEX_PRESERVE_KEYS` hoisted to `vault::schema`; this design extends that pattern by *excluding* `ingested:` from preservation.
- Empirical evidence from the 2026-05-11 session: silent telegram-handler drops, three hung pipelines on 2026-05-08 / 2026-05-02, dashboard "This Week" off-by-one from preserved-date reingest, two image notes excluded by `source != null` filter.
