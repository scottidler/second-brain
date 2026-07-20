# Design Document: Harvest End-to-End Completion (clyde + second-brain)

**Author:** Scott Idler
**Date:** 2026-07-20
**Status:** Ready to build (all review findings dispositioned; Open Questions empty)
**Review Passes Completed:** 5/5 (author) + review-panel: Architect (Gemini) + Staff Engineer (Codex) + Staff Engineer (Opus, credit-substitute) — all findings folded

## Summary

The harvest feature (Pathway #1: distill each Claude Code coding session into a vault knowledge note) was built across clyde + second-brain and shipped as `sb` v0.11.0. It has never produced a single note. A serde strictness bug in `borg/src/harvest/contract.rs` aborts the entire run against the real clyde catalog, the clyde `files-touched` release the multi-repo grouping depends on was never merged, and the nightly timer was never installed. This doc completes ALL necessary code across both repos so knowledge actually lands in the vault, deterministically, unattended, with tests that bite.

## Problem Statement

### Background

- Goal (Scott, 2026-07-17): "I want this knowledge harvested in Markdown notes IN my Obsidian Vault." Restated 2026-07-20: "add to my knowledge stored in the obsidian vault."
- Vision doc: `docs/design/2026-07-18-harvest-knowledge-goals.md`. Two pathways over clyde's export contract:
  - Pathway #1 (WHAT): distill sessions -> vault `inbox/` notes. Built as `2026-07-17-harvest-clyde-sessions.md`, phases 0-13, shipped v0.11.0.
  - Pathway #2 (HOW, paramount): mine how Scott works -> candidate edits to `rules/`, `CLAUDE.md`, `VOICE.md`, `IDENTITY.md`. Explicitly undesigned, its own future doc.
- clyde (`tatari-tv/clyde`) is the raw tap: it parses `~/.claude/projects/**/*.jsonl` into `sessions.db` and exposes `clyde session export` (versioned JSON contract). That parsing works.

### Problem

harvest is built end-to-end but DEAD ON ARRIVAL. Verified this session:

1. **Contract parse aborts the whole run.** `contract.rs` types `cwd`, `created`, `title`, `first_prompt` as non-null `String`; clyde emits JSON `null` for all four (untitled / one-shot / empty sessions). The payload is one `serde_json::from_slice` over the full `sessions` array, so ONE null-string field in ANY session in the window kills the batch. Live catalog: 9 null titles, 11 null first-prompts in 60d; the default 7d window also trips it.
   - Reproduced: `sb borg harvest --dry-run --since 60d` -> `invalid type: null, expected a string at line 796 column 19`.
2. **A null `created` ALSO aborts downstream, past any type fix.** Even after `created` becomes present-null, `borg/src/harvest/cluster.rs:96` does `parse_ts(&r, "created", &r.created)?` and returns `Err` for the WHOLE plan on an absent/unparseable `created`; selection (`select.rs`) never guards it. So the type fix alone leaves it dead on arrival. (Staff Engineer, code-verified.)
3. **Zero output, ever.** 0 vault notes with `source: clyde://`; 0 `entities/repo-*.md` hubs; 0 `session`-kind receipts; `harvest-state.json` never written.
4. **clyde `files-touched` never shipped.** Coded on local branch `files-touched-export` (unmerged, unpushed, untagged, uninstalled). Installed `clyde v0.10.1` does not emit `files-touched`/`repos-touched`. Multi-repo subject grouping has no data.
5. **Multi-repo bridging in sb is deferred/unbuilt.** `repos_touched` is modeled (`Option<Vec<String>>`, `frontmatter.rs:59`) and carried but not consumed: no DB column, no bridge edge; and `collect_stubs` (`hub.rs:193-204`) mints hubs only from `frontmatter.repo`, never `repos_touched`.
6. **Never scheduled.** `sb-harvest.timer` code exists but is not installed; `otto deploy` only restarts existing units. `mode` never flipped from dry-run to live.
7. **The bug class can recur.** The Phase 0 contract spike validated against a hand-curated 8-session fixture that never contained a null title/cwd/created, so the strictness gap survived 5 review passes and 13 phases.

### Goals

- harvest lands distilled knowledge notes in the vault `inbox/` from real sessions. (Scott, 2026-07-17, 2026-07-20)
- Complete ALL necessary code across BOTH repos: no incomplete, no green-but-broken. (Scott, this invocation, 2026-07-20)
- Multi-repo subjects group deterministically: a session touching repos X+Y is visible under both, forward AND historically. (Scott, `harvest-knowledge-goals.md` decision A2 + "SEE the subject across sessions"; historical scope confirmed 2026-07-20)
- Runs unattended nightly with zero manual effort. (Scott, 2026-07-03 draft)
- Tests bite: the null-tolerance and monotonicity guarantees have regression tests that fail when the guarantee is removed. (Scott, `rules/taste.md` "tests must bite")

### Non-Goals

- **Pathway #2 (mine HOW Scott works -> rules/CLAUDE.md/VOICE.md/IDENTITY.md).** Paramount but explicitly its own design doc per `harvest-knowledge-goals.md` §1. Parked. Revisit condition: Pathway #1 lands notes reliably across a one-week live soak.
- **Multi-host harvesting** (laptop sessions). Catalog is per-host; desk.lan is the daemon host (clyde fold-in ruling). Excluded.

(Historical multi-repo backfill was considered a Non-Goal in draft; Scott pulled it into scope as Phase 7 on 2026-07-20. See Resolved Decisions.)

## Proposed Solution

### Overview

Seven phases. The contract fix (Phase 1) is independent of clyde and unblocks single-repo harvest immediately: notes start landing. The clyde release (Phase 3) and multi-repo bridging (Phase 4) add the "see a subject across repos" capability going forward. Timer (Phase 5) makes it unattended. Hardening (Phase 6) makes the bug class impossible to reintroduce. Phase 7 backfills historical multi-repo subjects once the forward spine is live.

### Architecture

- **Anti-lossy seam is the contract.** clyde exposes raw truth and does ZERO knowledge work; borg owns 100% of selection, clustering, distillation, grouping. No knowledge logic ever leaks into clyde (`harvest-knowledge-goals.md` §2). This doc holds that line.
- **Two-layer grouping** (unchanged from goals doc §3a): the deterministic `(cwd, git-branch, gap)` micro-thread decides what becomes ONE note; the repo HUB gathers every note carrying a `repo:` across sessions/days/runs. Multi-repo bridging is a note joining MULTIPLE repo hubs, keyed on `repos-touched`, never on cosine.

### Data Model

- **sb `SessionRecord` (contract.rs):** borg's contract already tolerates unknown clyde fields (no `deny_unknown_fields`; that is why the 3d window parsed at all), so the fix scope is only the fields borg DECLARES as non-null `String` that clyde can null: `cwd`, `created`, `title`, `first_prompt`. Each becomes present-null. Already-Option (`repo`, `git_branch`, `model`, `summary`) are fine.
- **`host`/`scope` stay non-null (resolved, code-verified):** clyde emits `host` non-null and re-derives `scope` via `scope::classify(cwd)` (`clyde/sessions/src/export.rs:116-120`), so both are always present. Not relaxed.
- **`BodyMessage.role`/`text` get a defensive Option** as future-malformed-element tolerance on the `--with-body` path, NOT because clyde nulls them today (it constructs them non-null, `clyde/sessions/src/db/query.rs:205`). Labeled as such in code.
- **Null `created` is rejected before clustering (not just parsed):** widening the type stops the parse abort, but `cluster.rs:96` errors the whole plan on an absent/unparseable `created`. Phase 1 adds a selection-stage rejection for a null/unparseable `created` (receipted, `rejection.yml`) before clustering. clyde exposes `created` nullable (`export.rs:129`); `modified` is non-null (`export.rs:130`), so only `created` needs this guard.
- **Per-record resilience (new parse API):** `parse_export` (`contract.rs:171`) changes shape from one `serde_json::from_slice` to element-by-element deserialize returning `(records, parse-rejections)`. A malformed record is skipped, logged (WARN; session-id when readable, byte-offset fallback when the id itself is unreadable), and carried out as a parse-stage rejection so it gets a receipts row + `rejection.yml`. This boundary does not exist today (`reader.rs:144` calls `parse_export` directly; rejections are only minted later in `plan_harvest`, `harvest.rs:162`). The schema-version check stays fail-closed (a wrong MAJOR still refuses the whole run).
- **clyde `files-touched`/`repos-touched`:** additive export fields (already coded on branch); on-disk catalog `SCHEMA_VERSION` bumps to 6, `EXPORT_SCHEMA_VERSION` stays 1 (additive-within-major).
- **sb `repos-touched` consumption (resolved: reuse, no new field):** reuse the EXISTING `repos-touched` frontmatter (`vault/src/frontmatter.rs:59`, serialized `:312`). Add a `repos_touched` column on `notes`, bound in upsert, carried on `GraphNoteRow`, driving a deterministic multi-repo-member edge. `collect_stubs` (`hub.rs:193-204`) must iterate `repos_touched` too (dedupe when it equals `frontmatter.repo`), or a secondary repo's hub is never minted and its bridge edge is silently dropped (`graph.rs:22-25` skip-if-dst-missing).
- **`repo:` canonical form:** stored form is clyde's `<org>/<repo>`; inbound values normalize to it; a non-normalizable value is rejected loudly and mints nothing (`validate_repo_slug` at `vault/src/schema.rs:453`; confirm it is wired at harvest emission and hub minting). Hub slugs come from `repo_hub_slug` (`cortex/src/hub.rs:80`), never a hand-written `repo-x` placeholder.

### API Design

- No new user-facing verbs. `sb borg harvest [--dry-run] [--since] [--install]`, `sb cortex hub --apply|--synthesize`, `sb cortex graph`, `sb cortex concept-promote` already exist. `harvest.mode: dry-run|live` in `~/.config/sb/borg.yml` controls what the timer runs.

### Implementation Plan

#### Phase 0: Real null-bearing fixtures + failing regression (spike)
**Model:** sonnet
- Capture REAL `clyde session export` payloads covering the FULL null class: null `title`/`first-prompt`/`cwd`/`created`, the already-Option null classes (`repo`/`git-branch`/`model`/`summary`), a malformed/undeserializable record, and an empty body. Redact bodies to benign placeholders. Store under `config/eval/distill-fixtures/session/`.
- Add a parse test feeding those fixtures to `contract::parse_export` that currently FAILS (documents the bug, locks it).
- Zero production code.
- **Success criteria:** a named test (`parse_tolerates_null_string_fields`) exists and is RED against current `contract.rs`; fixtures are real exports, not hand-authored.

#### Phase 1: Contract null-tolerance + per-record resilience + created guard
**Model:** opus
- Relax `cwd`/`created`/`title`/`first_prompt` to present-null. Leave `host`/`scope` non-null. Defensive Option on `BodyMessage.role`/`text` (labeled future-malformed).
- Fix the call sites the relaxation breaks, behavior-preserving (Opus SE C4): `select.rs:149` exclusion match on `title`/`first_prompt` -> `.as_deref().unwrap_or("")` (a `None` matches no pattern); `pipeline/session.rs:160` empty-title check must handle `None` (the `Session <id>` fallback today handles empty string, NOT `None`).
- Change `parse_export` to per-record deserialize returning `(records, parse-rejections)`. A skipped record lands a DURABLE `received->rejected` receipt keyed by `session_id` (extract it by parsing the element as `serde_json::Value` first, since `session_id` is always present), so a skip is loud, `sb doctor`-visible, and replayable, never silent. On a LIVE run the receipt is written BEFORE the watermark advances (`harvest.rs:157` sets `new_cursor = export.cursor`, so an un-receipted skip would be lost forever). Dry-run persists nothing and advances no watermark, so a WARN-only skip there is safe by construction. Keep schema-version fail-closed.
- Add a selection-stage rejection for a null/unparseable `created` so it never reaches `cluster.rs:96`.
- **Success criteria:** Phase 0 test goes GREEN; `sb borg harvest --dry-run --since 60d` exits 0 with `publishable > 0`, and `--since 7d` exits 0 (no parse abort). NOTE (2026-07-20, code-verified in Phase 1): `--since 7d` cannot yield `publishable > 0` because clyde defaults `--dormant-after 7d` and dormancy is the first selection gate, so a 7d window is mutually exclusive with a 7d dormancy floor (live: 0 dormant of 174). The `publishable > 0` assertion therefore rides the 60d window only; `--since 7d` proves the abort is gone. A corrupted single record in a fixture is skipped (test asserts the rest still parse) AND a LIVE-path skip writes a `received->rejected` receipt before the cursor advances; a null-`created` fixture row is rejected (receipted), not fatal.

#### Phase 2: Prove single-repo harvest live (VERIFICATION GATE, not a peer code phase)
**Model:** sonnet (any fixes this surfaces ride as their own small commits, per Opus SE K3)
- Run `sb borg harvest --since 60d` as an explicit on-demand LIVE invocation on the daemon host. Do NOT flip the config `mode` here; the timer's dry-run soak -> live flip lives once, in Phase 5.
- Verify notes land in `inbox/` with `method: harvest`, non-empty `claims`, `source: clyde://`, `repo: <org>/<repo>`.
- Run the EXACT hub path (not `cortex sweep`, which is tag/proposal/cold-sweep only, `sweep.rs:42`): index the note into the search DB -> `sb cortex hub --apply` (mint repo-hub stubs) -> `sb cortex graph` (wire repo-member edges) -> `sb cortex hub --synthesize` (render the hub body from `hub_members`, `vault/src/search/graph.rs:236`).
- Fix whatever the live run surfaces (repo canonicalization, distillation degradation, filename collisions).
- **Success criteria:** >= 1 note in `inbox/` with `method: harvest` and non-empty claims; after hub-apply -> graph -> synthesize, >= 1 `entities/repo-<slug>.md` hub whose body wikilinks its member notes; `sb doctor` shows session receipts.

#### Phase 3: Ship clyde files-touched
**Model:** sonnet
- Merge `files-touched-export` -> `main` in `tatari-tv/clyde` via PR: main is CONFIRMED gated (Opus SE C5: classic protection with code-owner reviews + org rulesets `deletion`/`non_fast_forward`/`workflows`), so PR-with-code-owner-review is mandatory, no direct push. Then bump, tag, `cargo install`, `clyde reindex --reparse` on the daemon host.
- Version/tag reality (Opus SE C5): main and branch are BOTH `0.10.1` (version does not distinguish them), and the last tag `v0.9.1` predates the entire `session export` contract, so this release cuts the FIRST tag ever to contain session export. The bump level is a human call at release time.
- **Success criteria:** `clyde session export --limit 1` emits `files-touched`/`repos-touched`; `clyde --version` matches a tag on `origin/main`; the live `sessions.db` is at `user_version = 6`.

#### Phase 4: Multi-repo bridging in sb
**Model:** opus
- Consume `repos-touched` (reuse the existing frontmatter, no new field): add the `notes.repos_touched` column + upsert binding + `GraphNoteRow` plumbing + a deterministic multi-repo-member edge.
- `collect_stubs` mints a Repo hub stub for EVERY validated element of `repos_touched` (dedupe against `frontmatter.repo`), so no secondary-repo edge drops silently.
- **Success criteria:** a note carrying `repos-touched [X,Y]` joins hub `repo-<slug(X)>` AND `repo-<slug(Y)>` (real `repo_hub_slug` output) on every sweep; the frozen-corpus determinism test covers the multi-repo case AND the `None` vs `[]` vs populated distinction byte-for-byte.

#### Phase 5: Install the timer, soak, flip live
**Model:** sonnet
- Wire `sb borg harvest --install` (writes `sb-harvest.service` + `sb-harvest.timer`) into `sb bootstrap` ONLY, where unit-writing belongs per `CLAUDE.md`. `otto deploy` stays restart-only (documented contract preserved). Run `--install` once on the current daemon host. Update `CLAUDE.md` to document the new units.
- Soak in `mode: dry-run` for one cycle, review selections, flip `mode: live`.
- **Success criteria:** `sb-harvest.timer` is installed + enabled; two consecutive runs double-ingest nothing (watermark holds); a live timer run publishes a note.

#### Phase 6: Regression hardening + eval
**Model:** sonnet
- The eval fixture set (from Phase 0) covers every null/edge class; assert each nullable field's revert turns a named test RED.
- Extend the monotonicity + frozen-corpus determinism harness to cover repo + multi-repo membership and `apply_linking` add-only.
- Break-the-code negatives: removing the `Option` on a nullable field, the per-record skip, or the `created` guard MUST each fail a named test.
- Add a standing `sb doctor` drift guard (Opus SE K2, mirroring the existing `degraded_24h` pattern): WARN when the harvest timer has RUN but produced ZERO `session` receipts over N days. This is the durable structural guard against a FUTURE clyde contract drift that frozen CI fixtures cannot see.
- **Success criteria:** named tests exist and pass; reverting Phase 1's Option on any nullable field, or the per-record skip, turns a test RED (proves the tests bite); `sb doctor` WARNs on a simulated zero-receipt-since-timer-run state.

#### Phase 7: Historical multi-repo backfill (one-time)
**Model:** opus
- One-time LLM pass over pre-`files-touched` sessions whose transcripts are still un-reaped, proposing cross-repo bridges as approve-gated hub-body wikilink diffs (never applied silently; goals-doc §3a attachment mechanism). Gates on Phase 4 live + clyde `files-touched` shipped.
- Scope-bounded: runs ONCE over history, reach limited by surviving transcripts; fail-closed + receipts discipline applies; every proposal carries provenance (which sessions drove it).
- **Success criteria:** the pass emits approve-diffs (hub-body `[[member]]` additions) with per-proposal provenance; a forced-failure of the LLM pass yields ZERO proposals + a visible error, never silent partial output; no landed note is modified.

## Acceptance Criteria

- [ ] `sb borg harvest --dry-run --since 60d` exits 0 with `publishable > 0` against the live catalog, and `--since 7d` exits 0 (no parse abort); a null-`created` and a malformed record are each rejected (receipted), not fatal. (The `publishable > 0` assertion rides the 60d window only: a 7d `--since` window is mutually exclusive with clyde's default 7d dormancy floor, so it cannot select a dormant session -- code-verified 2026-07-20.)
- [ ] After a live run, `inbox/` contains >= 1 note with `method: harvest`, `source: clyde://`, and non-empty `claims`; `sb doctor` reports >= 1 `session` receipt.
- [ ] A note carrying `repo: X` joins hub `repo-<slug(X)>` on every sweep; a note carrying `repos-touched [X,Y]` (post-Phase 4) joins BOTH hubs deterministically, with both hub stubs minted.
- [ ] `clyde session export` emits `files-touched`/`repos-touched` from a released tag on `origin/main`; installed `clyde --version` matches it.
- [ ] `sb-harvest.timer` is installed and enabled via `sb bootstrap`; two consecutive runs double-ingest nothing.
- [ ] Reverting the null-tolerance on any clyde-nullable field, the per-record skip, or the `created` guard turns a named regression test RED (the tests bite).
- [ ] Shipped + probed: `sb` bumped past v0.11.0 with a tag on `origin/main`, installed `sb --version` matches, and a post-deploy live probe produces a vault note.
- [ ] A LIVE-path parse skip writes a durable `received->rejected` receipt keyed by `session_id` before the watermark advances; `sb doctor` WARNs when the timer has run but produced zero session receipts over N days.
- [ ] Phase 7 backfill emits approve-diffs with provenance and modifies no landed note; a forced LLM failure yields zero proposals + a visible error.

## Resolved Decisions

- **2026-07-20 (Scott): historical multi-repo backfill is IN SCOPE as Phase 7**, overriding the review panel's + Opus Staff Engineer's + author's park recommendation. Rationale: the highest-value multi-repo subjects (okta-auth-rs + target-repo) are historical; forward-only bridging leaves them fragmented until their transcripts reap. Sequenced last (gates on Phase 4 live) because it is the one LLM/nondeterministic piece.
- **Per-record parse skip must be DURABLE, not silent** (Opus SE K1): a skipped record lands a `received->rejected` receipt keyed by `session_id` before the watermark advances, visible in `sb doctor`/`sb borg log`. Type-widening remains the actual fix for the known contract; the skip is the future-drift defense, recorded loudly (consistent with harvest's "never limp along silently" founding contract, `contract.rs:7-9`).
- **Phase 2 is a verification gate, not a peer code phase** (Opus SE K3): it proves "done means live" via an explicit on-demand run; fixes it surfaces ride as their own commits; the timer's dry-run soak -> live flip lives once, in Phase 5.
- **repos-touched frontmatter reused, no `related-repos`** (panel, code-verified `frontmatter.rs:59`). A new field is unrequested churn against "names tell the truth / siblings behave identically."
- **`host`/`scope` stay non-null** (panel, code-verified `export.rs:116-120`). clyde always emits them.
- **Phase 2 hub path = index -> `hub --apply` -> `graph` -> `hub --synthesize`; `cortex sweep` is NOT the path** (panel, code-verified `sweep.rs:42`, `graph.rs:236`). Closes the "does Phase 2 need full sweep" question: hub-apply-before-graph is mandatory.
- **Per-record parse resilience over widen-types-only** (panel; Alternative 3). One future unexpected null must degrade to a skipped record, not abort ~1500 sessions.
- **Timer install scoped to `sb bootstrap`; `otto deploy` stays restart-only** (panel). Preserves the documented `CLAUDE.md` deploy contract; `CLAUDE.md` updated when the units ship.

## Alternatives Considered

### Alternative 1: Disable sccache / hand-edit versions to force a green build
- **Why not chosen:** not the problem. The feature is broken at the data-contract layer, not the build layer. Rejected on sight.

### Alternative 2: Make only `title` nullable (the one field at line 796)
- **Description:** Patch the single field the reproduction tripped on.
- **Why not chosen:** clyde emits null for 13 string fields; a one-field patch leaves the same landmine under `first_prompt`, `cwd`, `created`, and the `--with-body` path. Fix the class, not the instance (`rules/taste.md`: "comprehensive regression tests so the bug class cannot recur").

### Alternative 3: Keep whole-batch fail-closed parsing, just widen the types
- **Description:** Relax the field types but keep one `from_slice` over the whole array.
- **Why not chosen:** a single future unexpected null (a field we did not anticipate) would again nuke ~1500 sessions. Per-record resilience matches the "fail on the edge, still publish the knowledge" discipline the rest of the design already uses (a malformed `repo:` skips the edge but still publishes the note).

### Alternative 4: Ship multi-repo bridging on a semantic (cosine) guess now, skip the clyde release
- **Description:** Bridge multi-repo subjects with similarity instead of `files-touched`.
- **Why not chosen:** decision A2 already chose the deterministic end state over shipping sooner. Cosine bridging reintroduces the nondeterminism the panel parked. clyde grows the field; forward bridging stays deterministic; the historical semantic pass (Phase 7) is the ONE bounded, approve-gated exception.

## Technical Considerations

### Dependencies
- clyde `files-touched-export` branch (built, needs release). Cross-repo, work org.
- fastembed model + fabric binary for distillation (already deployed).

### Performance
- Nightly batch, tens of candidates, few selected threads/day. Distill cost bounded by selection + token cap. No pagination until it is a problem. Phase 7 is a bounded one-time pass.

### Security
- Sessions can be work or personal; the note carries `scope-work`/`scope-personal` and `redacted-source` when `redaction_count > 0`. No new secret custody. clyde parsing strips tool blocks; bodies are consumed intermediates.

### Testing Strategy
- Real null-bearing fixtures (Phase 0). Per-record skip tests + `created`-guard test. Frozen-corpus determinism + monotonicity harness covering repo + multi-repo + `None`/`[]`/populated (Phase 6). Break-the-code negatives.

### Rollout Plan
- Fix-forward on the shipped `sb` v0.11.0 (no revert). Per-phase commit, otto ci green each; version bump + push + `otto deploy` at the end per `/how-to-execute-a-plan`.
- Ship order: Phase 1 (sb contract fix, unblocks notes) -> Phase 2 (prove live) -> Phase 3 (clyde release) -> Phase 4 (bridging) -> Phase 5 (timer) -> Phase 6 (harden) -> Phase 7 (historical backfill). Phases 3/5/6 can interleave; Phase 4 gates on Phase 3; Phase 7 gates on Phase 4 live.
- First timer week: `mode: dry-run`, review selections, then flip live.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Another unanticipated null field | Med | High | Per-record skip+log (Phase 1) degrades gracefully instead of aborting |
| Null `created` slips past to clustering | Med | High | Selection-stage `created` guard + dedicated test (Phase 1/6) |
| Secondary-repo hub stub never minted | Med | High | `collect_stubs` iterates `repos_touched` + dedupe (Phase 4); determinism test asserts both hubs |
| Per-record skip silently advances watermark past a dropped record (permanent loss) | Med | High | Durable `received->rejected` receipt keyed by `session_id` BEFORE cursor advance (Phase 1); zero-receipts `sb doctor` guard (Phase 6) |
| clyde main is gated (tatari-tv) | Med | Med | Check both gates before Phase 3; PR flow if gated (`rules/git.md`) |
| Distillation degrades silently (fabric misconfig) | Med | Med | `degraded` flag + `sb doctor` warn already exist; Phase 2 exercises the live path |
| repo canonical-form drift (abs path vs org/repo) | Low | Med | Normalize-or-reject at ingest; acceptance criterion covers it |
| Live harvest floods inbox on first backfill | Med | Low | `initial-since` backfill bound + dry-run soak before live flip |
| Phase 7 transcripts already reaped | Med | Low | Bounded-reach by design; backfill covers what survives, no guarantee of full history |

## Open Questions
- (none - all closed in Resolved Decisions across the panel + Opus Staff Engineer passes)

## References
- `docs/design/2026-07-17-harvest-clyde-sessions.md` (pathway #1 build)
- `docs/design/2026-07-17-harvest-clyde-sessions-implementation-notes.md`
- `docs/design/2026-07-18-harvest-knowledge-goals.md` (two-pathway vision, decisions A-D)
- `docs/design/2026-07-19-harvest-subject-grouping-handoff.md`
- `tatari-tv/clyde/docs/design/2026-07-19-files-touched-export.md`
- Bug reproduction + investigation + review panel: this session (2026-07-20)
