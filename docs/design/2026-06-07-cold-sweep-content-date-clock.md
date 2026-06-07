# Design Document: Cold-Sweep Content-Date Clock

**Author:** Scott Idler
**Date:** 2026-06-07
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

The cortex cold-note sweep measures note age by **filesystem mtime**, but on this
actively-rewritten, Syncthing-migrated vault the mtime is always recent, so the
report is permanently empty even though notes carry content dates going back to
2023. This change repoints the age test at the `date:` content frontmatter -
already stored and indexed in the `notes` table - so the cold report surfaces
genuinely stale notes. To make the lexical date comparison safe, the indexer is
hardened to normalize `date:` to canonical `YYYY-MM-DD` (or empty when
unparseable) before it reaches the column. No schema migration (the `date`
column and `idx_notes_date` already exist), but the normalization does clean the
shared `date` column for every consumer, not just the cold query.

This ticket also rolls in the **root cause** of the dirty data the normalization
defends against: the frontmatter parser stringifies non-string YAML scalars via
a `{:?}` debug fallback (`vault/src/frontmatter.rs`), producing literals like
`"Number(2023)"` across eight fields. That is fixed at the source with a proper
scalar-to-string coercion, so every consumer of those fields - not just cold -
sees clean values.

## Problem Statement

### Background

`cortex sweep --cold` generates `system/views/cold-notes.md`, a janitorial
checklist of notes that look abandoned: old, never surfaced in search, never
accessed, no inbound links, not pinned. A reviewer triages each row
(archive / delete / leave / promote). The daemon regenerates it on a timer
(`cold_interval`) on the desk.lan host; Syncthing fans the file out to every
device.

The cold query lives in `vault/src/search.rs:916`:

```sql
SELECT path, title, domain, modified_at
FROM notes
WHERE search_hit_count = 0
  AND last_accessed_at IS NULL
  AND inbound_link_count = 0
  AND pinned = 0
  AND modified_at < ?1          -- ?1 = now - older_than_days*86400, unix seconds
ORDER BY modified_at ASC
LIMIT ?2
```

`modified_at` is populated from **filesystem mtime** at index time
(`search.rs:512`: `std::fs::metadata(&abs_path).modified()`), not from the
note's `date:` frontmatter.

### Problem

On this vault the filesystem clock and the content clock are decoupled:

- The vault was bulk-imported during migration, so every file's mtime is the
  import date, not its authored date.
- borg/cortex continuously rewrite notes (autotag, distill, summarize, embed,
  `audit --fix`, sweep). Every pipeline touch resets mtime to "now."

So `modified_at < (now - 180 days)` is satisfied by approximately nothing. The
report shows `total-surfaced: 0` permanently, even though notes carry
`date: 2023-01-13`. The "coldness" signal measures a clock the tooling keeps
permanently warm. The feature is effectively dead.

### Secondary problem: the frontmatter parser corrupts non-string scalars

`Frontmatter::from_value` (`vault/src/frontmatter.rs`) extracts eight scalar
string fields - `title`, `date`, `type`, `domain`, `origin`, `status`,
`source`, `creator` - with the same pattern:

```rust
date = match val {
    serde_yaml::Value::String(s) => Some(s),
    other => Some(format!("{other:?}")),   // bug: Debug-formats the enum
};
```

The `other` arm was meant as a defensive "don't drop a non-string value," but
`format!("{:?}")` renders the `serde_yaml::Value` **enum's Debug form**, not the
scalar's text. So `date: 2023` (a bare integer in YAML) is stored as the literal
string `"Number(2023)"`; `status: true` becomes `"Bool(true)"`. The common case
is unaffected (borg writes these as strings, and YAML-1.2 resolves bare
`2023-01-13` to a `String`), so this is latent - but it is the exact mechanism
that can poison the `date` column the cold sweep now depends on, and it
mis-renders any of the eight fields for anomalous input. `tags` (a sequence) and
`pinned` (strict bool, which already documents at line 119 why it avoids this
lenient branch) are not affected.

### Goals

- Measure cold-note age by the `date:` content frontmatter (when the note's
  content is from), not by filesystem mtime.
- Fix the frontmatter `{:?}` scalar-coercion bug at the source so all eight
  affected fields store clean values vault-wide.
- Surface genuinely old, unlinked, never-read, unpinned notes for triage.
- Zero schema change - reuse the existing `notes.date` TEXT column and its
  `idx_notes_date` index.
- Make the `date` column structurally trustworthy (canonical `YYYY-MM-DD` or
  empty) at index time, so a lexical range query over it is provably correct
  rather than reliant on an unenforced "users always type ISO" assumption.
- Keep the report's shape, frontmatter keys, and grouping identical so nothing
  downstream (the dashboard cross-link, the operator's habits) breaks.

### Non-Goals

- **Not** touching the other three cold predicates (`search_hit_count`,
  `last_accessed_at`, `inbound_link_count`, `pinned`). They stay as-is. (Note:
  the two oracle-tracked signals are mostly empty today, so the effective filter
  is "old content + no inbound links + not pinned" - acceptable and unchanged in
  spirit.)
- **Not** changing the `older_than_days` config default (180) or any config
  field.
- **Not** adding a schema migration. The `date` column already exists and is
  already populated.
- **Not** removing the `idx_notes_modified_at` index (other code reads
  `modified_at`; dropping it would be a schema migration for no benefit).
- **Not** deploying to client-only hosts. Only the desk.lan daemon generates
  this file.

## Proposed Solution

### Overview

Swap the age predicate from an integer-seconds mtime comparison to a lexical
ISO-date-string comparison against `notes.date`. A lexical `<` over `YYYY-MM-DD`
strings is a correct chronological comparison - **but only if the column is
guaranteed to hold canonical ISO strings.** It is not guaranteed today: the
frontmatter parser writes `date` from raw YAML via a `{:?}` debug fallback
(`vault/src/frontmatter.rs:71`), so a non-string scalar like `date: 2023`
becomes the literal `"Number(2023)"`, and a hand-typed `date: 05/12/2026` would
sort *older* than any real ISO date (`'0' < '2'` lexically) - a false positive
that flags a brand-new note as ancient. The common case is safe (borg writes
ISO strings, and serde_yaml's YAML-1.2 core schema resolves bare `2023-01-13`
back to a `String`), but the design must not *lean* on an invariant the column
doesn't enforce.

So this change has two parts:

1. **Harden the indexer (`index_one`)** to normalize `date` to canonical
   `YYYY-MM-DD` before writing the column: parse the first 10 chars as
   `%Y-%m-%d`; on success store the canonical form, on any failure store `''`
   (undated). This makes the column a structurally trustworthy ISO date, closing
   both the `"Number(2023)"` garbage path and the non-ISO false-positive path at
   the source. It is identity for the already-ISO common case.

2. **Repoint the cold query** at the now-trustworthy `date` column, with the
   empty/null guard.

**Explicit decision - undated notes are excluded from the cold sweep.** A note
whose `date` normalizes to `''` (absent, or unparseable like a slash format or a
Templater literal) is *not* surfaced. The alternative - treating `''` as
maximally old - would surface every metadata-less note as the *coldest* thing in
the vault, the opposite failure. Age cannot be inferred from a note with no
parseable date; cold-by-content-age is the wrong tool for it. Metadata-less
notes are the responsibility of the lint / quality sweep (which already flags
missing required frontmatter), not the cold sweep. In this vault `date:` is
schema-required, so the practical undated population is near-zero regardless.

### Architecture

Touchpoints in `vault/src/frontmatter.rs` and `vault/src/search.rs`, plus the
renderer and fixtures in `cortex/src/sweep.rs`:

A. `Frontmatter::from_value(...)` - replace the eight `other => format!("{:?}")`
   arms with a shared `scalar_to_string(serde_yaml::Value) -> Option<String>`
   helper that coerces a scalar to its natural text and drops non-scalars to
   `None`. Fixes the corruption at the source. (`pinned` stays strict bool;
   `tags` stays sequence-handling.)
0. `index_one(...)` - normalize `date` to canonical `YYYY-MM-DD` (or `''`)
   before the INSERT/UPDATE writes the column. New `normalize_date(&str) ->
   String` helper. This is the safety foundation the lexical compare rests on.
1. `cold_notes(&ColdQuery)` - the SELECT.
2. `count_pinned_excluded(...)` - the sibling COUNT that reports how many notes
   the pin-floor rescued; must use the identical age predicate or the two
   numbers describe different populations.
3. `cold_with_index(...)` in cortex - computes the threshold; emits a date
   string instead of unix seconds.
4. `render_cold_report_at(...)` - displays the content date instead of
   formatting an mtime timestamp; wording shifts from "last modified" to "dated".

The `date != '' AND date IS NOT NULL` guard in the query is belt-and-suspenders:
after normalization the column is either a canonical ISO date or `''`, so the
guard is what enforces the undated-exclusion decision above.

### Data Model

`ColdQuery` carries the age floor. Today it is `older_than: i64` (unix seconds).
It becomes a date-string floor:

```rust
/// Parameters for `SearchIndex::cold_notes`. `before_date` is an exclusive
/// ISO `YYYY-MM-DD` floor: a note qualifies when its `date:` frontmatter is
/// strictly older - lexically less than `before_date`. A note dated exactly
/// on the floor day does NOT qualify.
#[derive(Debug, Clone)]
pub struct ColdQuery {
    pub before_date: String,
    pub limit: u32,
}
```

(`Copy` is dropped - `String` is not `Copy`. The only construction site is
`cold_with_index`, so this is inert.)

`ColdNote` carries the per-row data the report renders. `modified_at: i64`
becomes `date: String`:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ColdNote {
    pub path: String,
    pub title: String,
    pub domain: String,
    pub date: String,   // was: modified_at: i64
}
```

`ColdNote` is consumed only by `cortex/src/sweep.rs` (renderer + tests), so the
field rename is fully contained.

### API Design

The new cold query:

```sql
SELECT path, title, domain, date
FROM notes
WHERE search_hit_count = 0
  AND last_accessed_at IS NULL
  AND inbound_link_count = 0
  AND pinned = 0
  AND date != ''
  AND date IS NOT NULL
  AND date < ?1                 -- ?1 = floor date string 'YYYY-MM-DD'
ORDER BY date ASC
LIMIT ?2
```

`count_pinned_excluded` takes the same `before_date: &str` and applies the
identical `date != '' AND date IS NOT NULL AND date < ?1` predicate with
`pinned = 1`.

Threshold computation in `cold_with_index`:

```rust
let floor = (chrono::Utc::now().date_naive()
    - chrono::Duration::days(cold.older_than_days as i64))
    .format("%Y-%m-%d")
    .to_string();
let query = ColdQuery { before_date: floor, limit: cold.limit };
```

Index-time normalization helper (`vault/src/search.rs`), called by `index_one`
on the value it currently passes straight through (`fm.date` /
`search.rs:611`):

```rust
/// Normalize a raw frontmatter `date:` to canonical `YYYY-MM-DD`. Returns ``
/// for anything that does not parse as a leading ISO date - absent, a bare
/// `Number(2023)` debug-string, a slash format, or a Templater literal. The
/// `notes.date` column is written exclusively through this, so downstream
/// lexical comparison is over guaranteed-canonical data (or the empty
/// sentinel, which every consumer treats as "undated").
fn normalize_date(raw: &str) -> String {
    let head = raw.get(..10).unwrap_or(raw);
    match chrono::NaiveDate::parse_from_str(head, "%Y-%m-%d") {
        Ok(d) => d.format("%Y-%m-%d").to_string(),
        Err(_) => String::new(),
    }
}
```

Both the INSERT and UPDATE arms of `index_one` write `normalize_date(date)`
instead of the raw `date` binding.

Frontmatter scalar coercion helper (`vault/src/frontmatter.rs`), replacing the
eight `other => Some(format!("{other:?}"))` arms with `field = scalar_to_string(val);`:

```rust
/// Coerce a YAML scalar to its natural string form for a string-typed
/// frontmatter field. A number/bool renders as its plain text; null and
/// non-scalar values (sequence/mapping/tagged) yield None. Never stores a
/// `{:?}` debug rendering of the `Value` enum (the `"Number(2023)"` bug).
fn scalar_to_string(val: serde_yaml::Value) -> Option<String> {
    match val {
        serde_yaml::Value::String(s) => Some(s),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::Null => None,
        serde_yaml::Value::Sequence(_)
        | serde_yaml::Value::Mapping(_)
        | serde_yaml::Value::Tagged(_) => None,
    }
}
```

The two fixes compose as defense-in-depth: the parser stops *creating* dirty
date strings, and `normalize_date` still rejects any that slip through (e.g. a
bare-year `date: 2023` now coerces to `"2023"`, which `normalize_date` then maps
to `''` for failing the full `%Y-%m-%d` parse - excluded, not mis-aged).

### Implementation Plan

#### Phase 1: Indexer normalization + query + types
**Model:** opus
- `vault/src/search.rs`: add `normalize_date(&str) -> String`; call it in both
  the INSERT and UPDATE arms of `index_one` so the `date` column is written
  canonical-or-empty.
- Change `ColdQuery` to `{ before_date: String, limit }`; change
  `ColdNote.modified_at: i64` to `date: String`.
- Rewrite `cold_notes` SELECT to filter and order by `date` with the empty/null
  guards.
- Rewrite `count_pinned_excluded` to take `before_date: &str` and use the same
  date predicate.

#### Phase 2: Cortex wiring + renderer
**Model:** opus
- `cortex/src/sweep.rs::cold_with_index`: compute the `before_date` floor string;
  build `ColdQuery` with it; pass the same floor to `count_pinned_excluded`.
- `render_cold_report_at`: render `row.date` directly (no timestamp formatting);
  change the per-row suffix from `last modified {date}` to `dated {date}`; keep
  all frontmatter keys and the intro/footer text otherwise unchanged.

#### Phase 3: Tests + fixture
**Model:** sonnet
- Update the three cold tests in `vault/src/search/tests.rs`
  (`cold_notes_returns_only_floor_satisfying_rows`, `cold_notes_orders_oldest_first`,
  `cold_notes_excludes_once_read_notes`) to seed meaningful `date:` values and
  assert against a date floor; add a case proving an empty-`date` row is excluded.
- Add `normalize_date` unit tests: canonical ISO passes through; ISO + time
  suffix keeps the date; `"Number(2023)"`, `"05/12/2026"`, a Templater literal,
  and `""` all normalize to `''`.
- Add an `index_one` test proving a non-ISO `date:` lands as `''` in the column
  (so it is excluded from cold rather than mis-aged).
- Update `cortex/src/sweep.rs` test helpers (`make_cold_note`,
  `snapshot_fixture_input`) to build `ColdNote { date }`.
- Regenerate `cortex/src/sweep/fixtures/cold-notes-expected.md` to match the new
  "dated {date}" wording.
- `otto ci` green.

#### Phase 4: Frontmatter scalar coercion (independent)
**Model:** sonnet
- `vault/src/frontmatter.rs`: add `scalar_to_string(serde_yaml::Value) ->
  Option<String>`; replace the eight `other => Some(format!("{other:?}"))` arms
  (`title`, `date`, `type`, `domain`, `origin`, `status`, `source`, `creator`)
  with `field = scalar_to_string(val);`. Leave `tags` and `pinned` untouched.
- Tests in `vault/src/frontmatter/tests.rs` (or the existing test module): a
  number coerces to its plain text (`date: 2023` -> `"2023"`, not
  `"Number(2023)"`); a bool coerces (`"true"`); null and a sequence value yield
  `None`; a normal string passes through.
- `otto ci` green.
- This phase has no ordering dependency on Phases 1-3 (`normalize_date` is robust
  to both the old and new parser output), so it may land first or last.

## Alternatives Considered

### Alternative 1: Add a `content_date_at INTEGER` column (unix seconds)
- **Description:** Parse `date:` into unix seconds at index time, store it in a
  *new* column, compare integers.
- **Pros:** Integer comparison is unambiguous; no reliance on lexical ordering.
- **Cons:** Requires a schema change + backfill - exactly the Rust schema
  migration the project rules forbid. The chosen design gets the same
  parse-at-index-time safety by normalizing *into the existing TEXT column*, so
  lexical compare over `YYYY-MM-DD` is then provably correct - without a new
  column.
- **Why not chosen:** New column = migration. Normalizing the existing `date`
  column buys the same correctness guarantee with no schema change.

### Alternative 2: Keep mtime but stop the pipeline from touching unchanged notes
- **Description:** Make borg/cortex skip rewrites when content is unchanged so
  mtime reflects real edits.
- **Pros:** mtime becomes meaningful for many features, not just cold.
- **Cons:** Enormous blast radius across every writer; the migration import would
  still leave every note's mtime at the import date, so cold stays broken for the
  legacy corpus regardless.
- **Why not chosen:** Wrong clock for "how old is this note's content." Content
  age is what the report claims to measure; `date:` is that clock.

### Alternative 3: Fix only the `date` arm of the frontmatter parser
- **Description:** Narrow the coercion fix to just `date`, leaving the other
  seven fields on the `{:?}` fallback.
- **Pros:** Smaller diff; only touches what the cold sweep strictly needs.
- **Cons:** Leaves the identical latent bug live in `title`/`type`/`domain`/
  `origin`/`status`/`source`/`creator`; the next field to hit anomalous input
  reintroduces the same surprise. The fields share one root cause.
- **Why not chosen:** A shared `scalar_to_string` helper fixes all eight at once
  for the same effort; fixing one arm is a partial fix of a single defect.

### Alternative 4: Kill the feature
- **Description:** Remove the cold sweep entirely.
- **Pros:** Less code.
- **Cons:** The janitorial function is legitimately wanted; the bug is a wrong
  clock, not a wrong idea.
- **Why not chosen:** User chose to fix, not remove.

## Technical Considerations

### Dependencies
- `chrono` (already a dependency) for the floor-date computation.
- No new crates.

### Performance
- `idx_notes_date` already exists (`search.rs:282`), so the `date < ?1`
  filter + `ORDER BY date ASC` stays index-backed - same query *shape* as the
  mtime query it replaces (filter an indexed column, then check the non-indexed
  signal columns per candidate).
- With `ORDER BY date ASC` + `LIMIT`, SQLite walks `idx_notes_date` in order and
  stops once `LIMIT` rows pass the signal filters; it does not scan the full
  old-date range. So the only real change vs. today is that *more candidates now
  qualify* (the point of the fix), and that set is `LIMIT`-capped. No new
  random-read blow-up.
- `normalize_date` runs once per note per index pass - a single `parse_from_str`
  on 10 chars, negligible against the existing per-note indexing cost.

### Security
- None. Read-only query over local SQLite; no new inputs.

### Testing Strategy
- Unit: `normalize_date` cases (ISO passthrough, ISO+time-suffix, `"Number(2023)"`,
  `"05/12/2026"`, Templater literal, `""` → `''`).
- Unit: the three rewritten `cold_notes` tests + a new empty-`date`-excluded
  case + an `index_one` test proving a non-ISO `date:` lands as `''`, all in
  `vault/src/search/tests.rs`.
- Unit: `scalar_to_string` / `Frontmatter::from_value` cases - number coerces to
  plain text (`date: 2023` -> `"2023"`, not `"Number(2023)"`), bool coerces,
  null and sequence yield `None`, string passes through.
- Snapshot: byte-exact `cold-notes-expected.md` fixture via the existing
  `render_cold_report_at` test with a fixed `now`.
- Manual smoke on desk.lan after deploy: run `sb cortex sweep --cold` and
  confirm `total-surfaced` is now non-zero and rows show real 2023-era dates.

### Rollout Plan
- cortex-only change. Build + test on the laptop (this repo host).
- Deploy on the **desk.lan daemon host** only: `otto install` then
  `systemctl --user restart cortex`. **Not** `otto deploy` (that re-signs the
  Firefox extension via AMO - unnecessary here).
- The next `cold_interval` tick regenerates `cold-notes.md` with real data;
  Syncthing propagates it to other devices. No per-host action elsewhere.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Non-ISO / slash-format `date:` (`05/12/2026`) lexically sorts *older* than the floor → false-positive "cold" on a new note | Med | High (pre-fix) | `normalize_date` parses leading 10 chars as `%Y-%m-%d`; anything non-ISO becomes `''` → excluded, never mis-aged. This is the primary reason normalization is in the design |
| Non-string YAML scalar (`date: 2023` → `"Number(2023)"` via `frontmatter.rs:71`) indexes as infinitely new → never goes cold | Low | Med | Same: `normalize_date` fails to parse the debug-string and stores `''`; the note becomes undated (lint's job), not silently warm |
| `date:` carries a time suffix (`2023-01-13T..`) | Low | Low | `normalize_date` parses the 10-char head and canonicalizes; comparison is over clean `YYYY-MM-DD` |
| Report suddenly surfaces a large backlog of legacy notes | Med | Low | `limit` (config) already caps rows; this is the intended behavior - the backlog is the point |
| `count_pinned_excluded` and `cold_notes` drift to different predicates | Low | Med | Both edited in the same phase against the same floor string; covered by the pin-floor test |
| Normalization writes `''` over a previously-garbage `date` for non-cold consumers (e.g. dashboard sort) | Low | Low | Net improvement - garbage like `"Number(2023)"` sorting as text was already wrong; `''` is an honest "undated". Common (ISO) case is identity |
| `scalar_to_string` changes parser output for all eight fields vault-wide (not just cold) | Low | Low | Only changes the *anomalous* path (non-string scalars); the string common case is byte-identical. New output (`"2023"`) is strictly more correct than old (`"Number(2023)"`). Covered by parser unit tests; `pinned`/`tags` untouched |
| `scalar_to_string` non-exhaustive over future `serde_yaml::Value` variants | Low | Low | Match is exhaustive over current variants (String/Number/Bool/Null/Sequence/Mapping/Tagged); a new upstream variant is a compile error, caught by CI, not a silent fallthrough |

## Open Questions
- [x] Per-row label wording. **Resolved:** use `dated {date}`. The displayed
      value is now the `date:` content date, not an mtime, so "last modified"
      would be a lie. Trivially reversible if the operator prefers other phrasing.
- [x] Date normalization location. **Resolved:** `index_one` normalizes the DB
      column (the raw-frontmatter → DB boundary), AND the shared `frontmatter.rs`
      parser is fixed at the source via `scalar_to_string` (Phase 4). The
      `{:?}` debug fallback - originally flagged as out of scope - is now rolled
      into this ticket per operator request, since it is the root cause of the
      dirty data normalization defends against.
- [x] Round-2 Architect consensus (reached): (a) common ISO case is sound today,
      normalization is hardening not a shipped-bug fix; (b) undated-exclusion is
      correct with `cortex::quality` owning metadata-less notes; (c) no
      performance regression given the index-walk + `LIMIT` early-exit.
- [ ] **To Architect (round 3, consensus pending):** sign-off on the rolled-in
      Phase 4 frontmatter `scalar_to_string` fix - specifically that coercing all
      eight fields (vs. only `date`) is the right call and the
      `Number→to_string`/`Null→None`/non-scalar→`None` mapping has no surprising
      consumer downstream.

## References
- `vault/src/search.rs:916` - `cold_notes` query
- `vault/src/search.rs:611` - `date` binding passed to `index_one` (normalization site)
- `vault/src/search.rs:512` - mtime population at index time
- `vault/src/search.rs:282` - `idx_notes_date` (already present)
- `vault/src/frontmatter.rs:71` - `date` parsed via `{:?}` debug fallback (garbage for non-string scalars)
- `cortex/src/sweep.rs:361` - `render_cold_report_at`
- `cortex/src/sweep/fixtures/cold-notes-expected.md` - snapshot fixture
- `~/.claude` rules: no Rust schema migrations; extension-resign avoidance on deploy
