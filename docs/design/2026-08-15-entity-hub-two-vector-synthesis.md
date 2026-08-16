# Design Document: entity hub bodies, merging the two knowledge vectors

**Author:** Scott Idler (via agent)
**Date:** 2026-08-15
**Status:** Draft (rev 2)
**Provenance:** Rewrite of the same-day rev 1 draft. Rev 1's measurements were
independently re-verified against the live vault and oracle index and carry
over (three corrected, noted inline). Rev 1's mechanism (per-hub LLM prose
synthesis with a `## Tension` section) is superseded by deterministic assembly;
the LLM path is recorded under Alternatives with the reasoning. Panel review of
this revision: round 1 complete (4 must-fix, 6 cheap wins, all folded; both
seats endorse the deterministic-assembly move); round 2 complete (7
must-fix, 3 cheap wins, all folded and independently re-verified; the
architect seat failed rc=1 in-round and its re-dispatch — round 2b —
independently confirmed 4 of the 7 and settled member ordering:
date-descending, path tiebreak); round 3 complete, both seats (3 must-fix —
per-vector digest floors, tldr definition sentence, RenderConfig plumbing —
plus 5 cheap wins, all folded); round 4 complete, both seats (4 must-fix —
mass-reset hazard split into load-error-preserves + run backstop, digest
integer math pinned, offline tokenizer fixture, N/M pinned to full counts —
plus 3 cheap wins, all folded; simulation validates the digest contract
22/22); round 5 complete, adversarial, both seats (3 must-fix — round 4's
`hub-synthesized:`-as-manual-marker REVERSED after both seats proved the 15
are Fabric output not hand-written prose, so they are overwritable and the
real override key is the new `hub-body: manual`; atomic hub writes via
`write_atomic`; digest arithmetic byte-pinned and the budget key renamed
`summary-byte-budget` — plus 2 cheap wins, all folded; the four-branch
discipline + backstop held under adversarial review, and member ordering
was scrutinized on the merits and endorsed by both seats). Review loop
closed by owner decision after round 5's folds; the round-5 fixes are
mechanical spec changes, not open design questions.

## Summary

Two ingestion vectors feed the vault: external content (index counts: youtube
1055, article 376, github 58, social 26, research 28) and Claude Code sessions
(261). They share a schema, a distill contract, a tag vocabulary, a retrieval
index, and an `entities/` hub layer. The hub layer is where they are supposed
to meet, and it is empty in three ways:

1. **Synthesis is fed filenames, not content.** `FabricHubSynthesizer`
   (`cortex/src/hub.rs:438-446`) prompts the `summarize` pattern with a bare
   list of member note paths. The model cannot see any note, so it invents or
   refuses. **134 of 857 hub bodies are the literal refusal** ("I don't have
   access to the actual content of those files"), and all 134 are
   `quality=medium`, so they pass oracle's stub filter and are served as live
   search results today.
2. **The repo-hub layer is code-complete and data-empty.** 261/261 session
   notes carry `repo:` across 33 distinct values (32 valid); `entities/repos/`
   holds ONE hub and the index holds 4 `repo-member` edges. The built
   `hub --apply` -> `oracle index` -> `graph` chain has never been run against
   the harvest corpus.
3. **The linker mints false membership from common words.**
   `entities/every.md` (the every.to publication, a legitimate creator hub) has
   603 members because the word "every" in prose case-insensitively matches a
   5-char title under `min-word-length: 5` (`cortex/src/linking.rs:141`).

This doc runs what is already built (Phase 0), gives creator/source hubs a
real membership primitive and kills the common-word matcher (Phase 1), then
replaces LLM synthesis with **deterministic assembly**: a hub body is the
member notes' already-distilled claims, grouped by vector, quoted and
wikilinked (Phase 2). Phase 3 is the asymmetry report. **Zero Fabric calls in
the hub path**; the plan's one LLM cost is pre-existing (Phase 1's mandatory
`graph --backfill` re-runs the fact-layer extraction, stated in-phase).

**Terminology, fixed for the whole doc.** *Sources* = notes of type
`youtube | article | github | social | research` (the external-content
vector). *Sessions* = notes of type `session` (the Claude Code vector).
*Vector* means one of those two, never a math vector; the embedding kind is
always written "embedding".

## Problem Statement

### Background

The hub layer came from `docs/design/2026-06-05-graph-augmented-memory.md`
(Phase 3: auto-stub entity/creator/source hubs) and was extended by
`docs/design/2026-07-17-harvest-clyde-sessions.md` (Phase 10: `HubKind::Repo`
at `entities/repos/<org>/<repo>.md`; Phase 12: `cortex hub --synthesize`,
success criterion: a stub "synthesizes into a readable 'these N notes are this
subject' body"). Both shipped; the criterion was never executed against a real
hub. The unit tests inject a `HubSynthesizer` double whose `_members` argument
is ignored (`cortex/src/hub/tests.rs:371,377,389`), and `FabricHubSynthesizer`
has zero test references, which is exactly how paths-instead-of-content
shipped green.

### Why the hubs are empty (measured)

**Not a memberless problem.** Exactly ONE hub of 858 has zero inbound edges;
857 have members and would synthesize. The input carries no content.

**Membership is broader than any deliberate signal.** `hub_members`
(`vault/src/search/graph.rs:244`) is `SELECT DISTINCT src FROM edges WHERE
dst = ?1` over every kind. Edges pointing at hubs, live:

| kind | count | deliberate? |
|---|---|---|
| `semantic` | 8613 | no (embedding similarity) |
| `wikilink` | 8317 | yes |
| `shared-tag` | 3408 | no (co-tagged) |
| `fact` | 359 | hub->hub, not note->hub |
| `repo-member` | 4 | yes |

All 359 `fact` edges have an `entities/%` src, and 1685 `wikilink` edges into
hubs ALSO have a hub as src (25 of those hub-sources currently hold the
refusal string). Any hub body built from unfiltered membership feeds other hub
bodies — including refusals — back into itself.

(Rev 1 claimed "other callers depend on the broad view" of `hub_members`.
False: its only production caller is synthesis itself, `hub.rs:402`;
everything else is tests. Corrected here.)

**No membership primitive exists for creator/source hubs.** `shared-creator`
edges (9466) are note->note only; zero point at `entities/`.
`entities/every.md`'s entire inbound is `wikilink` 569 + `semantic` 55, all
minted by the prose matcher. Remove the matcher alone and every creator hub
goes to zero deliberate members. And the two source-host functions DISAGREE:
`cortex/src/hub.rs:183` returns `Option<String>` (`None` on schemeless) while
`cortex/src/graph.rs:431` returns `String` (lowercased passthrough), so the
hub side mints nothing where the graph side yields a bucket key.

### Evidence (re-measured 2026-08-15 against the live vault and index)

- 857 flat hubs in `entities/*.md` + 1 nested repo hub = 858 index rows.
- 134 hub bodies contain the refusal string; all 134 are `quality=medium`
  (hub quality: 47 low / 811 medium), so oracle's stub filter
  (`oracle/src/server/pipeline.rs:389-415`, drops `low` only) passes them.
- All 858 hubs are in `notes`, `notes_fts`, and `note_embeddings`: BM25- and
  embedding-retrievable.
- 185 hubs still carry the literal `Auto-stubbed by \`sb cortex hub\``
  sentence (rev 1 said 184 in one place and 185 in another; 185 is measured).
- Zero `entities/%` notes carry a `## Claims` section.
- 33 distinct `repo:` values on disk; the 33rd is an absolute path
  `validate_repo_slug` rejects, so the mintable ceiling is 32.
- 282 notes carry a schemeless `source:`; 261 of them are session notes with
  `source: clyde://<uuid>` (208 distinct). `clyde://` is the only non-http
  scheme in the corpus (rev 1 parsed frontmatter from disk to establish this;
  a line grep miscounts because of fenced YAML examples).
- Two-vector split of hub inbound, re-measured with a stated predicate (any
  inbound edge kind; source types as defined above), over all 858 hubs:
  **27 both / 315 external-only / 3 session-only / 513 neither** (sum 858).
  (Rev 1 printed 22/207/1/626 without naming its predicate and it does not
  reproduce; corrected here. Phase 3 reports over deliberate edges only and
  defines its buckets exactly.)
- Hubs with at least one claim-bearing deliberate member under TODAY's edge
  kinds (`wikilink`, `repo-member`): **161 of 858**, of which **160** have
  one typed source-or-session (the Phase 2 body-writing cohort; the odd one
  out is an `image`-typed member). Rev 1's disk simulation of the
  post-Phase-1 state put the cohort at ~480-510.
- Entity-link rate by vector (rev 1 measurement, not re-run): sessions
  135/261 (51%), external content 1181/1598 (73%).

### Who reads a hub body

- **Scott, in Obsidian.** Landing on `[[claude]]` from a note is the point of
  the layer. Today that lands on a stub or a refusal.
- **Oracle retrieval.** Hub bodies are BM25- and embedding-searchable like any
  note. A body carrying the members' claim text is directly retrievable; a
  stub contributes a title match; a refusal is retrieval poison.
- Graph-expansion search would be a third reader but is OFF in the live
  config (`oracle/src/config.rs` defaults the graph retriever disabled;
  `~/.config/sb/oracle.yml` has no `retrieval:` section). Latent, not live.

Both live readers are served by *assembled* content. Neither requires
generated prose.

### Goals

- Run the existing `hub --apply` -> `oracle index` -> `graph` chain so the 32
  repos get hubs and edges. Zero code.
- A hub body carries its members' claim text, grouped `## From sources` /
  `## From your sessions`, each claim wikilinked to its member.
- Hub membership for body-building is deliberate note->hub edges only, and a
  hub is never a member of a hub (structural SQL predicate, not convention).
- A common English word never mints hub membership.
- A hub with nothing to say keeps its stub; the 134 refusal bodies are reset.
- Re-running the builder with unchanged inputs writes zero bytes.
- A hub with one vector and not the other is visible as such (`--asymmetry`).
- Zero LLM calls in the hub pipeline itself (`cortex::hub` never touches
  Fabric). The plan's only LLM cost is the pre-existing fact-layer
  re-extraction inside Phase 1's `graph --backfill`.
- Both vectors of a rendered hub are reachable by the LIVE retriever, not
  just by BM25 (which is off by default in the shipped pipeline).

### Non-Goals

- **Changing content-note bodies.** The harvest doc's binding rule stands: hub
  bodies are rewritable, content notes are not.
- **LLM prose synthesis and `## Tension`.** Deferred, recorded under
  Alternatives with a revisit condition.
- **Raising the 51% session entity-link rate.** Different cause
  (glossary/alias coverage); revisit once hubs are worth linking to.
- **A new hub kind.** `{Concept, Creator, Source, Tag, Repo}` is enough.
- **Cross-vector dedup.** Application vs acquisition is the signal; collapsing
  them destroys it.

## Proposed Solution

### Overview

```
hub_members_deliberate(hub) =
    SELECT DISTINCT src FROM edges
    WHERE dst = ?1
      AND kind IN ('wikilink','repo-member','creator-member','source-member')
      AND src NOT LIKE 'entities/%'
    ORDER BY src                    -- fetch determinism only; the renderer
                                    -- re-sorts by date desc, path tiebreak
  -> load each member, vault::search::parse_body_claims(body)
  -> partition by the member's `type:` into {sources, sessions}
  -> deterministic render:
       ## Summary
       <Title>: hub of N sources and M sessions.
       Sessions: <claim>; <claim>; ...
       Sources: <claim>; <claim>; ...
       ## From sources
       - <claim text> ([[member]])
       ## From your sessions
       - <claim text> ([[member]])
  -> write only when the rendered body differs from the current body
```

No Fabric call. The claims already exist — the L2 distill contract produced
them; the hub is an arrangement of them, not a rewrite. That makes the body:

- **honest** (it can only say what a member note says, with the link to prove
  it),
- **idempotent** (pure function of membership + claims; unchanged inputs write
  zero bytes, so no bookkeeping table and no cost gate),
- **retrievable** (the claim text lands in the hub's FTS row and embedding),
- **cheap** (re-runnable on every hub, every time, for free).

**The `## Summary` digest is load-bearing for retrieval, not decoration**
(panel round 1, M1). The indexer sets `notes.summary` from
`parse_body_summary` (a literal `## Summary` heading,
`vault/src/search.rs:159-179`) with `detail::extract_summary` as fallback —
and the fallback is **the first H2 section in document order**
(`vault/src/detail.rs:147-160`), i.e. `## From sources` alone. Cortex's
summary embedding is `title + capture_note + summary` from that column
(`cortex/src/embed.rs`), claim embeddings are zero for hubs by this doc's
own invariant AND globally default-off (`EmbedKindsConfig { claim: false }`,
`cortex/src/config.rs:228-255`, off since the v0.9.0 nDCG regression; the
live DB has zero claim rows), and the live pipeline is vector-only
(`Bm25Method::enabled` defaults false, `oracle/src/config.rs:117-119`;
`~/.config/sb/oracle.yml` has no `retrieval:` section). So the summary text
is the ONLY embedding surface a hub has. Without a `## Summary` carrying
BOTH vectors, the sessions section is invisible to the live retriever and
the end-to-end acceptance criterion cannot pass.

**And the digest must fit the embedding window** (panel round 2). The
encoder silently truncates at 512 tokens
(`vault/src/embedding/candle.rs:79` `MAX_SEQ_LEN`, `with_truncation` at
`:317`). A claims-per-member cap does not bound the digest: measured on the
22 both-vector hubs, a `Sources:`-first digest pushes the sessions text past
token 512 on all but a handful (`claude.md`'s capped digest runs ~5.2k
tokens with `Sessions:` starting near token 1559 — sessions would never
reach the embedding on exactly the hubs this design exists for). So the
digest is pinned exactly:

- **A static definition sentence leads the digest** (panel round 3):
  `<Title>: hub of N sources and M sessions.` — deterministic, ends with a
  period. This is what `first_sentence` (`vault/src/detail.rs:123-130`,
  cuts at the first terminator) returns, and FIVE oracle handlers default to
  `DetailLevel::Tldr` (`domain_brief`, `find_similar`, `recent_activity`,
  `inbox_status`, `quality_report` — `oracle/src/server.rs:476,715,782,954,
  981`). Without it, every one of them renders a hub as a truncated claim
  wall; with it, tldr is a sane one-liner and the full digest still reaches
  the embedding. **`N`/`M` are the FULL claim-bearing membership counts,
  not the capped set** (panel round 4, M4, pinned): "hub of 20 sources" on
  a 408-member hub would be a false statement on the tldr surface five
  tools render, and truth beats write-avoidance. Accepted cost: a
  membership-count change rewrites the sentence and therefore the file —
  that is a genuine input change under body = f(membership), not churn. On
  a ONE-VECTOR hub the sentence names only the present vector
  (`hub of N sources.` — never "and 0 sessions"), the absent vector's
  digest line and body section are omitted, and the present vector gets the
  entire remaining budget (panel round 4, C2).
- **`Sessions:` line before `Sources:`.** Sessions are the scarcer vector on
  every large hub (claude: 63 session vs 345 source claim-bearing members)
  and the tail is what truncation eats.
- **Drawn from the SAME capped member set that renders in the body**
  (date-descending, path tiebreak) — never a second selection rule — claims
  in body order. (Round 2's two seats read the previous wording two
  different ways and got 5/22 vs 1/22 survival; that gap is why this is now
  pinned.)
- **Bounded by `entities.render.summary-byte-budget` (default 1200 bytes),
  allocated PER VECTOR, not as one shared pool** (panel round 3, both
  seats: a shared Sessions-first pool starves `Sources:` to zero on 8 of
  the 22 both-vector hubs — including `claude.md`, the flagship fixture —
  which just swaps which vector is invisible). The integer math, pinned so
  two correct implementations cannot disagree on a byte (panel round 4,
  M2):

  1. **Everything is counted in UTF-8 BYTES of the exact emitted text**
     (panel round 5, M3: a byte-comparison guarantee cannot rest on a char
     count — non-ASCII claim text makes them differ). The key is named for
     what it counts: `entities.render.summary-byte-budget`.
  2. `remaining = summary-byte-budget - len(definition_sentence + "\n")` —
     the definition sentence AND its trailing newline are always emitted
     and count first.
  3. One vector present: it gets all of `remaining`.
  4. Both present: `sessions_budget = remaining / 2` (integer division);
     `sources_budget = remaining - sessions_budget` (the odd byte goes to
     sources).
  5. A vector line's cost against its budget is the FULL emitted line: the
     `Sessions: ` / `Sources: ` label, every claim, every `; ` joiner, and
     the trailing newline. Single forward pass, sessions first: append
     whole claims while the line stays within `sessions_budget`;
     **sessions never exceed their budget** (a floor is only enforceable
     for sources if sessions are capped). Unused session budget is added to
     `sources_budget` — ceding is ONE-directional (sessions -> sources), no
     lookahead, no second pass; sources slack going unused is the accepted
     price of determinism.
  6. Whole claims preferred, but a vector whose FIRST claim would overflow
     gets it truncated so the line INCLUDING a literal `...` (3 bytes)
     still fits the budget, cut at a UTF-8 character boundary — never mid
     code point (the longest live member `claims` blob is 3460 bytes,
     bigger than any floor, so drop-if-over would zero real vectors).

  Validated by simulation (panel round 4, staff + panel independently,
  after the panel corrected its own first broken run): 22/22 both-vector
  hubs pass the retrieval contract; embed-text token counts run 198-301
  against the 512 window, nothing truncated — the byte budget binds long
  before the tokenizer, which is the intended safety margin.
- **The retrieval contract, asserted under the REAL tokenizer:** for every
  both-vector rendered hub, the embedded text (`title + capture_note +
  summary`) tokenizes to include at least one session claim AND one source
  claim inside the 512-token window. The byte budget is a render-path
  proxy, not the assertion. Measured corpus density: 4.73 chars/token
  aggregate (session 4.07, source 4.96), so 1200 bytes ≈ 254 tokens on
  this ASCII-dominant corpus —
  conservative on purpose; the failure mode was allocation, never density.

Consequence, accepted out loud: oracle's `detail=summary` returns this
digest verbatim (`oracle/src/server.rs:308-312`) and `detail=tldr` — the
default for the five handlers above — returns the definition sentence.
(A claim containing `;` is display-ambiguous inside the digest line only;
`parse_body_summary` is line-oriented and unaffected.)

The hub-feeds-hub hole is closed structurally by `src NOT LIKE 'entities/%'`
in the membership query, not by a pattern-authoring convention. Belt and
suspenders: a rendered body must also parse to zero claims
(`parse_body_claims` keys on a literal `## Claims` heading, which the renderer
never emits), asserted by test.

### Data Model

- No SQL schema change. `entities` and `edges` are untouched. (Rev 1's
  `hub_build_state` table existed only to make repeated LLM runs cheap; a
  deterministic compare-before-write needs no state.)
- New edge kinds `creator-member` and `source-member`, mirroring
  `repo-member`. `edges` already carries `kind`.
- **One new frontmatter key: `hub-body: manual`** marks a genuinely
  human-authored body the builder must never overwrite. ZERO carriers
  today. Reader: the Phase 2 skip check.

  **The 15 `hub-synthesized: 2026-07-02` hubs are NOT hand-written and are
  NOT protected** (panel round 5, M1, reversing round 4's C1 which misread
  the marker). Verified three ways: all 15 bodies carry Fabric `summarize`
  boilerplate (`ONE SENTENCE SUMMARY:` / `MAIN POINTS:` / `TAKEAWAYS:`);
  `docs/design/2026-07-18-harvest-knowledge-goals.md:225-227` records them
  as written by "three parallel agents in the 2026-07-02 system-review
  session"; and the set is exactly the flagship cohort (`claude`, `agents`,
  `mcp`, `rag`, ...). Freezing the 15 highest-traffic hubs in LLM-generated
  prose is the opposite of this design, so the builder overwrites them like
  any other builder-owned body. If a specific body is worth keeping, that is
  an eyes-open per-file Phase 0 decision (set `hub-body: manual` on it);
  the Phase 0 snapshot makes every choice recoverable. `hub-synthesized:`
  keeps its true meaning — a provenance stamp of the 2026-07-02 pass — and
  grants nothing.
- New config, all under existing namespaces:
  - `graph.wikilink-stopwords` (list; the graph builder must not read the
    auto-linker's `actions.linking.*` namespace),
  - `entities.render.max-members-per-section` (default 20),
    `entities.render.max-claims-per-member` (default 3),
    `entities.render.summary-byte-budget` (default 1200), and
    `entities.render.max-render-resets-per-run` (default 20): readability
    caps with a deterministic overflow line, the embedding-window bound on
    the `## Summary` digest, and the mass-reset backstop.
- No new fabric pattern, so no bootstrap registration. `HUB_SYNTH_PATTERN`
  and `FabricHubSynthesizer` are deleted, not `#[allow]`-silenced.

### API Design

- `SearchIndex::hub_members_deliberate(&self, hub_path) -> Vec<String>` with
  the SQL predicate above. `hub_members` (kind-agnostic) stays for the graph
  tests that use it as a generic inbound probe; synthesis stops calling it.
- `pub struct HubMember { path, title, note_type, claims: Vec<Claim> }`.
- `render_hub_body(members: &[HubMember], caps) -> Option<String>` — pure,
  directly testable, `None` when no member carries claims (leave stub). The
  `HubSynthesizer` trait goes away with the LLM; the file-handling
  fail-safe in `synthesize_hub` (frontmatter preserved verbatim, empty output
  never overwrites) is kept as-is.
- `sb cortex hub --synthesize` keeps its flag and its `--apply` gate; only
  the body-production mechanism changes. No new top-level verb.
- `sb cortex hub --asymmetry`: report only, no writes.

### Implementation Plan

#### Phase 0: run what is already built, and snapshot first
**Model:** sonnet

Zero production code. On the daemon host, against the live vault:

- **Snapshot first.** `git -C ~/repos/scottidler/obsidian` commit the current
  `entities/` tree, AND copy the oracle DB
  (`cp ~/.local/share/oracle/oracle.db{,.pre-phase0}`) — the flagless dry
  run below writes its `entities` table until Phase 2 lands the gate, so the
  vault-git snapshot alone does not cover everything Phase 0 touches (panel
  round 3). Then the one editorial decision: review the 15
  `hub-synthesized: 2026-07-02` bodies (enumerate via
  `grep -rl '^hub-synthesized:' entities/`) and set `hub-body: manual` on
  any body worth freezing — **expected outcome: none** (they are Fabric
  output the deterministic render supersedes; panel round 5, M1). Record
  the frozen-vs-released split in the phase notes, and **assert nothing
  carrying `hub-body: manual` contains the refusal marker** (one mis-marked
  refusal would be skipped forever and make the refusal-count acceptance
  criterion silently unreachable). Nothing below runs until the snapshots
  exist: they are what makes every later overwrite recoverable.
- `sb cortex hub` (dry run) -> record what it would stub. **Disclosure: the
  "dry run" is not fully read-only today** — `populate_entities` runs
  whenever the oracle index opens, regardless of `--apply`
  (`cortex/src/hub.rs:379-383`; `upsert_entity` is
  `INSERT ... ON CONFLICT DO UPDATE`), so a flagless run writes oracle's
  `entities` table. The vault is untouched, and the Phase 0 snapshot covers
  the vault. Phase 2 gates it behind `--apply`.
- `sb cortex hub --apply` -> mint the missing repo hubs.
- **`sb oracle index`** -> ingest the new hub files. NOT optional:
  `insert_edges` requires BOTH endpoints present in `notes`
  (`vault/src/search/graph.rs:306-316`) and silently skips otherwise, and
  cortex never writes oracle's `notes` table.
- **No `sb cortex graph` step: it is a measured no-op here** (panel round
  2). The incremental pass picks targets by SOURCE-note staleness
  (`content_edge_targets`, `vault/src/search/graph.rs:410-420`:
  `modified_at > content_built_at`); indexing new DESTINATION hub notes
  makes no source note stale. Measured live: 0 stale targets against 3076
  watermarked `edge_build_state` rows, so a plain run processes zero notes
  and reports success. The `repo-member` edges therefore land in Phase 1's
  mandatory `graph --backfill`, where their success criterion now lives.
  (The rejected alternatives: running `--backfill` here drags Phase 1's
  O(N²) + fact-wipe into the zero-code phase; hand-invalidating
  `edge_build_state` rows is manual surgery on oracle's DB for a one-run
  shortcut.)
- Do NOT run `--synthesize`: it is known broken until Phase 2.

**Success criteria:** `entities/` committed to vault git AND the oracle DB
copied before any hub command runs; the identified `manual` set contains no
refusal-marker body; `find entities/repos -name '*.md' | wc -l` >= 30 after
`--apply`; `select count(*) from notes where path like 'entities/repos/%'`
>= 30 after the reindex (the hubs are visible to the index, ready for
Phase 1's edges).

#### Phase 1: membership primitives for creator/source, and stop the false one
**Model:** opus

Deterministic, no LLM decisions. Runs before the body-builder because
removing the prose matcher without the new edges strips creator hubs to zero
deliberate members (Every: 569 -> 0).

- **`creator-member` edge** in `build_edges_for` beside the repo-member block
  (`cortex/src/graph.rs:353-368` is the template), dst
  `format!("{}/{}.md", HUB_DIR, slugify(&row.creator))` — byte-identical to
  `HubStub::hub_path()` because the Creator stub's slug IS `slugify(creator)`
  (`hub.rs:224-226`).
- **`source-member` edge via ONE shared function.**
  `hub::source_hub_path(&str) -> Option<String>`, used by BOTH the stub side
  and the edge side, mirroring `repo_hub_path`. It calls
  `vault/src/search.rs:436 extract_host` (made `pub`; it already has the
  exact semantics and tests) then slugifies. `None` on schemeless input =
  skip, emit no edge. This kills the `hub.rs:183` / `graph.rs:431`
  divergence instead of writing a fourth copy.
  - Skipping schemeless loses nothing: `clyde://` (261 session notes) is the
    only non-http scheme, and those hubs could never be minted
    (`collect_stubs` returns `None` for them), so a naive edge would be
    dropped forever by resolve-or-skip. Sessions get membership via
    `repo-member`. The remaining 21 schemeless values (`pais-migration`,
    `youtube-transcript`, ...) are provenance markers, not publishers.
- **Neither new edge uses `fanout_cap`.** `metadata_edges` emits NOTHING
  above the cap (100), which is right for quadratic note->note edges and
  catastrophic for linear note->hub: `www.youtube.com` has 1026 notes and is
  the only host over the cap; a copied cap would silently zero exactly the
  largest source hub. Follow `graph.rs:283` (the over-cap hub routing), not
  `metadata_edges`.
- **Wikilink stopword, consulted at EDGE-BUILD time** on the raw slug from
  `extract_wikilinks`, BEFORE `resolve_note_path`, case-insensitive
  (`[[Every]]` must not slip through; `resolve_wikilink`'s third fallback is
  a bare `LIKE '%target%'`, so checking after resolve is wrong). Config:
  `graph.wikilink-stopwords`, seeded with `every` and `brief`.
- **The new kinds are counted, not swallowed.** `tally`
  (`cortex/src/graph.rs:448-457`) has a `_ => {}` arm that already hides
  `repo-member` from the run report. Add explicit arms, new `GraphStats`
  fields (the struct at `graph.rs:55-64` carries none of the three today),
  AND the operator-facing log line that prints them — the struct alone
  surfaces nothing (panel round 4, C3).
- **Config plumbing, in full:** `graph.wikilink-stopwords` gets a
  kebab-case serde field with a default (empty list), an annotated entry in
  the `config/templates/` cortex example, and a config-load test. **This
  phase ADDS `deny_unknown_fields` to `GraphConfig` and `EntitiesConfig`** —
  both are `#[serde(default, rename_all = "kebab-case")]` today
  (`cortex/src/config.rs:52-54, 86-88`) with no unknown-key rejection, so a
  typo'd `wikilink-stopwords` key would silently no-op the stopword. Named
  behavior change: a stray key in an existing `cortex.yml` becomes a hard
  load failure on upgrade (see Rollout); fail-loud is the point.
- **`graph --backfill` is required and has known costs, stated in full.**
  The incremental path keys on `modified_at`, so a config change is
  invisible to it. Costs:
  - `full_rebuild` calls `clear_edges()` (`DELETE FROM edges`), wiping all
    359 `fact` edges with no durable triple store; the fact layer recovers
    gradually via the **pre-existing** `extract_fact_layer` Fabric calls
    (`fact_max_per_run` = 50 per pass) — the plan's only LLM spend.
  - The semantic layer rebuild is O(N²): `semantic_neighbors`
    (`vault/src/search/vector.rs:307-345`) brute-force scans every
    `note_embeddings` row per note — ~3.1k × ~3.1k ≈ 9.4M BLOB decodes +
    384-dim dot products in one run.
  - The `edges` table is empty/partial for the duration. Operationally: run
    on the daemon host with the cortex daemon stopped, and Phase 2's builder
    and Phase 3's `--asymmetry` (both read `edges`) run only after the
    backfill completes.
- Landed body wikilinks are NOT retracted (note bodies are immutable). They
  keep rendering in Obsidian and stop minting edges. Consequence:
  `recompute_inbound_link_counts` reads bodies, not edges, so
  `entities/every.md` will show `inbound_link_count` ~569 with zero wikilink
  edges. Expected, not a bug.

**Success criteria:** `select count(*) from edges where kind='repo-member'`
> 200 after the backfill (moved here from Phase 0, which cannot reach it);
zero `wikilink` edges into `entities/every.md` survive
a `graph --backfill` with `every` stoplisted, while a note whose `creator:`
is Every produces a `creator-member` edge into it; a `source: clyde://<uuid>`
note produces NO edge and no dangling target; a YouTube note produces a
`source-member` edge into `entities/youtube-com.md` and that hub's membership
is non-zero (the fanout-cap regression); the graph run report shows non-zero
counts for `repo-member`/`creator-member`/`source-member` (nothing swallowed
by the `_ => {}` arm); an unknown key under `graph:` fails config load; no
landed note body is modified (assert byte-identical bodies across the run).

#### Phase 2: deterministic hub bodies
**Model:** opus

- `hub_members_deliberate` with the SQL predicate (deliberate kinds only,
  `src NOT LIKE 'entities/%'`).
- `HubMember` loading: read each member, `parse_body_claims`, carry `type:`.
  A missing/unreadable member is skipped with a warn — **but the hub
  remembers that loads failed**: "nothing to say" and "could not find out"
  are different conditions and get different write outcomes below (panel
  round 4, M1).
- `render_hub_body`: partition by `type:` (sources / sessions / other, other
  excluded from both sections); emit the `## Summary` two-vector digest,
  then `## From sources` and `## From your sessions` with
  `- <claim> ([[member]])` bullets; omit empty sections and empty digest
  lines; deterministic member order: **`date:` descending, path-ascending
  tiebreak; a member without `date:` sorts last.** The sort key must be BOTH
  stable per member and relevance-correlated, and the panel's seats split
  exactly along that line (rounds 1 and 2b): claim-count-descending is a
  relevance proxy but mutates on every re-distill, so unrelated work
  rewrites a 400-member hub (round 1's measured defect); bare path is
  stable but arbitrary — a 20-capped 408-member hub would render whatever
  sorts alphabetically first (numeric-prefixed paths). `date:` is both: the
  schema convention preserves it across reingest, and recency is a real
  relevance signal **for sources** (140 distinct dates spanning ~2.6 years).
  Honesty about the other vector (panel round 3 addendum, measured):
  sessions currently carry only 4 distinct dates (batch-harvested,
  2026-07-20..2026-08-15, tie groups up to 53), so session order degenerates
  to the path tiebreak within a batch. Accepted: when every session is from
  the last month, no ordering over them carries much relevance signal, and
  the key still strictly dominates bare path (arbitrary on BOTH vectors) and
  claim-count (re-distill churn). The no-`date:`-sorts-last rule is a pure
  guard — 0 members lack `date:` today on either vector. Claims stay in
  note order.
  Apply the two readability caps with a deterministic
  `...and N more claim-bearing members` overflow line (the full membership
  stays visible in Obsidian's backlinks pane; the caps bound the body, and
  the top claim-bearing cohorts are large: `claude` 408, `every` 383,
  `agents` 306 under the simulated post-Phase-1 membership); return `None`
  when zero members carry claims. Two accepted costs of the chosen order
  (panel round 2b): capped mega-hubs get a recency bias (an old high-value
  member falls below the cap — the digest, backlinks, and `--asymmetry`
  carry the tail), and a newly ingested recent member rewrites the capped
  body once (a legitimate input change, unlike re-distill churn).
- Write discipline — the ownership rule, stated as a rule (panel round 2,
  M4+M5): **a hub body without `hub-body: manual` is builder-owned and will
  be rewritten or reset without warning, this run or any future one.** Four
  branches, because "nothing to say" and "could not find out" are different
  conditions (panel round 4, M1 — the previous three-branch collapse merged
  them, and a vault-root misconfig would then have mass-reset every
  builder-owned hub to a stub in one silent run, contradicting the carried
  fail-safe AC):
  1. `hub-body: manual` present -> never touched.
  2. ANY member-load (IO) error on this hub -> **Preserved**, counted
     failed in the run report. Errors never license a reset; this is the
     same posture as the existing loud fail-safe (`hub.rs:468-471`).
     (`parse_body_claims` is infallible — `-> Vec<Claim>`, a malformed body
     yields an empty vec indistinguishable from no-claims — so a parse
     REGRESSION cannot trip this branch; the run-level backstop below is
     the defense for that case, panel round 5, C1.)
  3. All members loaded, render `Some` -> write iff it differs from the
     current body (byte compare), frontmatter preserved verbatim.
  4. All members loaded, render `None` -> **reset to the stub sentence** iff
     the body differs from it. Covers the 134 refusal bodies (124 with zero
     claim-bearing deliberate members today — measured, panel round 1 M2)
     and the stale-render case (claim-bearing members legitimately gone by
     run N+1; membership churn is the norm, Phase 1 deletes the `every`
     wikilinks wholesale).
  **Run-level backstop for the failure mode branch 2 cannot see** (a
  claim-parse regression that "succeeds" everywhere with zero claims): the
  builder computes ALL outcomes before writing ANY; if the resets of
  previously-RENDERED bodies (first heading `## Summary`; refusal and stub
  bodies excluded — the first run's ~124 refusal resets are expected) exceed
  `entities.render.max-render-resets-per-run` (default 20), the run aborts
  loudly and writes nothing.
  The body is always f(current membership) or the stub, never a fossil.
  A genuinely hand-written body is protected by setting `hub-body: manual` —
  that IS the contract; vault git is the backstop. Operator path past a
  tripped backstop: the abort message lists the hubs that would reset; fix
  the regression it indicates, or raise `max-render-resets-per-run` in
  config and re-run when the resets are genuinely intended.
- **Every hub write goes through `vault::note::write_atomic`**
  (`vault/src/note.rs:112`; precedent `cortex/src/summarize.rs:295-299`),
  never the plain `std::fs::write` at `hub.rs:522` — this pass rewrites
  hundreds of files on a Syncthing'd vault, where a torn write propagates
  to every machine (panel round 5, M2). Partial-failure semantics: each
  file write is atomic, so a failure mid-pass leaves every already-written
  hub complete and valid; the run reports which hubs failed, and the next
  run resumes idempotently (byte-compare skips the done ones).
- **Gate `populate_entities` behind `--apply`.** Today it runs whenever the
  index opens (`hub.rs:379-383`), so a flagless `sb cortex hub` upserts
  oracle's `entities` table while presenting as a dry run. Truth in naming:
  a dry run writes nothing anywhere.
- The run report counts each branch (bodies written, unchanged, stubs kept,
  refusals reset) plus members skipped as missing/unreadable, so a
  systematic member-load breakage is visible instead of silent.
- **Config plumbing, in full:** all FOUR render keys —
  `entities.render.max-members-per-section` (default 20),
  `entities.render.max-claims-per-member` (default 3),
  `entities.render.summary-byte-budget` (default 1200), and
  `entities.render.max-render-resets-per-run` (default 20) — and
  `EntitiesConfig` is FLAT today (`cortex/src/config.rs:52`), so
  `entities.render.*` means a new nested `RenderConfig` substruct with
  serde defaults, kebab-case renames, `deny_unknown_fields`, annotated
  entries in the `config/templates/` cortex example, and the unknown-key
  config-load test. Not three loose fields.
- **Measured expectation:** **160** hubs get a body under today's edges
  (161 have a claim-bearing deliberate member, but `entities/usa-football.md`'s
  only one is an `image` note — neither source nor session, so it renders
  nothing); ~480-510 after Phase 1's edges land (rev 1's disk simulation).
  Every other hub keeps its stub. If the first full run's written-body count
  is wildly off that band, something is wrong — check it, don't shrug.
- Delete `FabricHubSynthesizer`, `HUB_SYNTH_PATTERN`, and the
  `HubSynthesizer` trait; keep `SynthOutcome` and the preservation
  semantics.
- **Regression guard (required).** Rev 1's root defect was doubles that
  ignore their members argument. The renderer is pure: test it directly on
  real claim fixtures, and add a test asserting a rendered body contains a
  member's claim TEXT and wikilink, not just its path.

**Success criteria:** a both-vector hub renders both sections, every bullet
wikilinked to its member; a source-only hub emits no `## From your sessions`
heading; on a MEGA-HUB fixture built from the real `claude.md` cohort sizes
(hundreds of source claims, tens of session claims), the embedded text
tokenized by the REAL BGE tokenizer contains at least one session claim AND
one source claim inside the 512-token window — a small-fixture string
assertion proves nothing about truncation, and asserting only one vector's
survival is exactly the half-test that let the starvation flip vectors
between rounds (panel rounds 2 and 3); a hub already carrying a stub keeps it byte-identical when
nothing renders, while a refusal body OR a previously rendered body whose
claim-bearing members are gone is reset to the stub (the stale-render case,
asserted explicitly); a flagless `sb cortex hub` writes neither the vault
nor oracle's `entities` table;
`grep -rl "don't have access to the actual content" entities/ | wc -l` -> 0
after the first run; a second run with unchanged inputs writes zero bytes
(vault byte-identical); a `hub-body: manual` hub is never rewritten while a
`hub-synthesized: 2026-07-02` hub without that key IS rewritten (the 15
flagship hubs get deterministic bodies — asserted, since round 4 briefly had
this backwards); every hub write goes through `vault::note::write_atomic`; a
run
in which every member is unreadable preserves every body byte-identical and
reports the failures (the mass-reset guard), and the run-level backstop
aborts before any write when previously-rendered resets exceed the
configured max; the retrieval-contract test runs in offline `otto ci` via a
committed `tokenizer.json` fixture (~711 KB, tokenization needs no weights —
the only real-tokenizer test today is opt-in `CANDLE_TESTS_REAL=1` and
downloads ~133 MB from hf-hub at runtime); a
rendered body parses to zero claims via `parse_body_claims`; a `shared-tag`
member, a `semantic` member, and an `entities/%`-src member are all absent
from builder membership while a `wikilink` member is present; the run report
carries the per-branch counts and the skipped-member count; no
`vault::fabric` call remains reachable from `cortex::hub`.

#### Phase 3: asymmetry report
**Model:** sonnet

- `sb cortex hub --asymmetry`: per hub, inbound counts split session vs
  source over deliberate edges only (the Phase 2 filter), classified
  `both | learned-not-applied | applied-not-read | unlinked`.
- CLI wiring, explicitly (the most-skipped step class): a new `asymmetry`
  flag on `HubArgs` (`sb/src/cli/cortex.rs:362`, currently `apply` +
  `synthesize` only) threaded through `HubOpts` (`cortex/src/opts.rs:202`,
  same two fields).
- READ-ONLY: writes nothing to the vault or the index, asserted by test.
- This is the payoff of the original question: "what have I read about X but
  never applied" — answered with zero LLM spend.

**Success criteria:** the four buckets sum to the hub count;
`entities/claude.md` reports `both`; two runs produce byte-identical output;
a run leaves every vault file and the index byte-identical.

## Acceptance Criteria

Executed against `main` at 4621907 on 2026-08-15; observations recorded.

- [ ] `hub --apply` + `oracle index` mints a repo hub for every valid
      `<org>/<repo>` on a session note (ceiling 32; the 33rd value is an
      absolute path `validate_repo_slug` rejects), and Phase 1's
      `graph --backfill` wires the edges (plain `sb cortex graph` is a
      measured no-op: 0 stale content targets against 3076 watermarked rows,
      so it cannot wire anything today).
      `Observed: entities/repos holds 1 hub; repo-member edges = 4. FAILS.`
- [ ] A flagless `sb cortex hub` is fully read-only.
      `Observed: populate_entities runs whenever the index opens, regardless
      of --apply (hub.rs:379-383, upsert on conflict). FAILS.`
- [ ] An unknown key under `graph:` or `entities:` fails config load.
      `Observed: GraphConfig and EntitiesConfig are #[serde(default,
      rename_all = "kebab-case")] with no deny_unknown_fields; stray keys
      are silently tolerated. FAILS.`
- [ ] Builder membership excludes `semantic`, `shared-tag`, `fact`, and any
      `entities/%` src, structurally in SQL.
      `Observed: hub_members takes all kinds; all 359 fact edges and 1685
      wikilink edges into hubs have an entities/% src. FAILS.`
- [ ] A note->hub membership primitive exists for creator and source hubs.
      `Observed: creator-member count = 0; shared-creator edges into
      entities/ = 0. FAILS.`
- [ ] No `source-member` edge targets a hub `collect_stubs` cannot mint
      (guards the 261 `clyde://` sessions). `N/A today; guards Phase 1.`
- [ ] `entities/youtube-com.md` has non-zero `source-member` membership (the
      fanout-cap regression guard). `N/A today; guards Phase 1.`
- [ ] A common English word mints no hub membership and that survives a
      backfill.
      `Observed: entities/every.md has 603 members (wikilink 569 + semantic
      55); 577 notes carry a literal [[every, so a backfill rebuilds them.
      FAILS.`
- [ ] A hub body contains member claim TEXT with a `[[member]]` link, and a
      both-vector hub carries `## From sources` and `## From your sessions`.
      `Observed: grep -rl '## From your sessions' entities/ -> 0. FAILS.`
- [ ] No hub body contains a model refusal string.
      `Observed: 134. FAILS.`
- [ ] A rendered hub body parses to ZERO claims (hub-to-hub wikilinks stay
      inert by construction AND by test). `N/A until Phase 2.`
- [ ] Re-running the builder with unchanged membership and claims writes zero
      bytes. `N/A until Phase 2.`
- [ ] A `hub-body: manual` hub is never rewritten (zero carriers today —
      pure guard), and the 15 `hub-synthesized: 2026-07-02` hubs are NOT
      exempt: they are Fabric output, not hand-written, and get
      deterministic bodies like every other builder-owned hub.
      `Observed: all 15 carry ONE SENTENCE SUMMARY boilerplate;
      2026-07-18-harvest-knowledge-goals.md:225-227 records agent
      authorship. N/A until Phase 2.`
- [ ] Hub writes are atomic (`vault::note::write_atomic`, never plain
      `fs::write`), and a mid-pass failure leaves every already-written hub
      complete. `Observed: hub.rs:522 is std::fs::write. FAILS.`
- [ ] Phase 0's notes record the frozen-vs-released decision for each of
      the 15 (expected: all released). `N/A until Phase 0.`
- [ ] A member-load failure preserves the hub body byte-identical (never a
      stub reset), and a run whose previously-rendered resets exceed the
      configured max aborts before writing anything. `N/A until Phase 2;
      guards the mass-reset hazard.`
- [ ] Zero Fabric calls reachable from `cortex::hub`.
      `Observed: FabricHubSynthesizer calls run_pattern per hub, unbounded
      (hub.rs:395-409 has no cap). FAILS.`
- [ ] A synthesized hub is retrievable by its content under the LIVE
      (vector-only) pipeline: `knowledge_search` for a claim appearing only
      in a hub's `## From your sessions` returns that hub. Passable only
      because the renderer's `## Summary` digest carries both vectors into
      `notes.summary` and thence the hub embedding (`vault/src/search.rs:159`,
      `vault/src/detail.rs:147` first-H2 fallback, `cortex/src/embed.rs`).
      `Cannot run yet; depends on Phase 2. This is the original ask, end to
      end.`
- [ ] On a mega-hub-sized fixture, the embedded text tokenized by the REAL
      BGE tokenizer contains at least one session claim AND one source claim
      inside the 512-token window (the encoder silently cuts there,
      `vault/src/embedding/candle.rs:79`; a one-vector assertion is the
      half-test that let starvation flip vectors between panel rounds).
      `N/A until Phase 2.`
- [ ] `--asymmetry` buckets sum to the hub count and the run is read-only.
      `N/A until Phase 3.`
- [ ] A forced body-build failure leaves the prior hub body byte-identical
      (carried from harvest Phase 12; must not regress).
      `Observed: SynthOutcome::Preserved path, covered by
      synthesize_hub_writes_body_preserves_frontmatter_and_is_failsafe.
      PASSES.`
- [ ] `otto ci` green. `Observed: "All CI checks passed!" PASSES.`

## Resolved Decisions

- **Deterministic assembly, not LLM prose** (this revision's core change; see
  Alternative 1 for the deferred LLM path). Both live readers are served by
  arranged claims; prose adds hallucination risk and an unbounded Fabric bill
  for no named reader benefit.
- **Extend `cortex hub`, no new verb.** Same flag, same file handling, same
  failure preservation; only the body producer changes.
- **Hub bodies are rewritable; content notes are not.** Carried verbatim from
  `2026-07-17-harvest-clyde-sessions.md`.
- **Creator/source hubs get `creator-member` / `source-member` edges**
  mirroring `repo-member`, the proven in-house pattern; a query-at-build-time
  would leave the graph itself wrong for every other consumer.
- **Creator and source hubs share the flat concept namespace, deliberately.**
  `collect_stubs` inserts concepts first with `or_insert`, so
  `langchain`-the-creator merges into `langchain`-the-concept: one hub per
  subject. Verified real (`langchain`, `mem0`, `neo4j`, `vercel`). No
  namespace redesign.
- **Sources and sessions stay separate sections, never merged prose.** The
  provenance split IS the product.
- **Stopword hubs are suppressed, not deleted.** Deleting a hub with 500
  inbound links breaks the links; membership is fixed instead
  (`entities/every.md` stays, its false members go).

## Alternatives Considered

### Alternative 1: LLM prose synthesis with a `## Tension` section (rev 1's design)

Deferred, not rejected outright. Rev 1 proposed feeding member claims to a
new `synthesize-hub` Fabric pattern emitting `## From sources` /
`## From your sessions` / `## Tension`. What it required: a per-run Fabric
attempt cap, a `hub_build_state` hash table to make re-runs affordable, token
truncation with deterministic member selection, a claim-bearing skip gate
(sized: ~510 eligible hubs post-Phase-0 at threshold 1), pattern registration
in bootstrap, and a "must cite two claims" instruction plus two fixtures as
the only gate on `## Tension` inventing disagreement across ~510
nondeterministic calls. All of that machinery exists to manage the LLM; none
of it serves a reader. The genuinely novel capability — cross-vector
contradiction surfacing — is real but unbounded-risk as specified.

**Revisit condition:** after living with deterministic bodies, if
contradiction surfacing is still wanted, design it separately with the
rendered body as its input and a real verification gate. The deterministic
body is a strict prerequisite either way (it IS the claims-gathering half).

### Alternative 2: pass full note bodies instead of claims

Rejected on cost and signal. Large hubs blow any input budget, and the claims
ARE the distilled assertions — that is what the L2 contract is for.

### Alternative 3: direct session <-> article wikilinks

Rejected. Hub-and-spoke is already wired; the hub is merely empty. Direct
edges are O(n^2) and do not survive a rename.

### Alternative 4: raise `min-word-length` instead of a stopword list

Rejected. The same blunt gate over-links `every` (5 chars) and under-links
`rust` (4 chars); raising it drops legitimate short concepts while admitting
any 6-char common word. The stopword list names actual offenders.

### Alternative 5: delete the noise hubs

Rejected. `entities/every.md` is a legitimate creator hub for every.to with
603 inbound edges; deleting it breaks real links to fix fake membership.

### Alternative 6: fix the 51% session link rate first

Parked. Linking sessions harder into empty hubs adds edges to nothing.
Revisit after Phase 2 makes hubs worth reaching.

## Technical Considerations

### Dependencies

None added. `vault::search::parse_body_claims` exists and round-trips
kind/who/quote/anchor (the vector indexer already uses it). No Fabric
dependency remains in the hub path.

### Performance

The builder is IO-bound: per hub, one SQL query + N file reads + a render +
a byte compare. Re-runs with unchanged inputs write nothing. No cap needed,
no daemon concern (`hub` has zero references in `cortex/src/daemon.rs`; the
sole caller is `sb/src/cli/cortex.rs:626`). Phase 1's `graph --backfill` is
the one expensive step, and its fact-layer cost is stated in-phase.

### Security

None. No new file, network, or credential surface; the LLM surface shrinks
to zero for hubs.

### Testing Strategy

The renderer is a pure function tested directly — no injection seam, which is
the structural fix for how rev 1's bug shipped (doubles that ignored their
input). The retrieval contract is asserted under the REAL BGE tokenizer via
a committed `tokenizer.json` fixture (~711 KB; tokenization needs no model
weights), so it runs inside offline `otto ci` — the repo's only
real-tokenizer test today is opt-in (`CANDLE_TESTS_REAL=1`, downloads
~133 MB from hf-hub at runtime) and cannot gate anything. Fixtures from the real cohort: `entities/claude.md` (both vectors)
for partition and caps, `entities/terraform.md` (8 session / 4 external) for
a small both-vector case, `entities/getvoibe-com.md` as the refusal-reset
regression. File-handling preservation tests carry over unchanged.

### Rollout Plan

Phases 0-3 are ordinary commits plus a version bump; Phase 0 and the first
full builder run are operator-run on the daemon host. No systemd change, no
schema change, no bootstrap change. The vault is Syncthing'd, so hub bodies
propagate as data. One named upgrade edge: Phase 1 adds
`deny_unknown_fields` to `GraphConfig`/`EntitiesConfig`, so a stray key in
an existing `cortex.yml` turns from silently tolerated into a hard load
failure — the error names the key; fixing it is the point.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| A hub write clobbers a body someone wanted kept | Med | Med | The 15 `hub-synthesized:` bodies are Fabric output and overwriting them is the point (panel round 5, M1); genuine keeps are an explicit Phase 0 `hub-body: manual` decision; vault git + the oracle-DB copy make every choice recoverable |
| A member-load or claim-parse failure cascades into a mass stub reset | Low | High | Load errors are branch 2 (Preserved, counted failed), never a reset; the run-level backstop computes all outcomes before writing and aborts if previously-rendered resets exceed `max-render-resets-per-run` (default 20) |
| Stopping the prose matcher strips creator hubs of ALL membership | High | High | `creator-member` lands in the SAME phase as the stopword, never after |
| `graph --backfill` reinstates the false edges | High | High | Stopword consulted at edge-build time in `graph.rs`, not only in `linking.rs`; 577 bodies carry `[[every` and are never retracted |
| Backfill wipes the 359 `fact` edges and runs O(N²) | Certain | Med | Known, stated cost; fact recovery via the pre-existing Fabric extraction at `fact_max_per_run`; the semantic rebuild is ~9.4M embedding comparisons; run with the cortex daemon stopped, and Phase 2/3 reads of `edges` wait for completion. No durable triple store exists today and building one is out of scope |
| Claims are thin for undistilled members (youtube ~692/1055 undistilled) | High | Low | Undistilled members contribute no claims and are excluded, not faked; the hub says less rather than lying |
| Rendered mega-hubs are unreadable | Med | Low | Deterministic per-section member cap + per-member claim cap with an overflow line; full membership remains in Obsidian backlinks |

## Open Questions

None. Rev 1's premise question ("is synthesis broken, or just unrun?") was
closed by measurement: 134 refusal bodies prove `--synthesize` ran broadly
with content-free input. This revision's mechanism question (LLM vs
deterministic) is closed by the reader analysis: both live readers consume
arranged claims; neither needs prose.

## References

- `docs/design/2026-06-05-graph-augmented-memory.md`: hub layer, Phase 3.
- `docs/design/2026-07-17-harvest-clyde-sessions.md`: repo hubs (Phase 10),
  `--synthesize` (Phase 12), the stability disciplines.
- `docs/design/2026-07-20-harvest-completion.md`: the
  `hub --apply` -> `graph` -> `hub --synthesize` chain, Phase 2.
- `docs/design/2026-08-15-harvest-note-identity-trace-keyed-replace.md`: the
  ready-to-build gate the acceptance criteria follow.
