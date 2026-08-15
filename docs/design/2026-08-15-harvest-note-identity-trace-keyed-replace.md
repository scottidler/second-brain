# Design Document: harvest note identity -- trace-keyed replace

**Author:** Scott Idler (via agent)
**Date:** 2026-08-15
**Status:** Draft
**Review Passes Completed:** review-panel rounds 1-3 (Architect + Staff Engineer, cross-model). Round 1: 5 must-fix, folded. Round 2: 7 must-fix, folded. Round 3 (delta review of the un-deferrals): 2 must-fix + 3 approved-with-edits, folded. Nothing deferred; no open questions.

## Summary

Every `sb borg replay` of a harvest session trace writes a NEW vault note
instead of replacing the one that trace already produced. The note's filename
stem is the model-generated `slug` from the distill pass, and the model does not
produce the same slug twice, so "overwrite the same path" silently becomes
"write a new path". Trace `hv-e5d240` has 15 notes in the vault from a single
session. Across the whole vault: 272 harvest notes for 208 distinct primary
sessions, 22 sessions owning more than one note, 14 of those from a single
trace.

The fix does not need a new identifier. Three stable identity anchors are
already written to every harvest note (`trace:`, `source: clyde://<primary-id>`,
`cortex-session-ids:`) and the trace's landed note path is already recorded in
the receipts DB. This makes the publish path RESOLVE the prior note for a trace
and write to that exact path, preserving cortex-owned and user-owned
frontmatter. It also persists the input transcript hash on the note, so the
vault -- not just a non-fsynced JSON file -- can answer "have I already
published this?" across a crash.

Scope note: this doc carries **no deferrals**. Four defects found during review
that could have been split out (the cortex `resolve_collision` loop bug, the
intra-export duplicate candidate, the missing follow-up back-link, and replay not
taking the harvest lock) are all phases here.

## Problem Statement

### Background

Harvest publishes a session note through `pipeline::session::process_session_inner`
(`borg/src/pipeline/session.rs`). Note identity has three layers today:

- **Durable identity:** the primary clyde session id, in `source:` and as the
  watermark key (`borg/src/harvest/watermark.rs:35`, `PublishedEntry.note_path`).
- **Run identity:** the trace id, minted per candidate at selection time,
  reused verbatim by `sb borg replay`, and carried in `trace:` frontmatter and
  the receipts DB (which records the landed `note_path`, `borg/src/receipts.rs:445`).
- **Addressing:** the filename, derived from the distiller's content slug
  (`session.rs:238`), disambiguated on collision by a primary-id suffix
  (`session.rs:48`).

Layers 1 and 2 are stable. Layer 3 is model output.

### Problem

`replay_session_stage2` (`borg/src/replay.rs:441-443`) calls the pipeline with
`force=true` and this comment:

> `force=true`: the note already landed, so re-derivation overwrites the same
> path in place rather than minting a uniquified sibling.

`force` does not do that. In `harvest_publish_path` (`session.rs:48-55`) it only
means "skip the `--<id8>` collision suffix and use the bare `{slug_stem}.md`".
The stem is recomputed from the new distill pass, so `force=true` overwrites
`<new-slug>.md`, a file that has never existed.

### Prior attempts and where each one failed

This problem was anticipated. FOUR separate mechanisms were designed to prevent
it, all shipped, all defeated. Reviewers should assume the obvious fix has been
tried, and check this table first.

| # | Mechanism | Where it lives | What it assumed | Why it failed |
|---|---|---|---|---|
| 1 | `force=true` on replay means "overwrite in place" | `borg/src/replay.rs:441-443` (`d150970`, 2026-07-20) | The filename stem comes from stable export metadata, so re-deriving a trace yields the same path | TRUE when written (stem was clyde's session *title*). Falsified 4 days later by attempt 2, which nobody re-checked against this caller |
| 2 | Persist the chosen stem as frontmatter `slug:` "for cross-harvest stability" | `borg/src/pipeline/session.rs:244` (`dcd370e`, 2026-07-24 07:15) | Writing the stem somewhere durable makes it stable | Write-only. `session.rs:244` is the ONLY writer in the workspace and no borg path reads it back. A field that is written and never read cannot stabilise anything |
| 3 | Primary-id collision suffix + "collision = association", residual to cortex | `session.rs:48-55` (`2a6d6c3`, 2026-07-24 07:52) and `cortex/src/association.rs:66` | Two notes that are really the same thing share a slug, so grouping by slug finds them | Partially inverted. `2026-07-24-cortex-association-sweep.md` scoped the mirror image (different sessions, same slug). It catches the 9 forks sharing a title-fallback slug and is blind to the other 6 |
| 4 | "Same source URL means reingest replacement -- overwrite, don't create `-2`" | `cortex/src/classify.rs:983-1012` (`resolve_collision`), call site `:556` | A same-`source:` note at the destination is a replacement, not a collision | **Reached, and failed inside its own loop** -- see below. The same-source check is applied ONLY to the base path and never re-applied to the numeric candidates (`classify.rs:1004-1007`) |

**Attempt 4's real mechanism, verified in the live vault.** I previously wrote
that attempt 4's guard "is never reached". That is wrong for 11 of the 15 forks.
`notes/review-ci-workflow-security-changes.md` and `-2`, `-3`, `-4`, `-6` belong
to five DIFFERENT clyde sessions (`hv-353663`/`eb65b08e`, `hv-95813b`/`1a31236d`,
`hv-efc530`/`ee0b75a3`, `hv-067d05`/`bc5c376c`, `hv-e5c476`/`7eff9ae9`) -- the
generic title-fallback slug collides ACROSS sessions. So the promote of an
`hv-e5d240` note did collide at the base path, `existing_note_has_source`
correctly answered "different source", and then:

```rust
for i in 2..100 {
    let candidate = parent.join(format!("{stem}-{i}.{ext}"));
    if !candidate.exists() { return candidate; }
}
```

walked straight past `-5`, `-7` .. `-14`, every one of which carries
`source: clyde://8d6b6ef3...`, because the same-source check is never re-applied
to a numeric candidate. That is a real cortex bug and Phase 5 fixes it.

Root cause of the whole class, in one sentence: a naming change flipped the
filename stem from stable metadata to model output, and neither the caller that
had encoded "the stem is stable" as an invariant nor the guards keyed on path or
slug equality was re-checked.

Four process signals this design must obey, because it is vulnerable to all of them:

- **Write-only fields are not mitigations.** `slug:` was cited as the stability
  mechanism while having no reader. Every field this doc adds names its reader.
- **Value stability is a contract, not a property of the producing code.**
  Attempt 1's invariant lived only in a comment, so attempt 2 could not violate
  it visibly. Phase 3 carries executable regression guards.
- **A guard that fires only on exact equality of a derived value is not a
  guard.** Attempts 3 and 4 both key on the filename. Resolution here keys on
  identity anchors only.
- **A guard applied at one point in a loop is not applied in the loop.** Attempt
  4 checked the base and not the candidates.

### Evidence (live vault, re-verified 2026-08-15)

All 15 `hv-e5d240` notes carry identical identity frontmatter (`source:`,
`trace:`, `cortex-session-ids:`) and 15 different filenames, across **6**
distinct `slug:` values plus one legacy note with no `slug:` key:

```
slug: review-ci-workflow-security-changes                x9   (files ...-7.md .. ...-15.md)
slug: ci-yml-public-repo-reusable-workflow-migration     x1
slug: ci-yml-public-reusable-workflow-migration          x1
slug: clyde-ci-public-reusable-workflow-migration        x1
slug: clyde-ci-public-reusable-workflow-migration-review x1
slug: clyde-ci-yml-public-reusable-workflow-migration    x1
(no slug: key)                                           x1   notes/review-ci-workflow-security-changes-5.md
```

`cortex::association::group_by_slug` therefore sees exactly ONE group of nine and
drops six as singletons/legacy (it keeps groups of >= 2, `association.rs:66-77`).

**Quality is anti-correlated with age**, which decides the survivor rule below.
All four notes ingested 2026-07-24 (`-5`, `-7`, `-8`, `-9`) carry
`[missing-summary]` / `[yaml-parse-error]` / `cortex-needs-review`; six of the
eleven ingested 2026-08-15 are clean. `ingested:` is a DATE, not a timestamp, so
those cohorts are a 4-way and an 11-way tie.

**Compounding factor.** Notes publish into `inbox/` (`session.rs:267`) and cortex
moves them to `notes/`. All 15 now live under `notes/`, while the receipts row
for `hv-e5d240` still points at `inbox/review-ci-workflow-security-changes-5.md`.
Path staleness is the common case.

### Goals

- One trace produces exactly one note, no matter how many times it is replayed.
- Replacement preserves cortex-owned frontmatter and user-owned state (`status:`).
- Replacement preserves the existing filename, so wikilinks keep resolving.
- Resolution survives cortex having moved the note between directories.
- A genuine `FollowUp`, and `--force`, still fork a new note. Notes are immutable
  once published; only a replay of the SAME trace replaces.
- A crash between "note landed" and "watermark saved" does not fork on the next run.
- Retire the surplus notes by an identity rule, not a similarity rule, without
  breaking a single inbound wikilink.

### Non-Goals

- Changing the filename scheme to a session id or UUID (Alternative 1).
- Making the model's slug deterministic (Alternative 5).
- Cross-session near-duplicate merging. That stays `cortex::association`'s job.

## Proposed Solution

### Overview

Resolve before you name: **if this trace already has a landed note, write to that
note's current path.** Otherwise fall through to today's slug-derived path.

### Architecture

New module `borg::harvest::identity`:

```rust
/// Why we are publishing. Decides which resolution branches are legal --
/// `FollowUp` is never a replace, because notes are immutable once published
/// (`harvest/publish.rs:156-162`).
pub enum ResolveIntent {
    /// `sb borg replay <trace>`: the trace is authoritative, steps 1-2 only.
    Replay,
    /// Harvest planner said `NewNote`: steps 1-2, plus the crash-recovery
    /// fallback (step 3).
    NewNote,
    /// Harvest planner said `FollowUp`, or `--force`: never resolves.
    FollowUp,
}

/// Resolve the note this publish should REPLACE, if any. Absolute path or None.
pub fn resolve_prior_note(
    conn: &Connection,
    vault_root: &Path,
    trace_id: &str,
    primary_source: &str,   // `clyde://<primary-id>`
    body_hash: &str,
    intent: ResolveIntent,
) -> Result<Option<PathBuf>>
```

`ResolveIntent::FollowUp` returns `None` immediately. This is load-bearing:
`classify_reappearance` returns `FollowUp` **unconditionally when `force` is
set, two lines before it examines the body hash** (`watermark.rs:257-260`), so a
`sb borg harvest --force` over an unchanged session produces a decision whose
`source:` and `harvest-body-hash:` both match the existing note. Without the
intent gate, step 3 would match and overwrite, directly contradicting
`publish.rs:156-162` ("a brand new note, never an overwrite of the prior one").

Resolution order:

1. **Receipts fast path.** `receipts::get(conn, trace_id)` gives the recorded
   `note_path`. Accept only if the file exists and passes the confirmation guard.
2. **Vault index.** A `trace -> Vec<PathBuf>` index (plus a `stem -> Vec<PathBuf>`
   index for the tombstone follower) built from `vault::note::scan_vault`, paths
   normalized to ABSOLUTE (`vault::note::Note.path` is vault-relative,
   `vault/src/note.rs:13`, while receipts and `config.inbox_dir()` are absolute).
   Same guard.
3. **Crash-recovery fallback** (`ResolveIntent::NewNote` only). No trace match,
   but a note exists with the same `source:` AND the same `harvest-body-hash:`.
   That is the same transcript already published under a trace whose watermark
   entry was lost. Replace it. Both keys are required; a note lacking the hash is
   not eligible.
4. **None.** Publish new.

**Confirmation guard** (steps 1-2), all three conditions:
`trace:` equals `trace_id`, AND `source:` equals `primary_source`, AND
`harvest-body-hash:` either equals `body_hash` or is ABSENT (legacy notes predate
the key and cannot be compared; trace+source carries them). The `source:` term
makes a 24-bit trace collision non-destructive; the hash term is what stops a
same-source, same-trace-collision mismatch from overwriting.

**Index freshness: self-insert on write, no TTL.** The index is memoized for the
process lifetime, and every successful publish inserts `(trace, absolute path)`
into it immediately. Within one process borg is the sole creator of harvest
notes, so the in-memory view is exact by construction, O(1), and clock-free.
External staleness (a cortex move between runs) is covered by the receipts fast
path -- which `update_note_path` finally makes repairable -- plus the re-stat
before write.

A time-based rebuild was specced and is **withdrawn**: the vault is 3,141
markdown files / 10.0 MB (measured) and `parse_note` reads each file whole
(`vault/src/note.rs:22-34`), while EVERY nightly `NewNote` publish is a step-1
miss by construction -- the door writes a `received` receipts row with no
`note_path` before dispatch (`publish.rs:140-148`). A rebuild-on-miss policy,
whether TTL-gated or not, is therefore ~140 full vault scans a night, which is
exactly the cost Alternative 4 is rejected for. If a daemon-side session ingest
ever lands, that phase adds explicit invalidation, for a caller that exists.

**Tombstones are followed, never overwritten.** `cortex::association`'s merge
executor strips `slug:` and inserts `superseded-by:` holding a bare filename STEM
(`association.rs:951-962`), leaving `trace:` intact. Rule: if a resolved note
carries `superseded-by:`, follow it via the stem index, transitively, with a
visited set and a depth bound of 8. Refuse (WARN, return `None`, publish new) if:
the stem is ambiguous (**33 duplicate filename stems exist vault-wide**, 26
excluding `system/`; re-counted 2026-08-15 at the ready-to-build gate -- the
earlier figure of 32 was stale, the `system/` weekly-note series drifted by one),
the stem resolves to nothing, a cycle is detected, or the depth bound trips.
Never rewrite a live body onto a tombstone. The multi-match tie-break skips
tombstones entirely.

**Receipts write-back.** `receipts::mark_succeeded` carries
`WHERE trace_id=? AND status='received'` (`receipts.rs:454`) so it cannot repair
a terminal row, and `replay_session_stage2` bypasses `process_content`'s
receipts chokepoint (`replay.rs:443` vs `pipeline.rs:404`) so a replay writes no
receipt at all. Phase 1 adds terminal-state-safe
`receipts::update_note_path(conn, trace_id, note_path)`; Phase 3 calls it on
every session publish including replay. Verified during review: nothing in
borg/cortex/oracle/sb/vault depends on `note_path` having a single writer.

### Data Model

- **`harvest-body-hash:`** (new) -- SHA-256 of the canonical input transcript,
  already computed at `harvest/publish.rs:133` and today written only to the
  watermark JSON. Readers: the confirmation guard, resolution step 3, and Phase
  6's `--rebuild-state`. Absent from all 272 existing notes, so step 3 is
  forward-only until Phase 6's backfill runs.
- **`PublishedEntry.trace`** (new, serde-default) -- lets a follow-up resolve its
  prior note through the same resolver instead of trusting a stale `note_path`.
- **`follows:`** (new, Phase 4) -- the wikilink target of the note this follow-up
  continues. One-way by decision: a reciprocal `followed-by:` would require
  writing the PRIOR note, breaking "notes are immutable once published"
  (`publish.rs:156-162`) and dragging multi-file failure semantics into the phase.
  **Readers, named per the write-only-field rule:** the body wikilink is what
  Obsidian's backlink pane resolves, and the frontmatter key is the queryable
  form for Dataview / `.base` views and oracle's frontmatter passthrough. The key
  is the DURABLE carrier -- a replace rewrites the body from the distiller
  (`markdown.rs:217-224`), so the body link must be re-emitted from `follows:` on
  every render rather than being the source of truth.
- **`slug:`** stays borg-owned and is set to the RESOLVED filename stem on a
  replace, so it stops lying about the file it names. NOT justified by
  "un-blinds `group_by_slug`" -- that claim was false and is withdrawn. Phase 5
  replaces slug-grouping with identity-grouping so the invariant costs nothing.
  Confirmed safe: `slug:` is written only at render time (`session.rs:238-244`),
  never in bulk, so historical notes keep theirs until individually replayed.

**Frontmatter merge on replace: borg rewrites the keys it owns and carries every
other key forward verbatim.** The borg-owned set is DERIVED FROM THE WRITER, not
hand-listed, so it cannot drift the way `slug:` did: exactly the keys
`markdown::render_note` emits (`borg/src/markdown.rs:140-215`) -- `title`, `date`,
`ingested`, `source`, `type`, `origin`, `status`, `method`, `trace`, `tags`, and
`creator` when `frontmatter.default_creator` is non-empty (`markdown.rs:186`) --
plus session additions `repo`, `trace-expires` (`session.rs:221-229`), `slug`
(`:244`), `harvest-body-hash` (new), `follows` (new), plus distiller additions
`distilled`, `distilled-extractor`, `cortex-session-msg-count`,
`cortex-session-ids` (`distillers/src/render.rs:93,177`). Everything else
(`domain`, `cortex-classified*`, `cortex-quality*`, `superseded-by`, user keys)
is preserved. A unit test asserts the policy's key set matches the writer's, so
adding a key to `render_note` without updating the policy fails CI.

**`status:` is a deliberate ownership change**, not a description of current
behavior: `session.rs:262` hardcodes `Status::Unread` and `markdown.rs:151`
writes it unconditionally. Phase 3 reads the existing note's `status:` and feeds
it back so a replay does not reset a note the user marked `read`.

`tags` stays borg-owned and rewritten: the distill pass is the tag source and a
replay is a deliberate re-derivation. Manual tag edits are lost on replay (Risks).

### API Design

- `identity::resolve_prior_note(...)` and `identity::ResolveIntent` as above.
- `receipts::update_note_path(conn, trace_id, note_path) -> Result<bool>` -- new,
  terminal-state-safe, the only other writer of `note_path`.
- `harvest_publish_path` keeps its signature and role (new-note naming); the
  prior-note branch short-circuits before it.
- Phase 6 adds `sb borg dedupe-sessions [--apply] [--purge]`.

### Implementation Plan

#### Phase 1: resolution primitives (sonnet)

`identity::resolve_prior_note` with all four branches and the three intents, the
trace and stem indexes (memoized for the process lifetime, self-insert on
write, no TTL - see Architecture), the three-term confirmation
guard, absolute-path normalization, the tombstone follower, re-stat before
return, and the tombstone-skipping multi-match tie-break. Plus
`receipts::update_note_path`. Plus widen `vault::trace::generate` from 24 to 32
bits. Exactly ONE length assumption exists in the workspace (verified by grep for
slices, `chars().take`, `len()`, and shape regexes): the test regex at
`vault/src/trace/tests.rs:6`. Everything else treats a trace as an opaque string
-- staging dir components, the receipts TEXT key and its exact-equality lookups,
`sb borg log --trace`, `sb borg replay`, frontmatter, the search schema, and
cortex's staged-transcript join. Complete edit list: `trace.rs:36` (`{:08x}`,
mask `0xFFFF_FFFF`), the comments at `trace.rs:12` and `:31`, the collision
paragraph at `trace.rs:14-21`, and the regex at `trace/tests.rs:6`. Mixed-width
ids coexist as opaque strings.

**Widening is a delay, not an elimination, and does NOT retire the three-term
guard.** It moves the birthday bound from ~4,800 to ~77,000 ids per prefix: at
140 publishes a night, roughly one year to roughly fifteen. The guard remains the
thing that makes a collision non-destructive.

No publish behavior change in this phase.

Acceptance:
- [ ] Unit test per branch: receipts hit; receipts-stale -> vault hit; step 3 hit
      by (`source`, `harvest-body-hash`); miss -> `None`.
- [ ] `ResolveIntent::FollowUp` returns `None` even when every key matches.
- [ ] Guard rejects on `trace:` match with mismatched `source:`, and on mismatched
      `harvest-body-hash:`; ACCEPTS when the hash key is absent (legacy).
- [ ] Tombstone followed to its survivor; ambiguous stem, missing stem, cycle,
      and depth-bound-exceeded each return `None` with a WARN.
- [ ] Every returned path is absolute; a stale index entry fails re-stat and
      falls through.
- [ ] `update_note_path` updates a `succeeded` row; returns `false` for absent trace.
- [ ] Existing 6-hex trace ids still resolve after the widening.

#### Phase 2: harvest-run integrity (sonnet)

- `WatermarkState::save` uses `vault::note::write_atomic` (`note.rs:112-128`)
  instead of `fs::write` + `rename` (`watermark.rs:95-98`), which fsyncs neither
  the temp file nor the parent directory. Today the note survives power loss and
  the record that it exists does not: durability is inverted.
- Save after each published thread, persisting `published` ONLY. The cursor
  advance stays in `apply_plan_to_state` at end of run (`harvest.rs:347-354`), so
  a mid-run crash never steps the cursor past unprocessed candidates.
- Handle a duplicate `session_id` in one export (`harvest.rs:160-171`) by FAILING
  CLOSED, not by merging. clyde's schema declares `session_id TEXT NOT NULL
  UNIQUE` (`tatari-tv/clyde/sessions/src/db.rs:125`, verified) and the export
  selects from `sessions` with no fan-out join, so a well-formed export CANNOT
  contain the same id twice. Rule: byte-identical duplicates collapse with a
  debug log; the same `session_id` with ANY differing content **fails the run
  loudly** as an export-contract violation. A "keep the record with the most
  messages" rule was specced and is withdrawn -- it invents a merge policy for a
  case the contract forbids, and silently discards data while masking an upstream
  export bug. This matters because today `trace_by_id` overwrites by session id
  while `selected` keeps both records; after Phase 3 the second would resolve the
  first by trace and OVERWRITE it, turning a duplicate-note bug into a
  silent-overwrite bug.
- `sb borg replay` acquires `watermark::acquire_lock` (`harvest.rs:476`) for
  session traces only (a URL replay has no business taking it). This does NOT
  block manual replays: `acquire_lock` is `try_lock_exclusive` and returns
  `HarvestLockHeld` immediately (`watermark.rs:143-148`), so a replay during the
  nightly run fails instantly with the lock path named, rather than waiting or
  racing. A per-trace lock would not work: the nightly side takes THIS lock
  (`harvest.rs:476`), and a lock only serializes when both sides take the same one.
- Add `PublishedEntry.trace` (serde-default) for Phase 4.

Acceptance:
- [ ] Killing the process between two threads leaves the first thread's
      `published` entry on disk AND the cursor unadvanced.
- [ ] A `save` is durable across a simulated crash (fsync path exercised).
- [ ] An export containing one session twice with byte-identical records yields
      one candidate and a debug log; with ANY differing field it fails the run
      with an export-contract error naming the session id.
- [ ] Replay during a held harvest lock fails with `HarvestLockHeld`, not a race.

#### Phase 3: wire replace-in-place (opus)

`process_session_inner` calls `resolve_prior_note` with the intent threaded from
the harvest decision (or `Replay` from `replay_session_stage2`); on a hit it
writes the resolved path, applies the writer-derived merge, preserves `status:`,
sets `slug:` to the resolved stem, writes `harvest-body-hash:`, and calls
`receipts::update_note_path`. On a miss the existing path logic is unchanged.

Acceptance:
- [ ] Replay the same trace three times -> exactly one note, same path each time.
- [ ] `cortex-classified`, `cortex-quality*`, and a user-set `status: read`
      survive all three replays.
- [ ] Filename unchanged when the model emits a different slug; `slug:` equals
      the filename stem afterwards.
- [ ] A note moved `inbox/` -> `notes/` is resolved and its receipts row updated.
- [ ] A `FollowUp` and a `--force` re-harvest each produce a second, distinct note.
- [ ] A trace whose note was deleted republishes cleanly as new.

**Regression guards (required, not optional).** Prior attempt 1 was killed by a
change in a different file that no test could notice.
- `borg/tests/replay_lands_same_note.rs`: publish a trace, re-publish the SAME
  trace with a deliberately different injected slug, assert exactly one file for
  that trace. Must fail if resolution is stubbed out.
- `borg/tests/body_hash_agrees_across_paths.rs`: the live publish path hashes
  freshly-fetched member bodies (`publish.rs:132-133`) while replay hashes the
  staged `body.txt` (`replay.rs:420`). They agree only because staging wrote
  those bytes; nothing asserts it, and a change to `thread_body_text` would
  silently kill step 3 with no test failing -- prior attempt 1's exact failure
  mode. Assert the two agree byte-for-byte.

#### Phase 4: follow-up back-link (sonnet)

`watermark.rs:32` specs it, `ThreadDecision.decision` already carries the prior
`PublishedEntry`, and `harvest/publish.rs` never passes it into the pipeline, so
follow-ups land unlinked. Thread it through to `ContentKind::Session` and emit
`follows:` plus a body wikilink. The prior note's path is subject to the same
cortex-move staleness, so it is resolved through `resolve_prior_note` using
`PublishedEntry.trace` (Phase 2), never used raw.

Acceptance:
- [ ] A follow-up note carries `follows:` pointing at the prior note's CURRENT
      path, including when cortex has moved it.
- [ ] An unresolvable prior note omits `follows:` and WARNs; it never blocks the
      publish.

#### Phase 5: cortex fixes (sonnet)

- `group_by_session_identity`, keyed on `trace:`, falling back to
  `cortex-session-ids` overlap ONLY when `trace:` is absent (legacy). Never group
  two notes carrying different non-empty traces: that is the follow-up case.
  Carry over `group_by_slug`'s `superseded-by` skip guard (`association.rs:63`)
  verbatim, or absorbed notes re-group and re-merge every daemon tick.
- Fix `resolve_collision` (`classify.rs:1004-1007`): re-apply
  `existing_note_has_source` to each numeric candidate, not just the base. This
  is prior attempt 4, repaired.

Acceptance:
- [ ] Fixture built from the real `hv-e5d240` cohort: all 15 group together;
      the five same-title notes from OTHER sessions do not join them.
- [ ] Two different-trace notes sharing a primary session id do NOT group.
- [ ] A `superseded-by:` note is never a group member.
- [ ] A promote whose base collides with a different-source note and whose `-N`
      candidate matches the SAME source overwrites the candidate rather than
      minting `-N+1`.

#### Phase 6: retire the existing forks (sonnet + operator-run)

**Not** the association merge executor: `decide` (`association.rs:193-220`) only
merges above a similarity threshold and treats uncomputable pairs as
below-threshold, and `claim_key` (`:856`) dedupes claims by exact trimmed text,
so 15 re-renderings either fail to merge or produce a survivor carrying ~15
paraphrases of every claim. Similarity is the wrong instrument for notes that are
identical by construction.

`sb borg dedupe-sessions [--apply] [--purge]`, dry-run by default:

- Group harvest notes by `trace:`.
- **Survivor rule, stated once and referenced everywhere else:** prefer a note
  with `distilled: true` and no degradation markers (`cortex-needs-review`,
  `[missing-summary]`, `[yaml-parse-error]`); among those, the greatest receipts
  `terminal_at` (a real timestamp) if present, else the greatest file mtime, else
  the lexicographically greatest path. "Earliest `ingested:`" is REJECTED: it is
  a date, not a timestamp, and it provably selects
  `review-ci-workflow-security-changes-5.md`, a missing-summary note with no
  `slug:`, because every one of the four earliest notes in the cohort is degraded.
- **Losers become tombstones, not deletions**: strip `slug:`, insert
  `superseded-by: <survivor-stem>`, replace the body with a redirect, exactly the
  shape `association::tombstone_content` already ships. **This is why there is no
  wikilink rewrite**: every inbound `[[link]]`, piped, path-qualified, embedded,
  or in a `.base` file, still resolves and redirects. Rewriting was specced last
  round and is withdrawn: `cortex::links` handles only `[[t]]` and `[[t|alias]]`
  (`links.rs:9-11`) and 32 vault-wide duplicate stems make bare-stem rewriting
  ambiguous.
- `--purge` rkvr-archives a tombstone only when zero inbound links resolve to it.
  Separate, opt-in, run later.
- Backfill `harvest-body-hash:` onto existing harvest notes from the staged
  `body.txt` where staging survives retention; leave absent otherwise.

Acceptance:
- [ ] Dry-run prints an exact plan: per trace, the survivor and the exact
      tombstoned set. Operator approves that list before `--apply`.
- [ ] For `hv-e5d240` the survivor is one of the six clean notes, never `-5`.
- [ ] Post-run: `sb borg dedupe-sessions` reports zero groups, and every inbound
      wikilink in the vault still resolves.
- [ ] `--purge` refuses a tombstone with a live inbound link.
- [ ] Backfill covers every note whose trace still has staging; the rest are
      reported, not silently skipped.

## Acceptance Criteria

Per-phase criteria above are the contract. Cross-cutting:

- [ ] Both regression guards exist and fail when their mechanism is stubbed out.
- [ ] Every frontmatter key this design writes has a named reader.
- [ ] The borg-owned key policy is derived from `markdown::render_note`, with a
      test that fails when the writer gains an unknown key.
- [ ] `otto ci` green (`--features vec`).

## Resolved Decisions

- **Key on the trace, not the session-id set.** The id-set cannot distinguish a
  replay from a follow-up. The discriminating value is the input `body_hash`
  (`watermark.rs:202`), and (id-set, transcript) already has one name: the trace.
- **Guard on `trace:` + `source:` + `harvest-body-hash:`.** The hash term is what
  makes `--force` and a collision both non-destructive, and it is the value the
  design already named as discriminating.
- **Intent gates the branches.** `FollowUp` never resolves; only `NewNote` gets
  the crash-recovery fallback. This encodes `publish.rs:156-162` in the type.
- **Tombstone, don't delete, in Phase 6.** It makes the wikilink problem
  disappear instead of solving it.
- **Survivor = latest valid distill**, not earliest ingested. Driven by measured
  quality/age anti-correlation in the cohort.
- **`tags` is replaced, not unioned**, consistent with the ownership model.
- **Widen the trace field** despite the panel judging it unnecessary: zero
  parsers depend on its length, and it is the remedy the code's own comment names.

## Alternatives Considered

### Alternative 1: session-id-keyed filenames

`8d6b6ef3-....md`. Deterministic, no lookup. Rejected: Obsidian derives the
displayed title from the filename, so the quick-switcher, graph, and file tree
become UUID soup; and a 29-member thread
(`notes/audit-remediation-plan-across-eight-ci-phases.md`, `hv-7894c8`) has no
sane filename, degenerating to "primary id only" -- which `source:` already is.

### Alternative 2: key replacement on `source:` instead of `trace:`

Rejected as the PRIMARY key: `source:` cannot distinguish a replay from a
follow-up. Its useful residue is adopted narrowly, as the collision guard and
(with the hash) the crash-recovery fallback.

### Alternative 3: rewrite inbound wikilinks in Phase 6

Withdrawn after review. `cortex::links` handles two of the five link forms in the
vault, and 32 vault-wide duplicate filename stems make bare-stem rewriting
ambiguous.
Tombstones make the rewrite unnecessary.

### Alternative 4: vault frontmatter scan as the primary resolver

Correct but O(vault) per publish; 140 publishes a night makes it the dominant
cost. The proposal is this with a repaired index in front.

### Alternative 5: prompt-only slug determinism

`distill-session.md` already instructs determinism; `hv-e5d240` produced 6
distinct slugs under that instruction. Same conclusion as `bcbbf73`.

## Technical Considerations

### Dependencies

None added.

### Performance

The index is built at most once per process (memoized for the process
lifetime, self-insert on write, no TTL - see Architecture's "Index freshness"
paragraph, which withdraws an earlier timestamped/TTL rebuild as ~140 full
vault scans a night). `scan_vault` is rayon-parallel and cortex already pays
it per sweep. With Phase 1's receipts repair, steady-state publishes hit the
fast path and never scan.

### Security

None. No new file, network, or credential surface.

### Testing Strategy

Per-phase acceptance above, plus: the two regression guards, the
policy-matches-writer test, and the `hv-e5d240` cohort as a real fixture for
Phase 5's grouping claim (a property claim needs a witness, not an assertion).

### Rollout Plan

Phases 1-5 ship as ordinary commits plus a version bump. Phase 6 is operator-run,
dry-run first, reversible (tombstones, then opt-in `--purge` via `rkvr`). No
systemd, config, or schema change, so no bootstrap step.

`association` is NOT enabled under daemon actions in the live
`~/.config/sb/cortex.yml`, so Phase 5's grouping change does not start
auto-merging on a daemon tick; the `superseded-by` skip guard is what makes it
safe if that is ever enabled.

### Concurrency and failure modes

- Resolver DB error fails the publish CLOSED, with the trace id and the SQLite
  error in the message; the note is not written and the receipts row is left for
  a later replay. SQLite gives 5s of `busy_timeout` (`receipts.rs:109`).
- Replay now takes the harvest state lock (Phase 2), so timer-vs-manual is a loud
  `HarvestLockHeld`, not a race.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Trace ids were 24-bit and explicitly not unique (`vault/src/trace.rs:14-21`), and this design makes the trace an overwrite key. | Three-term guard (`trace` + `source` + `harvest-body-hash`) makes a collision non-destructive, and Phase 1 widens the field to 32 bits. |
| `--force` re-harvest of an unchanged session matches step 3's keys exactly (`watermark.rs:257-260` returns `FollowUp` before consulting the hash). | `ResolveIntent::FollowUp` short-circuits to `None`. Tested. |
| Crash between note write and watermark save. | fsynced `save` (Phase 2), per-thread save, and `harvest-body-hash:` in frontmatter as the vault-side backstop. |
| `harvest-body-hash:` is absent from all 272 existing notes, so step 3 is forward-only. | Stated, and Phase 6 backfills from staging where retention allows. |
| Manual tag edits lost on replay. | Documented in the module doc comment; replay is an explicit operator action. |
| Phase 6 tombstones a note the operator wanted. | Dry-run by default with an exact printed plan; tombstones are reversible; `--purge` is separate and link-checked. |

## Open Questions

None.

## References

- `docs/design/2026-07-17-harvest-clyde-sessions.md` -- harvest architecture.
- `docs/design/2026-07-24-harvest-content-slug-naming-handoff.md` -- the change
  that moved the filename stem to model output.
- `docs/design/2026-07-24-cortex-association-sweep.md` -- the safety net and the
  mirror-image case it was scoped to.
- Commits `d150970`, `dcd370e`, `2a6d6c3` -- the first three links in the chain.
- `/tmp/review-panel/SCvEJ1OC/synthesis.md` -- rounds 1 and 2.
- `/tmp/claude/handoff-second-brain-harvest-2026-08-15.md` -- Task 3.
