# Implementation notes: harvest note identity -- trace-keyed replace

Append-only. Each phase adds a section with all four buckets.

## Phase 0: ready-to-build gate (pre-implementation verification)

The design doc's acceptance criteria carried no `Observed on main:` lines, so
every criterion and rationale claim naming an existing symbol, path, flag, or
count was executed against current `main` and the live vault before Phase 1.

### Verified exactly as written

- `vault/src/trace.rs:36` is `format!("{:06x}", mixed & 0x00FF_FFFF)` -- matches
  the Phase 1 edit list's "before" state.
- Phase 1's "exactly ONE length assumption exists in the workspace": confirmed.
  Grep for slice patterns, `chars().take`, `len()` comparisons, and hex shape
  regexes returns exactly one trace-length assertion,
  `vault/src/trace/tests.rs:6` (`^[a-z]{2}-[0-9a-f]{6}$`). The one other
  `chars().take` near trace code is `borg/src/pipeline/session.rs:53`, which
  truncates the *primary session id*, not the trace.
- `borg/src/harvest/watermark.rs:97-99` is `fs::write` + `fs::rename` with no
  fsync; `vault/src/note.rs:112-128` `write_atomic` fsyncs temp and parent.
  Phase 2's "durability is inverted" premise holds.
- `watermark.rs` `acquire_lock` uses `try_lock_exclusive` and returns
  `HarvestLockHeld` -- Phase 2's "fails instantly, does not wait" claim holds.
- `watermark.rs` `classify_reappearance` returns `FollowUp` on `force` **before**
  consulting `fresh_hash`. Phase 1's `ResolveIntent::FollowUp` gate is required
  exactly as the doc argues.
- `receipts.rs` `mark_succeeded` carries `WHERE trace_id=? AND status='received'`
  -- cannot repair a terminal row, so `update_note_path` is needed.
- `cortex/src/classify.rs` `resolve_collision`: the `existing_note_has_source`
  check is applied only to the base path; the `for i in 2..100` loop tests
  `!candidate.exists()` alone. Attempt 4's bug is real and Phase 5's fix is
  correctly scoped.
- `cortex/src/association.rs` `group_by_slug` skips `superseded-by` and keeps
  groups of `>= 2` -- the guard Phase 5 must carry over exists where stated.
- `.otto.yml` uses `--features vec` for check/clippy/test. `rkvr` is on PATH at
  `~/.cargo/bin/rkvr` (Phase 6 `--purge` dependency).
- `sb borg dedupe-sessions` does not exist yet -- correct, Phase 6 introduces it.
- Live vault: 3,141 markdown files, 10.0 MB (exact match). 272 harvest notes,
  208 distinct `clyde://` sources, 22 sources owning more than one note (all
  three exact). Trace `hv-e5d240` has exactly 15 notes, all under `notes/`, with
  the filenames the doc lists.

### Doc defects found and amended

- **Duplicate filename stems: doc said 32 vault-wide, actual is 33.** Measured
  `find . -name '*.md' -not -path './.obsidian/*' -printf '%f\n' | sort | uniq -d
  | wc -l` -> `33`; excluding `system/` -> `26` (the doc's 26 is exact). The
  drift is in the `system/` weekly-note series. Amended in the doc's tombstone
  paragraph with the re-count date. This is a stale measurement, not a load-
  bearing change: the ambiguity conclusion that kills Alternative 3 holds at 33
  as it did at 32.

### Doc imprecision recorded, not amended

- **The borg-owned key enumeration is under-inclusive.** The doc lists
  `markdown::render_note`'s emitted keys as `title`, `date`, `ingested`,
  `source`, `type`, `origin`, `status`, `method`, `trace`, `tags`, `creator`.
  `render_note` also emits `asset`, `capture-note`, `slides`, `duration`, and
  `language` (`borg/src/markdown.rs`, the `ContentType`-conditional branches).
  Session notes never take those branches, so the omission is harmless for this
  design's behavior -- but the cross-cutting criterion requires the policy set be
  DERIVED from the writer with a test that fails on an unknown writer key, and a
  hand-typed list from the doc would fail that test on day one. Phase 3 must
  derive the set from `render_note` itself, per the doc's own governing rule
  ("DERIVED FROM THE WRITER, not hand-listed"), and treat the doc's prose list
  as illustrative. No doc amendment: the governing rule is already correct.

### Open questions

- None. Gate passes; Phase 1 proceeds.

## Phase 1: resolution primitives

### Design decisions

- `resolve_prior_note` builds/fetches a process-lifetime memoized `VaultIndex`
  (`trace_index`, `stem_index`, `source_hash_index`) from ONE `scan_vault` call
  per vault root, keyed by canonicalized root in a global
  `Mutex<HashMap<PathBuf, VaultIndex>>` — `borg/src/harvest/identity.rs::get_or_build_index`.
  Keying by root (not a single global) lets production (one root) and tests
  (one tempdir per test) coexist in the same process without cross-talk.
- `note_published(vault_root, trace_id, absolute_path)` is the Phase 3
  self-insert hook: it mutates the trace_index entry of an ALREADY-BUILT cached
  index in place, and is a no-op if no index has been built yet for that root
  (the next `resolve_prior_note` call builds fresh from disk, which already
  includes the just-landed note) — `identity.rs::note_published`.
- `source_hash_index` is populated in the SAME `scan_vault` pass as
  `trace_index`/`stem_index`, not a second pass on a step-3 miss — this is what
  keeps the crash-recovery fallback from ever paying a second full-vault walk
  — `identity.rs::build_index`.
- Tombstone-chain tie-break resolved to: a stem with an ambiguous (>1) file
  count is disambiguated by filtering OUT any candidate that is itself a
  tombstone (`superseded-by:` present); a stem with exactly ONE match is used
  regardless of whether that single match is itself a tombstone (this is what
  makes the chain actually transitive — filtering unconditionally would turn
  every multi-hop chain into a false "missing stem") —
  `identity.rs::follow_tombstone_chain`. This reading was inferred from the
  Architecture section's two adjacent but distinct sentences ("follow it...
  transitively... depth bound of 8" vs. "the multi-match tie-break skips
  tombstones entirely") — no open question was raised because the two
  sentences are only mutually consistent under this reading; recorded here for
  scrutiny.
- `try_resolve_candidate` (steps 1-2, three-term guard) and
  `try_resolve_crash_candidate` (step 3, source+hash only, no trace term, no
  hash-absence leniency) are separate functions rather than one parameterized
  guard, because step 3's Data Model rule ("both keys are required; a note
  lacking the hash is not eligible") is the OPPOSITE of the confirmation
  guard's legacy-hash-absent-passes leniency — folding them into one function
  with a flag would obscure that the leniency direction flips.

### Deviations

- **Doc prose said "timestamped rebuild" (Phase 1 section) and "built at most
  once per 60s" (Performance section); both are stale leftovers from an
  earlier TTL design that the Architecture section's "Index freshness"
  paragraph explicitly withdraws in favor of no-TTL/self-insert-on-write.**
  Implemented per the withdrawal (no TTL, no timer, no 60s window — memoized
  for the process lifetime, refreshed only by `note_published`'s self-insert).
  Fixed both stale sentences in the design doc itself (Phase 1 section and
  Performance section) rather than leaving the contradiction for a reader to
  trip over.
- **`resolve_prior_note`'s signature takes no index parameter** (the doc's API
  Design section shows only `conn, vault_root, trace_id, primary_source,
  body_hash, intent`), so the "memoized for the process lifetime" index is
  necessarily internal (a module-private static), not caller-supplied. Same
  effect as a caller-owned index, correct seam for a signature the doc fixed.
- Step 2 (vault index) and step 3 (crash-recovery) do not special-case a
  multi-candidate match with an explicit ambiguity WARN the way the tombstone
  follower does — the doc only specs ambiguity handling for the STEM lookup.
  For steps 2/3, multiple index candidates are tried in `scan_vault`'s
  deterministic path-sorted order and the first guard-passing one wins; the
  three-term guard (trace+source+hash) is expected to narrow this to at most
  one legitimate match in practice (trace-collision is the only way step 2 can
  have >1 same-trace candidate, and the guard's source+hash terms are exactly
  what the doc says makes that collision non-destructive). No doc contradiction
  — just an implementation choice the doc left unspecified.

### Tradeoffs

- Full-vault `scan_vault` cost is paid once per process per vault root instead
  of amortized/streamed — accepted per the doc's own rejection of Alternative
  4 (O(vault) per publish) and its Performance section's math (~140 misses/night
  would make ANY rebuild-on-miss policy the dominant cost). A long-lived daemon
  process that both harvests AND has cortex actively moving notes around
  between publishes would see this index drift stale until `note_published`
  fires again — accepted because the receipts fast path plus re-stat already
  cover that case (Concurrency and failure modes: "External staleness... is
  covered by the receipts fast path... plus the re-stat before write").
- `VaultIndex` is `Clone`d out of the mutex on every `resolve_prior_note` call
  (rather than holding the lock or returning a guard) so the mutex is never
  held across the (parse-heavy) resolution work that follows — trades a
  per-call `HashMap`/`Vec` clone for lock-hygiene; the clone is cheap relative
  to the `scan_vault` it replaces and is only paid per publish, not per vault
  scan.

### Open questions

- None.

## Phase 2: harvest-run integrity

### Design decisions

- `WatermarkState::save` now serializes to bytes and calls
  `vault::note::write_atomic` directly rather than hand-rolling its own
  temp-then-rename - `write_atomic` already fsyncs the temp file and the
  parent directory, which is exactly the durability gap the doc names
  (`watermark.rs::save`).
- `record_published` gained a `trace_id: &str` parameter and now writes
  `PublishedEntry.trace = Some(trace_id)` on every fresh publish - the doc's
  Phase 4 section calls this field "`PublishedEntry.trace` (Phase 2)", i.e.
  Phase 2 is credited with actually populating it, not just adding an
  always-`None` schema field (which the doc's own "write-only fields are not
  mitigations" rule would otherwise flag). `classify_reappearance`'s
  snapshot-advance branch (unchanged n-msgs->hash, only `n_msgs` bumps)
  carries the PRIOR entry's `trace` forward unchanged, since that path is not
  a new publish (`watermark.rs::classify_reappearance`).
- Per-thread durable save is implemented as: `publish_thread_inner` calls
  `state.save(state_path)` immediately after `record_published`, inside the
  per-thread loop (`publish.rs::publish_thread_inner`). This required
  threading a `state_path: &Path` parameter through `publish_plan` ->
  `publish_thread` -> `publish_thread_inner`; the one production call site
  (`harvest::run_with`) already had `state_path` in scope. Because
  `state.cursor` is never touched anywhere in this per-thread loop (the doc's
  own constraint - it stays in `apply_plan_to_state` at end-of-run), a save
  here is a `published`-only persist BY CONSTRUCTION: no special-cased partial
  serialization was needed to satisfy "persisting `published` ONLY".
- A per-thread save failure is logged (`log::error!`, naming primary_id +
  trace_id + the error) but does NOT flip the thread's outcome to `Failed` -
  the note already landed by that point (mirrors the existing best-effort
  policy on the `members.yml` staging write two lines below it in the same
  function). Flipping to `Failed` would misrepresent a landed note as failed
  and would additionally cause `intake::record_failure_at_door` to write a
  false `FetchFailed` receipts row for a publish that actually succeeded.
- Duplicate `session_id` handling lives as a pre-pass in `plan_harvest`,
  before trace generation / selection: a `HashMap<String, SessionRecord>`
  keyed by `session_id` walks `export.sessions` once; a repeat that is
  `==` (derived `PartialEq`) to the first occurrence is dropped with a
  `log::debug!` and the run continues; a repeat that differs in ANY field
  calls `eyre::bail!` naming the session id, failing `plan_harvest` (and
  therefore the whole run) before any candidate from this export is selected,
  clustered, or published (`harvest.rs::plan_harvest`).
- `replay_session_stage2` acquires `watermark::acquire_lock` as its very
  first action (before even reading the staged body), unconditionally -
  including for a `--dry-run` session-trace replay. The doc frames this as
  serializing "timer-vs-manual" generally (Concurrency and failure modes
  section), not merely protecting the watermark file specifically, so the
  lock is taken for the whole function rather than carving out a
  dry-run-only exemption the doc never specifies (`replay.rs`). The lock
  error propagates via a bare `?` (no `.context()` wrapping), matching
  `harvest::run_with`'s own call site and cortex's `EmbedLockHeld` precedent,
  so callers can `downcast_ref::<HarvestLockHeld>()` the top-level error
  rather than substring-matching a message.

### Deviations

- None. The doc's four Phase 2 bullets and its four acceptance checkboxes are
  implemented as specified; no signature in the doc's own API section
  constrained this phase's seams, so no "doc says X, correct seam is Y"
  divergence arose.

### Tradeoffs

- A per-thread watermark save costs one `write_atomic` (temp file + two
  fsyncs + rename) per publish instead of one per run. Accepted: this is
  exactly the doc's tradeoff (durability over one syscall batch's worth of
  I/O), and harvest runs are nightly/on-demand, not a hot loop.
- The duplicate-`session_id` guard is a `HashMap` clone-and-compare pre-pass
  over the whole export rather than a streaming check woven into the
  existing single loop. Accepted for clarity: the doc's rule ("byte-identical
  collapses; ANY difference fails the whole run") reads far more directly as
  its own pass than interleaved with trace generation and the selection gate,
  and an export page is small (harvest runs paginated, `limit`-bounded)
  relative to the vault-scan costs the rest of this design is careful about.

### Open questions

- None.

## Phase 3: wire replace-in-place

### Design decisions

- **Intent rides on `ContentKind::Session`.** `publish_thread_inner` maps the
  planner's `Reappearance` to a `ResolveIntent` (`NewNote` -> `NewNote`,
  `FollowUp` -> `FollowUp`, and a `Skip` that should be unreachable -> logged
  `log::error!` + `FollowUp`, i.e. fail closed / never replace) and carries it
  in the variant; `pipeline::process_content` threads it to
  `process_session`. `replay::replay_session_stage2` bypasses `ContentKind`
  entirely and passes `ResolveIntent::Replay` straight to `process_session` -
  `borg/src/types.rs`, `borg/src/harvest/publish.rs`, `borg/src/replay.rs`.
- **Resolution happens BEFORE the distill pass**, not just before naming
  (`session.rs::process_session_inner`). The fail-closed path is a receipts DB
  error, and failing after an LLM call would burn a distill for a publish that
  cannot land. Nothing in the resolve inputs (trace, `clyde://<primary>`,
  `watermark::body_hash(body)`) depends on the distill.
- **`body_hash` is computed inside `process_session_inner` from the `body`
  argument**, so the live path and the replay path derive it from the same
  place by construction rather than by two callers agreeing.
  `borg/tests/body_hash_agrees_across_paths.rs` pins the remaining assumption
  (that staging holds those exact bytes).
- **The borg-owned key set is derived from a new
  `markdown::RENDER_NOTE_KEYS`** (declared next to `render_note`, the writer)
  plus `SESSION_OWNED_KEYS` plus `DISTILLER_OWNED_KEYS`
  (`session.rs::borg_owned_keys`). Two tests hold the derivation up:
  `markdown::tests::render_note_keys_matches_the_writer` renders every
  `ContentType` variant (enumerated through an EXHAUSTIVE match, so a new
  variant fails to compile until the matrix covers it) with every optional
  field populated and asserts emitted-keys == `RENDER_NOTE_KEYS` in BOTH
  directions; `session::tests::borg_owned_key_policy_matches_the_writer`
  asserts the session policy contains every writer key and that the three
  sources do not overlap.
- **`status:` is carried forward as a RAW `serde_yaml::Value`, not parsed
  through `vault::schema::Status`.** The doc's own acceptance criterion names
  `status: read`, which is not in the schema (`unread`/`reading`/`reviewed`/
  `starred`). Parsing would silently reset any off-schema operator value to
  `unread` - the exact failure the ownership change exists to prevent. On a
  replace, `NoteContent.status` is set to `None` and the prior value is
  re-emitted through `frontmatter_additions`, so exactly one `status:` line is
  written. Cosmetic consequence: on a replaced note `status:` renders in the
  additions block (after `tags`/`creator`) rather than its usual slot.
- **`read_prior_frontmatter` fails the publish CLOSED** on an unreadable or
  unparseable prior note rather than replacing it with a note that has
  silently lost its `cortex-*` fields. The resolver parsed that same file
  moments earlier, so either error means the file changed underneath us.
- **`record_landed_path` runs on EVERY session publish, hit or miss**
  (`receipts::update_note_path` + `identity::note_published`), per
  Architecture's "Phase 3 calls it on every session publish including replay"
  and "every successful publish inserts (trace, absolute path)". It is
  best-effort AFTER the note lands: failing an already-published note over a
  bookkeeping write would misreport what happened (same policy as Phase 2's
  per-thread watermark save).
- **`identity::reset_index_cache_for_tests`** (new, `#[cfg(test)]`) drops the
  memoized index for one vault root. The "note moved inbox/ -> notes/" case is
  a NEXT-PROCESS case in production (cortex promotes between borg runs); in a
  single test process the self-inserted index entry would otherwise pin the
  pre-move path. The test resets the cache to stand in for that next process,
  and then proves the receipts row is repaired.

### Deviations

- **The doc's PROSE list of `render_note`'s keys is under-inclusive** (flagged
  at the Phase 0 gate). `RENDER_NOTE_KEYS` therefore also carries `asset`,
  `capture-note`, `slides`, `duration`, and `language`. Session notes never
  take those branches, so no behavior differs; the doc's governing rule
  ("DERIVED FROM THE WRITER, not hand-listed") is what was implemented, and a
  hand-typed list from the prose would have failed the required
  policy-matches-writer test on day one.
- **`follows:` is deliberately NOT in `SESSION_OWNED_KEYS`,** though the doc's
  Data Model lists it among the borg-owned session additions. Nothing writes it
  until Phase 4, and until Phase 4 re-emits it on every render, owning it here
  would mean a replay DROPS a follow-up's back-link (a stage-2 replay has no
  way to re-derive it - it never sees the prior `PublishedEntry`). Left out, it
  is carried forward verbatim instead. **Phase 4 must move it into
  `SESSION_OWNED_KEYS` at the same time it starts emitting it**, or a
  re-derived `follows:` and a carried-forward one will fight (the carry-forward
  loop skips keys the publish already derived, so the derived value would win -
  but the ownership would be implicit rather than declared).
- **Verified stub-out of the required regression guard.** With
  `session::resolve_prior_note` temporarily replaced by `Ok(None)`,
  `borg/tests/replay_lands_same_note.rs` fails as designed:
  `["inbox/a-wholly-different-subject-line.md", "inbox/session-871f6428-work.md"]`,
  `left: 2, right: 1`. The stub was reverted and the guard re-run green.
- **The existing `pipeline::session` tests gained XDG isolation**
  (`XdgSandbox`, serialized on the shared `harvest::TEST_XDG_LOCK`). They
  previously wrote the operator's REAL success ledger; after this phase they
  would also read and write the real receipts DB. Not a spec deviation - a
  consequence of this phase that had to be handled rather than shipped.

### Tradeoffs

- `resolve_prior_note` and `record_landed_path` open the receipts DB
  separately (two short-lived connections per publish) instead of holding one
  across the whole handler. Holding a SQLite handle across a multi-minute LLM
  distill for the sake of one saved open is the worse trade.
- The prior note is read twice on a replace: once by the resolver (to apply
  the confirmation guard) and once by `read_prior_frontmatter` (to take the
  carried keys). Threading the parsed `Note` back out of `resolve_prior_note`
  would save the read but change the signature the doc's API Design section
  fixed. One extra read of one file per replace, against a phase-crossing
  signature change: not worth it.
- Integration-test harness lives in `borg/tests/common/mod.rs` with a
  module-wide `#![allow(dead_code)]`. Cargo compiles it separately into each
  test binary and each uses a different subset, so without the allow the
  `-D warnings` clippy gate fails on the unused half. This is the shared-test-
  helper case, not a suppressed warning about production code.

### Open questions

- None blocking. One handoff item for Phase 4, stated above: when `follows:`
  starts being written, add it to `SESSION_OWNED_KEYS`.

## Phase 4: follow-up back-link

### Design decisions

- **`ContentKind::Session` gained `follows_prior: Option<watermark::PublishedEntry>`**,
  threaded from `harvest::publish::publish_thread_inner`'s own match on
  `thread.decision` (mirroring how `intent` is already derived there):
  `Reappearance::FollowUp { prior }` -> `Some(prior.clone())`; `NewNote` and the
  fail-closed `Skip` arm -> `None`. `replay.rs`'s `replay_session_stage2` -
  which builds no `ContentKind` at all - passes `None` directly to
  `process_session` (`borg/src/types.rs`, `borg/src/harvest/publish.rs`,
  `borg/src/replay.rs`).
- **A second, distinct `ResolveIntent::Replay` call resolves the PRIOR note**,
  separate from the current publish's own `resolve_prior_note` call
  (`pipeline::session::resolve_follows_stem`). The prior entry's `.trace` is
  already known (Phase 2 populates it), so only the receipts-fast-path +
  vault-index steps apply (with tombstone-follow) - exactly `Replay`'s
  documented semantics ("the trace is authoritative, steps 1-2 only"). `NewNote`
  would pull in step 3's crash-recovery fallback (irrelevant - a trace is
  already known) and `FollowUp` refuses to resolve at all (wrong intent
  entirely for a lookup that ISN'T the current publish's own identity). This
  answers the task brief's explicit question: the current publish uses
  `FollowUp` (never resolves itself); the auxiliary prior-note lookup uses
  `Replay` (resolves by a known trace, steps 1-2 only) - two different intents
  for two different lookups against two different traces.
- **`follows:` stores a bare filename stem**, the same convention
  `superseded-by:` already uses, not a path and not the prior trace id. This is
  deliberate: Obsidian resolves a `[[stem]]` wikilink by filename across the
  WHOLE vault, not by directory, so the body link keeps resolving after a
  cortex move WITHOUT this handler re-resolving it on every later replay - the
  fresh-resolution-via-trace machinery only has to run ONCE, at the moment the
  follow-up is first published. A plain replay of that note (which carries no
  `follows_prior` - see `replay.rs`) instead carries the EXISTING stem forward
  unchanged from the note being replaced (`resolve_follows_stem`,
  `read_prior_frontmatter`'s new `follows` field, `process_session_inner`'s
  `follows_stem` fallback chain).
- **`follows` moved into `SESSION_OWNED_KEYS`** exactly as Phase 3's notes
  flagged it would need to. It is still "derived", not "carried generically":
  `read_prior_frontmatter` special-cases it alongside `status` (captured into
  `PriorFrontmatter.follows`, excluded from the generic `carried` map) so the
  handler controls precisely which of the two sources (fresh `follows_prior`
  resolution vs. carried-forward prior value) wins, rather than letting the
  blanket carry-forward loop silently decide.
- **The body wikilink is re-derived from `follows_stem` on every render**
  (`render_follows_link`, prepended to `distilled_body` right before
  `NoteContent` construction) rather than ever being read back out of the old
  body - the whole body is replaced by the fresh distill pass on every
  publish/replay, so anything not re-emitted from the frontmatter key would
  silently vanish, per the Data Model's explicit warning.
- **Never blocks the publish.** `resolve_follows_stem` returns `None` (WARN
  logged) rather than propagating an `Err` on: a missing `prior.trace`, a
  receipts-open failure, a resolution error, or a resolution miss (`Ok(None)`)
  - all four are exercised by
  `an_unresolvable_prior_note_omits_follows_and_never_blocks_the_publish`.

### Deviations

- None. `follows_prior`'s exact field name/shape and `resolve_follows_stem`'s
  seam are not fixed by the doc's API Design section (which only fixes
  `resolve_prior_note`'s own signature), so no doc-vs-implementation signature
  conflict arose here the way Phase 1 hit for the index.

### Tradeoffs

- `resolve_follows_stem` opens its own receipts connection (a second
  short-lived `Connection` alongside the current publish's own
  `resolve_prior_note` call) rather than threading one connection through both
  lookups. Accepted for the same reason Phase 3's notes accepted it for
  `resolve_prior_note`/`record_landed_path`: a short-lived open is cheaper than
  restructuring the call graph to share a handle across an LLM-distill-bounded
  function.
- Storing a bare stem in `follows:` (instead of the prior trace) means a
  RENAME of the target file (not a cortex directory move, which Obsidian's
  stem-based resolution already tolerates) would break the link with no
  automatic repair. Accepted: notes are immutable once published and cortex
  only moves between directories, it does not rename; `superseded-by:` already
  carries the identical risk/convention and this keeps `follows:` consistent
  with it rather than introducing a second addressing scheme.

### Open questions

- None.

## Phase 5: cortex fixes

### Design decisions

- **`group_by_session_identity` is two independent tracks, never mixed**
  (`association.rs::group_by_session_identity`): every note with a present,
  non-empty `trace:` groups by that value alone, exact match; a note with NO
  `trace:` falls back to transitive `cortex-session-ids` overlap (union-find).
  A legacy (no-trace) note is never absorbed into a trace-keyed group by
  sharing a session id with one of that group's members, and two notes with
  DIFFERENT non-empty traces are never grouped even if every
  `cortex-session-ids` entry matches - that is exactly the genuine follow-up
  case Phase 4 made real (`follows:`), not a duplicate to clean up.
- **The `superseded-by` skip guard was carried over verbatim** (same
  `if note.frontmatter.extra.contains_key("superseded-by") { continue; }` line,
  same position in the loop, before the trace/legacy branch) so an absorbed
  note is filtered out before it can even be classified into either track -
  it never re-enters `trace_groups` or the legacy union-find.
- **`resolve_collision`'s numeric-candidate loop re-applies
  `existing_note_has_source` to each `-N` candidate**, not just the base path
  (`classify.rs::resolve_collision`). The very first same-source candidate
  found in the `2..100` scan is returned as the overwrite target; the loop
  never reaches a `-N+1` slot once a same-source `-N` is found. This is prior
  attempt 4's guard, applied where the doc's Evidence section shows it was
  missing (`-5`, `-7` .. `-14` in the real cohort).
- **`resolve_collision`'s test fixtures write real files to a `tempfile`
  tempdir** (mirroring the crate's existing tempdir convention) rather than
  faking `existing_note_has_source`'s file read, because that function reads
  real bytes off disk with no seam to inject a fake - the correct-seam choice
  is a real file, not a mock.

### Deviations

- **None from the doc's two Phase 5 bullets themselves** (grouping mechanism,
  collision-loop fix) - both are implemented exactly as specified.
- **Five pre-existing `association` tests were updated, not left green by
  accident.** `apply()`'s only production caller of a grouping function now
  points at `group_by_session_identity` instead of `group_by_slug`. Two tests
  (`apply_executes_the_plan_and_writes`,
  `dry_run_reports_the_plan_and_writes_zero_bytes`) exercised `apply()`'s full
  group -> decide -> execute composition using two notes that shared only a
  hand-picked `slug: foo` and DIFFERENT primary session ids/no trace - under
  the new grouping they would never form a group at all, so both were
  silently passing for the wrong reason (an empty outcome list, not "the
  quiescence/exclude guard fired"). Both literally FAILED once
  `group_by_session_identity` landed (`left: 0, right: 1`) and were fixed, not
  papered over: their fixtures now share a `trace:` (added a
  `write_session_file_with_trace` variant of the existing fixture writer,
  used only where a real trace-keyed group needs to form) so the tests still
  exercise the genuine "same trace, not yet cleaned up, decide() picks
  Merge" case. Three more tests
  (`whole_group_is_skipped_when_any_member_is_within_quiescence_window`,
  `quiescence_skip_is_whole_group_never_half_merged`, `excluded_path_never_groups`)
  did NOT fail (they assert `outcomes().is_empty()`, which is trivially true
  whether the guard fired or the group never formed in the first place) but
  were silently defanged as regression coverage for their actual guards
  (quiescence, exclude) - updated to the same traced fixture so a broken
  quiescence/exclude guard would fail them again.
- **`associate_run`, the test-only helper in `tests.rs` that exercises
  `execute_merge`/`execute_cross_link` directly, deliberately still calls
  `group_by_slug`**, not `group_by_session_identity`: its fixtures
  (`write_session_file`) predate this design and carry no `trace:`, and its
  purpose is proving the merge/cross-link executors' section-union and
  tombstone logic against realistic bodies - not proving which grouping
  function `apply()` uses (`apply()`'s own tests do that). Left as-is with an
  updated comment (the old comment claimed it mirrored `apply()`'s
  composition, which stopped being true the moment `apply()`'s grouping
  function changed).
- **`NoteBuilder` gained a `.trace(&str)` setter** (`testutil.rs`) - the
  builder already threads every other `Frontmatter` field through a matching
  setter, and `trace` is a promoted (non-`extra`) field, so building a
  session-identity fixture without it meant reaching into `note.frontmatter.
  trace` by hand at every call site instead of one shared setter.
- **`association/tests.rs` split into `association/tests.rs` +
  `association/tests/session_identity.rs`.** Phase 5's new tests pushed
  `tests.rs` to 1597 lines, over the workspace's 1500-line `BLOAT_MAX_LINES`
  gate (`otto ci`'s `bloat` task). Followed the precedent already in the repo
  (`vault/src/search/tests.rs` + `tests/{group_a,group_b,trace}.rs`): shared
  imports/helpers stay in `tests.rs`, the new self-contained block (the
  `hv_e5d240_note`/`other_session_note` fixture builders and their four
  tests) moved to its own file with `use super::*;` picking up the parent
  module's imports.

### Tradeoffs

- The legacy fallback's union-find visits every `cortex-session-ids` entry of
  every no-trace note in one pass (`O(total ids)`), same asymptotic cost as
  the trace-keyed `BTreeMap` grouping it runs alongside. Accepted: this
  mirrors `group_by_slug`'s own single-pass-over-notes cost and `apply()`
  already pays one `scan_vault` per invocation, which dominates.
- `resolve_collision`'s fix re-reads each numeric candidate's frontmatter
  header (`existing_note_has_source`, a bounded 2048-byte read) instead of
  batching all `-N` reads up front. Accepted: the loop already stats each
  candidate path one at a time (`!candidate.exists()`), and the realistic
  fork count per collision is small (double digits at worst, per the doc's
  own `hv-e5d240` evidence), not worth restructuring into a batch read for.

### Open questions

- None blocking. Worth a note for a future reader: `group_by_slug` now has
  ZERO production callers (`apply()` is its only caller and now calls
  `group_by_session_identity`; every remaining reference is a test, either
  `group_by_slug`'s own unit tests or the `associate_run` test helper
  described above). It is kept, not deleted, per this phase's explicit scope
  (fix grouping and collision resolution, not retire the prior mechanism) -
  flagged here rather than silently removed or silently left implying it is
  still load-bearing.

## Phase 6: retire the existing forks

### Design decisions

- **New `borg::dedupe` module, not a `cortex` module.** `sb borg
  dedupe-sessions` groups and tombstones BORG's own duplicate publishes, so it
  lives in the crate that owns the writer it is cleaning up after. It does
  NOT depend on `cortex` (no `cortex` dependency was added to `borg/Cargo.toml`
  - checked: `cortex` does not depend on `borg` either, so a `borg -> cortex`
  edge would be structurally fine, but it would still cross the workspace's
  one-way capture/governance layering the root `CLAUDE.md` documents). The
  tombstone SHAPE (`slug:` stripped, `superseded-by: <stem>` inserted, body ->
  `Merged into [[stem]].`) is reproduced as a CONTRACT rather than imported
  from `cortex::association::tombstone_content` - `borg/src/dedupe.rs`'s
  `apply_group`.
- **Group by `trace:`, full stop - with one defensive addition beyond the
  doc's literal wording.** A trace bucket is split by `source:` before being
  accepted as a duplicate cohort (`dedupe.rs::split_by_source`). The doc's
  Risks table names a 32-bit trace collision as "vanishingly unlikely but not
  impossible" everywhere else in this design (the whole reason the
  resolution path in Phases 1-3 carries a three-term guard); grouping by
  `trace:` alone here would tombstone a genuinely different session that
  happened to collide into the same bucket. A same-trace, different-source
  split WARNs and evaluates each source's sub-cohort independently; a
  sub-cohort of size 1 is not a duplicate group. The real `hv-e5d240` cohort
  is unaffected (all 15 forks share one `source:`), so this changes nothing
  observable for the concrete fixture the doc names, only the collision edge
  case.
- **Survivor rule implemented as a single lexicographic tuple**
  `(is_clean, effective_timestamp, path)`, MAX wins
  (`dedupe.rs::survivor_key`). `is_clean` requires `distilled: true` AND
  neither degradation signal: `cortex-needs-review: true` (frontmatter) OR a
  literal `[missing-summary]`/`[yaml-parse-error]` marker in the body (the
  first line `distillers::validate::fallback_distilled` writes into `##
  Summary` on a degraded distill pass - there is no separate frontmatter flag
  for this, confirmed by reading the real `hv-e5d240` notes in the live vault:
  `distilled: true` is set on every one of the 15 forks, degraded or not, so
  it is a necessary but not sufficient gate exactly as the doc states).
  Reading the real cohort confirmed the is_clean predicate identifies exactly
  6 clean notes among the 15 (the doc's own count) and that the 4 notes
  ingested 2026-07-24 are all excluded, matching the doc's evidence exactly.
- **"Greatest receipts `terminal_at` if present, else greatest mtime" is
  PER-NOTE, not a group-wide tier switch** (`dedupe.rs::effective_timestamp`).
  A shared trace has exactly ONE receipts row, so `terminal_at` can only ever
  attach to whichever single fork the row's `note_path` currently names (most
  forks in a group will not match, and fall through to their own mtime).
  Both are converted to the same Unix-seconds scale so they compose into one
  `Option<i64>` field; `None` (both reads failed) still falls through to the
  path tie-break in the same tuple.
- **Backfill and tombstoning share one `run_with` pass** so a note this
  invocation is ABOUT to tombstone is excluded from the backfill target set
  (`tombstoned_this_run`, a `HashSet<&PathBuf>` built from the just-computed
  plan) - a hash is only ever useful on a live, resolvable note, and Phase 1's
  identity resolver never looks up a tombstone directly. A tombstone from an
  EARLIER run (already carrying `superseded-by:`) is excluded the same way,
  via the ordinary `is_tombstone` scan filter.
- **`--purge`'s candidate set unions two sources**: every on-disk Session note
  already carrying `superseded-by:` (covers `--purge` run alone, later, after
  an earlier `--apply`), plus this run's freshly-planned tombstones (which, on
  a dry run, are not yet reflected on disk or in the `notes` scan at all) -
  `dedupe.rs::run_purge`. This lets `--apply --purge` in one pass, or
  `--apply` now / `--purge` later, both work.
- **Inbound-link detection reuses one regex** (`\[\[([^\]|]+)(?:\|[^\]]+)?\]\]`)
  for piped, path-qualified, AND embedded links - an `![[embed]]` matches the
  same capture group because the `!` sits outside it, so no separate embed
  pattern was needed. `.base` views were deliberately NOT scanned: reading a
  real one (`system/views/borg-ledger.base`) confirmed they are property-filter
  queries (`filters:`/`properties:`/`views:` YAML), not literal `[[wikilink]]`
  references to specific notes - there is nothing there to break or resolve.
- **`rkvr::remove` (already `pub(crate)`) is reused verbatim for `--purge`**,
  not reimplemented - it already has the "prefer `rkvr rmrf`, fall back to
  non-recoverable removal when the binary is absent, bypass to the fallback
  under `cfg!(test)`" contract Phase 6 needs, with no changes.

### Deviations

- **Same-effect, correct seam: tombstone/backfill rewrites go through
  `Frontmatter::to_yaml()` + a full-file rebuild, not a targeted
  insert/remove-single-field text patch.** `cortex::association::
  tombstone_content` (which this phase's shape is modeled on) mutates the raw
  frontmatter TEXT in place via `cortex::scope::insert_frontmatter_fields`/
  `remove_frontmatter_fields`, preserving the original key order. `borg` has
  no equivalent helper (and, per the module-doc decision above, does not
  import `cortex`'s), so `dedupe.rs` instead parses the note via
  `vault::frontmatter::parse_frontmatter`, mutates the typed `Frontmatter`,
  and re-serializes with `.to_yaml()` - the exact pattern already used by
  `cortex::summarize::rewrite_note_file`. Effect is identical (every
  pre-existing key, including ones this design does not own like `domain:`
  or `cortex-classified:`, survives the rewrite byte-for-byte in VALUE, only
  possibly reordered to the writer's canonical order); a unit test
  (`apply_group_writes_the_tombstone_contract_shape`) asserts a non-owned
  field survives across the rewrite.
- **No other deviations from the doc's Phase 6 bullets or acceptance
  criteria.** `--apply`/`--purge`/dry-run-by-default, tombstone-not-delete,
  no wikilink rewrite, and backfill-report-not-skip are all implemented
  exactly as specified.

### Tradeoffs

- `run_purge`'s inbound-link index (`build_link_index`) is built from a full
  `scan_vault` pass regardless of how many tombstone candidates exist.
  Accepted: this is a maintenance command run rarely (operator-invoked, not
  nightly), and `association::apply` already pays the same full-vault-scan
  cost for the same class of reason.
- The purge test suite (`run_purge_*`) is exercised as `borg` LIB unit tests
  (`#[cfg(test)] mod tests` inside `dedupe.rs`), not `borg/tests/*.rs`
  integration tests. `rkvr::remove`'s `cfg!(test)` bypass to the non-recoverable
  fallback is only `true` when the CRATE ITSELF is compiled in test mode,
  which holds for the crate's own lib unit tests but not for a separate
  integration-test binary (which links a normal-mode build of `borg`) - an
  integration test would have shelled out to the REAL `rkvr rmrf` (or its
  NotFound fallback) and touched the operator's actual `~/.local/share/rkvr/`
  archive store. Lib-unit-test placement was the correct seam to keep the
  test suite from touching real machine state, matching this design's own
  "never touch the live vault" boundary.
- `plan_groups`' safety split (`split_by_source`) costs one extra `BTreeMap`
  pass per trace bucket with more than one candidate. Negligible: 22 of 208
  distinct sessions in the live vault currently own more than one note per
  the doc's own measured evidence, so this is a small, rarely-hit path.

### Open questions

- None blocking. Flagged for the operator, not a question: `sb borg
  dedupe-sessions` (dry-run) has NOT been run against the live vault at
  `~/repos/scottidler/obsidian` as part of this phase - per this phase's
  explicit scope boundary, only tempdir fixtures were exercised. Running the
  dry-run against the live vault, reviewing the printed plan (survivor +
  tombstoned set per trace, including the real `hv-e5d240` cohort), and then
  `--apply` (and later, separately, `--purge`) are the operator's own next
  steps.

## Post-implementation audit (review-panel round 1, Mode 2)

Synthesis: `/tmp/review-panel/MR3tal8b/synthesis.md`. Architect (Gemini) +
Staff Engineer (Codex), both rc=0. Panel call: ship after one fix. The panel also
ran verification neither seat ran: full suite 2371 passed / 0 failed, clippy
clean, and a READ-ONLY Phase 6 dry-run against the live vault
(`groups=20 backfilled=216 uncovered=0`; `hv-e5d240` survivor =
`notes/clyde-ci-yml-public-reusable-workflow-migration.md`, clean and distilled,
NOT `-5`) -- which verifies two Phase 6 acceptance criteria against real data.

### Must-fix, applied

- **`borg/tests/body_hash_agrees_across_paths.rs` did not catch the failure mode
  it exists for.** Proven by mutation: changing `thread_body_text`'s member
  separator from `"=== session "` to `"=== SESSION "` left the test PASSING. The
  test built its `live_text` by calling `thread_body_text` -- the same function
  the publish path calls -- so both sides of every assertion moved together. It
  pinned the cross-PATH invariant (staging holds the bytes the hasher saw; a
  third mutation confirms that half does bite) but NOT the cross-VERSION
  invariant that `harvest-body-hash:` continuity actually rests on, which is
  precisely what the doc's Phase 3 bullet says the guard is for.
  Fix: pinned the canonical text AND its SHA-256 to literals
  (`827b8566cdde7d6c24d37b38998aca91df150cf4e7df54e45574c1101b149b20`), with a
  comment stating that a failure here is a decision about every stored hash in
  the vault, not a test to update. **Re-ran the panel's mutation 1 after the
  fix: it now FAILS** ("the canonical thread-body format changed - every stored
  harvest-body-hash: is now stale"). Mutation reverted.

  This is the same defect class the doc was written about, one level up: the
  guard against "an invariant living in one file, falsified in another" was
  itself written so it could not observe that falsification.

### Cheap-wins, applied

- **Tombstone-shape drift across crates.** `cortex::association` and
  `borg::dedupe` each carried independent hardcoded `"superseded-by"` /
  `"Merged into [[{stem}]].\n"`, each pinned only by its own crate-local test, so
  changing one crate and its test left the other green. The panel proposed a
  cross-crate test in `sb`; `tombstone_content` is private, so instead the
  contract moved to the shared crate: NEW `vault/src/tombstone.rs` owns
  `SUPERSEDED_BY_KEY`, `SLUG_KEY`, and `redirect_body()`. Both WRITERS
  (`association::tombstone_content`, `dedupe::apply_group`) and the READERS
  (`association::group_by_slug`, `association::group_by_session_identity`,
  `harvest::identity`'s tombstone follower) now import it. Drift is now
  impossible rather than merely detectable -- consistent with the repo's
  schema-is-law rule against hardcoding shared strings in consumer crates.
  Deliberately NOT unified: cortex does text-level frontmatter surgery, borg
  parses and re-serializes. Key names and body text are the contract; YAML key
  order is not, and the module says so.
- **Misleading test name.** `borg_owned_key_policy_matches_the_writer` renamed to
  `borg_owned_key_policy_matches_the_declaration`, with a doc comment stating it
  matches `RENDER_NOTE_KEYS`, that the writer guard is
  `markdown::tests::render_note_keys_matches_the_writer`, and that the
  cross-cutting criterion is covered by the two together. The probe result
  (adding an undeclared key to `render_note`: first test fails, second passes)
  was correct behavior all along, just misnamed.
- **Phase 4 acceptance criterion reworded** from "pointing at the prior note's
  CURRENT path" to naming the current filename STEM, with the reasoning inline,
  so the criterion and the Data Model agree and the next auditor does not
  re-raise it.

### Corrected in the doc

- **Duplicate stem count, corrected twice.** Original doc said 32 vault-wide / 26
  excluding `system/`; the ready-to-build gate said 33/26; the audit re-measured
  33/**27**. The gate's 26 was wrong. Now 33/27 with max multiplicity 3.
- **The `follows:` bare-stem concern raised at Phase 4 was based on a bad
  premise, and is withdrawn.** "`review-ci-workflow-security-changes` appears 15
  times" counted the `slug:` FRONTMATTER VALUE, not filename stems -- those 15
  files have 15 distinct filenames. Measured decisively: **zero of the 33
  duplicate stems carries `trace: hv-`**, so no duplicate stem is a harvest note
  and `follows:` (which only ever names a harvest note) cannot currently be
  ambiguous. The staff engineer's proposed fix (store a path) would be strictly
  WORSE: `follows:` is written once and never re-resolved (a replace carries it
  forward verbatim, `session.rs`; a stage-2 replay passes `follows_prior: None`,
  `replay.rs`), so a stored path goes permanently stale on the first cortex move
  with nothing to repair it, while a stem survives. No code change; the doc now
  records the measurement.

### Deferred, NOT fixed (flagged for the operator)

- **`group_by_slug` is dead in production and `slug:` now has no code reader.**
  `apply` calls only `group_by_session_identity`; `group_by_slug`'s body was the
  last production reader of `slug:`. The doc keeps `slug:` for a separate stated
  reason (it should stop lying about the file it names), so this is disclosed
  rather than a deviation -- but `slug:` is now effectively human/Obsidian-facing
  only. Either delete `group_by_slug` or state that `slug:` has no code reader.
- **One vault note has malformed YAML frontmatter** (surfaced by the panel's
  dry-run: "line 4 column 49"; falls back to EMPTY metadata). Pre-existing, but
  it matters more now: such a note has no visible `trace:`, so it is invisible to
  both resolution and dedupe and would silently fork on replay. Out of scope
  here; wants an `sb cortex lint` pass.

### Open questions

- None blocking. The two defers above are the operator's call, and the Phase 6
  `--apply` / `--purge` runs remain un-run against the live vault.
