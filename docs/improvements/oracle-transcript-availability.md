# Oracle should advertise transcript availability

## Idea

- When the oracle MCP returns a note, it should tell the caller that a raw transcript / staged trace **exists** and is **cheaply referenceable**, without pulling or searching the transcript content.
- Goal is *awareness*, not retrieval: an agent should learn "a verbatim source exists, here's the handle, here's the deadline" so it can decide whether to go fetch it instead of trusting the lossy assisted summary.

## Why

- Note bodies served by oracle are the *assisted summary*; the summarizer can compress important details out (e.g. an on-screen prompt the user actually wanted).
- The verbatim source already lives on disk for 60 days as a staged trace, but oracle gives no signal it exists — so callers re-fetch from the network or give up.
- The data to surface this is already in the note frontmatter; oracle just drops it.

## What's already on disk (no new capture needed)

- Note frontmatter already carries:
  - `trace:` — e.g. `ht-95aa4e`, the staged-trace handle → `~/.local/share/sb/borg/stages/<trace>/distilled.yml#transcript`
  - `ingested:` — ISO timestamp, the clock start for the retention window
- Retention window is config-driven: `staging.retention-days` (default 60) in `borg.yml` (`borg/src/config.rs` `StagingConfig::retention_days`).

## Proposed surface (metadata detail level)

- Add a `transcript` (or `trace`) block to the metadata projection, e.g.:
  - `available: true`
  - `ref: ht-95aa4e`            (from `frontmatter.trace`)
  - `ingested: 2026-06-20T20:40:27-07:00`
  - `expires: 2026-08-19`       (`ingested` + `retention-days`)
  - `within_window: true`
- Content stays out of band — no transcript text, no new search path.

## Recommended scope (option 2 of 3)

- **Option 1 — echo only:** surface `trace` + `ingested`, let the caller do the math. Cheapest; zero coupling.
- **Option 2 — computed (recommended):** oracle reads `staging.retention-days` and emits `expires` + `within_window`. Answers "exists and easy to reference inside 60d" exactly, stays decoupled from borg's filesystem.
- **Option 3 — verified:** `stat()` the stage dir to confirm the trace wasn't swept early. Most accurate, but couples oracle to borg's on-disk layout; degrades poorly. Not recommended.

## Code touch-points (carry the two fields through the chain)

- `vault/src/frontmatter.rs` (`Frontmatter`, ~L20) — promote `trace` + `ingested` out of the `extra` HashMap into real fields.
- `vault/src/search.rs` (`NoteRow`, ~L372) — add columns; plus the SQLite `notes` schema/migration.
- `vault/src/search/index.rs` (`index_one`, ~L77) — write the two fields during indexing.
- `oracle/src/server.rs` (`format_note`, ~L229) — emit the `transcript` block in the metadata projection (and carry into tldr/summary/full).
- Pull `retention-days` so oracle can compute `expires`/`within_window` (option 2).

## Open questions

- Block name + field names (`transcript` vs `trace`; `ref` vs `trace_id`).
- Do non-youtube notes ever carry a usable trace? If so this generalizes beyond transcripts to "raw staged source available."
- Backfill: older notes may lack `trace:`/`ingested:` — surface `available: false` rather than erroring.
