# Design Document: Fix the cortex daemon self-write oscillation loop

**Author:** Scott Idler
**Date:** 2026-07-05
**Status:** Implemented
**Review Passes Completed:** 5/5 + cross-model review panel (Architect/Gemini + Staff-Engineer/Codex), findings folded; implemented across 8 phases (commits 803255e..35fd828 on branch fix-cortex-oscillation-loop)

## Summary

The cortex governance daemon has been re-running a full governance sweep every ~5 minutes for 46 days, permanently latched into oscillation-backoff, while genuinely rewriting the daily digest note on every cycle. Three independent defects drive it: (1) the daemon's oscillation fingerprint counts *detections* (unfixable lint violations, unappliable link suggestions) instead of actual writes, so it is byte-identical every cycle and permanently trips the backoff; (2) `intel` and `sweep` are locked in a two-writer fight over the digest note's tags — the only real perpetual write — which fires the daemon's own watcher and clears the backoff latch; (3) the embed tick re-scans the same ~127 unembeddable notes every tick because nothing marks them "examined." Plus a 16 GB unrotated debug log whose live unit has drifted from its install template. This doc fixes each at its source and adds a regression test asserting the structural invariant: two consecutive steady-state sweeps produce an empty fingerprint.

## Problem Statement

### Background

`sb cortex daemon` watches the Obsidian vault (`~/repos/scottidler/obsidian/`) and, on a 300s poll interval and on file-change events, runs a governance sweep across `classify`, `link`, `duplicates`, `intel`, `auto-tag`, `sweep`, `broken-links`, `lint`, `state`, `quality`. Fixable violations are auto-applied. The daemon is a systemd user unit (`~/.config/systemd/user/cortex.service`) running with `--log-level debug`.

Because the daemon writes into the directory it watches, two guards already exist:

1. `applying: Arc<AtomicBool>` (`vault/src/watcher.rs`, `cortex/src/daemon.rs`) — set `true` around the periodic sweep so the watcher callback drops events.
2. Oscillation detection (`cortex/src/daemon.rs:208-256`) — if two consecutive periodic sweeps produce an identical non-empty fix fingerprint, latch `oscillating = true` and back off to classify-only until a real watcher event arrives.

The following are ALSO already correct and are NOT the bug (verified in code, stated so the fix does not re-invent them):

- Every **lint/sweep fixer** apply path writes only when serialized bytes change: `linking.rs:284`, `quality.rs:181`, `tags.rs:117`, `scope.rs:87`, `frontmatter.rs:314`. (Scoped claim: `intel` and `state` do NOT guard — `intel.rs:427` and `state.rs:198` write unconditionally; intel's is defect 2 below, state's did not appear in the oscillation fingerprint.)
- `vault::note::write_atomic` (`vault/src/note.rs:112`) is already temp-file → `sync_all` → rename → parent-dir fsync atomic; it is the single shared write seam.
- `quality::apply_quality` (`cortex/src/quality.rs:155`) has already converged: `already_set` guard (`:178`) + raw-string field writes round-trip cleanly. Quality was absent from the captured oscillation fingerprint.
- `vault/src/frontmatter.rs:264-266` sorts `extra` keys alphabetically, so `to_yaml` is deterministic. Frontmatter re-serialization is not a contributor.

### Problem

The daemon is permanently oscillating. Evidence from `~/.local/share/sb/cortex.log` (single append-only file, span **2026-05-20 to 2026-07-05, 46 days, 78 daemon restarts, 16 GB**) and the churned note on disk:

- Captured oscillation fingerprint (2026-07-05 16:00:08): `["link: 3 files", "sweep: 1 files", "lint: 2301 files"]` — byte-identical every cycle. Quality is absent (it converged).
- Today: 108 action cycles, one every ~5 min (matching `poll-interval: 300`); `oscillation detected` fired 49 times, `oscillation latched` 50 times.
- On disk, `notes/ai/daily/2026-07-05.md` currently reads `tags: []`; the log shows `rewrote … 1 -> 0 tags` for that one note **27 times**.
- The cycles are ~5 min apart, NOT the "4.6 cycles/sec" a first-pass diagnosis reported — that reading mistook a 46-day logfile for a 27-minute one. The severity is real (constant CPU bursts + Syncthing churn + a 16 GB log) but the mechanism is a slow perpetual loop, not a per-second storm.

Root-cause chain (all confirmed against the code by both reviewers):

1. **The oscillation fingerprint counts detections, not writes (phantom churn).**
   - **link (`link: 3 files`):** the link arm (`cortex/src/daemon.rs:539-557`) runs a lint pass and, if the lint report is non-empty, logs "applied wikilink fixes" unconditionally and fingerprints the **lint suggestion paths** — ignoring `apply_linking`'s real return (`fixed_count`, `cortex/src/lib.rs:228`; returned at `linking.rs:255`). The 3 files are perpetually-unappliable suggestions: the detection matcher `find_mention` (`cortex/src/linking.rs:344`, substring + `is_ascii_alphanumeric` boundary) disagrees with the mutation matcher `insert_first_wikilink` (`:540`, regex `\b{surface}\b` + `inside_structure` on a differently-sliced body). Detection emits a mention mutation cannot re-match → `new_content == content` → no write (guarded at `:284`) → lint re-suggests forever. Live log: only **5** actual `inserted wikilinks:` writes in the last 80 MB, versus a `link: 3 files` fingerprint every cycle.
   - **lint (`lint: 2301 files`):** the lint arm (`cortex/src/daemon.rs:500-509`) fingerprints **every violation path in the report** (`report.violations` at `:506`), including the majority with `fix: None` that are never auto-fixable (`tags.non-canonical`, `tags.orphan`, `frontmatter.date-format`, `frontmatter.enum.*`, `frontmatter.deprecated.*`; `cortex/src/frontmatter.rs:98-183`, `cortex/src/tags.rs:50-73`). The actual lint writers (`apply_naming`, `apply_frontmatter`, `apply_tags`, `apply_scope`) each guard on real change and converge. The 2301-file fingerprint is the stable set of permanent unfixable violations that recur on every scan by design.
   - Net: link + lint contribute a byte-identical non-empty fingerprint every cycle with **zero corresponding writes** — this permanently trips oscillation detection.

2. **The intel↔sweep two-writer fight is the only genuine perpetual write.**
   - `cortex/src/intel.rs:234` stamps a hardcoded `tags: [digest]` on the daily digest (weekly review = `tags: [review]`, `:330`), and the note is written **unconditionally** via `write_atomic` (`intel.rs:427`) every cycle — the daily/weekly render even calls the LLM every time (`intel.rs:245`, `:354`), so its bytes are nondeterministic (see Phase 2).
   - `digest` is not canonical (no `config/canonical-tags.yml` entry; `tag-mapping.yml` has `review: null` at `:3409`, no `digest`). So `sweep::migrate` → `canonical::filter_and_cap` (`vault/src/canonical.rs:124`) drops it, and `rewrite_note_tags` (`cortex/src/sweep.rs:250`) writes `tags: []`.
   - Next cycle intel re-stamps `[digest]`; sweep re-strips. `sweep` and `filter_and_cap` are each individually idempotent — the fight is the defect. This real write fires the watcher; the event lands after `applying` flips false (`daemon.rs:221-225`, `:245-249`; callback check `watcher.rs:92-97`), reaches the watcher-event arm, and that arm unconditionally clears the oscillation latch (`daemon.rs:227`) — so the next periodic sweep runs full again.
   - Compounding: the **scheduled** daily/weekly intel arms (`daemon.rs:258`, `:276`) are NOT wrapped in the `applying` guard at all (only the periodic-sweep arm is), so their writes always reach the watcher.
   - Secondary: `sweep::migrate` (`sweep.rs:174`) counts a note as `modified` whenever `new_tags != tags` even if `rewrite_note_tags` wrote nothing; the fingerprint should reflect real writes here too.

3. **Embed staleness never converges.** `stale_embedding_targets` (`vault/src/search/vector.rs:589-601`, TranscriptChunk arm) selects transcript-eligible notes where `e.id IS NULL OR e.source_modified_at < n.modified_at`. A transcript-eligible note with no `## Transcript` section is read by `process_transcript_batch` (`cortex/src/embed.rs:571-671`), skipped (`skipped_empty++`) **without writing any embedding row**, so `e.id` stays NULL → re-selected next tick. The `embedded == 0` halt (`embed.rs:318-334`) only breaks the inner loop within one tick; the next tick re-selects the identical ~127-note set (`skipped_empty=127` in live log). No "examined" marker exists.

4. **Each cycle is expensive.** `configured_actions` (`cortex/src/daemon.rs:446-714`) calls `vault::scan_vault` (2539 notes) up to 6 separate times per cycle plus LLM calls. Each sweep pegs the 8-thread rayon pool (`RAYON_NUM_THREADS=8`) for ~30-45s; recurring every 5 min is the observed load/heat.

5. **Unbounded debug log on a drifted unit.** The live `cortex.service` runs `--log-level debug`; `env_logger` writes one file with no rotation → 16 GB over 46 days (~350 MB/day). Critically, the live unit is a **hand-customized real file** (dated 2026-05-29, not manifest-tracked) carrying `RAYON_NUM_THREADS=8`, a secret-bootstrap `ExecStartPre` (`manifest age decrypt …/secrets/.secrets`), and `EnvironmentFile` — **none of which `install_systemd_service` (`daemon.rs:718-763`) emits.** The source default log level is already `info` (`config.rs:31`) and the template already threads `config.log_level` (`:737`), so a fresh `--install` would produce `info` — but it would also **clobber the rayon cap and secret bootstrap**, and `otto deploy` only restarts (never re-installs), so the live debug unit is untouched by a normal ship. The live `borg.service` has the identical drift (secret bootstrap at `keep/.secrets` + `--log-level debug`).

Blast radius: the vault is Syncthing-replicated, so every spurious digest-note write propagates to every peer. The fix is single-repo (`sb` workspace); ship is `otto deploy` + `systemctl --user restart cortex.service`, plus a one-time unit correction (Phase 6).

### Goals

- The oscillation fingerprint reflects **only files actually written**, never detections or `fix: None` violations.
- `intel` regenerates the digest only when its inputs changed (not a post-render byte compare — the render is LLM-nondeterministic), and the digest carries no tag `sweep` will strip.
- Scheduled daily/weekly intel writes cannot clear the oscillation latch via the watcher self-trigger.
- The embed staleness scan converges: a note examined and found unembeddable is not re-scanned until its indexed `modified_at` bumps.
- Link detection and mutation agree: every reported linking suggestion is appliable.
- One `scan_vault` per cycle with explicit rescan boundaries after mutating actions.
- The live daemon log is bounded and no longer `debug`, without losing the rayon cap or secret bootstrap.
- A regression test asserts the structural invariant — two consecutive steady-state sweeps produce an empty fingerprint — proven to fail on pre-fix code.

### Non-Goals

- Adding a write-only-if-changed guard (already present on every lint/sweep fixer) or a new write helper (`write_atomic` is the seam). Excluded — verified redundant.
- Changing `quality` (already converged). Excluded.
- Redesigning the governance rule set or fixer semantics beyond convergence/fingerprint correctness. Excluded.
- Replacing `notify`/inotify or the full "make the oscillation latch self-write-aware" redesign (Alternative 3). Excluded — Phase 2's `applying`-wrap of the scheduled arms plus the Phase 7 regression test are the targeted structural guard; the heavy latch redesign is unneeded once self-writes drop to zero.
- Redesigning embedding *kinds* (a `BodyChunk` for article body text). Parked with revisit condition: the Phase 3 sentinel stops the churn; whether article body should embed at all is a retrieval-semantics follow-up, revisited if article recall is poor.
- Fixing `borg.service`'s identical unit drift. Parked with revisit condition: same root cause (install template omits secret bootstrap), tracked as an immediate follow-up so the sibling stays consistent; excluded here to keep this doc scoped to the cortex oscillation. (Surfaced by the review panel; Scott to confirm scope — see note in Rollout.)
- Threading the glossary path through `LinkingConfig` for hermetic link tests. Parked follow-up (surfaced in Phase 7): `link_with_notes` reads the real `~/.config/sb/glossary.yml` with no test-redirect knob, unlike `SweepConfig`'s redirectable asset paths. Not required for the oscillation fix; a test-hermeticity improvement.

## Proposed Solution

### Overview

Fix each defect at its source, cheapest/most-deterministic first, LLM/expensive last; the regression test enforces the invariant so the class cannot recur.

### Architecture

- **Fingerprint source (`configured_actions`).** Change every arm to fingerprint the concrete written-path list the apply returns, never detection/suggestion paths. Requires a real API change on the lint appliers (below). Invariant: **fingerprint ⊆ files whose on-disk bytes changed this cycle.**
- **Lint apply API.** `lint` (`lib.rs:123`) currently runs `apply_naming`/`apply_frontmatter`/`apply_tags`/`apply_scope` and discards every return. Introduce `LintApplyReport { written_paths: Vec<String>, remaining_violations: usize }` returned from the lint apply path (covering all four appliers, `naming` included), and fingerprint `written_paths`.
- **intel idempotency.** Compute an **input-side idempotency key** (hash of the input note set + model + prompt) and persist it (recommended: an `intel-input-hash` field in the digest note's own frontmatter, or a small state entry). Skip regeneration — and therefore the LLM call — when the key is unchanged. A post-render byte compare cannot work: the LLM output is nondeterministic and is spliced into the digest bytes. Emit no non-canonical tag on generated digests.
- **Watcher self-trigger.** Wrap the scheduled daily/weekly intel arms (`daemon.rs:258`, `:276`) in the same existing `applying` guard the periodic sweep uses. (Full latch-redesign stays out of scope.)
- **embed sentinel.** On an "examined, nothing to embed" skip, persist a marker in a **side table / watermark, NOT a row in `note_embeddings`** (that table requires non-null text/embedding/dim at `search/schema.rs:100`, and `search_vector` scans all active-model rows without a sentinel filter at `vector.rs:207` — a tombstone row would poison cosine similarity). Re-qualification keys off the indexed `notes.modified_at`, not raw filesystem mtime.
- **link matchers.** Converge detection and mutation on one matcher (or gate lint suggestions through the `insert_first_wikilink` feasibility check) so every suggestion is appliable.

### Data Model

- `SweepFingerprint` (existing) — unchanged shape; the change is what feeds it (written paths only).
- `LintApplyReport { written_paths, remaining_violations }` — new return from the lint apply path.
- Embed "examined" marker — a new side table / watermark in the oracle SQLite index (cortex stays the only writer to embedding-adjacent tables, per `cortex/AGENTS.md`).
- intel idempotency key — `intel-input-hash` frontmatter field on the digest note (or equivalent state entry).

### API Design

- Lint appliers return their written paths, aggregated into `LintApplyReport`. No new cross-crate types beyond that.
- No new write helper — route through `vault::note::write_atomic`.

### Implementation Plan

Deterministic/cheap first; LLM/inference-bearing last. Each phase is independently committable and otto-ci-green.

#### Phase 0: Reproduce the fight + author the failing regression test
**Model:** sonnet
- Scripted two-cycle repro of the intel↔sweep fight on a temp vault, AND author the Phase 7 empty-fingerprint regression test now so it demonstrably bites on current `main` before any fix lands.
- **Success criteria:** (a) cycle-1 leaves the digest `tags: [digest]`, sweep rewrites to `tags: []`, cycle-2 restores `[digest]`; (b) the empty-fingerprint test FAILS on current `main` (the phantom fingerprint is non-empty).

#### Phase 1: Fingerprint only actually-written paths
**Model:** sonnet
- Introduce `LintApplyReport { written_paths, remaining_violations }` covering `apply_naming`/`apply_frontmatter`/`apply_tags`/`apply_scope`; fingerprint `written_paths`. Drop lint's use of `report.violations` paths and link's use of lint-suggestion paths; fingerprint `apply_linking`'s real applied paths and `rewrite_note_tags`'s real writes.
- This makes oscillation detection *more* accurate — a genuinely non-idempotent fixer still trips the latch; only phantom detection-churn stops tripping it.
- **Success criteria:** (a) a lint pass with zero writable fixes produces an empty fingerprint; (b) the fingerprint excludes every `fix: None` violation; (c) a unit test asserts `fingerprint ⊆ files whose bytes changed`.

#### Phase 2: End the intel↔sweep fight + contain scheduled-intel self-writes
**Model:** opus
- intel: compute an input-side idempotency key (input notes + model + prompt hash), persist it, and skip regeneration (and the LLM call) when unchanged. Emit no non-canonical tag on generated digests. Grep first that nothing downstream (oracle facets, `.base` views, queries) keys off `tags: [digest]` before removing it.
- daemon: wrap the scheduled daily/weekly intel arms (`daemon.rs:258`, `:276`) in the existing `applying` guard.
- **Success criteria:** (a) a second `intel` run on unchanged inputs makes **zero LLM calls and writes zero files**; (b) `sweep::migrate` reports 0 for `notes/**/daily/*.md` and digest/review notes across two runs; (c) a test in which scheduled intel writes during a latched-`oscillating` state, with a notify event delivered after `applying` flips false, does NOT clear the latch.

#### Phase 3: Embed "examined" marker
**Model:** opus
- Persist an examined-sentinel in a side table / watermark (not `note_embeddings`) so `stale_embedding_targets` excludes it until the indexed `notes.modified_at` bumps; verify `search_vector` returns no row for a sentinel note.
- **Success criteria:** (a) two consecutive `daemon_tick_with_model` calls on an unchanged vault — tick-2 `scanned == 0` for the previously-skipped set; (b) bumping a note's indexed `modified_at` re-qualifies it; (c) vector search returns no row for a sentinel note.

#### Phase 4: Reconcile link detection↔mutation
**Model:** sonnet
- One shared matcher (or gate lint suggestions through the `insert_first_wikilink` feasibility check) so every reported suggestion is appliable.
- **Success criteria:** (a) every `linking.*` violation, when applied, changes bytes or is not reported; (b) two consecutive link passes with no user edit produce 0 writes and 0 fingerprint entries.

#### Phase 5: Single scan per cycle with explicit rescan boundaries
**Model:** sonnet
- `configured_actions` scans once and shares `&[Note]`, but **rescans after any action that mutates the vault** (classify moves notes; taggers/frontmatter rewrite in place) before the next action that reads it. Define the boundaries explicitly rather than asserting "behavior unchanged."
- **Success criteria:** a cycle in which no action mutates the vault performs exactly one `scan_vault` call (assert via a counting fake); a cycle with mutations rescans exactly at the defined boundaries.

#### Phase 6: Correct the live unit + complete the install template
**Model:** sonnet
- Extend `install_systemd_service` (`daemon.rs:718-763`) to emit the complete unit the daemon actually needs: the secret-bootstrap `ExecStartPre` + `EnvironmentFile`, the rayon cap, `--log-level` from config (already `info`), and a bounded-log directive. Then re-install to correct the live drifted unit **without losing** the rayon cap or secret bootstrap. Pin a concrete rotation size + retention (choose a rotating-appender crate via `cargo add`, or a `logrotate`/systemd mechanism; state the size and file count in the doc when chosen).
- **Success criteria:** (a) `sb cortex daemon --install` emits a unit containing the secret `ExecStartPre`, `EnvironmentFile`, the rayon cap, and `--log-level info`; (b) after re-install + restart, the live unit no longer contains `--log-level debug` and still contains the secret bootstrap and rayon cap; (c) the daemon log is capped at the chosen size with the chosen retention.

#### Phase 7: Regression harness green (the structural guard)
**Model:** sonnet
- Verify the Phase 0 empty-fingerprint test now passes (after Phases 1-4), and that the scheduled-intel/watcher self-trigger test (Phase 2c) passes.
- **Success criteria:** two consecutive periodic sweeps on a fixture vault produce an empty fingerprint; the test bit on pre-Phase-1 code and passes now.

## Acceptance Criteria

- [ ] Two consecutive steady-state periodic sweeps over an unchanged vault: the second produces an empty `SweepFingerprint` and writes zero note files.
- [ ] `intel` run twice on unchanged inputs makes zero LLM calls and writes zero files; the daily digest is a fixed point of `intel` → `sweep::migrate`.
- [ ] The lint/link fingerprint contains no path that was not actually written (no `fix: None` violation, no unappliable suggestion).
- [ ] A scheduled-intel write during a latched `oscillating` state does not clear the latch.
- [ ] Two consecutive embed ticks over an unchanged vault: the second scans zero of the notes the first skipped as unembeddable.
- [ ] After Phase 6 re-install + restart, the live `cortex.service` contains `--log-level info`, the secret `ExecStartPre`, and the rayon cap; the daemon log is bounded.

## Resolved Decisions

- **2026-07-05 — write-only-if-changed guard rejected as the fix.** Every lint/sweep apply path already guards on byte change and `write_atomic` is atomic; the churn is fingerprint-of-detections + the intel↔sweep tag fight. (Author, from research brief; both reviewers verified.)
- **2026-07-05 — quality out of scope.** `apply_quality` already converged. (Author + both reviewers.)
- **2026-07-05 — the ~127 skipped notes are NOT mis-typed.** Verified `type: article` / `type: youtube`, both in `NoteType::transcript_eligible()` (`vault/src/schema.rs:215`), legitimately lacking a transcript. Phase 3 is a sentinel, not a type correction. (Author, verified on disk.)
- **2026-07-05 — intel emits NO non-canonical tag (OQ1 closed).** `Digest`/`Review` are `NoteType` variants (`schema.rs:160`); stamping them into `tags:` is a category error ("two signals never encode the same meaning"). Both alternatives (make canonical / path-exempt) encode or hide the schema violation. Both reviewers concur with the author's lean. *Scott may override.*
- **2026-07-05 — Phase 2 idempotency is input-side, not a byte compare (OQ raised by panel).** The digest embeds nondeterministic LLM output (`intel.rs:245`, `:354`), so a post-render byte compare rewrites forever. Idempotency key is hashed inputs, checked before the LLM call. (Both reviewers, strongest convergence.)
- **2026-07-05 — harden the watcher narrowly (OQ2 closed).** Wrap the scheduled daily/weekly intel arms in the existing `applying` guard + a notify-after-flip test; do NOT redesign the latch. The Phase 7 periodic-sweep test alone does not cover the scheduled-intel self-trigger. (Codex remedy; Gemini's "don't over-engineer the latch" caution honored.) *Scott may override.*
- **2026-07-05 — Phase 3 sentinel only; embedding-kind redesign deferred (OQ3 closed).** The sentinel stops the churn. Whether article body should embed via a dedicated `BodyChunk` is a retrieval-semantics follow-up, not this bug. (Both reviewers.)
- **2026-07-05 — systemd unit drift discovered.** The live cortex (and borg) units are hand-customized beyond the install template; a naive re-install clobbers secrets + rayon cap. Phase 6 completes the template so re-install is safe. (Both reviewers, verified against the live units.)

## Alternatives Considered

### Alternative 1: Write-only-if-changed guard at the note-write boundary
- **Why not chosen:** Already exists on every lint/sweep fixer; would fix nothing. The phantom churn is fingerprinting, not writing.

### Alternative 2: Just stop/disable the daemon
- **Why not chosen:** Abandons the feature's value; a fix that turns the feature off is not a fix (taste).

### Alternative 3: Make the oscillation latch fully self-write-aware / rebuild the watcher suppression
- **Why not chosen:** Over-engineering. Fixing the fingerprint (Phase 1) and the write fight (Phase 2, incl. wrapping the scheduled arms) removes self-writes; the residual race then never fires on a spurious write. Recorded for the road-not-taken.

### Alternative 4: Make `digest`/`review` canonical tags
- **Why not chosen:** They are `NoteType` values misused as tags; adding them to the tag vocabulary encodes the confusion. Schema-honest fix is emitting no non-canonical tag.

## Technical Considerations

### Dependencies
- Internal: `vault` (watcher, note, scan, search/vector, canonical), `cortex` (daemon, intel, sweep, linking, embed, lib). A rotating-log crate may be added via `cargo add` in Phase 6.

### Performance
- Single-scan-per-cycle removes ~5 redundant full-vault scans/cycle. Fingerprint + intel fixes drop steady-state note writes to zero, ending Syncthing churn. intel idempotency also removes a per-cycle LLM call.

### Security
- No change to `ProtectSystem=strict` / `ReadWritePaths`. Phase 6 must preserve the secret-bootstrap `ExecStartPre` exactly (it decrypts the daemon's env via `manifest age`); the template change carries it, it is not re-authored.

### Testing Strategy
- Every fix has a test proven to fail on pre-fix code (tests must bite): the Phase 0/7 empty-fingerprint regression, the intel input-hash idempotency test (zero LLM calls on unchanged inputs), the lint/link fingerprint-subset test, the scheduled-intel latch test, and the embed convergence test.

### Rollout Plan
- Single `sb` binary; ship via `otto deploy` then `systemctl --user restart cortex.service`. Because `otto deploy` does not re-install the unit, Phase 6 requires an explicit `sb cortex daemon --install` (post-template-fix) + `systemctl --user daemon-reload` + restart to correct the live drifted unit.
- One-time operator step: truncate/rotate the existing 16 GB `~/.local/share/sb/cortex.log`.
- Sibling follow-up (Scott to confirm scope): `borg.service` has identical drift; correct it via the same template-completion pattern in an immediate follow-up.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Phase 2 idempotency still churns because the key omits an input that varies | Med | High | Key covers input note set + model + prompt; test asserts zero LLM calls on unchanged inputs (bites) |
| Phase 6 re-install clobbers the secret bootstrap or rayon cap | Med | High | Template must EMIT both before re-install; AC (b) asserts they survive |
| Embed sentinel poisons `search_vector` | Med | Med | Side-table/watermark, never a `note_embeddings` row; AC (c) asserts no row returned |
| Reconciling link matchers changes which links get inserted | Med | Low | Phase 4 asserts appliability, not a specific link set |
| Phase 5 stale `&[Note]` changes action behavior | Med | Med | Explicit rescan boundaries after mutating actions; classify already runs first by design |

## Open Questions

(none — all closed; two decisions above are marked "Scott may override.")

## References
- Log: `~/.local/share/sb/cortex.log` (46-day span, oscillation evidence)
- Units: `~/.config/systemd/user/cortex.service`, `borg.service` (both hand-drifted)
- Research brief + cross-model review panel (Architect/Gemini + Staff-Engineer/Codex), 2026-07-05
- `cortex/AGENTS.md`, `vault/AGENTS.md`, root `CLAUDE.md`
- Known issue (linker over-linking): second-brain-known-issues memory
