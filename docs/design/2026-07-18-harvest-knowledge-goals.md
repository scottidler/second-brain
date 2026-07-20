# Harvest knowledge-extraction goals and clustering requirement (captured input)

**Status:** Captured input + agreed design direction (section 3, added 2026-07-19).
Not yet phased.
**Captured:** 2026-07-18, from Scott, during clyde session-export Phase 0.
**Updated:** 2026-07-19 -- decisions A-D closed with Scott; direction folded in from
`2026-07-19-harvest-subject-grouping-handoff.md`.
**Review:** Both panel passes ran 2026-07-19 and their findings are folded into
section 3. Architect (Gemini): determinism scoping in 3a, schema/plumbing and
edge-precedent corrections in 3d, hardened tests in 3c, acceptance criteria in
3f. Staff Engineer (persona re-run on Opus after Codex ran out of credits):
central determinism claim CONFIRMED to hold structurally; A2 clyde scoping
confirmed accurate; new findings folded -- the repo-anchor wiring chain and
canonical form (3d), the emission gap and its harvest-doc dependency (3d), the
A2 retroactivity caveat (3b), `--synthesize` as a core-value dependency (3d/3e),
and the semantic-layer fail-closed posture (3c). The two reviews converge on the
same top issue: the missing `repo:` plumbing, not the reframe itself.
**Relates to:** `2026-07-17-harvest-clyde-sessions.md` (pathway #1, built against the
frozen clyde export contract). This file records goals and a requirement that are
NOT yet reflected in that doc and will drive (a) a future pathway-#2 design doc and
(b) a revision to the harvest doc's clustering.

This is Scott's stated vision, captured close to verbatim, plus the synthesis we
reached and agreed on in the same session. Do not treat any of it as decided design;
treat it as requirements input for the design work it names.

## 1. The goal hierarchy for harvesting knowledge out of Claude sessions

**Paramount goal: harvest HOW Scott works.** Extract and distill how he works, how
he thinks, how he discerns and operates as a technical professional and architect.
The purpose is to distill those learnings over time to **sharpen the focus of the
files and setup used on future projects**. The end goal is a system that organically
develops and morphs the files/setup used in future sessions, enhancing his ability to
use AI to achieve his goals in writing, architecture, influencing, learning, teaching,
and so on.

**Primary goal: historical knowledge of what was worked on.** Extract the historical
record of what was built AND what was discovered/learned. Purpose: so Scott can share
with others what he worked on, built, and learned.

**Secondary (but important) goal: demonstrate how he works with AI.** Show others how
he works with and uses AI, so they can learn from his approaches.

### The synthesis we agreed on

- **The flywheel is the real prize.** sessions -> mined patterns of how Scott works ->
  sharpened rules/config/voice/identity files -> better future sessions -> repeat.
- **Two pathways over one substrate.** The clyde export contract is the shared raw tap.
  - **Pathway #1 (designed):** distill sessions into vault knowledge notes -- the WHAT
    (what was decided/built/learned). Output sink: vault `inbox/`. Cadence: per
    session/thread. This is what `2026-07-17-harvest-clyde-sessions.md` builds.
  - **Pathway #2 (paramount, undesigned):** mine how Scott reasons/decides/discerns --
    the HOW. Output sink: candidate edits to `rules/*`, `CLAUDE.md`, `VOICE.md`,
    `IDENTITY.md`. Cadence: longitudinal, aggregative across many sessions.
- **Pathway #2 already exists by hand.** `rules/taste.md` and `rules/interaction.md`
  state in their own headers that they were distilled from forensic passes over ~1,629
  and ~930 past sessions. So the target output format is proven; the goal is to make
  that forensic pass **organic and continuous** rather than a periodic manual grind.
- **Pathway #2 deserves its own design doc**, not a bolt-on to the harvest doc. Cramming
  "how Scott thinks" meta-analysis into per-session inbox notes would muddy both.
- **Two cautions for pathway #2 when designed:**
  - It is HARDER than knowledge extraction. "How he thinks" is not in any one session;
    it is in the deltas across many (how he responds to being wrong, what he triages as
    targeted-fix vs design-doc, what he defers).
  - **Self-referential drift.** Mining sessions that were themselves shaped by
    `taste.md` risks the pass converging on its own priors (circular authority, in loop
    form). A mined rule should be treated as a hypothesis; the next sessions are its
    eval. That also makes the flywheel measurable.

## 2. Grouping/clustering requirement: SEE the subject across sessions

Scott's stated concern, captured close to verbatim:

- How we group and store items from the record matters.
- Grouping relates to the path (usually a git repo) but sometimes MULTIPLE paths
  (example: `okta-auth-rs` + a target repo worked together as one effort).
- The important thing is to **SEE the subject through the various sessions.**
- Bigger efforts will be many sessions over many days, and maybe not even in the same
  path the whole time.
- Part of ingestion must be able to **evaluate and group sessions into appropriate
  themes, topics, subjects.**
- Scott will be **disappointed if we only blindly copy over each and every session**
  without some evaluation that groups them together.
- Worry: knowledge gets **lost in translation** when two different systems (clyde +
  sb borg) handle the knowledge-extract transfer.

### Why this reopens a prior decision

This directly tensions with a decision Scott had earlier confirmed (harvest doc
judgment call #2, and its panel-consensus Resolved Decision): clustering by a
**deterministic mechanical key** `(cwd, git-branch) + time-gap`, within a single run,
never spanning runs, with "Day-2 same-cwd = a new note, cortex links them later," and
similarity-based clustering explicitly PARKED. That mechanical rule **fragments**
exactly the multi-day, multi-path efforts Scott cares most about seeing whole
(different cwd -> different thread; different day -> different note). So Scott's
disappointment is correctly aimed at that rule. He is now un-parking semantic
clustering. **This reopens judgment call #2** and requires a harvest-doc revision plus
a review-panel pass (it reintroduces the nondeterminism/testability tradeoffs the panel
deliberately parked).

### The synthesis we agreed on

- **Lost-in-translation fix is one brain.** clyde exposes raw truth and does ZERO
  knowledge work; borg owns 100% of evaluation, grouping, theming, and distillation.
  Translation gets lost only if knowledge logic is split across the two systems. The
  contract is the anti-lossy seam (its whole reason to exist, superseding the
  read-sessions.db-directly sketch). Hold the line: no clustering/scoring/theming ever
  leaks into clyde.
- **The contract already exposes enough** for semantic clustering: `title`, `summary`,
  `tags`, `first-prompt`, and full `body` on demand. Grouping across cwd and across
  time is logic over those fields, not a new field. (This is why this requirement does
  NOT change the export contract or the Phase 0 7-item dispositions.)
- **Direction for subject-clustering (proposal, not decided):**
  1. **Mechanical atom, semantic subject.** Keep `(cwd, branch, gap)` only as the cheap
     MICRO-THREAD atom (contiguous work). A second pass groups micro-threads into a
     SUBJECT by CONTENT, not path -- so `okta-auth-rs` + target-repo merge because their
     content links, and a subject can span days.
  2. **Incremental assignment, not global reclustering.** borg already has oracle/cortex
     (retrieval). For each new micro-thread, ask "does this extend an existing subject?"
     by retrieving against existing subject notes. Tractable per-run; avoids reclustering
     the whole corpus.
  3. **Subject = a chain, reusing machinery.** A session that extends a subject becomes a
     FOLLOW-UP note in that subject's chain -- the same resumed-session follow-up
     mechanism (cousin of contract finding B1), which preserves note immutability while
     letting a subject accrete across days. The subject is the durable unit; member
     sessions are visible in the chain/footer.

## 3. Agreed design direction (2026-07-19; decisions closed with Scott)

Full elicitation, verification evidence, and the option analysis live in
`2026-07-19-harvest-subject-grouping-handoff.md`; this section is the agreed shape.
Evidence there is cited to `file:line` and was verified, not assumed.

### 3a. Two-layer reframe: deterministic note, hub-level subject

This is the key move, and it CLOSES the reopened judgment call #2 rather than
relitigating it:

- **Note level -- unchanged.** `(cwd, git-branch, gap)` within a run stays the
  deterministic micro-thread rule deciding what becomes one note. The panel's
  Resolved Decision stands at this level.
- **Subject level -- repo becomes a hub dimension.** "SEE the subject whole" lives
  in the HUB, not in note merging. A repo hub gathers every note carrying that
  `repo:` frontmatter across sessions/days/runs -- zero note-merging, zero LLM.
  The vault already mints hubs on three dimensions (concepts via glossary,
  source hosts, creators); repo is the fourth, and the only net-new one.

A session note lands in the SET of hubs its content anchors to (multi-anchor
membership): a build session anchors on repo; a research session anchors on
concepts + URLs. Non-file subjects are not a special case -- they fall through to
hub kinds that already exist. Hub membership is wikilink + deterministic-metadata
driven, not cosine driven (handoff finding 1), so the visible grouping is
stable-by-construction.

**Determinism claim, scoped honestly (Architect finding 1, code-verified).**
The determinism above holds for FRONTMATTER-driven membership: repo, creator,
and source hubs key on typed fields, so two sessions in the same repo land in
the same repo hub every time. It does NOT hold end-to-end for CONCEPT hubs:
concept membership is lexical -- `cortex link` matches glossary slugs/aliases
against the note BODY (`linking.rs:128-232`), and that body is LLM-generated by
`borg harvest`. Membership is deterministic GIVEN the body, but the body is not
stable across runs: session A's distillation may say "Anthropic API" where
session B's says "Claude API," and B silently misses the hub. "Not cosine-driven"
is not the same as "deterministic." So: repo-subject wholeness (the build-session
case Scott anchored on) is deterministic; concept-subject wholeness (the
research-session case) is best-effort RECALL bounded by glossary/alias coverage.
This is a coverage question, not a churn question -- membership never flaps, it
can only under-fill -- and it gets its own acceptance criterion in 3f (an
alias-coverage/recall check), because the frozen-corpus determinism test cannot
see it.

The LLM/semantic budget for STRUCTURAL decisions is confined to genuine edge
cases: multi-repo subjects (decision A), emerging off-glossary concepts (existing
`--discover` proposal pipeline), and anchorless recurring themes (decision C).
(Concept-hub recall above also rides LLM output, but as note-body surface forms,
not as structural decisions -- flagged so the "three edge cases" framing is not
read as "no other LLM influence.")

**How semantic assignments attach without editing a landed note (closes the
review's hardest question; code-verified).** There is no frontmatter field for
semantic subject assignments, and notes are immutable -- so semantic membership
lives in the HUB body, not the note. `cortex graph` builds wikilink edges from
EVERY note body, hub notes included (`graph.rs:252-262` runs per row with no
entities/ exclusion), and hub bodies are living documents (3c). A new anchorless
hub (decision C) or a membership override (decision D) is therefore a proposed
diff to the HUB body adding `[[member-note]]` wikilinks -- the edge appears in
the graph, the note is never touched, and the approve-diffs gate covers it.

**Supersedes:** the section-2 direction sketch item 3 ("subject = a chain of
follow-up notes"). Reasoning: chaining accretes the subject by mutating note
relationships per-run and rides the LLM for every extends-a-subject call; the hub
already IS the durable gather-many-notes-into-one-subject primitive (683 exist),
gets the same result deterministically, and preserves note immutability without
new machinery. Items 1 and 2 of that sketch survive in refined form (mechanical
atom = the note rule; incremental assignment = the stability discipline below).

### 3b. Decisions closed

- **A (multi-repo bridging) = A2: drive the clyde files-touched addition FIRST.**
  Verified (handoff finding 2): the contract v1 exposes a single `cwd`-derived
  `repo` per session; clyde's parser strips tool blocks, so file paths never reach
  the export. Scott chose the deterministic end state over shipping sooner:
  clyde grows files-touched (parser extraction + catalog migration + additive
  contract field + schema-version bump), and multi-repo subjects bridge on the
  shared files-touched set deterministically from day one, not on a semantic
  guess. Consequence accepted: second-brain's repo-hub bridging ships AFTER the
  clyde release. Rejected alternative A1 (ship now on `cwd`/`repo`, semantic
  bridge for multi-repo, files-touched as a later upgrade seam) is recorded in
  the handoff doc.
  **Retroactivity caveat (Staff Engineer, stated once, loudly):** files-touched
  is populable only going FORWARD, and only for sessions whose transcripts still
  exist (un-reaped; see B1). Historical multi-repo subjects -- including the
  motivating okta-auth-rs + target-repo example, which already happened -- get no
  deterministic bridge under A2. A2's deterministic bridging is a
  forward-looking guarantee, not a retroactive one.
  **Resolution (Scott, 2026-07-19): semantic fallback for history.** A ONE-TIME
  LLM pass proposes cross-repo bridges for pre-files-touched sessions, delivered
  as approve-diffs (hub-body wikilink additions, per 3a's attachment mechanism)
  -- never applied silently. Scope-bounded: it runs once over history, is limited
  by what un-reaped transcripts still expose, and the fail-closed + receipts
  discipline (3c) applies. Forward-looking bridging stays deterministic on
  files-touched; the semantic pass is a bounded backfill, not a standing
  mechanism. Rejected alternative: leave history unbridged (hand-add hub
  wikilinks case-by-case).
- **B (uncommitted hub-body synthesizer) = closed by evidence, no orphaned prod
  code.** The rich `entities/*.md` bodies were written ONCE, by three parallel
  agents in the 2026-07-02 system-review session, for the 15 highest-traffic
  hubs; committed in obsidian `8415336`; all 15 stamped `hub-synthesized:
  2026-07-02`; zero commits have touched `entities/` since. The session note
  (`notes/2026-07-02-second-brain-system-review.md:66`) records the intent: the
  durable feature is a future `--synthesize` mode in cortex. B therefore converts
  from governance smell to build item: repo-hub body synthesis = build
  `cortex hub --synthesize` in-repo.
- **C (new-hub eagerness) = propose-new, gated by recurrence.** When the semantic
  layer sees a recurring anchorless theme (N sessions, never a one-off), it
  proposes a new hub as a diff through the approve-diffs gate -- same model as the
  existing `entity-proposals.yml` pipeline. Attach-only rejected: novel subjects
  would stagnate invisibly.
- **D (mechanical vs semantic conflict) = provenance fixed, membership
  overridable.** The `repo:` stamp is always recorded as truth (the note never
  lies about where it happened); hub MEMBERSHIP is what the semantic layer may
  adjust, and only as a proposed diff. Mechanical-always rejected: the
  okta-auth-rs-style session would file under the wrong subject forever.

### 3c. Stability disciplines (Scott's "can later sweeps add groupings stably?")

Additions are MONOTONIC; grouping is never frozen:
- Additive, never retractive: a sweep may add membership, never silently remove.
- Incremental assignment, never global recluster (the load-bearing rule).
- Sticky: the bar to MOVE an existing assignment >> the bar to first-assign; a
  move is a proposed diff, never a silent sweep action.
- Durable hub identity, living hub body: re-synthesize summaries; never re-slug
  or delete.
- The approve-diffs gate IS the stability guarantee: no silent structural reorg.
- Tests that bite: a determinism test (sweep twice on a frozen corpus ->
  identical deterministic-anchor groupings) and a stability harness (assign, add
  N notes, re-run -> zero previously-assigned notes moved; additions only).
- Harness hardening (Architect finding 4, folded): the monotonic harness must
  also cover `apply_linking` -- promoting a term into `glossary.yml` makes
  `cortex link` rewrite landed notes to insert new wikilinks, so the harness
  asserts linking only ever ADDS wikilinks, never removes or alters an existing
  one (otherwise "additive, never retractive" is unenforced). "Never re-slug or
  delete a hub" likewise gets a programmatic guard/test, not just prose.
- Scope note: the frozen-corpus determinism test exercises cortex only. It
  cannot detect the concept-recall gap in 3a (LLM body variance upstream of
  linking); that is covered separately in 3f.
- The semantic layer fails closed (Staff Engineer): the LLM edge-case passes
  (multi-repo bridging, propose-new hub, off-glossary concepts) get the same
  loud-failure posture as the deterministic layer -- a failed or malformed LLM
  pass produces NO proposals plus a visible error, never silent partial output;
  every proposal carries provenance (which sessions/notes drove it), matching
  the harvest doc's receipts discipline.

### 3d. Net-new code

**second-brain side (every piece verified against the code 2026-07-19; larger
than the handoff's "three items" -- the schema/plumbing layer was missing from
that inventory, Architect finding 2):**
1. Repo-hub minting: a `repo` `HubKind`, OPEN-set like creator/source hubs
   (`cortex/src/hub.rs:121-175`). Any `repo:` value seen mints
   `entities/repo-<slug>.md`. Verified net-new: `HubKind` today is
   `{Concept, Creator, Source, Tag}`.
2. A deterministic `repo-member` edge (note `repo:` -> repo hub). Correction to
   the handoff (Architect finding 3, code-verified): the cited "mirror,"
   shared-tag -> hub routing (`graph.rs:274-288`), is CONDITIONAL -- it fires
   only for over-cap blanket tags and only if the hub note already exists;
   creator/source emit note<->note edges, never note->hub. There is no existing
   unconditional note->hub membership primitive to copy. The repo-member edge is
   genuinely new routing logic: unconditional, fires for every note with a
   `repo:`, minting/expecting the hub per item 1. Still deterministic and
   frontmatter-driven; just honestly new, not a mirror.
3. The `repo:` anchor wiring chain (both reviewers' top finding; the full
   end-to-end path a session note travels to acquire a `repo:` that reaches a
   deterministic edge -- each step verified missing 2026-07-19):
   - **Emission (in NEITHER doc today):** the harvest doc's session-note
     frontmatter spec has no `repo:` field, and in the live vault exactly ONE
     note (a hand-generated audit note, not a borg product) carries one. The
     harvest doc must be amended so borg's note renderer writes `repo:`, and
     harvest re-run before repo-hubs gather anything. This amendment is a
     required work item and ship-order dependency. It does NOT gate on the clyde
     release -- the single `cwd`-derived `repo` is already in contract v1
     (`clyde sessions/src/export.rs:122-126`); only files-touched does.
   - **Canonical form (determinism defect if unpinned):** the one live `repo:`
     is an absolute path (`/home/saidler/repos/scottidler/loopr-v5`); clyde
     exports `<org>/<repo>`. `collect_stubs` slugifies whatever it is handed, so
     the two forms mint DIFFERENT hubs for the same repo. Pinned: the canonical
     stored form is clyde's `<org>/<repo>`; inbound values normalize to it, and
     a value that cannot be normalized is rejected LOUDLY and mints nothing
     (fail closed -- this gate also answers the open-set garbage-mint concern:
     validation-at-ingest makes wrong-hub rollback a non-case rather than a
     policy fight with the never-delete discipline).
   - **Typed field:** `repo` on `vault::Frontmatter` (`frontmatter.rs:20-47`;
     today falls into the untyped `extra` map), plus the multi-repo storage
     field A2 implies (e.g. `related-repos:` derived from files-touched; exact
     name at design time).
   - **Index column + row plumbing:** edges are built from DB rows, not parsed
     frontmatter, so the edge REQUIRES a `repo` column on the `notes` table
     (idempotent `ALTER TABLE ADD COLUMN`, exactly the proven
     `ensure_trace_columns` pattern, `schema.rs:346-367`), bound in the upsert
     (`index.rs:230-245`), carried on `GraphNoteRow` + its SELECT
     (`search/graph.rs:83-91`, `:322`). (The existing `cortex_repo_*` columns
     are GitHub-repo-note enrichment, NOT this.)
   - **Hub-kind mechanics:** `HubKind::Repo` needs an explicit slot in the
     first-wins precedence chain (concept > creator > source > tag,
     `hub.rs:120`), and the `repo-` prefix must be baked into the collision key
     itself (hub paths are `<slug>.md`), or a repo sharing a name with a concept
     collides.
   - **Backfill stance:** existing notes without `repo:` simply never join repo
     hubs -- acceptable, but stated; no rewrite of landed notes.
   Honest framing (the Staff Engineer's hardest question, answered): repo is NOT
   "just the fourth hub dimension." Creator/source/concept are already typed,
   column-backed, upsert-populated fields with corpus-wide data; repo is a
   vault-schema + emission + cross-repo-contract feature that ALSO mints a hub
   kind. Small steps, but they span frontmatter.rs, index.rs, search/graph.rs,
   hub.rs, graph.rs, and the harvest doc -- phasing must treat it as such.
4. Concept promote command: `entity-proposals.yml` -> `glossary.yml` as a
   reviewable diff (inverse of `write_proposals`, which exists), plus
   repo-tracking the proposals file (today untracked under `~/.config/sb/`).
5. `cortex hub --synthesize` (from decision B). NOT a nice-to-have (Staff
   Engineer): a freshly minted repo hub is a one-line stub (`render_hub`,
   `hub.rs:178-186`) and membership otherwise lives only in the edges table --
   there is no readable "these N notes are this subject" view until synthesis
   exists. "SEE the subject whole" as a BROWSABLE page depends on this item;
   sequence it as a dependency of the core value. Failure handling required
   (Architect finding 6): LLM synthesis that fails or truncates fails LOUD and
   leaves the prior hub body intact -- never a blank or partial overwrite.

**clyde side (decision A2; ships FIRST):** parser extraction of files-touched
from tool blocks + catalog migration + additive contract fields. Version
correction (design-research, 2026-07-19, supersedes the earlier "two surfaces
bump" note): only the on-disk catalog `SCHEMA_VERSION` bumps (`db.rs:32`,
PRAGMA user_version migration). `EXPORT_SCHEMA_VERSION` (`export.rs:75`) stays
1 -- the contract's own rule is additive-within-major is compatible, and
files-touched is additive. Designed in
`clyde/docs/design/2026-07-19-files-touched-export.md`.

### 3e. Cross-repo blast radius and ship order

Decision A2 amends the earlier "Impact: None" call below: clyde now has forced
work. Ship order: **clyde (files-touched contract bump) -> second-brain
(repo-hubs + bridging) -> obsidian vault (data, no code)**. Refinement (review
finding 8): only MULTI-REPO BRIDGING actually consumes files-touched; the
single-repo repo-hub slice (3d items 1-3) depends only on the `cwd`-derived
`repo:` the contract already exposes, so it is not technically gated on the
clyde release. Whether to phase it ahead of clyde or keep one post-clyde unit is
a phasing-time call; A2's ship order stands for everything bridging touches.
Pathway #2 remains undesigned and deserves its own design doc (section 1); do
not let the subject-grouping detail crowd it out.

### 3f. Acceptance criteria (falsifiable; review finding 4)

- **Repo determinism:** a session note carrying `repo: X` joins hub `repo-x` on
  every sweep; a note carrying repos X+Y (post-A2 bridging) joins BOTH hubs
  deterministically.
- **Frozen-corpus determinism:** sweep twice on a frozen corpus -> identical
  repo/creator/source/tag groupings, byte-for-byte.
- **Monotonicity:** assign, add N notes, re-run -> zero previously-assigned
  notes moved; additions only. Includes the `apply_linking` add-only assertion
  and the never-re-slug/delete hub guard (3c).
- **Concept recall (NOT covered by the frozen-corpus test):** an eval set of
  session notes with known concepts -> measured fraction whose concept-hub
  membership actually landed, bounding the glossary/alias-coverage gap from 3a.
  A recall drop is a signal to grow aliases, never to loosen determinism.
- **Immutability:** no sweep, linking pass, or membership override ever
  modifies a landed note's body or frontmatter; semantic assignments appear
  only as hub-body diffs through the approve-diffs gate.
- **Canonicalization:** any inbound `repo:` value normalizes to the canonical
  `<org>/<repo>` form before minting -- the same repo seen as an absolute path
  and as a slug yields ONE hub; a non-normalizable value is rejected loudly and
  mints nothing.

## Impact on the in-flight clyde export contract

~~None.~~ **Amended 2026-07-19 by decision A2 (section 3b):** the export contract
gains a files-touched field via an additive schema-version bump, shipping in clyde
before second-brain consumes it. The Phase 0 7-item dispositions otherwise stand.
Both the pathway-#2 vision and the subject-clustering requirement remain consumer
(borg) concerns. Two dispositions are reinforced by this input:
- **B1 (staged-body fallback)** is doubly required: cross-day subjects pull bodies of
  old archived sessions whose live transcripts have been reaped.
- **B2 (subagent flag in body)** is signal for the "how I work with AI" goals.
