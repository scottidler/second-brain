# Handoff: subject-grouping + knowledge-corpus direction for the harvest work

**Status:** CLOSED 2026-07-19 -- all four decisions resolved with Scott
(A = A2 clyde files-touched first; B = closed by evidence, one-off 2026-07-02
session pass, obsidian commit `8415336`, durable feature = `cortex hub
--synthesize`; C = propose-new recurrence-gated; D = provenance fixed /
membership overridable). The agreed direction is folded into
`2026-07-18-harvest-knowledge-goals.md` section 3, which is now the doc of
record. This file remains as the evidence/option-analysis record.
**Written:** 2026-07-19, handoff from an interview + verification session with Scott.
**To:** the next agent (fable) picking up "what we should/shall do."
**Relates to (read these first):**
- `2026-07-18-harvest-knowledge-goals.md` (captured goals + the clustering
  requirement this session designs against)
- `2026-07-17-harvest-clyde-sessions.md` (pathway #1 build, frozen against the
  clyde export contract; its thread-clustering rule is discussed here)
- `clyde/docs/design/2026-07-17-session-export-contract.md` (the contract; a
  finding below turns on what it does and does not expose)

## 0. How to use this doc

The prior session did three things: (1) elicited Scott's requirements for the
knowledge-harvest flywheel, (2) worked out a subject/topic grouping design that
reuses the vault's existing machinery, and (3) ran four verification probes that
either confirmed or reshaped the design. This doc records all three plus the two
decisions still open. Your job, fable: help Scott close A and B, then fold the
agreed shape into `2026-07-18-harvest-knowledge-goals.md` as a design-direction
section and route it to the review panel.

Evidence is cited to `file:line` throughout; the claims were checked, not
assumed. Do not re-derive what is cited unless you doubt it.

## 1. The vision, as clarified this session

Two clarifications changed the frame from the 07-18 goals doc:

**1a. Capture the SUBJECT, not just the HOW.** The 07-18 doc leads with the
paramount goal (mine HOW Scott works -> morph setup files, pathway #2). Scott
clarified he does NOT only want the HOW lens. He also wants to capture **the
subject/topic of what he worked on** (the WHAT, pathway #1) -- and he wants a
Claude session treated as **just another borg source**, symmetric with how borg
already ingests a YouTube video or a blog: the source's subject gets distilled
into the vault. The external-source pipeline is the proven template; pathway #1
should reuse it, not build a parallel path.

**1b. One brain, many taps.** The session-mining flywheel is ONE input tap into
a larger personal knowledge corpus that also ingests external material (YouTube,
blogs, GitHub, his own notes). All of it lands in the same vault and is searched
by the same oracle. So "one oracle search spans my sessions AND my reading" is a
goal -- and, per the findings below, it comes essentially for free once sessions
share the hub primitive with external sources. NOTE: external ingestion
(webpages, articles, YT transcripts) already exists in borg today; Scott raised
it as a COMPARISON baseline, not a new build request. Do not scope net-new
external taps off 1b unless Scott asks; the live ask is making sessions
symmetric with what borg already does.

## 2. Elicited requirements (pathway #2 / the flywheel)

From a structured interview. These are Scott's answers, treat as requirements:

- **Trust model: proposed diffs Scott approves.** When the pass mines a candidate
  change to a setup file (`rules/*`, `CLAUDE.md`, `VOICE.md`, `IDENTITY.md`),
  nothing auto-lands; it arrives as a diff he accepts or rejects. Auditable.
- **Digest is two-tier by confidence.** High-confidence findings arrive as
  ready-to-apply diffs; lower-confidence as themes-with-evidence he judges.
- **Scope: everything** -- home + work (`tatari-tv`), all repos -- into **one set
  of personal files** (he explicitly chose one set over a persona split).
- **Delivery: a periodic digest** he sits down with (weekly/monthly), not silent
  background morphing, not mid-session nudges.
- **Success signals: all four matter** -- (i) setup files visibly sharpen over
  time, (ii) fewer repeated corrections (he stops re-teaching the same thing),
  (iii) others could onboard to "how Scott works," (iv) it is measurably scored.

**Hard constraint that falls out of "everything -> one set of files":** because
work sessions can feed shareable files, the extraction stage MUST scrub Tatari
specifics down to persona-neutral patterns before anything touches a file that
could be shared. This is a requirement, not an open question. (Scott's decision
to use one set of files stands; this is the guardrail on it.)

**Measurability -- Scott said "I don't know," and that is fine.** Proposed
direction he can react to (NOT decided):
- v1 = cheap proxies, immediately: accept/revert rate on proposed diffs + a
  one-line "did this help?" when a landed rule returns next cycle. Honest about
  what it measures: quality of extraction + his approval, not behavior change.
- North star = "the correction stopped recurring." The only signal that measures
  the flywheel actually working. It is buildable: the forensic passes that built
  `rules/taste.md` and `rules/interaction.md` already grouped sessions by
  correction class, so the metric is a longitudinal query -- did correction-class
  X stop appearing after rule Y landed -- riding the same subject/session
  clustering pathway #1 is being built to support. Build proxies first, grow into
  the north star as the corpus + clustering come online.

## 3. The subject-grouping design (pathway #1 / the WHAT)

This is the technical meat, and the four findings in section 5 shaped it.

### 3a. The anchor
Scott's stated anchor: **the repo is key; if there is no repo, the folder path of
files read/changed is key.** Grouping relates to a path (usually a git repo) but
sometimes MULTIPLE paths worked as one effort (his running example:
`okta-auth-rs` + a target repo). The important thing is to **SEE the subject
across many sessions and days**, not blindly copy each session over.

### 3b. Multi-anchor membership
"Subject" is not one key. A session note lands in the SET of hubs its content
anchors to, and different sessions anchor on different dimensions. The vault
already mints hubs on four dimensions; only repo is net-new:

| Anchor | Hub today? | Membership driver |
|---|---|---|
| Repo / path (files touched) | NO -- net-new | frontmatter `repo:` (+ files-touched later; see finding 2) |
| Concepts (terms mentioned) | YES -- glossary concept hubs | linker wraps first prose mention -> `entities/<concept>.md` |
| URLs / source hosts | YES -- source hubs (OPEN) | `source:` host + body URLs |
| People / creators | YES -- creator hubs (OPEN) | `creator:` field |

A file-heavy build session anchors on repo; a research session with no repo
anchors on concepts + URLs; a design discussion anchors on concepts. Same hub
machinery, different anchor. Non-file subjects (URLs, concepts) are NOT a special
case -- they fall through to hub kinds that already exist.

### 3c. The two-layer reframe (the key move)
The findings collapse the design into two clean layers, and this DEFUSES the
review panel's nondeterminism objection instead of reopening it:

- **Note level -- keep the harvest doc's deterministic thread rule as-is.**
  `(cwd, git-branch, gap)` within a run decides which sessions become one NOTE.
  Do not reopen judgment call #2 at this level.
- **Subject level -- add repo as a hub dimension.** The note may stay fragmented
  across days; the HUB is where the subject is whole. A repo hub gathers every
  note carrying that `repo:` across sessions/days/runs, with zero note-merging
  and zero LLM.

So "SEE the subject whole" lives in the **hub**, achieved deterministically. The
nondeterministic/LLM budget is reserved only for the genuine edge case: a subject
that spans repos or has no anchor at all.

### 3d. The edge cases that need the (sparing) semantic layer
1. Multi-repo subject with no shared anchor (e.g. `okta-auth-rs` + target repo).
   See decision A -- how this is bridged depends on the files-touched decision.
2. A concept not yet in the glossary (emerging vocab) -- reuses the existing
   proposal pipeline (finding 4).
3. A subject with NO anchor at all (Scott's "or something else": a recurring
   theme/idea/decision that is not a repo, known concept, or URL). This is the
   one place to PROPOSE a brand-new subject hub when a cluster shares a theme but
   hits no existing hub. New-hub creation lands on the approve-diffs model.

### 3e. Stability (Scott asked: "can later cortex sweeps add groupings stably?")
Answer: yes -- make additions MONOTONIC, do not freeze grouping. Disciplines:
- Additive, never retractive (a sweep may add membership, never silently remove).
- Incremental assignment, never global recluster (this is the load-bearing rule).
- Sticky/hysteresis: bar to MOVE an existing assignment >> bar to first-assign; a
  move is a proposed diff, never a silent sweep action.
- Durable hub identity, living hub body (re-synthesize the summary; never re-slug
  or delete).
- The approve-diffs gate IS the stability guarantee: no silent structural reorg.
- Prove it with tests that bite: a determinism test (run the sweep twice on a
  frozen corpus -> identical deterministic-anchor groupings) and a stability
  harness (assign, add N notes, re-run -> assert zero previously-assigned notes
  moved; additions only).

## 4. What the vault already does (grounding -- build on this, invent nothing)

From a full mapping pass over the vault + cortex:
- **Durable subject unit already exists: entity hubs.** 683 of them,
  `entities/*.md`, `type: entity`, auto-generated by `cortex hub`. This is the
  "gather many notes into one subject" primitive. Real ones: `entities/anthropic.md`,
  `entities/claude.md` ("roughly 480 notes link here").
- **MOCs are the wrong tool.** They exist (`type: moc`) but only ~5, all
  hand-authored, broad topics. NOT the auto-grouping mechanism. (An earlier
  strawman proposed MOC-per-repo; it was scrapped in favor of hubs.)
- **`repo:` frontmatter already exists** on generated notes (e.g.
  `repo: "/home/saidler/repos/scottidler/loopr-v5"`), plus a descriptive
  free-text `subject:` field.
- **Folders are flat** (`notes/`); `domain` is metadata, not a folder. No
  `by-repo/` tree. Placement is `cortex/src/classify.rs`.
- **Association = body wikilinks + DB edge graph.** There is NO `links:`/`related:`
  frontmatter field; do not introduce one.
- Key files: `cortex/src/hub.rs`, `linking.rs`, `graph.rs`, `classify.rs`,
  `tags.rs`, `entities.rs`; `config/{canonical-tags,tag-mapping,glossary}.yml`.

## 5. The four verification findings

### Finding 1 -- Stability is essentially FREE (confirmed)
Hub membership is **wikilink + deterministic-metadata driven, not cosine driven**.
A note joins a hub by a `[[slug]]` in its immutable body (wikilink edge,
`graph.rs:252-262`) or by deterministic shared-tag/metadata routing to the hub
(`graph.rs:274-288`). The auto-linker is provably idempotent: authored-note guard
(`linking.rs:117-121`), existing-link dedup (`linking.rs:81-83,135-136,307-313`),
first-mention-only (`linking.rs:254-305`), inside-wikilink guard
(`linking.rs:503-510`). Cosine `semantic` edges (`graph.rs:247-250`) DO drift as
the corpus grows, but they are note-to-note neighbors, NOT the hub-membership
mechanism. => The visible grouping is stable-by-construction, provided repo-hub
membership is built the same deterministic way. Caveat: see finding 3 -- the code
that writes the "N notes link here" roll-up is not in the repo, so if that
external pass counted semantic neighbors it would churn; the edge SUBSTRATE it
should draw from is stable.

### Finding 2 -- files-touched is NOT in the contract; it's a clyde-side add (reshaped the design)
The `session export` contract v1 exposes a **single `cwd` -> single derived
`repo`** per session (`clyde .../sessions/src/export.rs:122-127`,
`db/query.rs:23-53`). The `--with-body` payload is text-only: clyde's parser
strips every `tool_use`/`tool_result` block (`clyde .../session/src/parse.rs:247-272,
488-498`), so file paths never reach the export and are never stored in the
catalog (`db.rs:84-85`). The contract doc explicitly non-goals this: "Not
tool-call counts: requires new parser extraction. Excluded from v1"
(`...contract.md:36`). => "files-touched set bridges multi-repo subjects
deterministically" is NOT buildable against frozen schema-version 1. Getting it
is a three-layer clyde change (parser extraction + catalog migration + additive
contract field + schema-version bump) that must ship and release in clyde before
second-brain can consume it. Today the only path signal is the single
`cwd`/`repo` -- the harvest doc's stated fallback becomes the only option. This
is decision A.

### Finding 3 -- the hub-body synthesizer is in NO repo (governance smell)
The rich `entities/*.md` bodies ("roughly 480 notes link here",
`hub-synthesized:`) are written by something on **no branch and nowhere in git
history** (pickaxe `git log --all -S "hub-synthesized"` empty; same for "notes
link here", "Key insights across sources"). cortex's own `render_hub`
(`hub.rs:178-186`) writes only a one-line stub and `write_stubs` refuses to
overwrite an existing hub file (`hub.rs:198-202`) -- which is exactly why the
external bodies survive. => There is a synthesis pass running against the live
vault that lives in no repo. It is load-bearing (repo-hubs would want that same
synthesis) AND a standalone root-cause item: production code that exists in no
repo. This is decision B.

### Finding 4 -- concept grouping already has ~80% of the proposal machinery
Glossary is **semi-open**: `cortex entities --discover` runs a daily off-hot-path
LLM pass that proposes new concepts into `entity-proposals.yml` (frequency-ranked,
with provenance; `cortex/src/entities.rs:1-12,95-202`), bounded 50 notes/pass,
daily cadence (`daemon.rs:192-195,363-375`). It NEVER auto-promotes -- a human
hand-edits `glossary.yml`. Creator and source-host hubs are **OPEN** (minted from
any value seen in notes; `hub.rs:143-158`); only concept hubs gate on the glossary
(`hub.rs:132-138`). => For the approve-diffs model, net-new is small: a **promote
command** (`entity-proposals.yml` -> `glossary.yml` as a reviewable diff; today
promotion is manual) and repo-tracking the proposals file (it currently lives
only under `~/.config/sb/`, untracked). It is the inverse of `write_proposals`,
which exists.

## 6. What is net-new code (second-brain side) -- small, all mirrors existing patterns

1. **Repo-hub minting** -- add a `repo` `HubKind`, OPEN-set like creator/source
   hubs (`collect_stubs`, `hub.rs:121-175`). Any `repo:` value seen mints
   `entities/repo-<slug>.md`.
2. **A deterministic `repo-member` edge** (note.`repo:` -> repo hub), mirroring
   the existing shared-tag -> hub routing (`graph.rs:274-288`) exactly.
   Frontmatter-driven, so stable-by-construction (this is the repo analog of
   finding 1). NOTE: repo membership is edge/frontmatter driven, not
   wikilink/lexical driven (a note does not say "okta-auth-rs" in prose), so it
   does not ride the linker; it is its own deterministic edge.
3. **Concept promote command** (finding 4).

That is the whole build on the second-brain side. Deterministic, pattern-matched,
stable. The clyde side (finding 2 / decision A) is separate and larger if chosen.

## 7. OPEN DECISIONS -- Scott must close these (they are why this is a handoff, not a plan)

### Decision A -- multi-repo bridging strategy (the real fork; finding 2)
- **Option A1 (my lean): ship now on `cwd`/`repo` + a semantic bridge for the
  multi-repo case; treat files-touched as a LATER clyde upgrade** that swaps the
  semantic guess for a deterministic bridge once clyde exposes it. Unblocks the
  feature immediately; multi-repo leans on the small LLM budget until upgraded.
  Matches Scott's "implement one, write as if more are coming" + "defer capacity
  features until observed problem." Files-touched becomes a documented
  deterministic-upgrade seam, not a v1 blocker.
- **Option A2: drive the clyde files-touched addition FIRST** (parser + catalog +
  contract bump), so multi-repo is deterministic from day one. Cleaner end state,
  but blocks second-brain behind a clyde release + a real cross-repo migration,
  and forces ship order clyde -> second-brain again.

### Decision B -- the uncommitted hub-body synthesizer (finding 3)
Does Scott already know what writes those hub bodies (a hand-run fabric/LLM pass,
a script under `~/.config/sb`, another repo)? If yes, that closes it. If no, hunt
it down before leaning on hub-body synthesis for repo-hubs. Production code that
lives in no repo is a root-cause item regardless of this feature.

### Decision C -- new-hub creation eagerness (from section 3d, case 3)
- **Attach-only:** sessions only join existing hubs; a novel anchorless subject
  sits ungrouped until Scott mints a hub by hand. Zero drift; novel stuff
  stagnates.
- **Propose-new (my lean), gated by recurrence:** when the semantic layer sees a
  recurring anchorless theme (or an off-glossary concept recurring), it proposes
  a new hub/concept as a diff. Require the theme to appear in N sessions first --
  never mint a hub for a one-off. Catches novelty; costs approval clicks.

### Decision D -- mechanical vs semantic conflict resolution
When a session is in repo A but its content clearly belongs to the repo-B
subject: proposal (not decided) -- the `repo:` stamp is ALWAYS recorded as truth
(the note never lies about where it happened); hub MEMBERSHIP is what the semantic
layer may override. The note's provenance is deterministic; the subject it
belongs to may be smarter than its path.

## 8. Two caveats also worth flagging to Scott

- **Self-referential drift (pathway #2).** Mining sessions that were themselves
  shaped by `taste.md` risks the pass converging on its own priors. Treat a mined
  rule as a hypothesis; the next sessions are its eval (which also makes the
  flywheel measurable -- ties to section 2's north star).
- **Pathway #2 is still largely undesigned.** This session advanced pathway #1's
  grouping design substantially and elicited pathway #2's requirements, but
  pathway #2 (mine-HOW -> morph setup files) still deserves its own design doc per
  the 07-18 goals doc. Do not let the subject-grouping detail crowd it out.

## 9. Suggested next step (process)

1. Scott closes A and B (and ideally C, D).
2. Fold the agreed shape into `2026-07-18-harvest-knowledge-goals.md` as a
   design-direction section (per the funnel: everything agreed lands IN the doc).
3. Route to the review panel. The two-layer reframe (section 3c) is specifically
   what they should check -- it claims to DEFUSE the judgment-call-#2
   nondeterminism objection by keeping note-level clustering deterministic and
   confining the LLM to cross-repo/anchorless edge cases. Verify that claim holds.
4. Only then does any of this become phased implementation work.

## 10. References
- `docs/design/2026-07-18-harvest-knowledge-goals.md`
- `docs/design/2026-07-17-harvest-clyde-sessions.md`
- `clyde/docs/design/2026-07-17-session-export-contract.md`
- `docs/design/2026-06-05-graph-augmented-memory.md` (MemGraphRAG / fact edges)
- Code: `cortex/src/{hub,linking,graph,classify,tags,entities,daemon}.rs`;
  `config/{canonical-tags,tag-mapping,glossary}.yml`; live vault
  `~/repos/scottidler/obsidian/entities/`
