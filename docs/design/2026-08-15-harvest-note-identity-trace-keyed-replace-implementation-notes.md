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
