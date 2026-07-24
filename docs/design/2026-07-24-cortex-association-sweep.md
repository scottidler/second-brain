# Design Document: cortex association sweep for harvest session notes

**Author:** Scott Idler (via agent)
**Date:** 2026-07-24
**Status:** Implemented
**Review Passes Completed:** 5/5 + review-panel (Architect + Staff Engineer) + consensus loop

## Summary

Borg names harvest session notes from a content-derived `slug` and, on a
filename collision, disambiguates with a deterministic `{slug}--{shorthash}.md`
suffix (shipped v0.12.2). Two sessions that distill to the SAME slug are almost
certainly about the same subject. This adds a cortex governance action that
detects same-slug session notes and, per pairwise similarity, either MERGES them
into one note or CROSS-LINKS them. Association is cortex's job; borg only names.

## Problem Statement

### Background

- Harvest (`sb borg harvest`) turns dormant clyde sessions into vault notes.
- v0.12.2 (`docs/design/2026-07-24-harvest-content-slug-naming-handoff.md`) made
  the filename a content-slug and killed the old order-dependent `-N` collision
  suffix in favor of a deterministic `{slug}--{first-8-of-primary-uuid}.md`.
- The bare content-slug is persisted to frontmatter `slug:` (the sanitized stem
  WITHOUT the `--hash`), so two colliding notes carry the **identical** `slug:`
  value. That is the grouping key.

### Problem

- A slug collision is an association signal, not a naming accident: two sessions
  distilling to the same slug are probably the same subject.
- Nothing acts on that today. Same-slug notes just sit as `{slug}.md` +
  `{slug}--{hash}.md`, un-associated. The knowledge is fragmented across notes
  that should be one, or should at least cross-reference.

### Goals

- Group session notes by frontmatter `slug:`.
- For each same-slug group, decide per pairwise similarity: MERGE (union claims,
  grow `cortex-session-ids`, append `## Session Details`) when similar enough;
  CROSS-LINK (reciprocal `[[wikilink]]`) when distinct-but-related.
- Threshold and similarity methodology configurable in `cortex.yml`.
- Deterministic and idempotent: same vault state in -> same result; a second run
  writes nothing.
- Recoverable: no destructive file deletion; the absorbed note is soft-retired to
  a recoverable tombstone.

### Non-Goals

- Cross-slug dedup (two DIFFERENT slugs that are actually the same topic). That is
  `cortex::duplicates`' existing job; this action keys strictly on the slug.
- Re-slugging or renaming beyond the merge survivor keeping the bare `{slug}.md`.
- Associating non-session notes. Scoped to `content_type == Session`.
- Changing borg's naming (shipped, unchanged).
- Regenerating the ~121 legacy pre-slug notes (that is the harvest-slug Phase 4,
  a separate deploy-gated migration). Legacy notes with no `slug:` frontmatter
  simply do not group here until re-distilled.

## Proposed Solution

### Overview

A new cortex action `associate`, modeled on `cortex::duplicates` (config + lint +
apply + daemon-arm skeleton) and `cortex::graph` (open a `SearchIndex`, take the
embed lock, read `note_embeddings`). CLI-first (`sb cortex associate`, dry-run
default, `--apply` writes); daemon action registered but OFF by default.

Pipeline per run:

1. **Group** session notes by `frontmatter.extra["slug"]`; drop singletons and
   `slug == None` (legacy).
2. **Decide** per group by TRANSITIVE similarity clustering (real pairwise, not
   star). Compute pairwise similarity for every pair in the group; union-find
   any pair >= threshold into the same merge-cluster. Each multi-member cluster
   MERGEs to one survivor. Members left in singleton clusters, and distinct
   clusters within the same slug-group, are CROSS-LINKed. This handles the 3+
   case the star topology dropped (two members similar to each other but both
   sub-threshold to a third still merge). Fail-safe: when similarity cannot be
   computed for a pair (no embedding AND no claim overlap), that pair is treated
   as below threshold -> CROSS-LINK, never MERGE. An unknown never triggers the
   destructive path.
3. **Execute** (only with `--apply`): merge (rewrite survivor + soft-retire
   absorbed) or cross-link (reciprocal wikilinks). Every write via
   `scope::insert_frontmatter_fields` + `vault::note::write_atomic`.

**Quiescence is whole-group:** if ANY member's mtime is within
`min-quiescence-secs`, the entire group is skipped this run (never a half-merged
group). Tombstones never re-group: soft-retire removes `slug:`, so an absorbed
note drops out of grouping on every subsequent run.

### Architecture

- `cortex/src/association.rs` (new module; `association/tests.rs` beside it) --
  pure grouping + decision core returning typed data; executors that mutate via
  the shared atomic primitives. Cortex stays lib-only: it returns a typed
  `AssociationReport`, never prints.
- Reuses:
  - A NEW `vault::search` primitive `cosine_between(path_a, path_b) -> Option<f32>`
    for the EXACT pairwise embedding cosine of two notes. `semantic_neighbors`
    (`vault/src/search/vector.rs:305`) is global-top-k: a genuinely-embedded
    same-slug member can be pushed out of the top-k by unrelated high-similarity
    notes, silently misrouting a merge to cross-link. Pairwise cosine is
    therefore REQUIRED, not the optional refactor the earlier draft implied.
    `None` when either note lacks a summary embedding (-> claim fallback).
  - `cortex::embed::acquire_lock()` before opening the index (the `graph.rs:87`
    precedent) -- cortex commands do not share oracle's mutex.
  - `cortex::duplicates::cosine_similarity` + `tokenize` (`:347`/`:335`) for the
    TF-IDF fallback over claim text. These are PRIVATE `fn` today; Phase 1
    promotes them to `pub(crate)` (or extracts a shared `cortex::sim` module) so
    `association` can call them -- calling them cross-module as-is will not
    compile.
  - `cortex::scope::insert_frontmatter_fields` / `remove_frontmatter_fields`
    (`scope.rs:176`/`:234`) and `vault::note::write_atomic` (`note.rs:112`).
- Config under `actions.association` in `cortex.yml`, mirroring
  `actions.duplicates`.
- CLI verb `Associate` in `sb/src/cli/cortex.rs` (`--apply` split, the house
  convention for a destructive verb: Hub/Graph/Bridge all do this).
- Daemon: a NEW periodic INTERVAL arm in `cortex/src/daemon.rs` modeled on the
  `embed`/`cold`/`graph` interval arms (`:143`/`:172`/`:180`), NOT added to the
  on-change `configured_actions` / `configured_actions_with_scanner` loop
  (`:459`/`:503`) -- adding it there would run it on startup and every watcher
  change, exactly what "never per-change" forbids. Gated by
  `is_enabled("association")`, default OFF, own cadence config (default e.g.
  hourly), never per-change debounce. (There is no `run_actions` symbol; the
  earlier draft named it wrong.)

### Data Model

Config (`cortex/src/config.rs`, under `actions.association`,
`#[serde(deny_unknown_fields)]`):

```yaml
actions:
  association:
    threshold: 0.85                 # merge iff similarity >= this
    similarity-source: both         # embedding | claim | both
    min-quiescence-secs: 600        # skip notes modified within this window
    exclude:
      - "journal/**"
```

- `threshold: f64` (default 0.85, mirrors duplicates).
- `similarity-source: enum { Embedding, Claim, Both }` (default `Both`:
  embedding cosine primary via `semantic_neighbors`, claim TF-IDF fallback when a
  note has no `kind=summary` embedding). Selecting the active methodology is
  legitimate config per the `general.md` carve-out.
- `min-quiescence-secs: u64` (default 600) -- skip any note whose file mtime is
  within this window (an actively-edited note is never merged mid-edit).
- `exclude: Vec<String>` glob paths.

Typed results (so sb formats without re-inspecting opts, the `SweepMode`
precedent):

```rust
enum AssociationOutcome {
    Merge { survivor: PathBuf, absorbed: Vec<PathBuf>, session_ids: Vec<String> },
    CrossLink { notes: Vec<PathBuf> },
}
enum AssociationReport {
    WouldAssociate(Vec<AssociationOutcome>),  // dry-run
    Associated(Vec<AssociationOutcome>),      // --apply; changed paths for the daemon fingerprint
}
```

### Merge semantics

- **Survivor selection (deterministic):** the note whose primary session is
  earliest by frontmatter `date`; ties broken by lexicographically-smallest
  primary session id. The survivor keeps its OWN existing filename -- NO rename.
  Forcing the bare `{slug}.md` name would require a file rename plus search/index
  and receipts-path implications for zero real benefit (the filename is
  display/addressing only, per this feature's own principle). Dropped.
- **Idempotent union:** survivor's `cortex-session-ids` becomes the sorted union
  of all cluster members' ids (dedup); `## Session Details` gains only the
  `- clyde://<id> - ...` bullets not already present; `## Claims` gains only
  absorbed claims whose trimmed text is not already present. Union is idempotent
  by construction: re-running never double-adds. This is what makes a
  partial-failure self-heal (below).
- **Soft-retire (no deletion, content-preserving):** each absorbed note is
  rewritten in place to a tombstone -- frontmatter gains
  `superseded-by: <survivor-stem>`, `slug:` is REMOVED (so it never re-groups),
  and the body becomes a single `Merged into [[survivor-stem]].` redirect. No
  `status:` change (`vault::schema::Status` has no `Archived` variant;
  `superseded-by:` IS the tombstone marker). Nothing is deleted. The absorbed
  note's knowledge is preserved in the survivor (claims + ids were unioned in
  first); the tombstone records the merge. "Content-preserving," not
  "fully recoverable" -- un-merge (splitting a session back out) is a manual
  operation; the merge itself is a git-diff away from revert. Physical cleanup
  of tombstones is a separate future sb-level verb, not this action.
- **Tombstones are excluded downstream (required):** `cortex embed`, quality,
  search grouping, and this action's own grouping all skip notes carrying
  `superseded-by:`, so a tombstone never pollutes the embedding index, never
  re-groups, and never surfaces in retrieval. This exclusion is part of the
  feature, not an afterthought.
- **Apply order + atomicity (multi-file safety):** a merge touches N+1 files but
  `write_atomic` is per-file. Order: (1) preflight-read every cluster member;
  (2) `write_atomic` the enriched SURVIVOR first (full union); (3) then
  `write_atomic` each absorbed tombstone. If a tombstone write fails mid-cluster,
  the run WARN-and-continues (the `duplicates.rs:189` contract): the survivor
  already holds the full union, and the un-retired absorbed note still has its
  `slug:`, so the NEXT run re-groups and re-absorbs it -- and because the union
  is idempotent, no duplication results. Half-merge is self-healing, not
  corrupting.

### Cross-link semantics

- Insert a reciprocal `## Related` section (or append to it) in every group
  member with `[[<other-slug-or-stem>]]` for each sibling, using the house
  wikilink form (`cortex/src/linking.rs`).
- Idempotent: skip a link already present; a second run writes zero bytes.

### API Design

- `cortex::association::group_by_slug(notes: &[Note]) -> Vec<Vec<usize>>` --
  BTreeMap-ordered groups of session notes sharing `extra["slug"]`, singletons
  and `None` dropped.
- `cortex::association::decide(group, ctx) -> Vec<AssociationOutcome>` -- pure
  transitive-clustering decision. For each pair, similarity = `cosine_between`
  (embedding) when both notes are embedded, else `duplicates::cosine_similarity`
  over claim text, else uncomputable (-> below-threshold). Union-find pairs
  >= threshold; deterministic (sorted member order, stable cluster ids).
- `cortex::association::apply(vault_root, notes, config) -> Result<AssociationReport>`
  -- executes under `--apply`; per-note failure WARN-and-skip, never `?`-aborts
  the run (the `duplicates.rs:189` contract).
- sb: `sb cortex associate [--apply]`.

### Implementation Plan

(Environmental fact, already probed and closed 2026-07-24: harvest Session notes
ARE summary-embedded -- 213 `kind=summary` rows for `note_type=session` in the
live oracle DB. So `similarity-source: both` with embedding primary is valid; no
spike phase needed.)

#### Phase 1: Config (fail-closed) + grouping + shared sim primitives
**Model:** sonnet
- **Loader fail-closed (Scott, 2026-07-24):** change `Config::load_inner`
  (`cortex/src/config.rs:880`) to hard-error (`bail!`) when a config file is
  PRESENT but unparseable, instead of warning and falling back to defaults. A
  MISSING file still defaults. This is cross-cutting (all cortex config) and lands
  as its own commit within this feature.
- `AssociationConfig` under `actions.association` (`deny_unknown_fields`);
  `Associate` opts; pure `group_by_slug` (keyed on `extra["slug"]`, session-only,
  skips `superseded-by:` tombstones and `slug==None`). Promote
  `duplicates::{tokenize, cosine_similarity}` to `pub(crate)` (or extract
  `cortex::sim`); add `vault::search::cosine_between(a, b) -> Option<f32>`.
- **Success criteria:** a present config with a typo'd key now FAILS LOUD
  (`load_inner` returns Err), a missing config still defaults; a unit test groups
  two same-slug session notes, excludes a `slug==None` note, a tombstone, and a
  non-Session note, and drops singleton groups; `cosine_between` returns `Some`
  for two embedded notes and `None` when either is unembedded; the promoted sim
  fns are callable from `association`.

#### Phase 2: Similarity decision core (transitive clustering)
**Model:** opus
- Pure `decide`: pairwise similarity (`cosine_between` primary, claim TF-IDF
  fallback, uncomputable -> below-threshold); union-find clustering at threshold;
  deterministic cluster + output ordering.
- **Success criteria:** a >=threshold pair clusters to `Merge`; a <threshold pair
  -> `CrossLink`; a 3-member group where A~B>=threshold but C<threshold-to-both
  yields Merge{A,B} + CrossLink{C}; an uncomputable pair -> CrossLink never Merge;
  positive AND negative cases.

#### Phase 3: Merge executor
**Model:** opus
- Survivor selection (earliest date, keeps own filename); idempotent union of
  claims + `## Session Details` + `cortex-session-ids`; preflight-read the
  cluster, `write_atomic` the survivor FIRST, then `write_atomic` each absorbed
  tombstone (`superseded-by:` set, `slug:` removed, redirect body, NO `status:`
  change); tombstones excluded from grouping/embed.
- **Success criteria:** survivor frontmatter carries the union of both id sets and
  both `## Session Details` bullets; absorbed note is a tombstone
  (`superseded-by:` set, `slug:` gone); a second run is a byte-level no-op
  (idempotent union); simulating a tombstone-write failure leaves the survivor
  correct and the absorbed note re-absorbed cleanly on the next run (self-heal,
  no duplicate claims/ids); receipts DB rows unchanged; a tombstone gets no new
  embedding row.

#### Phase 4: Cross-link executor
**Model:** sonnet
- Reciprocal `## Related` `[[wikilink]]` insertion in each member; idempotent.
- **Success criteria:** each member gains the others' wikilink; a second run
  writes zero bytes.

#### Phase 5: CLI + daemon wiring
**Model:** sonnet
- `sb cortex associate` (dry-run default, `--apply`); typed `WouldAssociate`/
  `Associated`. Daemon: a NEW interval arm modeled on `embed`/`cold`/`graph`
  (NOT `configured_actions`), gated by `is_enabled("association")`, default OFF,
  own cadence.
- **Success criteria:** dry-run prints the plan and writes zero bytes; `--apply`
  executes; the daemon arm is an interval tick (not per-change), only runs when
  explicitly enabled; a group with any member modified within
  `min-quiescence-secs` is skipped whole.

## Acceptance Criteria

- [ ] Two same-`slug` session notes with similarity >= threshold merge into one
      note whose `cortex-session-ids` is the union of both, with both
      `## Session Details` bullets, and the absorbed note becomes a recoverable
      tombstone (nothing deleted).
- [ ] Two same-`slug` notes with similarity < threshold gain reciprocal
      `[[wikilink]]`s and are NOT merged.
- [ ] `sb cortex associate` (no `--apply`) writes zero bytes and prints the plan;
      `--apply` executes it.
- [ ] Re-running `sb cortex associate --apply` on the same vault is a no-op
      (idempotent; deterministic grouping and survivor selection).
- [ ] A group with any member modified within `min-quiescence-secs` is skipped
      whole (never half-merged).
- [ ] When similarity cannot be computed for a pair (no embedding and no claim
      overlap), the outcome is CROSS-LINK, never MERGE.
- [ ] A 3-member same-slug group where A~B >= threshold but C is below threshold
      to both yields MERGE{A,B} + CROSS-LINK{C} (transitive clustering, not star).
- [ ] A tombstone (a note carrying `superseded-by:`) is excluded from grouping and
      is never re-embedded.
- [ ] A simulated tombstone-write failure mid-merge leaves the survivor correct
      and self-heals on the next run with no duplicate claims or session-ids.
- [ ] A PRESENT cortex config with an unparseable/typo'd key makes the loader
      return Err (fail loud); a MISSING config still defaults.
- [ ] `otto ci` green; unit tests cover grouping, both decision branches
      (positive + negative), merge union, soft-retire, and cross-link idempotency.

## Resolved Decisions

- **2026-07-24 -- cortex owns association, borg only names.** Per Scott's
  per-similarity + `cortex.yml`-threshold calls. Supersedes the handoff's "modify
  `resolve_publish_path` in borg" sketch.
- **2026-07-24 -- soft-retire, never delete (OQ1 closed).** The merge absorbs into
  a survivor and rewrites each absorbed note to a content-preserving tombstone
  (`superseded-by:` + redirect body, `slug:` removed). No `status:` change:
  `vault::schema::Status` has no `Archived` variant, so `superseded-by:` is the
  marker (fixes panel finding 1, schema-is-law). Chosen over (a) shelling to
  `rkvr` (breaks cortex's lib-only, no-side-effects contract) and (b) an archive
  dir move. Nothing is deleted; works identically in CLI and daemon. A
  physical-cleanup verb can rkvr tombstones later at the sb layer.
- **2026-07-24 -- CLI-first, daemon OFF by default (OQ2 closed).** A destructive
  merge must not fire on every debounced change. `sb cortex associate` is the
  primary surface; the daemon action is registered but disabled by default and
  runs on a periodic tick when enabled.
- **2026-07-24 -- quiescence guard (OQ3 closed).** Skip any note whose mtime is
  within `min-quiescence-secs` (default 600) so an Obsidian-open note is never
  merged mid-edit; `write_atomic` already prevents torn writes.
- **2026-07-24 -- survivor = earliest date, keeps its own filename (OQ4 closed;
  revised per panel finding 9).** Deterministic: earliest frontmatter `date`,
  ties by smallest primary session id. The survivor does NOT rename to bare
  `{slug}.md` -- a rename drags search/index/receipts-path implications for zero
  benefit (filename is display-only). Dropped the earlier "takes the bare
  filename" clause.
- **2026-07-24 -- similarity-source default `both` (OQ5 closed by probe, not a
  spike).** Embedding cosine primary via a REQUIRED pairwise `cosine_between`
  (not global-top-k `semantic_neighbors`, panel finding 2), claim TF-IDF fallback.
  Probed the live oracle DB: 213 `kind=summary` embeddings exist for
  `note_type=session`, so `both`/embedding-primary is valid. No Phase 0.
- **2026-07-24 -- transitive clustering, not star (panel finding 6).** Group
  resolution is union-find over pairs >= threshold (real pairwise), so 3+-member
  groups with a close pair and a distant third still merge the pair and cross-link
  the third. Supersedes the earlier star-topology draft.
- **2026-07-24 -- merge is idempotent + self-healing (panel finding 3).** Union
  dedups ids/claims; apply order is survivor-first then tombstones; a failed
  tombstone write leaves the absorbed note re-groupable and the next run
  re-absorbs it with no duplication. Multi-file merge has no all-or-nothing
  transaction, but half-merge is a safe, self-correcting state.
- **2026-07-24 -- tombstones excluded downstream (panel finding 11).** Notes with
  `superseded-by:` are skipped by grouping, embed, quality, and search -- a
  required part of the feature, not a follow-up.
- **2026-07-24 -- daemon is a periodic interval arm, not `configured_actions`
  (panel finding 4).** Modeled on `embed`/`cold`/`graph`; the earlier `run_actions`
  symbol does not exist.
- **2026-07-24 -- fix the cortex config loader fail-closed IN this work (Scott,
  panel finding 8).** `Config::load_inner` will hard-error on a present-but-
  unparseable config instead of warning + defaulting. Cross-cutting (all cortex
  config); lands as its own Phase 1 commit. Chosen over tracking separately.

## Alternatives Considered

### Alternative 1: merge in borg at publish time
- **Description:** borg's `harvest_publish_path` detects the collision and merges
  then and there.
- **Pros:** one-pass; no separate sweep.
- **Cons:** borg has no embedder (one-embeddings-writer invariant), no whole-vault
  view, and publish is incremental (the sibling may not exist yet). Merge is
  vault governance, which is cortex's role.
- **Why not chosen:** wrong subsystem; contradicts Scott's `cortex.yml` steer.

### Alternative 2: physical delete of absorbed notes via rkvr
- **Description:** merge deletes the absorbed file through `rkvr rmrf`.
- **Pros:** no tombstone clutter.
- **Cons:** cortex is lib-only with no side effects; shelling to `rkvr` from a
  library daemon breaks the contract and testability. Deletion (even recoverable)
  is heavier than needed.
- **Why not chosen:** soft-retire is lib-pure and equally recoverable.

### Alternative 3: extend `cortex::duplicates` instead of a new action
- **Description:** add slug-grouping into the existing duplicates lint.
- **Pros:** less new surface.
- **Cons:** duplicates keys on body similarity across ALL notes and only tags; it
  never merges. Overloading it with slug-keyed merge/crosslink conflates two
  different governance behaviors.
- **Why not chosen:** distinct behavior earns a distinct action; copy the skeleton,
  do not overload.

## Technical Considerations

### Dependencies

- `vault::search` (embedding read), `cortex::embed::acquire_lock`,
  `cortex::duplicates` primitives, `cortex::scope`, `vault::note::write_atomic`.
- Reads the oracle DB (`config.oracle_db_path()`) read-only for embeddings; cortex
  already does this in `graph.rs`.

### Performance

- Grouping is O(notes). Similarity is per-group pairwise; groups are small (same
  slug), so cost is bounded. Embedding cosine reuses the existing brute-force
  `semantic_neighbors` (already the vector-search cost, acceptable at vault size).

### Security

- No new secrets, no network. Reads/writes the local vault + reads the local
  oracle DB. Soft-retire means no deletion path to get wrong.

### Testing Strategy

- Unit tests in `cortex/src/association/tests.rs` with mini-vault fixtures
  (TempDir): grouping, both decision branches, merge union, soft-retire tombstone
  shape, cross-link idempotency, quiescence skip. Break-the-code checks: flip a
  pair below threshold and assert CrossLink not Merge.

### Rollout Plan

- Ship in the sb binary via the normal direct-to-main + tag flow (second-brain is
  unprotected). `otto deploy` picks up the new `cortex.yml` example. Daemon action
  stays OFF until the operator enables it; `sb cortex associate` (dry-run) is safe
  to run any time.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Merge clobbers an in-flight Obsidian edit | Med | High | `min-quiescence-secs` mtime guard; `write_atomic`; daemon OFF by default |
| Wrong merge (distinct sessions folded) | Med | Med | Threshold + soft-retire (recoverable); CLI dry-run before apply |
| Session notes not summary-embedded | Med | Low | Phase 0 proves it; `both` falls back to claim TF-IDF |
| Tombstone clutter accumulates | Low | Low | Recoverable + honest; future sb cleanup verb |
| Non-idempotent oscillation trips daemon fingerprint | Low | Med | BTreeMap/sorted grouping; idempotency test; skip-if-link-present |
| Half-merged group (tombstone write fails mid-cluster) | Low | Med | Survivor-first apply order + idempotent union -> next run self-heals; no all-or-nothing txn needed |
| Config typo silently runs on defaults (loader fails open) | Med | Med | FIXED in Phase 1: `Config::load_inner` hard-errors on a present-but-unparseable config (Scott, 2026-07-24) |

## Open Questions

- None. All 11 review-panel findings are dispositioned (10 folded above; finding
  8 resolved by Scott's decision to fix the loader fail-closed in this work,
  Phase 1). The one environmental unknown was closed by probing the live oracle
  DB (session notes ARE summary-embedded, 213 rows), not deferred to a spike.

## References

- `docs/design/2026-07-24-harvest-content-slug-naming-handoff.md` (the borg half)
- `docs/design/2026-07-24-harvest-content-slug-naming-implementation-notes.md`
- `cortex/src/duplicates.rs`, `cortex/src/graph.rs`, `cortex/src/scope.rs`
- `vault/src/search/vector.rs:305` (`semantic_neighbors`)
- `vault/src/note.rs:112` (`write_atomic`)
