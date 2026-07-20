# Design Document: Harvest - Claude Sessions into the Vault

**Author:** Scott A. Idler
**Date:** 2026-07-17
**Status:** Draft (supersedes `2026-07-03-sessions-to-vault-loop.md`)
**Review Passes Completed:** 5/5 (Phases 0-8); Repo Hubs section + Phases 9-13 added 2026-07-19, decisions closed via two additional panel passes (see `2026-07-18-harvest-knowledge-goals.md`, `2026-07-19-harvest-subject-grouping-handoff.md`)
**Amended:** 2026-07-19 - added `repo:`/`repos-touched:` frontmatter, the Repo Hubs and Subject Grouping section, Phases 9-13, and their acceptance criteria; folds in section 3 of `2026-07-18-harvest-knowledge-goals.md`. Field name corrected same day against clyde's actual `2026-07-19-files-touched-export.md` design (was a placeholder `related-repos:`; clyde's contract ships `repos-touched` - see Repo Hubs section).

## Summary

`sb borg harvest`: a pull-based borg ingestion source that reads clyde's versioned session-export contract, selects the sessions worth remembering, distills each selection into a Markdown note, and lands it in the vault inbox through borg's normal pipeline - staged, receipted, classified, embedded, oracle-searchable, replayable. Claude Code engineering days become a capture channel exactly like YouTube and Pixel Discover.

## Problem Statement

### Background

- Every day of Claude Code work produces root causes, decisions, gotchas, and working patterns - and today it evaporates. clyde catalogs ~1k+ sessions (FTS-searchable, enriched with haiku summary+tags), but nothing distills them back into the vault as first-class knowledge.
- The 2026-07-03 draft (`2026-07-03-sessions-to-vault-loop.md`) spec'd this loop but never left DRAFT; its four open questions are closed here. This doc supersedes it.
- The 2026-07-17 conductor review (marquee: `inside-conductor-s-session-harvesting-pipeline`) validated the shape externally - conductor ships a session-harvest pipeline whose two stealable ideas (resume back-pointer in every note, compaction piggyback) inform this design - and demonstrated the failure mode to avoid: activity ledgers (WHAT) dumped as notes without knowledge (WHY), unselected, piling up unread.
- The hand-written `obsidian/notes/ai/2026-07-02-claude-sessions-summary.md` is the target output: that reconstruction, produced automatically.

### Problem

- The only sessions->vault return path is a manual `/closeday` step that has never run and is bounded to "today's 1-2 sessions." Anything not caught that evening is lost.
- clyde's knowledge is locked in SQLite: queryable, not readable, not in the vault, not oracle-searchable alongside everything else.

### Goals

- Nightly (and on-demand) harvest: clyde catalog -> select -> distill -> inbox note. Zero manual effort. (Requested by: Scott, 2026-07-03 draft + 2026-07-17 "I want this knowledge harvested in Markdown notes IN my Obsidian Vault".)
- Selection, not transcription: most sessions do NOT earn a note. (Requested by: Scott, 2026-07-03 non-goals; reaffirmed 2026-07-17 debate - "harvest must SELECT".)
- Rides the established channel end to end: Stage-0 durable capture, receipts, distillation contract, Gate-2, atomic publish, `trace:`/`ingested:`/`trace-expires:`, cortex classify, oracle index, replay/reingest. No side channel into the vault. (Requested by: Scott, 2026-07-17 debate verdict.)
- Never reads clyde's SQLite or raw transcript JSONL: consumes only the versioned `clyde session export` contract (companion doc). (Requested by: Scott, 2026-07-17 debate verdict.)

### Non-Goals

- Not real-time; nightly reflection, not a hot-path hook.
- Not a replacement for `/closeday`'s interactive recap; this is the automated floor under it. `/closeday` delegates to the subcommand.
- Not multi-host: catalog is per-host and desk.lan is where the daemons run; non-desk hosts stay second-class (per the clyde fold-in ruling). Revisit if laptop sessions prove worth harvesting.
- Not conductor-style SessionEnd hooks: batch beats per-session; no harness coupling.

## Proposed Solution

### Overview

New borg ingestion source, the first pull-based one. Core loop:

1. Read watermark (last cursor) -> `clyde session export --cursor <revision>` -> candidate metadata (steady state). First run ever (no watermark): `clyde session export --since <harvest.initial-since>` (human-time filter on `modified`, default `7d`), so a fresh install never inhales months of history unasked; a deliberate deep backfill is `sb borg harvest --since 90d`. The two clyde flags are distinct by contract: `--cursor` is the opaque revision, `--since` is a time filter
2. Selection gate scores candidates; groups survivors into threads (thread = sessions sharing a cwd within a time window); rejected candidates leave `rejection.yml` forensic artifacts (one-time per session, thanks to the watermark; the retention sweep ages them out)
3. Per selected thread: fetch bodies via `clyde session export --id <id> --with-body`, stage as trace artifacts, distill via `DistillKind::Session`, publish note to `inbox/`
4. Persist new cursor + published-id state; receipts row per CANDIDATE. Traces are generated per candidate AT SELECTION TIME (before body fetch) - a rejected candidate needs a trace_id to key its receipts row and host its `rejection.yml`; without this the reject path has no home (round-2 panel: the trace/receipt paradox). NOTE the real receipts cost: the table has hard SQL CHECK constraints on both `status` and `kind` (`borg/src/receipts/schema.sql`), so Phase 1 is a receipts migration + `schema_version` bump adding `rejected` to the status CHECK and a new honest `ReceiptKind::Session` (calling a session `text` would be a lying identifier), plus `ReceiptStatus::Rejected` + `GateId::Selection` in code; `sb borg log` filters on it

### Architecture

- **Source addition per house pattern** (`borg/AGENTS.md`, most recent precedent: signal transport): side-by-side module, `trace::generate("harvest")`, `intake::record_received_with_sidecar()` at the door, then `pipeline::process_content()`.
- **New enum arms:** `Method::Harvest` (`vault/src/schema.rs:372`), `IngestKind::Session` (`borg/src/types.rs:42`), `ContentKind::Session { .. }`, `DistillKind::Session` + `SessionDistiller` (documented add-a-kind pattern, `distillers/AGENTS.md`; nearest sibling: `ThreadDistiller`).
- **Selection gate** follows the documented gate shape (`fn -> Result<(), RejectionRecord>`, writes `rejection.yml`), so rejects are forensically inspectable. Gate-0 (URL blocklist) is a structural no-op for sessions; the selection gate is the real gate and the doc says so plainly.
- **Trigger:** `sb borg harvest` subcommand + systemd timer (nightly). On-demand and scheduled share one core. The borg daemon is untouched (it is a transport listener; harvest is a batch job). Timer hardcodes nothing; all tunables in config (harvest section).
- **Watermark + durable identity:** state file under the `vault::paths` borg state dir holds the cursor plus, per published session id: the note path, `n-msgs` at publish, and the **input body hash** (hash of the export body fed to the distiller - NOT the distillation output; an LLM pass is nondeterministic, so an output hash can never anchor identity; round-2 panel finding). Re-appearance semantics (a published id past the cursor):
  - Cheap filter first: `n-msgs` unchanged AND metadata-only cursor bump -> skip without fetching the body
  - Otherwise fetch the body, hash it: hash changed -> the session gained real content (resume, compaction rewrite) -> follow-up note (`source:` same id, body links the prior note); hash unchanged -> skip. Notes are immutable once published; borg never edits a landed note
  - EVERY processed appearance advances the published snapshot (n-msgs + hash), so an unchanged session can never re-distill run after run
  - `--force`: re-distill in-scope published ids and land the notes (force means "I want a fresh distillation"; nothing suppresses it)
  - A NEVER-published session re-appearing (late enrichment) is an ordinary candidate - the id map only guards published ids
  - Rollback is explicit, not hidden state surgery: `sb borg harvest --since <span>` re-scans a window; the id map dedups what is already landed
  - Locking: the state file takes an exclusive lock; a second concurrent invocation (nightly timer vs hand-run) fails loudly instead of racing the cursor

### Data Model

Note frontmatter (rides existing schema, one new NoteType variant):

```yaml
type: session            # new NoteType::Session variant
domain: <cortex classifies>
origin: generated
status: unread
method: harvest          # new Method::Harvest variant
source: clyde://<session-id>     # primary session (most messages); full member list in the body footer
repo: <org>/<repo>        # canonical form (folded in 2026-07-19, see Repo Hubs section); derived from the export contract's `repo` field, the single cwd-derived repo for the thread's primary session
repos-touched: [<org>/<repo>, ...]   # name matches clyde's export contract field exactly (naming consistency across the seam); direct pass-through of the contract's `repos-touched` (already derived by clyde via repo_slug, never re-derived here); populated once clyde ships files-touched (2026-07-19 decision A2, `clyde/docs/design/2026-07-19-files-touched-export.md`); omitted (not empty) until then, matching the contract's own omit-when-NULL shape
trace: <borg staging handle>                      # UNCHANGED semantics: staged artifacts
ingested: <date>
trace-expires: <date>
tags: [<from distillation>, scope-work|scope-personal]
```

- `trace:` keeps its existing meaning (borg staged-source handle with retention); the clyde pointer rides `source:` as `clyde://<session-id>`, which also feeds replay's `read_source_from_note`. `clyde session resume <id>` recovers full fidelity - the conductor-validated back-pointer, vault-native.
- `repo:` is new (folded in 2026-07-19 from `2026-07-18-harvest-knowledge-goals.md` section 3d, both review passes' top finding): today's frontmatter spec has no `repo:` field and the note renderer never wrote one, so repo hubs would have nothing to gather. The value comes from the export contract's `repo` field (already present in contract v1, derived from `cwd`), normalized to clyde's canonical `<org>/<repo>` form before it reaches the renderer - see Repo Hubs and Subject Grouping below for the full anchor-wiring chain and canonicalization rule.
- Note body: the distillation - what was decided / what was learned / what is reusable, 5-15 tight lines, plus a footer listing member sessions (id, title, repo, duration) for thread notes.
- Staged artifacts per trace: `envelope.yml` (the export metadata for the thread), `body.txt` (concatenated parsed bodies), `distilled.yml`.
- **Replay scope (panel-verified gap):** today `replay --from-stage > 0` bails ("only --from-stage 0 is wired", `borg/src/replay.rs:371`) and reingest re-POSTs a `url` to `/ingest` - neither works for a `clyde://` source. v1 ships STAGE-ONLY replay: Phase 7 wires `--from-stage 2` dispatch from staged artifacts for non-URL sources (cross-crate wiring named explicitly, the most-skipped class). Source-based re-fetch via a `clyde://` handler is out of scope; parked, revisit if stage retention proves too short for a needed re-derive.
- **Embedding policy:** only the distilled note is embedded and oracle-searchable. The staged transcript is trace-recallable through the existing `trace:` machinery, never embedded.

### Repo Hubs and Subject Grouping (folded in 2026-07-19)

Scott's stated concern after the 07-17 review: thread-clustering (below) fragments exactly the multi-day, multi-repo efforts he most wants to SEE as one subject ("Day-2 same-cwd = a new note" was correctly identified as the friction point). This reopened, then closed, judgment call #2. Full elicitation, verification evidence, and option analysis live in `2026-07-18-harvest-knowledge-goals.md` and `2026-07-19-harvest-subject-grouping-handoff.md`; this section carries the agreed shape into the buildable doc.

**Two-layer reframe.** The note-level rule is UNCHANGED: `(cwd, git-branch) + gap` within a run still decides what becomes one note (Selection section below, Resolved Decisions). What changes is where "SEE the subject whole" lives - not in note merging, but in a HUB. A repo hub gathers every note carrying that `repo:` frontmatter across sessions/days/runs, zero note-merging, zero LLM, via the SAME hub-minting machinery the vault already uses for concepts/creators/sources. A session note lands in the SET of hubs its content anchors to (a build session anchors on repo; a research session anchors on concepts/URLs).

**Determinism, scoped honestly.** Repo-hub membership is deterministic: it keys on the typed `repo:` field, so two sessions in the same repo land in the same hub every time. This is genuinely new routing logic, not a mirror of the existing creator/source pattern (those emit note<->note edges; repo needs an unconditional note->hub edge that doesn't exist yet).

**Decisions closed (2026-07-19, Scott, both review passes converged):**
- Multi-repo bridging (e.g. `okta-auth-rs` + a target repo worked together) waits on clyde shipping a files-touched contract addition (decision A2) rather than a semantic guess - deterministic end state over shipping sooner. Consequence: second-brain's cross-repo bridging ships AFTER that clyde release; the single-repo repo-hub slice does not wait, since it depends only on the `repo` field the contract already exposes. Historical (pre-files-touched) multi-repo subjects get a one-time, bounded LLM backfill pass delivered as approve-diffs, never applied silently - not a standing mechanism.
- Repo-hub body synthesis is a real build item: `cortex hub --synthesize`, generalizing the one-off hand-written synthesis already done for 15 hubs in the 2026-07-02 system review. Without it a repo hub is a one-line stub with no readable "these N notes are this subject" view.
- New anchorless recurring themes propose a new hub through the existing approve-diffs gate (same model as `entity-proposals.yml`), gated by recurrence (N sessions, never a one-off) so novel subjects don't stagnate invisibly.
- The `repo:` frontmatter value is always truth (the note never lies about where it happened); hub MEMBERSHIP is what a semantic layer may later adjust, and only as a proposed diff through the approve-diffs gate - never a silent sweep action.

**Canonicalization (determinism-critical).** Inbound `repo:` values must normalize to clyde's canonical `<org>/<repo>` form before minting a hub; an absolute path and a slug for the same repo must yield the SAME hub. A value that cannot be normalized is rejected loudly and mints nothing (fail closed).

**Stability disciplines** (apply to every sweep that touches hub membership): additive, never retractive; incremental assignment, never a global recluster; sticky (moving an existing assignment is a proposed diff, never a silent sweep); durable hub identity with a living, re-synthesizable body (never re-slug or delete a hub).

**Net-new code** (second-brain side, phased below): a `Repo` `HubKind` (today's set is `{Concept, Creator, Source, Tag}`); a deterministic `repo-member` edge (note `repo:` -> repo hub, genuinely new routing, not a mirror of the conditional shared-tag path); the full `repo:` anchor-wiring chain (frontmatter field, index column, upsert, `GraphNoteRow`); a concept-promote command (`entity-proposals.yml` -> `glossary.yml` as a reviewable diff); `cortex hub --synthesize`.

**Ship order:** clyde (files-touched contract bump, `clyde/docs/design/2026-07-19-files-touched-export.md`) -> second-brain (repo-hubs + bridging) -> obsidian vault (data, no code). Only cross-repo bridging is gated on clyde; the single-repo repo-hub slice (Phases 9-10 below) is not, and neither are Phases 11-13.

### Selection (what earns a note)

Signals (from the 2026-07-03 draft, config-tunable):

- `dormant: true` AND `enrich-status: ok` (frozen contract values: `ok | skipped-personal | skipped-empty | failed | null`). Both required explicitly: enrichment does NOT imply dormancy (clyde enrich can be invoked directly by id, `clyde/src/main.rs:541`), and the contract exposes `dormant` as its own field - never harvest a session mid-flight. A session whose enrichment lands later reappears past the watermark because every enrichment write (including skip/failure) bumps the export cursor
- `n-msgs` >= threshold (substantive, not one-shot)
- cwd is a real repo (matches `~/repos/<org>/<repo>`)
- Exclusion patterns on title/first-prompt: auto-fired security reviews, bare "sure"/empty prompts, navigational lookups
- Thread-clustering: survivors sharing cwd within a time window merge into one thread note (the 07-02 token-broker arc: 4 sessions = 1 note). A thread of size 1 is just a session note - the design collapses to trivial at N=1.
- Thread boundary rules (v1, deterministic): threads never span harvest runs; within a run, the cluster key is (cwd, git-branch) + inter-session gap < window - branch is free from the contract and keeps concurrent same-repo work (frontend + backend in one monorepo) from blindly merging. Day-2 work in the same cwd is a NEW note (notes are immutable; cortex's link pass connects them). Known limitation, tested not hidden: the golden fixtures include a same-cwd-unrelated-sessions case so the window is tuned against real noise; a smarter similarity fallback is parked until fixtures prove the deterministic rule wrong.

### Distillation

- New fabric pattern `distill-session.md` (+ chunk/reduce variants), deployed via the existing pattern path. Prompt contract: decisions made, approaches rejected (and why), gotchas learned, reusable patterns; no narration, no activity ledger.
- Input: parsed role-labeled bodies from the export contract (head+tail windowing for very long threads, token cap in config).
- Model: `harvest.model` config key inheriting `llm.model` (established per-feature override precedent).
- Output through the `Distilled` contract (`vault/src/distilled.rs`) with a `SessionPayload` in `KindPayload` (repo, session ids, msg counts, date range); Gate-2 paraphrase check applies.

### API Design

```
sb borg harvest [--since <span|date>] [--dry-run] [--limit <n>] [--force]
```

- `--dry-run`: list what would be selected/rejected, write nothing.
- `--force`: re-distill in-scope already-published sessions and land the notes. Part of the watermark invariant, so it is contract surface, not a hidden flag.
- Config: `harvest:` section in `~/.config/sb/borg.yml` (mirrored into `config/templates/borg.yml.example`): clyde binary path (absolute, tilde-expanded default), `initial-since` (first-run backfill bound), `mode: dry-run|live` (what the timer runs; the first-week soak flips this), selection thresholds, exclusion patterns, thread window, token cap, model override. The timer's `OnCalendar` is the ONE value that lives in the unit (it IS the timer), rendered at bootstrap; no behavioral tunable is ever baked into the unit.

### Implementation Plan

#### Phase 0: Contract spike (zero code)
**Model:** sonnet
- Run the shipped `clyde session export` against the live catalog; verify every selection signal and body need maps to a contract field
- **Success criteria:** captured envelope + one `--with-body` payload checked into eval fixtures; every signal in the Selection section names its source field

#### Phase 1: Schema seams
**Model:** sonnet
- `Method::Harvest`, `IngestKind::Session`, `ContentKind::Session`, `NoteType::Session` + round-trip tests. The NoteType cost is wiring, not one arm - corrected site list (round 2 fixed a wrong-crate cite): `vault/src/schema.rs:103` (enum + `as_str` + `all` + `FromStr` arms + `schema/tests.rs`), `vault/src/trace.rs:40`, `borg/src/types.rs:8` (`ContentKind`; `IngestKind` at :42 - both live in borg, not vault), `borg/src/markdown.rs:42` (frontmatter renderer lives in borg, not vault), `vault/src/distilled.rs:244`, `render.rs:94`, `vector.rs:558`
- Receipts migration: `schema_version` bump; `rejected` added to the status CHECK constraint; new `ReceiptKind::Session`; `ReceiptStatus::Rejected` + `GateId::Selection` in code
- **Success criteria:** enum round-trip tests green across ALL enumerated sites; workspace green; oracle schema_info shows the new type (schemars auto); receipts migration idempotent and a row round-trips `(session, rejected)`

#### Phase 2: Config
**Model:** sonnet
- `harvest:` section + example template + tilde-expanded paths + defaults
- **Success criteria:** config parses with and without the section; example documents every key

#### Phase 3: Export reader + selection gate + watermark
**Model:** opus
- Shell-out JSON reader against schema-version 1; scoring; exclusion patterns; thread clustering; watermark store; `rejection.yml` on rejects
- **Success criteria:** the checked-in 2026-07-02 catalog slice (golden fixture) selects the EXACT expected session ids, cluster count, and note count; a same-cwd-unrelated-sessions fixture does NOT merge; rerun with unchanged catalog is a no-op; a resumed-session fixture (body hash changed) produces a follow-up note and an unchanged-body fixture skips WITHOUT re-distilling; rejects leave `rejection.yml` + a `rejected` receipts row keyed by a selection-time trace

#### Phase 4: Session distiller
**Model:** opus
- `DistillKind::Session`, `SessionDistiller`, `distill-session.md` + chunk/reduce, `KindPayload::Session`, `render()` extension, `distill_for_publish_session`
- Truncation is never silent to the model: when the export flagged `body-truncated`, the prompt carries an explicit `[TRANSCRIPT TRUNCATED]` marker
- **Success criteria:** `FakeFabric` test emits bounds-valid `Distilled`; degraded fallback path sets `degraded`; Gate-2 applies; a truncated-body fixture shows the marker in the assembled prompt

#### Phase 5: Pipeline handler + publish
**Model:** sonnet
- Trace generate, sidecar intake, staged artifacts, atomic publish to `inbox/` with full frontmatter incl. `source: clyde://...`
- **Success criteria:** end-to-end test (fixture export -> inbox note) green; receipts row `received -> succeeded`; note carries both `trace:` and `source:`

#### Phase 6: CLI + observability
**Model:** sonnet
- `sb borg harvest` subcommand, `--dry-run`, `sb borg log` filters for `method=harvest`
- **Success criteria:** dry-run writes nothing and lists selections+rejections; log filter works

#### Phase 7: Eval + replay (BEFORE the timer - nothing runs unattended until fixtures exist)
**Model:** opus
- Golden session fixtures under `config/eval/distill-fixtures/` (reuse clyde's Phase 0 fixtures where they fit); eval-kind wiring
- Wire `replay --from-stage 2` for `Method::Harvest`/`IngestKind::Session` ONLY: dispatch from staged artifacts instead of the `replay.rs:371` bail. Every other kind's `--from-stage > 0` stays explicitly unsupported - one concrete case, not a generic non-URL replay framework (round-2 scope ruling)
- **Success criteria:** `sb borg eval` scores the session kind; `sb borg replay <trace> --from-stage 2` re-derives a STRUCTURALLY equivalent note (valid `Distilled`, same `source:`/`trace:` refs, bounds respected - byte-identity is not asserted over an LLM pass)

#### Phase 8: Timer
**Model:** sonnet
- systemd service+timer templates (bootstrap-installed like existing units); values from config, none hardcoded; clyde binary default is an absolute tilde-expanded path (timers run with a stripped PATH)
- **Success criteria:** two consecutive timer runs double-ingest nothing; timer unit contains nothing tunable beyond `OnCalendar`; the unit runs with an empty inherited PATH in a test harness

#### Phase 9: `repo:` anchor wiring chain
**Model:** opus
- Add `repo` typed field to `vault::Frontmatter` (`frontmatter.rs:20-47`, today falls into the untyped `extra` map); add the multi-repo storage field `repos-touched:` (name matches clyde's export contract field exactly - a direct pass-through of clyde's `repos-touched`, never re-derived in second-brain; populated once clyde ships files-touched, decision A2); canonicalize inbound `repo:` values to clyde's `<org>/<repo>` form before storage - the canonicalizer MUST mirror clyde's `repo_slug` semantics exactly (`clyde/session/src/scope.rs:84-93`: org = the path component after `repos`, repo = the next, first `repos` anchor wins, a deeper path resolves to its top repo slot) so a note's canonicalized single `repo:` and a pass-through `repos-touched` entry for the same repo produce the IDENTICAL slug and thus the identical hub; do NOT reuse `borg::github::extract_repo_slugs`, whose URL-text-oriented, case-insensitive dedup and different truncation would diverge from clyde's path-based slug and silently split one repo across two hubs - a non-normalizable value is rejected loudly and mints nothing (fail closed; `repos-touched` entries arrive pre-canonicalized from clyde and pass through unchanged); add a `repo` column to the `notes` table via idempotent `ALTER TABLE ADD COLUMN` (`schema.rs:346-367`, the proven `ensure_trace_columns` pattern), bind it in the upsert (`index.rs:230-245`), carry it on `GraphNoteRow` + its SELECT (`search/graph.rs:83-91`, `:322`); amend the Phase 5 note renderer to write `repo:` from the export contract's `repo` field (already present in contract v1) and `repos-touched:` from the contract's `repos-touched` field once present.
- **Success criteria:** a note publishing with a non-normalizable repo value is rejected loudly and mints nothing; the same repo seen as an absolute path and as a slug normalizes to the identical `repo:` value; the canonicalizer is idempotent on an already-`<org>/<repo>` value (`canon("scottidler/loopr") == "scottidler/loopr"`) and produces byte-identical output to clyde's `repo_slug` for the same repo, so a note's single `repo:` and a `repos-touched` entry for that repo mint ONE hub, not two (a paired test asserts both derivations land on the same `repo-<slug>`); migration idempotent on existing DBs; `GraphNoteRow` carries `repo` end to end; harvest re-run after this phase produces notes with `repo:` populated.

#### Phase 10: Repo hub kind + deterministic edge
**Model:** opus
- `HubKind::Repo` (`hub.rs:121-175`, today's set is `{Concept, Creator, Source, Tag}`), an explicit slot in the first-wins precedence chain (`hub.rs:120`), the `repo-` prefix baked into the hub-path collision key (hub paths are `<slug>.md`); the `repo-member` edge (note `repo:` -> repo hub) - unconditional, fires for every note carrying a `repo:`, minting the hub if it doesn't exist. This is genuinely new routing logic, not a mirror of the conditional shared-tag -> hub path (`graph.rs:274-288`), which only fires for over-cap blanket tags against an already-existing hub.
- **Success criteria:** a session note carrying `repo: X` joins hub `repo-x` on every sweep (3f repo determinism); sweep twice on a frozen corpus yields identical repo groupings byte-for-byte (3f frozen-corpus determinism); a repo sharing a name with an existing concept does not collide (prefix test).

#### Phase 11: Concept-promote command
**Model:** sonnet
- `entity-proposals.yml` -> `glossary.yml` promotion as a reviewable diff (inverse of the existing `write_proposals`); repo-track the proposals file (today untracked under `~/.config/sb/`).
- **Success criteria:** a proposed concept promotes into `glossary.yml` as a diff, never a silent write; the proposals file is versioned in-repo.

#### Phase 12: `cortex hub --synthesize`
**Model:** opus
- Generalize the one-off 2026-07-02 hand synthesis (`notes/2026-07-02-second-brain-system-review.md:66`) into a real `--synthesize` mode: re-synthesize a hub's body from its current membership. Failure handling is loud: a failed or truncated LLM pass leaves the prior hub body intact, never a blank or partial overwrite.
- **Success criteria:** a freshly-minted repo hub (today a one-line stub, `render_hub`, `hub.rs:178-186`) synthesizes into a readable "these N notes are this subject" body; a forced synthesis failure leaves the previous body byte-identical; re-synthesis never re-slugs or deletes the hub.

#### Phase 13: Monotonicity + acceptance harness
**Model:** sonnet
- Determinism test (sweep twice on a frozen corpus -> identical deterministic-anchor groupings, byte-for-byte); stability harness (assign, add N notes, re-run -> zero previously-assigned notes moved, additions only), extended to assert `apply_linking` only ever ADDS wikilinks to landed notes, never removes or alters an existing one; concept-recall eval set (known-concept session notes -> measured fraction whose concept-hub membership actually landed, bounding the glossary/alias-coverage gap from the Repo Hubs section above - a recall drop is a signal to grow aliases, never to loosen determinism).
- **Success criteria:** each of the four 3f acceptance criteria not already covered by Phase 9/10 (frozen-corpus determinism, monotonicity incl. `apply_linking` add-only, concept recall, immutability) has a named passing test; no sweep, linking pass, or membership override ever modifies a landed note's body or frontmatter.

## Acceptance Criteria

- [ ] The checked-in 2026-07-02 golden fixture produces the exact thread notes the hand-written summary captured (asserted ids, cluster count, note count), unattended
- [ ] A resumed session (input body hash changed past its publish snapshot) yields a follow-up note; a cursor bump without body change yields a skip and advances the snapshot (no re-distill loop)
- [ ] Every harvested note is oracle-searchable, carries `source: clyde://<id>` that `clyde session resume` accepts, and has a live `trace:` staged source
- [ ] A session below the selection bar produces a `rejection.yml` and no note; rerunning does not reconsider it silently (watermark)
- [ ] `replay --from-stage 2` re-derives a structurally equivalent session note after a distiller prompt change (stage-only replay; `reingest` re-fetch is explicitly out of scope for harvest v1)
- [ ] `otto ci` green at workspace root; no direct read of `sessions.db` or `.jsonl` anywhere in the diff
- [ ] A session note carrying `repo: X` joins hub `repo-x` on every sweep; a note carrying repos X+Y (post-files-touched bridging) joins BOTH hubs deterministically
- [ ] Sweep twice on a frozen corpus produces identical repo/creator/source/tag groupings, byte-for-byte
- [ ] Assign, add N notes, re-run: zero previously-assigned notes moved, additions only; `apply_linking` only ever adds wikilinks, never removes or alters one
- [ ] No sweep, linking pass, or membership override ever modifies a landed note's body or frontmatter; semantic assignments appear only as hub-body diffs through the approve-diffs gate
- [ ] Any inbound `repo:` value normalizes to the canonical `<org>/<repo>` form before minting; a non-normalizable value is rejected loudly and mints nothing

## Resolved Decisions

- 2026-07-17: **verb lives at `sb borg harvest`** (harvest is ingestion; ingestion is borg). `/closeday` delegates to it. Closes 2026-07-03 open question 4.
- 2026-07-17: **thread-clustered notes** (cwd + time window), degenerating to per-session at N=1; no daily roll-up in v1 (parked, revisit if thread notes prove too granular). Follows the 07-03 draft's own recommendation; the target output (07-02 summary) is thread-shaped. Closes open question 1.
- 2026-07-17: **work sessions included, scope-tagged** (`scope-work` tag from clyde's scope field), exclusion available via config pattern. The vault already carries work content (`work/` tree); uniform work-scoped data warrants no new privacy scaffolding. Closes open question 3.
- 2026-07-17: **model = config `harvest.model` inheriting `llm.model`**; cost bounded by selection (~few threads/day) + token windowing. Closes open question 2.
- 2026-07-17: `type: session` (new NoteType variant) over the draft's `type: note`. One enum arm buys retrieval faceting ("what did I work on in June") for near-zero cost; oracle schemas update automatically.
- 2026-07-17: clyde pointer rides `source: clyde://<id>`, NOT `trace:` - `trace:` already means borg staged-source handle with retention semantics oracle advertises. Overloading it would break the staged-source machinery.
- 2026-07-17 (panel consensus): resumed-session semantics = follow-up note on material `n-msgs` growth, skip on bookkeeping-only cursor bumps, `--force` to override. Notes are immutable once published; borg never edits a landed note.
- 2026-07-17 (panel consensus): replay is STAGE-ONLY for harvest v1 (Phase 7 wires the `--from-stage` dispatch that `replay.rs:371` lacks); `clyde://` re-fetch parked with a revisit condition. Receipts grow `Rejected` + `GateId::Selection` so below-bar sessions are auditable without lying as `failed`.
- 2026-07-17 (panel consensus): thread boundaries are deterministic (cwd + gap, never spanning runs; Day-2 same-cwd = new note, cortex links them); similarity-based clustering parked until golden fixtures prove the simple rule wrong.
- 2026-07-17 (panel round 2): durable identity anchors on the INPUT body hash, never the distillation output (LLM nondeterminism); every processed appearance advances the published snapshot. Traces are generated per candidate at selection time so rejects have a receipts key and a `rejection.yml` home. Receipts change is a real SQL migration (status/kind CHECK constraints) with an honest `ReceiptKind::Session`. Replay wiring scoped to the session kind only. Cluster key gains `git-branch`. Watermark file takes an exclusive lock (timer vs hand-run).
- 2026-07-19 (reopens then closes judgment call #2, both review passes converged): Scott's concern that thread-clustering fragments multi-day/multi-repo efforts is correctly aimed at the deterministic `(cwd, branch, gap)` rule - but the fix is a two-layer reframe, not relitigating the note-level rule. Note-level clustering (thread boundary rules, above) stands unchanged. "SEE the subject whole" is a hub-level concern: a `repo` hub gathers every note carrying that note's `repo:` frontmatter, deterministically, zero note-merging. See Repo Hubs and Subject Grouping above and Phases 9-13. Superseded sketch: "subject = a chain of follow-up notes" (would have mutated note relationships per-run and ridden the LLM for every extends-a-subject call); the hub already is the durable gather-many-notes-into-one-subject primitive and gets the same result deterministically without new machinery or touching note immutability.
- 2026-07-19: multi-repo bridging (decision A2) waits on clyde's files-touched contract addition rather than a semantic guess at cross-repo membership - deterministic end state over shipping sooner. The single-repo repo-hub slice (Phases 9-10) is NOT gated on this; only bridging is. Historical multi-repo subjects (pre-files-touched) get a one-time, bounded LLM backfill pass delivered as approve-diffs, never applied silently.

## Alternatives Considered

### Alternative 1: clyde writes vault notes directly ("clyde harvest")
- **Pros:** no cross-repo consumer; clyde owns transcript parsing anyway
- **Cons:** second writer into the vault bypasses Gate-2/receipts/retention/replay; cortex governs notes it never ingested; clyde couples to vault schema forever; shallow enrichment becomes the ceiling
- **Why not chosen:** settled 2026-07-17 debate: the vault has one gatekeeper. Clyde's strongest points (format ownership, schema privacy) are satisfied by the export contract instead.

### Alternative 2: read sessions.db / transcripts directly
- **Pros:** zero clyde-side work; the 07-03 draft and 06-22 fold-in doc both sketched it
- **Cons:** private schema as API; every clyde migration silently breaks harvest; two tools tracking Anthropic's JSONL drift
- **Why not chosen:** superseded by the export contract (companion doc); "shelling out is the lower-coupling start" was the draft's own lean

### Alternative 3: conductor-style SessionEnd hook capture
- **Pros:** proven in the wild; zero-lag capture
- **Cons:** per-session push = unselected pile (the conductor failure mode: WHAT without WHY, unbounded growth, no dedup); couples to harness hook lifecycle; conductor's own harvest never syncs anywhere
- **Why not chosen:** batch pull with selection is the whole point; the conductor review is the cautionary evidence

### Alternative 4: daily roll-up notes
- **Pros:** one note per day, matches the hand-written artifact's granularity
- **Cons:** buries per-thread knowledge; worse oracle retrieval unit; harder dedup
- **Why not chosen (parked):** thread notes first; roll-up revisited if thread volume proves noisy. Recorded here so it is not re-litigated ad hoc.

## Technical Considerations

### Dependencies
- **Cross-repo blast radius:** clyde (tatari-tv, public) must ship `session export` schema-version 1 FIRST (companion doc: `clyde/docs/design/2026-07-17-session-export-contract.md`, shipped/Implemented). Harvest Phases 0-8 build against that frozen contract and are not gated further.
- **Second cross-repo dependency (2026-07-19, Phases 9-13 only):** multi-repo bridging depends on clyde's files-touched contract addition (`clyde/docs/design/2026-07-19-files-touched-export.md`; design doc itself uncommitted, Status: In Review, 5/5 review passes, awaiting Scott's go). As of this writing clyde's own build is in progress, not shipped: Phase 1 (parser extraction of `files_touched` from whitelisted tool blocks) is committed (`eb0923f`); Phase 2 (catalog column, `SCHEMA_VERSION` 5->6 migration, `--reparse` backfill) has substantial code already in clyde's working tree (uncommitted - `db.rs`, `index.rs`, `model.rs`, `cli.rs`/`main.rs` for the flag) but no implementation-notes entry yet, so not phase-complete; Phases 3-4 (export fields `files-touched`/`repos-touched`, fixtures + contract doc) have not started. Ship order stands: clyde (files-touched) -> second-brain (repo-hubs + bridging, Phases 9-13) -> obsidian vault (data, no code). The single-repo repo-hub slice (Phases 9-10) depends only on the `repo` field the contract already exposes (contract v1, shipped) and is NOT gated on this second clyde release.
- In-workspace: borg, vault, distillers, cortex, sb crates; no new external crates expected.
- Runtime: `clyde` binary on PATH (config-overridable path).

### Performance
- Nightly batch, ~tens of candidates, ~few selected threads/day. Distill cost bounded by selection + token cap. Not a concern at this scale; no pagination until it is a problem.

### Security
- Vault is personal (Syncthing + Obsidian Sync). Work-session knowledge lands scope-tagged; config exclusion pattern available if policy changes. No secrets should survive distillation (prompt instructs; Gate-2 and note review are the backstops). The export contract exposes `redaction-count`; a session with a nonzero count gets a `redacted-source` tag on its note so sensitivity is visible downstream.

### Testing Strategy
- Golden export fixtures (from clyde Phase 0) drive selection tests; FakeFabric drives distiller tests; end-to-end fixture -> inbox note; eval fixtures for distillation quality (blind judge, existing harness); replay test proves trace sufficiency.
- Repo hubs (Phases 9-13): canonicalization test (path vs. slug -> one hub, non-normalizable value rejected); frozen-corpus determinism test (sweep twice -> identical groupings byte-for-byte); monotonic stability harness (assign, add N, re-run -> zero moves) extended to assert `apply_linking` is add-only; concept-recall eval set bounding the glossary/alias-coverage gap; synthesis-failure test (a forced `cortex hub --synthesize` failure leaves the prior body byte-identical).

### Rollout Plan
- Phases 0-8 land per-phase-commit, otto ci green each; timer installed last (Phase 8) so nothing runs unattended before eval fixtures exist (Phase 7).
- Phases 9-13 (repo hubs) land after Phase 8, since they consume notes Phase 5 already publishes; Phases 9-10 (single-repo slice) can land independently of the clyde files-touched release, Phases 11-13 have no such gate.
- First week: `--dry-run` via timer (config flag), review selections; flip to live after the selection bar looks right. Degrade visibly: harvest failures (clyde binary missing, contract version mismatch, export nonzero exit) land in receipts + `sb borg log` as loud failures; the timer keeps its schedule; never silent.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Distilled notes are activity ledgers, not knowledge | Med | High | prompt contract + eval fixtures with blind judge; the conductor review is the anti-pattern spec |
| Selection bar wrong (noise or misses) | Med | Med | dry-run first week; thresholds in config; rejects inspectable via rejection.yml |
| Contract drift with clyde | Low | High | schema-version pin; major mismatch fails the run loudly ONCE per run into receipts (one `failed` row per night, surfaced by `sb doctor` - loud without nightly alert spam); harvest never limps along on a mismatched contract |
| clyde abandoned while Claude Code usage continues | Low | Med | coupling surface is ONE reader module against a JSON shape (the yt-dlp pattern - borg already shells out per source); everything downstream of Phase 3's reader is source-agnostic; landed notes re-derivable from staged traces without clyde; recovery = rewrite one reader, not the feature. The alternative (sb parsing transcript JSONL itself) swaps a thin dependency for absorbing clyde's catalog/enrichment/staging wholesale - rejected (Scott raised, 2026-07-17) |
| Write-only trap (notes never read) | Med | High | success metric: oracle-hit/reviewed counts on session notes non-zero a month in (07-03 criterion carried forward) |
| Replay breaks for session sources (URL-assuming reingest) | Med | Low | replay-from-stage is the supported path (Phase 7 wires + tests it, scoped to session kind only); clyde:// re-fetch parked |
| clyde's files-touched release slips, blocking multi-repo bridging | Med | Low | only bridging is gated; the single-repo repo-hub slice (Phases 9-10) ships independently and covers the deterministic common case |
| Concept-hub membership silently under-fills (LLM body variance upstream of linking) | Med | Med | not a determinism failure - it's a recall question, covered by its own eval (Phase 13) rather than the frozen-corpus test; a recall drop grows aliases, never loosens determinism |

## Open Questions

(none - the 07-03 draft's four questions are closed in Resolved Decisions)

## References

- Supersedes: `docs/design/2026-07-03-sessions-to-vault-loop.md`
- Companion (ships first): `clyde/docs/design/2026-07-17-session-export-contract.md` (Implemented)
- Companion (ships before Phases 9-13's bridging only): `clyde/docs/design/2026-07-19-files-touched-export.md` (not yet shipped)
- Repo hubs / subject grouping fold-in (Phases 9-13, Repo Hubs section): `2026-07-18-harvest-knowledge-goals.md`, `2026-07-19-harvest-subject-grouping-handoff.md`
- Conductor evidence: marquee `~scott-idler/inside-conductor-s-session-harvesting-pipeline`
- Pipeline contracts: `2026-04-19-staged-ingestion-pipeline.md`, `2026-05-20-receipts-log.md`, `2026-07-05-distillation-knowledge-extraction.md`, `2026-06-20-oracle-trace-availability.md`
- Source-addition precedent: `2026-05-24-signal-as-borg-transport.md`
- Hub-mechanics precedent: `entities/*.md` synthesis, `notes/2026-07-02-second-brain-system-review.md`
