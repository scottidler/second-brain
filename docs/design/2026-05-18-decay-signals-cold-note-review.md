# Design Document: Decay & Promotion Signals + Cold-Note Review

**Author:** Scott Idler
**Date:** 2026-05-18
**Status:** Implemented
**Review Passes Completed:** 5/5 + 1 architect round (Round 1 caught: recompute trigger sub-second cadence collision, missing `modified_at` index, Performance-vs-Phase-4 contradiction, HashMap key normalization gap; deferred `cortex pin` UX as a follow-on)

**Parent:** [docs/scaling-roadmap.md](../scaling-roadmap.md) (Doc 3 of 3)
**Builds on:**
- [2026-05-16-extractor-contract-and-l2-summaries.md](2026-05-16-extractor-contract-and-l2-summaries.md) (Doc 1: L2 distilled summaries + the `index_vault` rewrite that preserves signal columns across reindex)
- [2026-05-16-hybrid-retrieval-fts5-vector-rrf.md](2026-05-16-hybrid-retrieval-fts5-vector-rrf.md) (Doc 2: hybrid retrieval surface that Doc 3's signals could later re-rank)
- [2026-05-17-candle-embedding-backend.md](2026-05-17-candle-embedding-backend.md) (Doc 2 follow-on: candle backend, no Doc 3 impact)

## Summary

Make the corpus self-curate by surfacing what is not being used. Doc 1 added three accumulator columns to the `notes` table (`search_hit_count`, `last_accessed_at`, `inbound_link_count`) and a fourth derived column will join them (`pinned`); today every counter is zero because nothing writes them. Doc 3 ships the writer paths: oracle increments `search_hit_count` and stamps `last_accessed_at` on explicit `note_read` only (not on `knowledge_search` matches), oracle materializes `inbound_link_count` as a post-pass after every `index_vault` run, and `pinned` is sourced from frontmatter through the same vault-derived UPDATE path that Doc 1 established. A new `cortex sweep --cold` subcommand reads those signals and produces a review checklist at `system/views/cold-notes.md`; the cortex daemon runs it weekly. The output is a checklist, not an action. The user decides per row: archive, delete, leave, promote.

## Problem Statement

### Background

Doc 1 (Phases 1-9) shipped the `Distilled` contract and the body-rendering format, and it rewrote `index_vault` from `INSERT OR REPLACE` to `UPDATE`-vault-derived-columns-only for existing rows so that Doc 3's signal columns survive every reindex. Doc 1 Phase 9 added three scaffolding columns to the `notes` table in anticipation of Doc 3:

```
search_hit_count   INTEGER DEFAULT 0
last_accessed_at   INTEGER
inbound_link_count INTEGER DEFAULT 0
```

Two regression tests in `vault::search` lock the preservation contract: `index_one_insert_zeroes_signal_columns` (`vault/src/search.rs:2155`) asserts new rows start at the floor, and `index_one_update_preserves_signal_columns` (`vault/src/search.rs:2168`) seeds `search_hit_count = 17`, `last_accessed_at = Some(999_999)`, `inbound_link_count = 3` and asserts those values survive a reindex round-trip with new content + new mtime.

Doc 2 (Phase A + B) shipped hybrid retrieval and intentionally did not consume Doc 3's signals: the doc states explicitly that "Reranking based on Doc 3 signals (`search_hit_count`, `last_accessed_at`). Doc 3 owns that scoring layer; Doc 2 produces a pure-similarity ranking that Doc 3 can later re-weight." That layer is out of scope here too; Doc 3 stops at producing the signals and the cold-note report.

The current state, verified against source on 2026-05-18:

- Schema columns exist (`vault/src/search.rs:256-258`, `vault/src/search.rs:390-392`).
- `index_vault` preserves them on UPDATE (`vault/src/search.rs:588-637`) and zeros them on INSERT (`vault/src/search.rs:652-657`).
- `find_inbound_links(path)` exists as a per-query body scan (`vault/src/search.rs:1129`); nothing materializes the count into the column.
- `oracle::note_read` reads the note and returns it without touching any signal (`oracle/src/server.rs:304-319`).
- `oracle::knowledge_search` returns matches without touching any signal (`oracle/src/server.rs:200-298`).
- `cortex sweep` accepts `--migrate`, `--proposals`, `--dry-run` (`cortex/src/cli.rs:178-190`); there is no `--cold` flag.
- `Frontmatter` has no `pinned` field; arbitrary keys would land in `extra: HashMap<String, serde_yaml::Value>` (`vault/src/frontmatter.rs:6-17`).
- The cortex daemon has two interval ticks: a `sweep_interval` driven by `poll_interval` and an `embed_interval` driven by `crate::embed::daemon_cadence(config)` (`cortex/src/daemon.rs:81-119`). There is no `cold_interval`.

### Problem

Without writers, the columns are dead weight: every cold-note query returns the entire vault, every reranker would multiply by zero, every "what am I not using?" report cannot be written.

Three concrete failures fall out of this:

1. **No usage signal.** The retrieval surface (oracle MCP, Obsidian itself) cannot tell which notes are read versus which are returned-in-lists-and-ignored. Without a read counter, the corpus has no way to distinguish a note the user actually consulted from one a BM25 match dragged in.
2. **No structural signal.** A note linked from twelve other notes is structurally hotter than an orphan; today the column that would capture that is always zero, and the only way to compute it is a per-query body scan over the whole vault.
3. **No surface for review.** At ~7K notes (one year) and ~21K (three years), the user cannot manually inspect every note to decide what to keep. The corpus needs a periodic report that says "these N notes are old, unlinked, never opened; here is a checklist - decide."

Underneath: Doc 2 makes the corpus more *findable* but does not bound its *cost over time*. Without a decay-and-review surface, every note remains equally weighted forever; most are not gold; the signal-to-noise of search results decays as the long tail grows.

### Goals

- Write `search_hit_count` and `last_accessed_at` on explicit human-intent reads (`note_read` only).
- Materialize `inbound_link_count` on every `index_vault` run, in a single post-pass.
- Add `pinned: bool` to `Frontmatter`, index it as a vault-derived column on `notes`, and treat it as a floor: pinned notes never appear in the cold report.
- Add `cortex sweep --cold` that produces `system/views/cold-notes.md` as a review checklist.
- Run the cold report weekly via the cortex daemon on a dedicated `cold_interval` tick (separate from the existing sweep tick).
- Configurable thresholds in `~/.config/obsidian-cortex/obsidian-cortex.yml`, defaults checked into code with the reasoning documented.
- Snapshot fixture coverage for the cold report rendering.

### Non-Goals

- Reranking `knowledge_search` results by Doc 3 signals. That is a future doc; Doc 3 produces signals and a report, nothing more.
- Auto-deletion or auto-archival. Always surface for human decision.
- Tracking Obsidian-side opens. `file atime` is unreliable; an Obsidian plugin is heavy and side-channel. Only oracle-mediated access counts.
- Counting `knowledge_search` matches as access. Per the roadmap: "Returning a note in a `knowledge_search` top-10 list does **not** count - that is a lexical match, not a human signal, and counting it creates a positive feedback loop where high-BM25-scoring notes become immortal and the entire decay premise collapses."
- Multi-tier promotion beyond a single `pinned: true` flag. Layered tiers (L3 / starred / featured / etc.) are deferred until human promotion patterns actually emerge from review reports.
- A separate "warmth" or "popularity" report. The cold report is the only surface; warm notes are the inverse-by-construction.
- Backfilling historical access events. Day-1 signal state is whatever the columns were when the writers turn on (zeros for `search_hit_count`, NULL for `last_accessed_at`, computed-from-current-vault for `inbound_link_count`). The first cold report on a populated vault will surface a lot; that is the intended audit moment.

## Proposed Solution

### Overview

Three writer paths into the same SQLite `notes` table, one reader path that consumes them, one rendered report. Oracle and cortex never call each other; they coordinate exclusively through the shared SQLite file under WAL mode.

```
                       ┌─────────────────────────────┐
                       │ SQLite (vault search DB)    │
                       │                             │
                       │   notes.search_hit_count    │◀──┐
                       │   notes.last_accessed_at    │◀──┤  WRITES
                       │   notes.inbound_link_count  │◀──┤  (oracle only)
                       │   notes.pinned              │◀──┘
                       │                             │
                       │   <SELECT cold rows>        │───┐
                       └─────────────────────────────┘   │  READS
                              ▲      ▲      ▲           │  (cortex only)
                              │      │      │           │
              ┌───────────────┘      │      └─────────────────┐
              │ bump_access  recompute_inbound  vault-derived │
              │ (note_read)  (reindex)          UPDATE        │
              │                                 (reindex)     │
              │                                               │
       ┌──────┴──────────────┐                       ┌────────┴──────────┐
       │ Oracle MCP          │                       │ Cortex            │
       │  - note_read        │                       │  - sweep --cold   │
       │  - knowledge_search │                       │  - daemon cold    │
       │    (no signal write)│                       │    tick           │
       │  - index_vault      │                       │                   │
       │    + post-pass      │                       │  writes report to │
       │      inbound        │                       │  system/views/    │
       │      recompute      │                       │  cold-notes.md    │
       │  + parses           │                       └───────────────────┘
       │    frontmatter      │
       │    `pinned` into    │
       │    notes.pinned     │
       └─────────────────────┘
                ▲
                │ filesystem mtime
                │
       ┌────────┴────────┐
       │ Vault note .md  │
       │   frontmatter:  │
       │     pinned: true│ ← user-edited in Obsidian
       └─────────────────┘
```

Five structural pieces:

1. **`note_read` signal write.** A new method on the search DB, `bump_access(path)`, performs a single bounded UPDATE incrementing the counter and stamping the timestamp. Oracle's `note_read` handler calls it before formatting the response. No other handler calls it. `knowledge_search` is untouched. This is the load-bearing decision the parent roadmap codifies; Doc 3 enforces it with a test that asserts `knowledge_search` does not bump.

2. **Inbound-link materialization.** A new `recompute_inbound_link_counts()` method on the search DB does a single pass over every note's body, computes a `HashMap<stem_lowercase, count>` of wikilink targets, and bulk-updates the `inbound_link_count` column with one UPDATE per row that changed. **Trigger:** a dedicated periodic background task in oracle (default 10-minute interval, modeled on the cortex embed tick) calls it. **NOT** called at the end of every `index_vault` pass: the watcher fires on every Obsidian auto-save with sub-second debounce, and at three-year scale (~21K notes) a 300ms full-table wikilink scan holding `Mutex<SearchIndex>` would block every concurrent `note_read` and `knowledge_search`. The cold report's freshness budget is days, not seconds; a 10-minute cadence is at worst minutes stale relative to the weekly cold tick. Cortex never invokes the recompute; the one-way data flow rule stays clean.

3. **`pinned` frontmatter and column.** Add `pinned: Option<bool>` to `Frontmatter`. Add `pinned INTEGER DEFAULT 0` to `notes`. `index_vault` UPDATE/INSERT includes it as a vault-derived column. The cold-note query filters `WHERE pinned = 0`.

4. **`cortex sweep --cold` subcommand.** Reads the `notes` table, applies the cold rule (all three signals at the floor + age > threshold + not pinned), groups by domain, and renders a markdown checklist to `system/views/cold-notes.md`. Configurable thresholds. Atomic write (temp file + rename) like every other system view.

5. **Cortex daemon `cold_interval` tick.** A third interval alongside `sweep_interval` and `embed_interval`. Default cadence 1 week. The tick calls the same `sweep::run_cold` function the CLI uses; both paths converge on one implementation. `block_in_place` around the body, matching the embed-tick pattern.

### Architecture

```
┌──────────────────────────────────────────────────────────────┐
│ vault crate                                                  │
│                                                              │
│  vault::frontmatter                                          │
│   + Frontmatter { pinned: Option<bool>, ... }   ← NEW field  │
│                                                              │
│  vault::search                                               │
│   + SearchIndex::bump_access(path)              ← NEW (Doc 3)│
│   + SearchIndex::recompute_inbound_link_counts() ← NEW       │
│   + cold_notes(query: ColdQuery)                ← NEW        │
│   + notes table gains `pinned INTEGER DEFAULT 0`             │
│   + ensure_distilled_columns() adds `pinned` migration       │
│   + index_vault UPDATE/INSERT includes `pinned`              │
│                                                              │
│  feature flags: no new feature; this is all behind `search`. │
└──────────────────────────────────────────────────────────────┘
                          │
        ┌─────────────────┴──────────────────┐
        ▼                                    ▼
┌──────────────────────────┐      ┌──────────────────────────┐
│ oracle                   │      │ cortex                   │
│                          │      │                          │
│  - note_read handler     │      │  - cli: SweepOpts.cold   │
│    bumps access before   │      │  - sweep::run_cold()     │
│    returning             │      │    queries cold notes,   │
│  - 10-min periodic task  │      │    renders report,       │
│    calls recompute_      │      │    writes               │
│    inbound_link_counts() │      │    system/views/         │
│    (NOT watcher path)    │      │    cold-notes.md         │
│  - knowledge_search:     │      │  - daemon: cold_interval │
│    NO signal write       │      │    tick calls run_cold   │
└──────────────────────────┘      └──────────────────────────┘
```

Crate responsibility, one-way data flow:

- **Vault file** is canonical for `pinned`. Users edit `pinned: true` in Obsidian or via `cortex` mutations.
- **Index** is canonical for signal accumulators (`search_hit_count`, `last_accessed_at`, `inbound_link_count`). They are not derivable from the vault file. They survive reindex by Doc 1's contract.
- **Oracle** is the sole writer of every Doc 3 signal column. `bump_access` is called from `note_read` only; `recompute_inbound_link_counts` is called from a dedicated 10-minute periodic background task, never from the watcher-driven reindex path.
- **Cortex** writes nothing to the `notes` table for Doc 3. It writes the report file (`system/views/cold-notes.md`) to the vault. The daemon's cold tick is a pure consumer.
- **Borg** is unchanged.

This split keeps the one-way rule honest: the vault file remains the canonical store of every user-edited field; oracle remains the single SQLite writer for index-derived state; cortex remains the single writer for `note_embeddings`. No new cross-crate writer is introduced.

### Data Model

#### Schema additions

One column, plus the three already in place from Doc 1 Phase 9:

```sql
-- Doc 1 Phase 9 already shipped:
--   search_hit_count   INTEGER DEFAULT 0
--   last_accessed_at   INTEGER     -- NULL until first read
--   inbound_link_count INTEGER DEFAULT 0
--
-- Doc 3 adds:
ALTER TABLE notes ADD COLUMN pinned INTEGER DEFAULT 0;

-- The cold-note SELECT filters on `modified_at < ?1`; the existing
-- indices (`vault/src/search.rs:261-264`) cover `domain`, `note_type`,
-- `status`, `date` but NOT `modified_at`. Without this index the cold
-- query falls back to a full table scan.
CREATE INDEX IF NOT EXISTS idx_notes_modified_at ON notes(modified_at);
```

`pinned` is a vault-derived column. Doc 1's `index_vault` UPDATE/INSERT pass gains it in the `distilled_columns` migration list (`vault/src/search.rs:379-393`) and in the UPDATE/INSERT bodies (`vault/src/search.rs:592-637`, `640-682`).

No new tables. The single-table shape keeps the cold query a one-statement SELECT and avoids cross-table join cost. The `modified_at` index is also added by the same `ensure_distilled_columns` migration so the cold query never sees a sequential scan.

#### `Frontmatter` field

```rust
// vault/src/frontmatter.rs
pub struct Frontmatter {
    // ... existing fields ...
    pub pinned: Option<bool>,    // NEW
    // ...
    pub extra: HashMap<String, serde_yaml::Value>,
}
```

Serialized as the YAML key `pinned`. `None` and `false` both index as `0`; `true` indexes as `1`. The frontmatter parser already handles missing keys as `None`; no migration needed for existing notes.

**Parser permissiveness.** Existing fields in `Frontmatter::from_value` are lenient by formatting non-string values via `format!("{other:?}")`; for a typed bool field that approach is the wrong shape (it would yield `Some("Bool(true)")` instead of `Some(true)`). Doc 3 adds a strict-but-quiet branch for `pinned`:

```rust
"pinned" => {
    pinned = match val {
        serde_yaml::Value::Bool(b) => Some(b),
        _ => None,   // accept only YAML bools; silently ignore strings/ints/null
    };
}
```

`pinned: true` and `pinned: false` work. `pinned: "true"`, `pinned: 1`, `pinned:` (null) all parse as `None` and index as `0`. Document the bool-only contract in the field doc comment; a string or int value in a user's frontmatter is treated as "not pinned" rather than an error so a typo never breaks reindex.

#### Bump semantics

```rust
// vault/src/search.rs
impl SearchIndex {
    /// Increment search_hit_count and stamp last_accessed_at = now.
    /// Single bounded UPDATE; no transaction wrapper needed because
    /// the statement is a single row in WAL mode.
    ///
    /// Returns Ok(()) for a path that is not in the index (the note
    /// may have been deleted between the read and the bump). The bump
    /// is best-effort signal; a missing row is not an error.
    pub fn bump_access(&self, path: &str) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.conn.execute(
            "UPDATE notes
                SET search_hit_count = search_hit_count + 1,
                    last_accessed_at = ?2
              WHERE path = ?1",
            params![path, now],
        )?;
        Ok(())
    }
}
```

A path that is not in the index (race: note deleted between `knowledge_search` and `note_read`) results in `rows_affected = 0`. That is intentional. The bump is best-effort signal; a missing row is not an error and not worth surfacing to the caller. Logged at TRACE only.

#### Inbound-link recompute semantics

```rust
// vault/src/search.rs
impl SearchIndex {
    /// Walk every note's body, count wikilink targets, materialize
    /// inbound_link_count for every row. One pass, bounded by vault size.
    /// Idempotent.
    pub fn recompute_inbound_link_counts(&self) -> Result<usize> {
        // 1. Read (path, stem_lower, body) for every note. Compute
        //    stem_lower = filename_stem(path).to_ascii_lowercase().
        // 2. For each body, parse wikilinks via `extract_wikilinks`.
        //    The existing parser strips `#heading` and `|alias` and
        //    skips fenced code blocks (`vault/src/search.rs:1502-1525`).
        // 3. For each (source_stem_lower, target_stem_lower) pair,
        //    increment a HashMap<String, u64> keyed by
        //    target.to_ascii_lowercase(). SKIP self-links where
        //    source_stem_lower == target_stem_lower: a note linking
        //    to itself is not structural signal.
        // 4. Open a transaction; for each row, look up its
        //    stem_lower in the HashMap (default 0 if absent) and
        //    UPDATE inbound_link_count where the new value differs
        //    from the old (skip no-op UPDATEs to keep WAL churn
        //    down). Rows whose stem is absent from the HashMap get
        //    UPDATE to 0.
        // 5. Commit. Return number of rows changed.
    }
}
```

Key normalization rule (load-bearing): the HashMap key is `target.to_ascii_lowercase()` on insert; the per-row lookup is `filename_stem(notes.path).to_ascii_lowercase()`. Both sides are lowercased before the lookup, so `eq_ignore_ascii_case` parity is automatic. Anything that compares stems without lowercasing first is a bug.

Specifically NOT counted:

- Self-links (`A.md` body contains `[[A]]`): zero structural value.
- Links inside fenced code blocks: the parser already excludes these.
- Wikilinks pointing to non-existent stems: nothing to UPDATE; the dangling reference contributes to nothing.

Counted regardless of:

- Case (target match uses `eq_ignore_ascii_case`, matching the existing `find_inbound_links` behavior at `vault/src/search.rs:1145`).
- Section anchors (`[[note#section]]` counts as one link to `note`; the `#section` is stripped by the regex).
- Alias text (`[[note|display text]]` counts as one link to `note`; the `|alias` is stripped).

#### Cold-note query

```rust
// vault/src/search.rs
pub struct ColdQuery {
    /// Notes with `modified_at` strictly less than this Unix-seconds
    /// value are eligible. Callers compute this as
    /// `now_unix_secs - older_than_days * 86_400`; the query stays in
    /// integer seconds so SQLite's index on `modified_at` is usable.
    pub older_than: i64,
    /// Limit on rows returned. Default 500 (config-overridable).
    pub limit: u32,
}

impl SearchIndex {
    pub fn cold_notes(&self, q: &ColdQuery) -> Result<Vec<NoteRow>> {
        // SELECT ... FROM notes
        //  WHERE search_hit_count = 0
        //    AND last_accessed_at IS NULL
        //    AND inbound_link_count = 0
        //    AND pinned = 0
        //    AND modified_at < ?1
        //  ORDER BY modified_at ASC
        //  LIMIT ?2
    }
}
```

The cold rule is the conjunction of every floor: zero reads + zero accesses + zero inbound links + not pinned + old enough. A note that scores anywhere on any axis stays out of the report.

**Note on the `last_accessed_at IS NULL` clause.** A note that was read once, ever, is treated as permanently warm by this rule (its timestamp is non-NULL forever). That is intentional: an explicit `note_read` is the strongest human-intent signal the system captures, and weighting it as "expires after X days" would re-introduce the positive-feedback dynamic this design exists to avoid. The decay model is binary at this axis; refinement (read-recency windowing, exponential decay) can layer on later if review reports show the floor is too generous.

### API Design

#### CLI: `cortex sweep --cold`

`SweepOpts` gains one boolean flag:

```rust
// cortex/src/cli.rs
pub struct SweepOpts {
    #[arg(long)]
    pub migrate: bool,

    #[arg(long)]
    pub dry_run: bool,

    #[arg(long)]
    pub proposals: bool,

    /// Produce the cold-note review report at
    /// `system/views/cold-notes.md`. Reads the materialized signals;
    /// does not modify any note files.
    #[arg(long)]
    pub cold: bool,
}
```

`--cold` is mutually exclusive with `--migrate` and `--proposals` (existing pattern: the dispatcher matches on which flag is set).

#### Cortex config additions

Per `CLAUDE.md`, cortex reads from `~/.config/obsidian-cortex/obsidian-cortex.yml`:

```yaml
# ~/.config/obsidian-cortex/obsidian-cortex.yml
sweep:
  # ... existing keys ...
  cold:
    # Notes whose modified_at is older than this many days are eligible
    # for the cold report. Defaults to 180 (~6 months): long enough that
    # short-term reference notes don't get surfaced before they've had a
    # chance to accrue signals, short enough that the long tail surfaces
    # within a year.
    older-than-days: 180

    # Cap on rows in a single report. Defaults to 500. The report is a
    # review checklist; >500 rows is unreviewable in one sitting.
    limit: 500

daemon:
  # ... existing keys ...
  cold-interval-secs: 604800   # 1 week
```

Config keys use kebab-case as the project convention; serde renames to snake_case via `rename_all = "kebab-case"` on the struct.

#### Daemon tick

A third interval alongside `sweep_interval` and `embed_interval`:

```rust
// cortex/src/daemon.rs
let mut cold_interval = tokio::time::interval(
    Duration::from_secs(config.daemon.cold_interval_secs),
);
cold_interval.tick().await; // consume immediate first tick

// in the select! loop:
_ = cold_interval.tick() => {
    log::info!("running periodic cold-note sweep");
    let res = tokio::task::block_in_place(|| {
        crate::sweep::run_cold(vault_root, config)
    });
    match res {
        Ok(stats) => log::info!(
            "cold sweep: scanned={} surfaced={} pinned_excluded={}",
            stats.scanned, stats.surfaced, stats.pinned_excluded,
        ),
        Err(e) => log::error!("cold sweep failed: {e}"),
    }
}
```

`block_in_place` matches the embed-tick pattern: the cold sweep does CPU + SQLite IO and must not starve the tokio runtime.

#### Report format

A markdown file rendered atomically (write to `cold-notes.md.tmp`, rename), grouped by domain, with one row per note and a leading checkbox:

```markdown
---
generated-at: 2026-05-25T03:00:01Z
generator: cortex sweep --cold
older-than-days: 180
total-surfaced: 142
pinned-excluded: 23
---

# Cold Notes

Notes older than **180 days** with no reads, no inbound links, and not pinned.
Decide per row: archive, delete, leave, promote.

This file is regenerated weekly by `cortex sweep --cold`. Do not edit
manually; pin a note (`pinned: true` in its frontmatter) to remove it
from this report.

## ai (47)

- [ ] `notes/ai/2025-08-12-some-paper.md` - "Some Paper Title" - last modified 2025-08-12
- [ ] `notes/ai/2025-09-03-other-thing.md` - "Other Thing" - last modified 2025-09-03
...

## diy (18)

...

## homelab (12)

...
```

Format invariants:

- Frontmatter has `generated-at`, `generator`, the active threshold, and counts. Mechanical metadata, no user editing.
- Frontmatter also carries `pinned: true` on the report file itself, so the cold-notes report can never qualify as cold (it would otherwise grow into the report after enough weeks of zero reads).
- One H2 per domain, with the domain count in parens. Notes with empty/missing `domain` frontmatter group under `## (no domain)` to keep them visible rather than hidden in a default bucket.
- One bullet per note: checkbox + relative path in inline code + title in quotes + last-modified date.
- Footer (omitted for brevity): cross-reference back to the [[borg-dashboard]] view.
- Empty-vault edge case: when `cold_notes` returns zero rows, render the frontmatter + a single line "No cold notes at the current threshold." rather than an empty body. Keeps the file's mtime fresh so the next regeneration is a clean overwrite.

### Implementation Plan

Five phases. Each modifies a small surface; Phase 1 unlocks the first observable signal in the DB, Phases 2-3 fill in the other axes, Phase 4 produces the report, Phase 5 puts it on autopilot.

#### Phase 1: `bump_access` and oracle wiring
**Model:** sonnet

- Add `SearchIndex::bump_access(path)` to `vault/src/search.rs`.
- Wire it into `oracle::note_read` immediately after the `db.get_note(&req.path)` call (before formatting the response).
- Add a unit test in `vault/src/search/tests.rs`: insert a note, call `bump_access` twice, assert `search_hit_count == 2` and `last_accessed_at` is set.
- Add an oracle-level test asserting that calling `knowledge_search` does NOT bump access on any returned note. This is the load-bearing guard; without the test, a future refactor could silently re-introduce the positive feedback loop.

#### Phase 2: Inbound-link materialization
**Model:** opus

- Add `SearchIndex::recompute_inbound_link_counts()` to `vault/src/search.rs`. Reuse the existing `extract_wikilinks` parser. HashMap is keyed by `target.to_ascii_lowercase()`; the UPDATE statement matches notes by `LOWER(<filename-stem>)` (or pre-lowercase the stem in Rust before binding the parameter) so case-insensitivity is enforced symmetrically on both sides of the lookup.
- Decide between full-recompute and per-note-incremental. Full-recompute is simpler and correct; incremental requires tracking which stems an edit affects (the edited note's outbound links *and* the edited note's stem, because removing a link from A to B drops B's count). Default to full-recompute; revisit if it shows up in profiling.
- Add an oracle background task that calls `recompute_inbound_link_counts()` on a 10-minute interval (config-overridable as `oracle.inbound_recompute_interval_secs`, default 600). Spawn it in `oracle/src/main.rs` next to the existing VaultWatcher spawn (`oracle/src/main.rs:84`). The task uses the same `db_handle.lock()` as the watcher reindex. **This is the only caller.** Do NOT add the call to `index_vault`'s end; the watcher-driven path must not pay the recompute cost on every Obsidian save.
- Tests: fixture with three notes where A→B and C→B; assert `inbound_link_count == 2` for B after recompute. Fixture with A→B then A edited to remove the link; assert B's count drops to 0 after the second recompute. Fixture with A linking to itself (`[[A]]` in A's body); assert A's `inbound_link_count == 0` after recompute (self-link exclusion). Mixed-case fixture: A's body contains `[[Some-Note]]`, target file is `some-note.md`; assert the target's count is 1 (case-insensitive match).

#### Phase 3: `pinned` frontmatter and column
**Model:** sonnet

- Add `pinned: Option<bool>` to `vault::frontmatter::Frontmatter`.
- Update `Frontmatter::from_value` to parse `pinned` from the YAML mapping.
- Update `Frontmatter::to_yaml` to emit `pinned` when `Some`.
- Add `pinned INTEGER DEFAULT 0` to the `ensure_distilled_columns` migration list and to the `CREATE TABLE` body in `ensure_schema`.
- Update `index_vault`'s UPDATE statement (`vault/src/search.rs:592`) and INSERT statement (`vault/src/search.rs:640`) to include `pinned` as a vault-derived column. Bind from `fm.pinned.unwrap_or(false) as i64`.
- Tests:
  - Roundtrip a `Frontmatter` with `pinned: true` through `from_value` / `to_yaml`.
  - Index a fixture note with `pinned: true`; SELECT pinned, assert it's 1.
  - Index a fixture note without `pinned`; assert pinned column is 0.
  - `pinned: "true"` (string), `pinned: 1` (int), `pinned: null` all parse as `None` and index as `0`. Asserts the strict-bool-only contract.
  - Bonus assertion: a note with `pinned: true` indexed once, then reindexed without any frontmatter changes, still has `pinned == 1`. (vault-derived UPDATE includes it; no preservation contract violation).
  - Flip test: a note with `pinned: true` is later edited to remove the field; after reindex, `pinned == 0`. (UPDATE path correctly clears the flag when frontmatter changes.)

#### Phase 4: `cortex sweep --cold` subcommand
**Model:** sonnet

- Add `pub cold: bool` to `SweepOpts`.
- Add `pub fn run_cold(vault_root: &Path, config: &Config) -> Result<ColdStats>` to `cortex/src/sweep.rs`.
- `run_cold` opens the search DB **read-only** (cortex writes nothing to `notes`; the inbound counts it reads are whatever oracle's 10-minute periodic recompute most recently materialized). Computes `older_than = now_unix_secs - sweep.cold.older_than_days * 86_400`, calls `cold_notes(&ColdQuery { older_than, limit })`, groups results by domain, renders the markdown report, writes atomically to `vault_root/system/views/cold-notes.md`.
- `ColdStats { scanned, surfaced, pinned_excluded }`. `pinned_excluded` is a separate SELECT counting rows that would have qualified except for `pinned = 1`; surfaced visibility into how the floor is doing.
- Wire `cmd.cold` into the dispatcher in `cortex/src/main.rs`; emit `eyre::bail!` if `--cold` is combined with `--migrate` or `--proposals`.
- Tests: synthetic vault with one cold note, one pinned note, one recent note, one note with `inbound_link_count > 0`. Run `run_cold`. Assert the report contains only the cold note. Assert `pinned_excluded == 1`.

#### Phase 5: Daemon `cold_interval` tick
**Model:** sonnet

- Add `cold_interval_secs: u64` to `DaemonConfig` (default 604_800).
- Spawn `cold_interval` alongside `sweep_interval` and `embed_interval` in `start_watching`.
- Add the `_ = cold_interval.tick() => ...` arm to the `select!` loop, mirroring the embed-tick block_in_place + match-on-result pattern.
- Test: drive the daemon with a 2-second `cold_interval_secs` override in a tempdir vault; assert `cold-notes.md` appears within a few seconds.

## Alternatives Considered

### Alternative 1: Sidecar `note_signals` table
- **Description:** Keep signal columns in a separate `note_signals (path, hits, accessed, inbound)` table instead of widening `notes`.
- **Pros:** Theoretical isolation between vault-derived and accumulator data; `notes` could keep using `INSERT OR REPLACE` if it ever wanted to (it doesn't, post Doc 1).
- **Cons:** Every cold query becomes a JOIN. Every reindex pass that asserts the preservation contract has to verify a separate table didn't drift. The Doc 1 rewrite already moved `notes` to UPDATE-only-vault-derived-columns; the conjunction-of-floors cold query benefits from a single-row read.
- **Why not chosen:** The preservation contract is already proven on the single-table shape (Doc 1 regression test). Splitting tables would buy nothing and cost a JOIN on the hot path.

### Alternative 2: Trigger-driven inbound-count maintenance
- **Description:** On UPDATE of `notes.body`, run a SQL trigger that re-parses wikilinks and updates `inbound_link_count` for affected notes.
- **Pros:** Always-fresh counts without an explicit recompute call.
- **Cons:** SQLite triggers can't call Rust. The only way to parse wikilinks from SQL is regex on `body`, which doesn't match the existing `extract_wikilinks` parser; the trigger would silently diverge on edge cases (e.g. wikilinks inside code fences). And the trigger would fire on every note rewrite, multiplying the cost of a single edit.
- **Why not chosen:** Same reason Doc 2 chose a recompute loop over a trigger for the embedding staleness signal: triggers in SQLite can't run Rust, and we want one parser, not two. Recompute on reindex is cheap enough at three-year scale.

### Alternative 3: Count `knowledge_search` matches as access
- **Description:** Bump `search_hit_count` for every note returned in a `knowledge_search` top-N list, not just on `note_read`.
- **Pros:** More signal volume; reranking later would have more rows to score.
- **Cons:** Positive feedback loop. High-BM25-scoring notes get hit on every query that surfaces them, then rank higher on any later signal-aware ranker, then surface in more lists, and so on. The decay premise collapses; no note ever goes cold.
- **Why not chosen:** This is the load-bearing decision the parent roadmap codifies. Doc 3 enforces it with a test that asserts `knowledge_search` does not bump.

### Alternative 4: Decay score instead of conjunction-of-floors
- **Description:** Compute a continuous "warmth" score from a weighted combination of the three signals, sort ascending, take the bottom N.
- **Pros:** Smoother, less binary; could distinguish "almost cold" from "totally cold."
- **Cons:** Weight tuning is a research project. Any weights chosen now will be wrong. The user has to interpret the score, which means a UI for scores rather than a checklist of paths.
- **Why not chosen:** The MVP is a review checklist, not a ranking. Conjunction-of-floors produces a defensible "definitely review this" list; a weighted score produces argumentative rankings. Start simple; add a score later only if floors prove insufficient.

### Alternative 5: Surface the report only via the MCP, not as a vault file
- **Description:** Add `mcp__oracle__cold_notes` and skip writing `system/views/cold-notes.md`.
- **Pros:** No drift between the report and the live signals; no atomic-write concerns.
- **Cons:** The user reviews the report in Obsidian; an MCP tool buries it behind a Claude-Code session. The other system views (`borg-intake.md`, `borg-ledger.md`, `borg-dlq.md`) are vault files for the same reason: they need to be reviewable without spinning up an LLM.
- **Why not chosen:** The report is a checklist the user opens in Obsidian, ticks rows, and acts on. It belongs in the vault, not behind a tool call. An MCP tool can be added later as a complement.

## Technical Considerations

### Dependencies

No new external crates. Doc 3 reuses:

- `rusqlite` (already in `vault` behind `search`).
- `serde_yaml` (already for `Frontmatter`).
- `tokio::time::interval` and `tokio::task::block_in_place` (already in `cortex::daemon`).
- `log` macros (already pervasive in cortex/vault).

### Performance

#### `bump_access`

Single-row UPDATE in WAL mode. Microseconds. Fires on every `note_read`; at the assumed Claude-Code session rate (single-digit per second peak), the cost is invisible.

#### `recompute_inbound_link_counts`

One pass over every note body. At today's scale (~1,345 notes, mean body ~2 KB), reading + parsing is ~3 MB and a few tens of milliseconds. At the three-year horizon (~21K notes), ~40 MB and a few hundred milliseconds.

Triggered from a dedicated 10-minute periodic background task inside oracle (modeled on the existing cortex embed tick cadence). NOT called from the watcher-driven reindex path: the watcher fires sub-second on every Obsidian auto-save, and at three-year scale this would be a 300ms full-table scan + exclusive write lock per save, blocking the shared `Mutex<SearchIndex>` against `note_read` / `knowledge_search`. NOT called from `cortex sweep --cold` either: cortex is read-only against `notes`. The 10-minute cadence makes the cold report at worst 10 minutes stale on inbound counts; at the weekly cold-tick cadence the cumulative recompute cost is ~6 × 10⁻⁴ of wall-clock time.

If profiling later shows this is hot, the fallback is incremental: track edited notes since last recompute and rescan only their outbound link sets. Defer until measured.

#### Cold-note query

Single SELECT with a four-clause WHERE on indexed `modified_at`. Sub-millisecond at any plausible scale. Add an index on `modified_at` if it isn't already (it is, per `vault/src/search.rs:264`).

#### Daemon overhead

Three intervals instead of two. The cold tick fires once per week; cost is the report write (low milliseconds).

### Security

No new attack surface. The cold report is a vault file the user controls. `bump_access` increments by 1 and stamps a timestamp; no path-traversal or injection vector (path comes from the existing `NoteReadRequest` which is already validated by the oracle handler).

### Testing Strategy

- **Unit tests in `vault/src/search/tests.rs`:**
  - `test_bump_access_increments` (Phase 1).
  - `test_bump_access_idempotent_on_missing_path` (Phase 1).
  - `test_recompute_inbound_link_counts_basic` (Phase 2).
  - `test_recompute_inbound_link_counts_handles_link_removal` (Phase 2).
  - `test_pinned_column_survives_reindex` (Phase 3; symmetric to the Doc 1 signal-survival test).
  - `test_cold_notes_excludes_pinned` (Phase 4).
  - `test_cold_notes_excludes_recently_modified` (Phase 4).
  - `test_cold_notes_excludes_any_nonzero_signal` (Phase 4).

- **Unit tests in `vault/src/frontmatter/tests.rs`:**
  - `test_frontmatter_pinned_roundtrip` (Phase 3).
  - `test_frontmatter_pinned_missing_is_none` (Phase 3).

- **Oracle-level test (`oracle/src/server/tests.rs`, 2018+ submodule pattern):**
  - `test_knowledge_search_does_not_bump_access` (Phase 1). Construct an `Oracle` against a tempdir DB pre-seeded with one note; invoke `knowledge_search` with a query that matches; SELECT `search_hit_count` from `notes`, assert `== 0`; then invoke `note_read` on the same path; SELECT again, assert `== 1`. This is the regression guard for the no-feedback-loop rule; flag it as load-bearing in the test comment with a back-reference to `docs/scaling-roadmap.md` "high-BM25-scoring notes become immortal" passage.
  - As a belt-and-suspenders structural check, keep `bump_access` callers limited to `note_read` in `server.rs`; a one-line `cargo +nightly-... grep` check is not worth adding to CI, but a comment on `bump_access` listing its sole intended caller raises the friction for an accidental future caller.

- **Cortex integration test (`cortex/src/sweep/tests.rs`):**
  - `test_run_cold_renders_report_grouped_by_domain` (Phase 4).
  - `test_run_cold_atomic_write` (Phase 4).
  - `test_run_cold_pinned_excluded_counter` (Phase 4).

- **Daemon test (`cortex/src/daemon/tests.rs`):**
  - `test_daemon_cold_tick_fires` (Phase 5) - drive the daemon for a few seconds with a 1-second `cold_interval_secs`, assert the report file appears.

- **Snapshot fixture:** check in `cortex/src/sweep/fixtures/cold-notes-expected.md` and assert byte-exact equality with the rendered output for a fixed input. Regenerate via `cargo test -- --ignored` + manual review on intentional format changes.

### Rollout Plan

Ship all five phases back-to-back as a single `bump` (per `feedback_no_phase_gating`). After install:

1. `cortex sweep --cold` once manually on the real vault to surface the first report. Expect a large list; this is the audit moment for the long tail.
2. Watch the cortex daemon log for the first periodic tick (default: one week post-install). Confirm the report regenerates with updated counts.
3. After two or three weeks of natural use, start spot-checking notes the user has actually read - assert their rows have non-zero `search_hit_count` in the DB.

No flag-gating, no soak window. The columns already exist; the writers are additive.

### Operational Notes

- **First-run flood.** On a populated vault with no prior signals, the first cold report will surface every old, unlinked, untouched note. That can be hundreds of rows. The `limit: 500` config cap bounds the first report; subsequent reports shrink as the user pins/archives/deletes.
- **First-run, no oracle.** `cortex sweep --cold` on a system where oracle has never opened the search DB will either find an empty `notes` table or fail to open the DB entirely. Mitigation: `run_cold` logs at WARN ("notes table empty - oracle reindex has not yet run") and writes the empty-vault placeholder report. The user then starts oracle, waits for reindex, and re-runs. The cortex daemon path has the same exposure (the `select!` ticks are independent; cold can fire before any other tick has run); the daemon opens its DB connection at startup and exits with a fatal error if the file cannot be opened, so the daemon process won't start in the no-DB case at all.
- **Daemon ordering.** The cold tick can fire concurrently with a sweep tick (same `select!` loop). They take the same DB connection; SQLite serializes. WAL mode keeps reads from blocking writes. No explicit lock needed.
- **Watcher feedback.** `cortex sweep --cold` writes `system/views/cold-notes.md`. The default `ScanConfig::ignore` (`vault/src/config.rs:15-21`) excludes `.git`, `.obsidian`, `.cortex`, `assets`, `attachments`. **`system/` is NOT excluded**, so the cold-notes file *is* indexed as a note row, same as the existing `borg-intake.md`, `borg-ledger.md`, `borg-dlq.md` views established by [2026-05-11-borg-intake-log-and-dlq.md](2026-05-11-borg-intake-log-and-dlq.md). This is fine: the report becomes a row in the index, no functional impact. Oracle's VaultWatcher will fire a reindex on the mtime change; that reindex will call `recompute_inbound_link_counts()`, which is bounded by a single full pass and converges immediately. No feedback loop, just one extra recompute per cold-tick fire. Acceptable at the weekly cadence; if a future change makes cold runs much more frequent, add `system/views/` to the default ignore list at that point.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Test for "knowledge_search does not bump" gets deleted in a future refactor and the feedback-loop rule silently flips | Medium | High | Comment in the test body marking it as load-bearing; cross-reference the parent roadmap line about positive feedback loops; assertion message echoes the rule |
| `recompute_inbound_link_counts` becomes expensive at scale | Low | Medium | Defer; incremental fallback documented in the Performance section. The three-year envelope is bounded by `find_inbound_links` already running per-query on the same body content today |
| `pinned` field collides with an existing key in a user's note | Low | Low | Field is namespaced via the typed `Frontmatter` struct; arbitrary keys land in `extra`. If a user already used `pinned` for something else, parsing will type-error on non-bool values; the parser is lenient (`Option<bool>`) and falls back to `None` on type mismatch |
| First-run cold report is overwhelming and the user dismisses it | Medium | Medium | `limit: 500` config cap; report grouped by domain so reviewing one domain at a time is tractable; weekly cadence means the user has natural cadence to triage |
| Cold report regenerates while the user is mid-review (e.g. on Sunday) and clobbers a partially-checked file | Medium | High | The report is regenerated as a fresh file every tick; checkboxes the user ticks live in their Obsidian session, not on the file - reviewing means the user *acts* (archives, deletes, pins) and the next regeneration drops the row. If a user wants to mark "decided to leave," pinning is the mechanism. Document this in the report's preamble |
| User puts non-bool in `pinned:` (e.g. `pinned: "yes"`, `pinned: 1`) and is surprised it's not honored | Medium | Low | Strict bool-only parsing documented above; the field doc on `Frontmatter::pinned` explicitly names the accepted values. A future lint rule in `cortex lint` could flag non-bool values for the user; not blocking |
| Self-link inflates `inbound_link_count` and rescues a note from cold qualification | Low | Low | Recompute explicitly skips self-links (specified above). Add a unit test asserting a note containing `[[self]]` ends with `inbound_link_count == 0` |
| Daemon `cold_interval` of 1 week is too long / too short | Low | Low | Config-overridable; the default is a starting point. The fact that the report is idempotent + safe-to-regenerate means tuning later costs nothing |
| Inbound-link recompute and a concurrent `bump_access` block each other | Low | Low | Both writers live inside oracle. The MCP tool handlers acquire `self.db.lock()` (e.g. `oracle/src/server.rs:308` for `note_read`, `oracle/src/server.rs:451` for `reindex`); the autonomous VaultWatcher task acquires the same Arc<Mutex<>> via `db_handle.lock()` (`oracle/src/main.rs:89`). One shared mutex serializes everything before SQLite ever sees the conflict. Cortex's `note_embeddings` writes run in a separate process under WAL mode (one writer + many readers across processes). Worst case is a few-ms wait on the oracle mutex; verified empirically by the Phase 2 test that runs both paths back-to-back |

## Open Questions

- [ ] Should the report include a "warmest 20 notes" appendix as a sanity check, or stay focused on cold only? Lean toward cold-only for the MVP; revisit if the user asks.
- [ ] Should `cortex sweep --cold` also write a JSON snapshot alongside the markdown for future tooling? Cheap to add; defer until a consumer exists.
- [ ] Does the daemon need a `--cold-now` one-shot mode for manual regeneration outside the tick? `cortex sweep --cold` already covers that path; the daemon-only flag would be duplicative.
- [ ] What's the right `older-than-days` default? 180 is a guess. Validate against the first report - if every note older than 60 days is cold, tighten; if no note ever surfaces, loosen.
- [ ] What's the right `oracle.inbound_recompute_interval_secs` default? 600 (10 min) mirrors the embed-tick cadence. Measure scan wall time on the real vault; if it's >250ms at three-year scale, consider 1800 (30 min) or moving to incremental.

## Deferred follow-ons (not blocking Doc 3 ship)

- **Promotion UX:** pinning a note today requires manually editing `pinned: true` into frontmatter, which is friction during cold-report review. A follow-on adds `cortex pin <path>` / `cortex unpin <path>` subcommands plus an oracle MCP tool `pin_note` so reviewers can promote rows without leaving the report. Schema and parsing stay unchanged; the new surface is pure ergonomics. Track separately; Doc 3 ships without it.
- **`pinned` filter on `list_notes` and `domain_brief`:** the MCP browse tools could accept `pinned: bool` to project the L3 tier. Cheap; tracked as a follow-on once the pin subcommand lands and there is something to query for.
- **Signal-aware re-ranking in `knowledge_search`:** Doc 2 explicitly defers this to a future doc. The signals Doc 3 produces are the necessary substrate; deciding the weighting and exposing a `rerank: bool` mode is its own design discussion.

## References

- Parent roadmap: [docs/scaling-roadmap.md](../scaling-roadmap.md)
- Doc 1 (extractor contract + L2 distilled): [2026-05-16-extractor-contract-and-l2-summaries.md](2026-05-16-extractor-contract-and-l2-summaries.md)
- Doc 1 Phase 9 cleanup (added the three scaffolding signal columns): [2026-05-16-extractor-contract-l2-phase-9-cleanup.md](2026-05-16-extractor-contract-l2-phase-9-cleanup.md)
- Doc 2 (hybrid retrieval): [2026-05-16-hybrid-retrieval-fts5-vector-rrf.md](2026-05-16-hybrid-retrieval-fts5-vector-rrf.md)
- Doc 2 follow-on (candle backend): [2026-05-17-candle-embedding-backend.md](2026-05-17-candle-embedding-backend.md)
- Vault-watcher / reindex trigger: [2026-03-22-vault-watcher-oracle-reindex.md](2026-03-22-vault-watcher-oracle-reindex.md)
- Borg intake/DLQ system-view patterns (the established model for `system/views/*.md` reports): [2026-05-11-borg-intake-log-and-dlq.md](2026-05-11-borg-intake-log-and-dlq.md)
- The preservation regression test that locks the contract Doc 3 depends on: `vault/src/search.rs:2140-2192`
