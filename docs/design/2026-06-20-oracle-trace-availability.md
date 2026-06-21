# Design Document: Oracle advertises trace (staged-source) availability

**Author:** Scott Idler
**Date:** 2026-06-20
**Status:** Implemented
**Review Passes Completed:** 5/5 + 2 external review rounds folded in
**Reviewed by:** Architect (agy / Gemini 3.1 Pro) and Staff Engineer (Codex), 2026-06-20 — both verified claims read-only against the codebase; round-1 findings and round-2 verdicts incorporated below.

## Summary

When the oracle MCP returns a note, it should also tell the caller that the note's
raw staged source (e.g. a YouTube transcript) still **exists** and is **cheaply
referenceable**, without pulling or searching that source. The note body oracle
serves is the lossy *assisted summary*; the verbatim source already lives on disk
for `staging.retention-days` (default 60) as a borg trace. Oracle currently drops
the two frontmatter fields (`trace`, `ingested`) that would let a caller find it,
so agents re-fetch from the network or trust the summary even when the original is
sitting in `~/.local/share/sb/borg/stages/<trace>/`.

## Problem Statement

### Background

- borg durably stages every ingest under `~/.local/share/sb/borg/stages/<trace>/`.
  For a YouTube note the staged `distilled.yml` contains the full `transcript:`
  block. These artifacts are swept after `staging.retention-days` (default 60;
  `borg/src/config.rs` `StagingConfig::retention_days`).
- Every published note already carries the join keys in frontmatter:
  - `trace:` — e.g. `ht-95aa4e`, the staged-trace handle (`borg/src/markdown.rs:138-140`).
  - `ingested:` — the retention clock start (distinct from `date:`, which preserves
    the original content date across reingest). **Dual-format reality (verified):**
    the fresh-publish path writes a bare `%Y-%m-%d` date (`borg/src/markdown.rs:120-122`);
    the main URL pipeline overwrites it with a full offset datetime
    (`borg/src/pipeline.rs:735-744`), and `backfill-ingested` writes the offset form
    (`borg/src/backfill.rs:115`). So both `2026-06-20` and `2026-06-20T20:40:27-07:00`
    exist in the vault today. Any expiry computation must parse **both** forms.
- oracle serves the note **body**, which is the assisted summary. The summarizer
  legitimately compresses details out (the motivating case: an on-screen prompt a
  user wanted verbatim was dropped from the summary but present in the transcript).

### Problem

oracle gives callers no signal that a richer, verbatim source exists or how to
reach it. The data flow that drops it:

- `vault/src/frontmatter.rs` (`Frontmatter`, ~L20) — `trace`/`ingested` are parsed
  but land in the `extra` catch-all, not as real fields.
- `vault/src/search/index.rs` (`index_one`, ~L77) — only the known fields are
  written to the SQLite `notes` table; `extra` is discarded.
- `vault/src/search.rs` (`NoteRow`, ~L372) — no columns for them.
- `oracle/src/server.rs` (`format_note`, ~L229) — can only emit what `NoteRow`
  holds, so no trace info reaches the client at any `DetailLevel`.

### Goals

- Oracle responses signal, at the `metadata` detail level (and richer), that a
  staged source **exists**, give its **reference handle**, and say whether it is
  **still inside the retention window**.
- Awareness only: no transcript text, no new search path, no transcript content in
  the index.
- Keep oracle decoupled from borg's config and filesystem (workspace invariant:
  each subsystem reads its own config; one-way data flow).
- Generalize beyond transcripts: any note with a live trace has a referenceable
  staged source (articles, repos, threads), transcripts being the richest case.

### Non-Goals

- Returning, searching, or embedding transcript/staged-source content.
- Verifying on-disk presence of the trace (size-based early sweep can delete it
  before the policy window ends; we advertise the *policy* window, not a guarantee).
- Exposing traces on the client-only hosts that own no stage directory.
- Any change to the borg staging/retention mechanism itself.

## Proposed Solution

### Overview

Carry the existing frontmatter join keys through the index into oracle's response,
and have **borg stamp an absolute expiry** at publish time so oracle never needs to
know the retention number. Three moving parts:

1. **borg (authoritative owner of retention):** at publish, alongside the existing
   `trace`/`ingested`, stamp `trace-expires: <ingested-date + retention-days>` into
   frontmatter. borg already owns both the publish path and `retention-days`, so the
   number is computed once, at its single source of truth, and frozen as an absolute
   date (honest to when the note was actually ingested, immune to later config
   changes).
2. **vault (schema):** promote `trace`, `ingested`, `trace-expires` out of `extra`
   into real `Frontmatter` fields; add three `notes` columns (idempotent migration);
   write them in `index_one`.
3. **oracle (pure echo + date math):** `format_note` emits a `trace` block. Existence
   and reference come straight from the columns; `within-window` is a calendar-date
   comparison against today — no borg config read, no filesystem touch.

This keeps oracle a read-only echo: it never reads `borg.yml` and never stats the
stage dir, satisfying the decoupling goal.

### Architecture

```
borg publish ──► note frontmatter: trace, ingested, trace-expires
                          │
                          ▼
cortex/borg index ──► vault::Frontmatter (named fields)
                          │  index_one
                          ▼
                   SQLite notes: trace, ingested, trace_expires
                          │
                          ▼
oracle format_note ──► JSON "trace": { available, ref, ingested, expires, within-window }
```

### Data Model

Frontmatter (`vault/src/frontmatter.rs`), promoted from `extra` to named optional
fields:

```rust
pub trace: Option<String>,          // "ht-95aa4e"
pub ingested: Option<String>,       // ISO-8601 instant
pub trace_expires: Option<String>,  // "2026-08-19" (date), stamped by borg
```

Note: `parse_frontmatter` is a **hand-rolled** parser (`vault/src/frontmatter.rs:284-299`;
dispatch at `:80-130`) that routes unknown keys into `extra` (`:217-223`), not a
serde-derive — there is no `#[serde(flatten)]` and no `deny_unknown_fields`. There
are therefore **two** edits, and the second is the one that bites:

1. Add match arms in `parse_frontmatter` so the three keys populate named fields.
2. **Add them to the named-field emission block in `Frontmatter::to_yaml()`
   (`vault/src/frontmatter.rs:152`).** This is a data-loss trap (both reviewers
   flagged it): once a key is a named field it no longer rides the `extra` loop
   (`:217-222`), so any path that *rewrites* a note via `to_yaml()` — notably
   `cortex summarize --backfill` — will silently **strip** `trace`/`ingested`/
   `trace-expires` from every note it touches unless they are emitted explicitly.
   Phase 1 must include a round-trip test through a **rewrite** path, not just
   parse→serialize.

`notes` table (`vault/src/search/schema.rs`) — three additive TEXT columns, added
to the `CREATE TABLE` (`:8`) and to a new idempotent `ensure_trace_columns()`
migration:

```sql
trace         TEXT DEFAULT '',
ingested      TEXT DEFAULT '',
trace_expires TEXT DEFAULT '',
```

**Correction (verified):** the sibling `ensure_distilled_columns` (`:162-199`) is
**not** transactional and does **no** version bump — it runs a `PRAGMA table_info`
check then individual `ALTER TABLE` calls. Following it verbatim is the
low-friction, consistent choice; per the Rust SQLite rules a single migration
transaction is preferable. Resolve this explicitly (see Open Questions); either
way snapshot the DB before first run.

`NoteRow` (`vault/src/search.rs:372`) gains the three matching `String` fields —
**but `NoteRow::from_row` is positional** (`:388-402`, 12 columns today) and the
SELECT projection is **repeated** across `vault/src/search/query.rs:18, :85, :133`
plus the stats variants. Every projection and the positional decode must be updated
together, or rows either lose trace metadata or fail to decode. This fan-out is the
real surface area of Phase 1.

### API Design

`format_note` (`oracle/src/server.rs`) adds a `trace` object to every detail level
(it is metadata, so it rides at `metadata` and above):

```jsonc
{
  "path": "notes/stop-prompting-claude-start-loop-engineering.md",
  "title": "...",
  // ...existing fields...
  "trace": {
    "available": true,            // a non-empty trace handle exists
    "ref": "ht-95aa4e",           // frontmatter.trace
    "ingested": "2026-06-20T20:40:27-07:00",
    "expires": "2026-08-19",      // frontmatter.trace-expires (policy window)
    "within-window": true          // today <= expires (UTC calendar date)
  }
}
```

When no trace is recorded (manual / pre-borg notes): `{ "available": false }`
and the other keys are omitted. When a trace exists but `trace-expires` is absent
(legacy note not yet backfilled): `available: true`, `ref`/`ingested` present,
`expires` omitted, `within-window: null` (unknown). When `trace-expires` is present
but unparseable: omit `expires`, set `within-window: null`, and `warn!` once (never
fail the response).

**What `within-window: true` means (the invariant the staff review pressed on).**
It means exactly: *borg's retention policy says this trace should still exist, based
on the note's recorded `ingested` time.* It is **not** a claim that the current
sweeper would not delete it (the sweeper keys on stage-dir **mtime**, not `ingested`
— `borg/src/retention.rs:29-67`), nor that a caller can actually dereference it on
this host (stage dirs are per-host; size-based sweep can delete early). Oracle can
only honestly guarantee the policy-says-so reading without touching storage, and
that is the one we commit to. Callers treat `true` as "worth trying," `false`/`null`
as "fall back to the summary or re-fetch."

### Implementation Plan

#### Phase 1: vault schema + frontmatter
**Model:** opus
- Promote `trace`, `ingested`, `trace-expires` to named `Frontmatter` fields via
  match arms in `parse_frontmatter` **and** explicit emission in `to_yaml()` (the
  data-loss trap above).
- Add the three columns to the `notes` `CREATE TABLE` (`:8`) and a new idempotent
  `ensure_trace_columns()` migration that **matches the established
  `ensure_distilled_columns` pattern**: PRAGMA `table_info` probe then individual
  `ALTER TABLE ADD COLUMN`, **no wrapping transaction and no `set_version`**. A single
  idempotent `ALTER ADD COLUMN` cannot half-apply, so the Rust "multi-statement DDL in
  one transaction" rule does not bite here, and there is no `set_version`/`user_version`
  infra in `vault/src/search/` to hang a version on (only `borg/src/receipts.rs:40`
  has one). Snapshot the DB before first run.
- Update **every** `NoteRow` SELECT projection (`query.rs:18, :85, :133`, stats
  variants) and the positional `from_row` (`search.rs:388-402`) together.
- Bind the three new params in `index_one`: it currently binds 30 params across its
  `UPDATE` and `INSERT`; this becomes 33 — update the bind arrays carefully (the
  off-by-one is the classic SQLite footgun).
- Forced repopulation: `index_vault` is **mtime-gated** and skips unchanged notes
  (`vault/src/search/index.rs:31-39`). Adding columns with defaults does **not**
  populate existing rows, so they would read `available: false` until each note's
  mtime changes. Add a **manual `--reindex`/force flag** (`sb oracle index` has none
  today — `sb/src/cli/oracle.rs:20`; the MCP `ReindexRequest` is empty —
  `oracle/src/tools.rs:427`). Do **not** build a self-healing schema-version gate:
  there is no version infra to hang it off, and the failure mode is benign — additive
  columns default to `''` and corrupt nothing, so a normal mtime-driven reindex
  backfills them over time regardless. (Open micro-question: whether even the flag is
  needed, or letting normal reindex backfill suffices.)
- Tests: frontmatter round-trip through a **rewrite** path (catches the `to_yaml`
  drop); all-three / trace-only / none; index read-back; migration idempotency on a
  pre-existing DB; forced-reindex populates pre-existing rows.

#### Phase 2: oracle response surface
**Model:** sonnet
- `format_note` emits the `trace` block at every `DetailLevel`.
- `within-window` = `today_utc <= parse(expires)`; `null` when `expires` is empty.
- Tests: JSON shape at metadata/tldr/summary/full; boundary cases
  (`today == expires`, expired, missing-expires, missing-trace).

#### Phase 3: borg stamps trace-expires at publish
**Model:** sonnet
- **Do not pass `StagingConfig` into `render_note`.** The renderer
  (`borg/src/markdown.rs`) deliberately receives only `FrontmatterConfig` and has no
  access to `retention_days`. Instead compute `trace-expires` upstream in the
  pipeline handler (where the full `Config` is in scope) and inject it via the
  existing `NoteContent::frontmatter_additions` (`markdown.rs:33`, merged at `:183`,
  already populated by the pipeline at `pipeline/text.rs`). This keeps the module
  boundary intact and is the design's committed answer to the staff/architect
  "hardest question."
- Compute `trace-expires = date(parse(ingested)) + retention_days`, parsing the
  **dual** `ingested` format (bare `%Y-%m-%d` and offset datetime), formatted back
  to `%Y-%m-%d`.
- Tests: published note carries a correct absolute `trace-expires` from each
  `ingested` format; reingest re-stamps from the new `ingested`; the stamp survives
  a `to_yaml()` rewrite.

#### Phase 4: backfill legacy notes
**Model:** sonnet
- Extend `sb borg backfill-ingested` (`borg/src/backfill.rs`) to also stamp
  `trace-expires`. **Keep the verb name** — it is wired through clap + dispatch + log
  output (`sb/src/cli/borg.rs:100, :472`) and has live docs/tests references (no
  cron/systemd caller); broaden its help/output text or add an alias rather than
  renaming. It already scans notes, reads `trace`, pulls precise timestamps from
  `receipts.db`, and has a dry-run. But two verified hazards:
  - **Skip logic must change.** The current loop skips notes whose `ingested:` is
    already a datetime or already matches (`backfill.rs:185, :207-208`) — those are
    exactly the notes that have `ingested` but are missing only `trace-expires`. A
    naive extension would skip them. The skip predicate must become "skip only when
    `trace-expires` is already present and correct," independent of the `ingested`
    check.
  - **No general inserter exists.** Only `apply_ingested_date` is a reusable atomic
    field-writer; `apply_cortex_fields` is filtered by `CORTEX_PRESERVE_KEYS`
    (`vault/src/schema.rs:8-16`) and will not insert an arbitrary key. Add a sibling
    `apply_trace_expires` (or generalize `apply_ingested_date`) rather than reaching
    for `apply_cortex_fields`.
- **receipts.db unavailable:** the backfill promotes `ingested` to local-midnight
  (flagged `precise=false`) rather than leaving it null (`backfill.rs:~202, :355-368`).
  So **compute `trace-expires` from whatever `ingested` was written** — midnight
  fallback included — inheriting the same `precise=false` best-effort semantics. Do
  **not** skip: skipping would leave `trace-expires` absent → `within-window: null`,
  which the design reserves for "no trace data," conflating *imprecise time* with
  *no data*. The window is already policy-not-guarantee, so a ±1-day midnight skew is
  within tolerance.
- Notes with no `trace` are left untouched (they surface `available: false`).
- Tests: backfill stamps notes that already have a datetime `ingested`; idempotency;
  notes missing `trace` skipped; dry-run writes nothing; receipts-unavailable path.

#### Phase 5: advertise the trace block to the consuming LLM
**Model:** opus
- Phases 1-4 put a `trace` block in every note-returning response, but nothing told
  the consuming LLM the capability exists or what the handle is for, so a model got
  the data with no cue to use it. Add a concise capability sentence to (a) the server
  instructions surfaced via `ServerHandler::get_info` and (b) the `description` of
  every note-returning tool: `knowledge_search`, `note_read`, `list_notes`,
  `domain_brief`, `tag_search`, `find_similar`, `recent_activity`, `find_links`,
  `creator_browse`, `source_browse`, `inbox_status`, `quality_report`. The seven
  non-note tools (`vault_overview`, `ingest_history`, `failure_history`,
  `schema_info`, `reindex`, `duplicate_groups`, `classify_status`) are left unmarked,
  since the marker would be misleading where no note is returned.
- Handle-only, broad framing: advertise that the `trace` block carries a handle to
  the verbatim staged source (transcripts called out as the richest case) and that
  the caller should prefer that source over the lossy summary when exact wording
  matters. No filesystem path in the text (keeps oracle decoupled from borg's storage
  layout); state explicitly that oracle advertises the handle only and never returns,
  searches, fetches, or verifies staged-source content.
- No new `fetch_trace`/`read_transcript` tool and no `outputSchema` change — this is
  description text only, riding the block Phase 2 already emits.
- Tests: a `list_tools()` regression guard asserting the marker (`` `trace` block ``)
  is present on every note-returning tool and absent on every non-note tool, plus a
  coverage check forcing every advertised tool into exactly one bucket so a
  newly-added tool cannot silently dodge the guard.
- The MCP `reindex` tool stays mtime-gated (CLI `index --force` is the back-catalogue
  populate path); documented here, not changed.

## Alternatives Considered

### Alternative 1: Oracle reads `borg.yml` retention-days and computes expiry at read time
- **Description:** oracle keeps only `trace`/`ingested`, reads `staging.retention-days`
  from `borg.yml`, computes `expires`/`within-window` per request.
- **Pros:** no new frontmatter key; no backfill.
- **Cons:** oracle reads another subsystem's config — violates the "each subsystem
  reads its own config" invariant and couples oracle to borg's config shape. A
  config change retroactively reinterprets old notes' windows.
- **Why not chosen:** the coupling and retroactive-reinterpretation cost outweigh
  saving one frontmatter key; stamping at the owner is cleaner and honest-to-history.

### Alternative 2: Oracle stats the stage dir to verify the trace exists
- **Description:** oracle `stat()`s `~/.local/share/sb/borg/stages/<trace>/` per
  result and reports verified presence.
- **Pros:** most accurate; reflects early/size-based sweeps.
- **Cons:** couples oracle to borg's on-disk layout; adds filesystem I/O per result;
  the stage dir is per-host, so a client-only host would report `false` for traces
  that genuinely exist on the daemon host. Breaks one-way data flow.
- **Why not chosen:** decoupling and host-portability matter more than verifying a
  rare early sweep; callers fall back gracefully on a fetch miss.

### Alternative 3: Echo `trace`/`ingested` only; caller does the window math
- **Description:** oracle surfaces the two raw fields; each consumer decides the window.
- **Pros:** smallest oracle change.
- **Cons:** pushes the retention number into every consumer; "is it referenceable
  inside the window" — the explicit ask — is then answered N times, inconsistently.
- **Why not chosen:** fails the goal of oracle *itself* knowing the source is
  cheaply referenceable.

## Technical Considerations

### Dependencies
- No new crates. Date math uses the existing `chrono` dependency, the same one
  `vault::search::normalize_date` uses to canonicalize `date:` to `YYYY-MM-DD` and
  `vault::ledger` uses for `Utc::now().format("%Y-%m-%d")`. `expires` =
  `ingested_date + retention_days`; `within-window` = `Utc::today() <= expires`,
  matching the ledger's existing UTC-date convention.

### Performance
- Three additive TEXT columns; no new queries, no new joins. `format_note` adds a
  small constant-size object. Negligible.

### Security
- No transcript/staged content is exposed — only a handle (`trace` id), the ingest
  timestamp, and a policy expiry date, all low-sensitivity. Oracle never opens the
  trace, so there is no path-traversal surface.

### Testing Strategy
- Unit: frontmatter parse; index round-trip; migration idempotency; `format_note`
  JSON shape and `within-window` boundaries; borg stamp correctness; backfill
  idempotency.
- Fixtures: a mini-vault note carrying `trace`/`ingested`/`trace-expires` and one
  carrying none, asserting both response shapes.

### Rollout Plan
- Additive only (columns, frontmatter keys, one JSON object) — no flag day.
- Order: ship Phase 1+2 first (oracle handles missing `trace-expires` as
  `within-window: null`), then Phase 3 stamping, then Phase 4 backfill so older
  notes gain `expires` over time. Each phase is independently safe.
- **Deploy requires a forced reindex** (Phase 1) so the back-catalogue populates the
  new columns; without it existing notes report `available: false` despite carrying
  a `trace` — the mtime gate would otherwise hide them indefinitely.
- The earlier draft claimed `trace-expires` "round-trips through `extra` before
  promotion, so borg can stamp it independently." That is true of the parser but
  moot under this ship order (Phase 1 promotes the field before Phase 3 stamps it);
  the claim is dropped to avoid implying an ordering that isn't used.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `to_yaml()` silently drops promoted keys on any rewrite (e.g. `cortex summarize --backfill`) | High if missed | High | Explicit emission in the named block + a rewrite-path round-trip test as a Phase-1 gate |
| New columns stay empty: `index_vault` mtime gate skips unchanged notes | High | Med | Forced `--reindex`/schema-marker path on deploy (Phase 1); test that pre-existing rows populate |
| `NoteRow` positional decode / repeated SELECT projections drift | Med | High | Update all projections + `from_row` together; a missed site fails decode or loses metadata |
| `expires` diverges from real sweep (sweeper keys on stage-dir mtime, not `ingested`) | Med | Low | `within-window` is explicitly the *policy-says-so* invariant, not a sweep prediction; documented in API Design |
| Size-based early sweep deletes a trace before `expires` | Med | Low | Best-effort `true`; callers fall back on fetch miss |
| Migration not transactional (matches existing `ensure_distilled_columns`) | Low | Med | Idempotent PRAGMA-guarded `ALTER`; DB snapshot before first run; transaction-vs-not resolved in Open Questions |
| Backfill skips notes that already have datetime `ingested` | Med | Med | Skip predicate keys on `trace-expires` presence, not `ingested` (Phase 4) |
| Dual `ingested` format breaks naive date math | Med | Med | Parser handles both bare-date and offset-datetime forms (Phase 3/4) |
| Calendar-date vs timezone fuzz at window edge | Low | Low | UTC calendar dates; ±1 day immaterial against 60 days |

## Resolved Decisions

All open questions resolved via two review rounds. Both reviewers (Staff Engineer /
Codex and Architect / agy) verified read-only against the codebase in both rounds.
Where they split (Q3/#3, Q4/#6, Q5/#7) the Architect's position won on code-grounded
reasoning, independently re-verified. Each decision carries its code evidence.

1. **`within-window` semantics** — means "borg's retention policy says this trace
   should still exist, based on `ingested`." Not a sweep prediction, not a dereference
   guarantee. (See API Design.)
2. **JSON key casing** — `within-window` (hyphenated), per the global JSON/YAML
   hyphen convention. SQLite column stays `trace_expires`; the wire key is hyphenated.
3. **Migration shape** — match `ensure_distilled_columns`: PRAGMA probe + individual
   `ALTER ADD COLUMN`, **no wrapping transaction, no `set_version`**. (Codex wanted a
   transaction; Architect dissented and won on the evidence — a single idempotent
   `ALTER ADD COLUMN` can't half-apply, so the Rust DDL-transaction rule doesn't
   bite, and no version infra exists in `vault/src/search/`. Both agreed on no
   `set_version`.)
4. **Block name** — `trace` (covers any staged source; `trace` is already the generic
   durable-capture handle at `borg/src/markdown.rs:138`, and a trace holds body,
   attachments, fetched bytes, transcript, summary, rejection records —
   `borg/src/stages/artifact.rs:11`).
5. **Phase-4 verb** — keep `backfill-ingested` (wired through clap/dispatch/log at
   `sb/src/cli/borg.rs:100, :472`; no cron/systemd caller); broaden help text or add
   an alias rather than rename.
6. **receipts.db unavailable during backfill** — compute `trace-expires` from the
   midnight-fallback `ingested` (best-effort, `precise=false`); do **not** skip.
   (Codex wanted warn+skip; Architect dissented and won — `ingested` is never null on
   a receipts-miss, so skipping `trace-expires` would emit a false `within-window:
   null` that the design reserves for "no trace data.")
7. **Forced-reindex mechanism** — a manual `--reindex`/force flag, not a self-healing
   version gate (no version infra to build on; benign failure mode — additive cols
   default to `''` and normal reindex backfills over time). (Codex agreed with the
   self-heal default; Architect dissented and won on the no-new-infra argument.)
8. **Alt-2 verified presence** — out of scope / future opt-in. Oracle config has no
   borg staging root (`oracle/src/config.rs:7`), borg owns retention
   (`borg/src/config.rs:490`), and the sweeper keys on stage-dir mtime anyway.

## Open Questions
- One micro-decision, non-blocking: whether the Phase-1 `--reindex` flag is needed at
  all, or letting normal mtime-driven reindex backfill the additive columns over time
  is sufficient. (Implementation-time call; both reviewers fine either way.)

## References
- Idea capture: `docs/improvements/oracle-transcript-availability.md`
- Retention config: `borg/src/config.rs` (`StagingConfig::retention_days`),
  `~/.config/sb/borg.yml` (`staging.retention-days: 60`)
- Durable-capture / `ingested` semantics: root `CLAUDE.md` "Borg durable-capture stores"
- Configurable retrieval precedent for oracle config: `docs/design/2026-06-06-configurable-retrieval-pipeline.md`
- Touch-points: `vault/src/frontmatter.rs`, `vault/src/search.rs`,
  `vault/src/search/schema.rs`, `vault/src/search/index.rs`, `oracle/src/server.rs`
