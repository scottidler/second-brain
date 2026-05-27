# Design Document: facet v2 — dialog-slice gems and narrative spectra

**Author:** Scott Idler (drafted by Claude)
**Date:** 2026-05-26
**Status:** In Review (Architect Round 2 consensus incorporated)
**Review Passes Completed:** 5/5 Rule-of-Five + 2 Architect rounds

## Summary

facet v1 captures **paraphrased one-line moments** of Scott's judgment and rolls them up into six **static** per-mode spectra (frame, iterate, reject, push-for, sequence, name-the-failure). That is the wrong unit and the wrong end-state. facet v2 reframes the corpus around **multi-turn dialog-slice gems** carrying the four-part anatomy from the Shopify-CEO talk — task, context, interaction, review — with the AI's verbatim output preserved alongside Scott's, and replaces the static mode rollups with **emergent narrative spectra**: story-shaped, presentation-grade artifacts whose existence is discovered from the fossil record rather than fixed up-front. A separate **dreaming** layer (per Anthropic's agent-memory talk) provides non-destructive enrichment — semantic dedup, cross-referencing, stale-spectrum detection — that writes to a clone directory and never mutates the canonical. A prototype of the new extract pattern was built and validated on a real session during Pass 1 of this doc (see "Prototype Findings" below); the schema and pipeline phases here reflect what that prototype proved out.

## Problem Statement

### Background

The current facet pipeline (`facet/src/`) operates as: scan JSONL → cluster turns into work-items (now prisms) → extract one-line `judgment_moments` per user turn → roll up moments per mode into one of six static spectrum files (now `notes/facet/spectra/{frame,iterate,reject,push-for,sequence,name-the-failure}.md`).

The output looks structured but is shallow:
- Prism notes are bins of one-liner moments organized by mode tag
- Spectrum notes are five-paragraph essays on "how Scott does mode X"
- The AI's actual outputs are paraphrased and lost (`ai_move: "AI proposed something"`)
- The four parts of the work — task framing, context loading, interaction, review — are not first-class anywhere

Two recently-ingested source notes reshape what facet should be doing:

1. **`shopify-ceo-reveals-their-secret-ai-developer.md`** (Nate B Jones). The valuable thing in AI-assisted work is the **senior dev's process**, not their final output. He names **four parts** of the work to make visible: task, context, interaction, review. "The good prompt disappears into one person's chat history. The clever correction stays inside one employee's browser tab. You learned the craft from the process as much as from the finished product."

2. **`agents-that-remember.md`** (Kevin / Anthropic). Three composable layers: **sessions** (ephemeral), **memory stores** (persistent), **dreaming** (asynchronous, **non-destructive** enrichment that clones-and-improves rather than overwrites). Dreaming organizes, fact-checks, removes duplicates, adds metadata.

The current facet maps to these as:
- sessions = JSONL transcripts (no change)
- memory store = the vault notes (already there, but the wrong unit of capture)
- dreaming = missing as a distinct layer (today's harvest IS the dreaming, destructively)

### Problem

The current capture-unit (a paraphrased one-line moment) destroys the apprenticeship value of the corpus. A reader cannot reconstruct *how the senior dev worked the model* from a row that says `ai_move: "AI proposed X"; scott_move: "rejected and renamed"; quote_excerpt: "no - call it foo"`. The recipe is gone; only the ruling remains.

The current spectrum-unit (one of six static mode files appended to over time) cannot tell a story. A spectrum can only describe "how Scott does *mode X*" because that's what it's bucketed by. There is no spectrum kind that says "the seven times Scott rejected the LLM's plausible-but-wrong migration suggestion across three sessions, and what the failure-mode pattern teaches" — because that's a *narrative* across modes, sessions, and time, and the current bucketing forbids it.

The two failures compound: the unit of capture is too small to inform good narratives, and the unit of output is too constrained to be a good narrative.

### Goals

1. Capture **multi-turn dialog slices** with the AI's verbatim output preserved alongside the user's, so the apprenticeship recipe is intact.
2. Surface the **four parts** (task, context, interaction, review) as first-class structure in every gem.
3. Generate **emergent narrative spectra** — story-titled, multi-gem syntheses that flow with the corpus. Spectra are NOT capped at six per-mode rollups; they're discovered.
4. Separate **canonical capture** (prisms) from **derived synthesis** (spectra and dreaming). Dreaming writes to a clone directory, never mutates the canonical.
5. Produce spectra rich enough to be **presentation-grade**: shareable to a teammate, usable as a talk outline, teachable as a worked example.
6. Preserve the fossil-record property: JSONL transcripts are immutable, so the corpus can always be re-clustered, re-extracted, re-dreamed without data loss.

### Non-Goals

- Replace the existing schema in place. The migration is a fresh ledger built next to the old one until cutover.
- Real-time UI. Output is markdown notes; navigation is Obsidian.
- Cross-user generalization. This is Scott's corpus. Future multi-user work is out of scope.
- LLM-cost optimization beyond what the existing tiering provides. Quality first.
- Time estimates for any phase (per `~/.claude/CLAUDE.md`).

## Proposed Solution

### Overview

A two-pass pipeline plus a non-destructive dreaming layer.

```
JSONL (fossil record, read-only)
        │
        ▼
[ scan + cluster ]        (existing, mostly unchanged)
        │
        ▼
[ extract v2 ]            (NEW: produces gems, not moments)
        │
        ▼
prisms/<slug>.md          (canonical store; one note per concept; gem-sequence body)
        │
        ▼
[ spectra discovery ]     (NEW: clusters gems, narrates each cluster)
        │
        ▼
spectra/<narrative-slug>.md   (emergent story-shaped notes; many, not six)
        │
        ▼
[ dreaming ]              (NEW: non-destructive enrichment, cross-references, dedup)
        │
        ▼
dreams/<slug>.md          (cloned + improved spectra/prisms; rebuildable from canonical)
```

### Architecture

Three planes:

**Capture plane (prisms)** — one prism per work-concept. Body is an ordered list of **gems**. Each gem is a four-part record (task, context, interaction, review). The interaction is multi-turn back-and-forth, both sides verbatim. The prism is the **fossil cast** — durable, append-only across re-extracts, idempotent on the (workitem_id, turn_range) key.

**Synthesis plane (spectra)** — one spectrum per **discovered narrative**. A narrative is a cluster of gems that hang together: a workflow archetype, a recurring failure mode plus its correction, a temporal arc, a cross-session pattern. Each spectrum has a story-shaped title, a 5-7 paragraph body that tells the story, citations back to source gems, and metadata describing what hold it together (semantic cluster, time-window, mode-mix, repo). Spectra are NOT mode-bucketed; mode is one of several axes the discovery pass can use.

**Dreaming plane (dreams)** — async, scheduled (not per-tick). Reads prisms + spectra. Identifies semantic duplicates across gems (same concept across sessions). Identifies stale spectra (clusters that have grown since the spectrum was last written). Identifies cross-references (gem A is the precursor to gem B). Writes consolidated, cross-referenced versions to `notes/facet/dreams/`. The dreams dir is rebuildable; the canonical prisms are not affected.

### Prototype Findings (Phase 1 — executed before schema commit)

A working prototype ran on a real slice (~200 turns spanning the
portrait→spectra rename exchange from today's session, lines 1100-1300
of `d81fb682-…jsonl`). Pattern: `facet/patterns/facet-extract-v2.md`.
Model: `claude-sonnet-4-6` via `fabric -V Anthropic`.

Outcome on first try:

- **JSON parse:** clean. Zero malformed-output retries needed. (The
  Phase 1 risk about JSON parse failure rate exceeding v1's 39% YAML
  rate did not materialise; v2's stricter schema gives the model
  less room to flub.)
- **Gems extracted:** 3 from the slice. All three had ≥2 interaction
  turns, populated `task`, populated `context_loaded` with multiple
  specific items, accurate per-turn tags from the closed list, and
  populated `review.accepted` / `review.verified_manually` fields
  with session-grounded content (not fabrication).
- **Four-part anatomy:** all four parts present and distinguishable.
  The model correctly distinguished "context_loaded" (what Scott
  pasted) from "context_missing" (what the AI didn't know).
- **Tag accuracy:** `name-the-failure` correctly applied to the turn
  where the assistant said "you're right, I missed the whole layer";
  `verify` correctly applied to sqlite3-output-inspection turns.
  No false positives in the 9 turn-level tag applications I
  hand-checked.
- **Cost:** ~9.5 KB output JSON for 3 gems (3.2 KB/gem average). v1's
  moment row averages ~250 bytes. So v2 is ~12× heavier per row, but
  rows are coarser; net session-cost increase ~2-4× depending on
  density. Inside the budget assumed in "Performance" below.
- **Latency:** ~60 s for one fabric call. Comparable to v1 extract.

Saved as reference fixtures:
- `facet/tests/fixtures/v2/slice-rename-input.yaml` (the input slice)
- `facet/tests/fixtures/v2/gems-prototype-rename-slice.json` (LLM output)

Known issues surfaced by the prototype, addressed in this revision:

1. **Tool-result blobs in `user_says`.** When the user's turn is a
   tool_result rather than a natural-language reply, the verbatim is
   a JSON blob preview rather than dialog. Initial framing was that
   this was a downstream render concern, but Architect Round 2
   flagged the real risk: a real session's tool-result (a `git diff`,
   a `sqlite3` dump) can be multi-KB. Asking the extractor to echo
   it verbatim inside the JSON output pushes a single fabric call
   past Sonnet's 8k output cap and produces truncated/broken JSON.
   Schema gains a capture-side truncation rule: tool-result turns
   over 800 chars are replaced in `user_says` with a placeholder
   like `<tool-result: 1247 lines, $tool_name>`; the raw text is
   not echoed. The renderer can resolve the placeholder back to a
   "Scott ran X and saw Y" line at presentation time. This is now
   a schema rule, not a downstream concern.
2. **`null` vs string `'None'`.** One review field came back as the
   string `'None'` instead of JSON null. Cheap to normalise at parse
   time; will add to the deserializer.
3. **The 1500-char per-turn cap** clipped some dense AI explanatory
   passages mid-thought. Considering raising to 2500 for AI turns and
   keeping 1500 for user turns; AI verbosity is the gem's value here.

The prototype validates the schema is achievable; the gem-per-row
schema below is unchanged from the draft.

### Data Model

#### Gem (replaces JudgmentMoment)

```rust
pub struct Gem {
    pub id: i64,
    pub workitem_id: i64,                  // FK -> work_items.id
    pub session_uuid: String,
    pub task: String,                      // Scott's stated/inferred goal
    pub context_loaded: Vec<String>,       // pasted/cited material he brought in
    pub context_missing: Vec<String>,      // what the AI plainly didn't know
    pub interaction: Vec<InteractionTurn>, // multi-turn back-and-forth
    pub review: Review,
    pub tags: Vec<String>,                 // scaffold modes + LLM-invented
    pub why_it_matters: String,            // one-line teaser for spectra synthesis
    pub extractor_model: String,
    pub extracted_at: DateTime<Utc>,
}

pub struct InteractionTurn {
    pub ai_says: String,                   // VERBATIM AI output (may be truncated with …)
    pub ai_turn_uuid: String,
    pub user_says: String,                 // VERBATIM user turn; tool-result turns > 800 chars replaced with `<tool-result: N lines, $tool>` placeholder (see "Known issues" #1)
    pub user_turn_uuid: String,
    pub tags: Vec<String>,                 // per-turn mode tags
}

pub struct Review {
    pub accepted: Option<String>,
    pub rejected: Option<String>,
    pub verified_manually: Option<String>, // command + output, if applicable
    pub rewrote_by_hand: Option<String>,
}
```

Idempotence key: `UNIQUE (workitem_id, content_hash)` where `content_hash = sha256(sorted turn UUIDs in span)`. A gem covers a turn-range; re-extracting the same logical span over the same workitem produces the same content_hash even if the chunker boundaries shift by one or two turns, so re-extracts upsert rather than insert duplicates. The boundary UUIDs (`first_user_turn_uuid`, `last_user_turn_uuid`) are still stored as columns for inspection and renderer cross-references, but they do not participate in the unique key. `facet::dedupe` provides a content-similarity backstop for cases where boundary heuristics shift more aggressively (e.g., a re-extract that grew a span by one turn so the UUID set changed). Architect Round 2 flagged the original boundary-UUID key as brittle; this is the consensus replacement.

#### Narrative (the discovery-side of a spectrum)

```rust
pub struct Narrative {
    pub id: i64,
    pub slug: String,                      // kebab-case story title
    pub title: String,                     // sentence-case
    pub thesis: String,                    // one-line claim the spectrum makes
    pub body_md: String,                   // the rendered 5-7 paragraph body
    pub gem_ids: Vec<i64>,                 // citations
    pub axes: NarrativeAxes,               // what holds the cluster together
    pub synthesised_at: DateTime<Utc>,
    pub synthesiser_model: String,
    pub revision: u32,                     // bumps when re-synthesised
}

pub struct NarrativeAxes {
    pub semantic_cluster_id: Option<i64>,  // embedding-cluster handle (post-dream)
    pub mode_mix: Vec<(String, u32)>,      // mode tag counts in the cluster
    pub time_window: Option<(DateTime<Utc>, DateTime<Utc>)>,
    pub repos: Vec<String>,
    pub workitem_ids: Vec<i64>,
}
```

#### Dream

A dream is a derived, regenerable artifact pointing at one or more narratives or gems with an improvement. Dreams have no SQLite table; they are computed in-memory per dream pass and rendered directly to markdown. Stored separately from canonical:

```rust
pub enum Dream {
    SemanticDuplicateGroup { gem_ids: Vec<i64>, canonical: i64 },
    CrossReference { from_gem: i64, to_gem: i64, relation: String },
    StaleSpectrum { narrative_id: i64, new_gem_ids_since: Vec<i64> },
    NarrativeCandidate { gem_ids: Vec<i64>, proposed_title: String, proposed_thesis: String },
}
```

Dreams render as markdown notes under `notes/facet/dreams/` for human review and as proposed mutations to the canonical (apply on confirmation).

### API Design

CLI surface on `sb facet`:

```
sb facet harvest                        # scan -> cluster -> extract v2 -> prism render
sb facet narrate [--limit N]            # spectra discovery pass: cluster gems, write narratives
sb facet dream                          # dreaming pass: dedup, cross-ref, stale-detect
sb facet present <narrative-slug>       # render a narrative as a presentation outline
sb facet retry <session-uuid|slug>      # existing
sb facet dedupe                         # existing (slug-suffix), absorbed into `dream` long-term
```

The `present` subcommand reformats a narrative into a slide-deck or talk outline (one slide per gem citation, speaker notes inline, suitable for paste into Slides/Keynote/markdown-slides).

### Implementation Plan

#### Phase 1: Prototype the extract v2 pattern on a real session — COMPLETE
**Model:** sonnet

Status: **shipped during doc Pass 2**. See "Prototype Findings"
section above. Artifacts in `facet/patterns/facet-extract-v2.md` and
`facet/tests/fixtures/v2/`. Exit criterion met on the first run; no
prompt iteration was required for this slice.

What the prototype taught the design (folded in above):
- The schema as drafted is achievable.
- Per-turn tags AND per-gem tags are both extractable; the model uses
  them distinctly without confusing the two.
- Tool-result turns and natural-language turns need different render
  treatment (capture is uniform; presentation differs).
- The 1500-char per-turn cap is fine for users; raise to 2500 for AI.

#### Phase 2: New ledger schema + bash migration
**Model:** sonnet

- Bash script `bin/migrate-facet-v2.sh` that creates the v2 tables (`gems`, `interaction_turns`, `narratives`, `narrative_axes`) alongside the existing v1 tables. Per `~/.claude/refs/dealing-with-large-files.md` and the user rule "NEVER write schema-migration / legacy-format-changeover code in Rust," this is bash + sqlite.
- **No `dreams` table.** Architect Round 2 flagged the missing schema. Resolution: dreams are derived, regenerable artifacts. Each dream pass queries `gems` and `narratives` in-memory, produces the `Dream` enum variants, and renders them directly to markdown under `notes/facet/dreams/`. Dreams have no persistent SQL state; if a dream pass crashes, the next pass produces the same findings from the same canonical inputs. This keeps the canonical/derived split sharp.
- v1 tables (`judgment_moments`, etc.) stay intact during cutover; no data migration of moments-to-gems is attempted. The fossil record (JSONL) re-extracts cleanly into v2.
- Add Rust models matching the new schema in `facet/src/gems.rs`, `facet/src/narrative.rs`, `facet/src/dream.rs`.

#### Phase 3: Gem extraction library
**Model:** opus

- Reshape `facet/src/extract.rs` into a dispatcher. The v1 extractor moves intact to `facet/src/extract/v1/mine.rs` (no behaviour change); the new v2 extractor lands at `facet/src/extract/v2/gems.rs` with return type `Vec<Gem>`. `extract.rs` selects between them via the existing `--v1` flag plumbed through `sb facet harvest`. This satisfies the Migration Plan's "v1 must coexist for one week of soak" requirement without forking the harvest entrypoint (Architect Round 2 flagged the original "replace mine.rs with gems/extract.rs" wording as contradictory).
- New chunking strategy. Gems can span more turns than the current moments-per-slice, so the per-call slice grows. The splitter requirements (Architect Round 2 promoted this from an implicit risk to a named Phase 3 sub-spec):
  - Split only on user-turn boundaries; never mid-AI-response.
  - Max-turns-per-chunk cap: 50 turns (configurable). Beyond this, the chunk is cut at the nearest user-turn boundary before turn 50.
  - Overlap window: when a chunk is split, the next chunk includes the last 4 turns of the prior chunk so an arc that crossed the boundary still has both halves visible to the extractor in one of the two windows.
  - The chunker is heuristic, not semantic; the LLM is responsible for recognising "this span doesn't actually contain a gem" and returning an empty array for that chunk. The prototype's manual slice approach in `bin/build-session-slice.py` is the v1 of this chunker, to be promoted to library code with the requirements above.
- Persist via `ledger::gems::upsert_gem` with idempotence on `(workitem_id, content_hash)` (see Data Model / Gem).
- The mode tag becomes per-turn AND per-gem (a gem can be "primarily reject" with internal frame and push-for moves).

#### Phase 4: Prism renderer v2
**Model:** sonnet

- `facet/src/render/prism.rs`. Body is a sequence of `## Gem N: <task headline>` sections, each with the four-part anatomy as fenced sub-sections.
- Top of note: index. "This prism contains 3 gems primarily about reject, 2 about frame, 1 about push-for." Each is a link to the gem section below.
- Frontmatter unchanged in shape (still `type: facet-prism`), gains `facet-gem-count`, `facet-tag-mix`.
- Operator content outside fenceposts is preserved (current fencepost merge logic carries over).

#### Phase 5: Spectra discovery (the new spectra layer)
**Model:** opus

Phase 5 implements **two distinct narrative archetypes** rather than a single clustering-then-narrate pipeline. Architect Round 2 established that thematic semantic clustering alone does not guarantee a narrative arc (the "Q4 hard question"); the two-archetype split decouples the discovery mechanism from the arc validation, and a strict LLM rejection gate does the validation work that cluster shape cannot.

**Archetype A: Session Arc** (no clustering required)

A Session Arc is a chronological run of gems inside one `session_uuid` that demonstrates a worked-through narrative on its own (the "two-hour death-match with one bug" shape). Architect Round 2 explicitly called this out as a case the proposed time-spanning constraint would have filtered out incorrectly. Discovery:
- Read all gems for a session, ordered by `extracted_at`.
- Submit to Opus if gem count >= 3 AND the session contains at least one `name-the-failure` or `reject` gem (signalling a real obstacle, which the apprenticeship-recipe framing depends on).
- No HDBSCAN, no embedding step. The session UUID is the cluster key.

**Archetype B: Cross-Session Arc** (HDBSCAN + chronological + rejection gate)

A Cross-Session Arc is a recurring theme across many sessions over time (the "seven times I rejected a plausible-but-wrong migration suggestion" shape). Discovery:
- Embed each gem's `task + why_it_matters + interaction.user_says[0]` (fastembed via `vault::embedding`).
- HDBSCAN cluster with epsilon tuned to produce **tight** clusters (specific incidents, not broad topics). A cluster of 100+ gems is a tuning signal that epsilon is too loose, not a runtime case to cap. Architect Round 2 explicitly rejected an arbitrary `max_cluster_size` cap as story-mutilation: if a 30-gem cluster is legitimately tight, Opus handles ~24k input tokens fine; if it's 100 gems, the operator re-tunes epsilon rather than the code slicing the array.
- For clusters with size >= 3 (configurable), order gems by `extracted_at` and submit chronologically to Opus.

**Narrative synthesis (shared by both archetypes)**

The `facet-narrate.md` pattern instructs Opus with a **strict rejection gate** (this is the load-bearing piece per Architect Round 2; it does the arc-validation work cluster shape cannot do):

> "Review this chronological sequence of gems. If they demonstrate a causal chain, an evolving mental model, or a recurring-and-resolved struggle, narrate it as a story with a thesis, setup, complication, and resolution. If they are merely disconnected events about the same topic (a changelog), return `title: ""` and `thesis: ""`. Do not invent narrative coherence that is not present in the gem sequence."

The empty-title path is the v1 portrait-skip semantic, retained verbatim. The `chronologically_ordered: true` flag is set on every narrative the synthesiser accepts (Architect Round 2 partial-accept on Resolution 2: "how thinking on X evolved over time" is the arc-by-construction frame).

Output: rendered narrative at `notes/facet/spectra/<narrative-slug>.md`. Frontmatter additions:
- `facet-spectrum-archetype: session | cross-session | evergreen`
- `facet-spectrum-status: active` (operator can edit to `rejected`; see below)
- `facet-spectrum-cluster-key: <session_uuid | hdbscan_cluster_id>`
- `facet-spectrum-gem-ids: [list]`

**Operator rejection (lifted from Open Questions to a named decision per Architect Round 2)**

The operator marks a discovered narrative as wrong by editing `facet-spectrum-status: active` to `rejected` in the spectrum's frontmatter. On the next narrate pass, the discovery step reads all existing spectra and their statuses. When a new candidate cluster has >= 80% gem-id overlap with a `rejected` spectrum, the candidate is suppressed (no Opus call, no new note). Suppressed clusters are logged at INFO for visibility but not regenerated. This makes rejection a one-edit operation in Obsidian rather than a separate CLI verb.

**Evergreen mode spectra (back-compat)**

The six mode-based spectra continue to exist as a Phase 5 special case: a synthetic "cluster" whose membership is "all gems with a primary tag of mode X." These render to `notes/facet/spectra/mode-<name>.md` with `facet-spectrum-archetype: evergreen`. Their existence is back-compat scaffolding; the design lean is toward dropping evergreens once Cross-Session Arc proves out (Open Question retained).

**Command:** `sb facet narrate` triggers both archetypes by default; `--archetype session|cross-session|evergreen` runs one in isolation for debugging.

#### Phase 6: Dreaming layer
**Model:** opus

- `facet/src/dream.rs`. Scheduled separately from harvest. Reads canonical, writes derived.
- Detects:
  - Semantic duplicates (two gems, same concept, different sessions) — propose merge or cross-reference.
  - Stale spectra (cluster has grown since spectrum was last written; needs revision).
  - Cross-references (gem A's review references the same constraint as gem B's task).
  - Narrative candidates (clusters that have reached threshold size but haven't been narrated yet).
- Output: one file per dream-finding under `notes/facet/dreams/`. NEVER modifies prisms or spectra directly.
- A separate "apply dream" subcommand (later) lets the operator confirm and apply.
- `sb facet dream` is the command.

#### Phase 7: Presentation rendering + CLI polish + tests
**Model:** sonnet

- `sb facet present <narrative-slug>` reformats a narrative into a slide-style outline: title slide, one slide per gem (task → first AI answer → push-back → resolution), speaker notes inline, suitable for paste.
- End-to-end test fixture: a small synthetic JSONL covering one session, expected gems and one narrative.
- Update `clauderize` / `CLAUDE.md` in second-brain to point at the new layout.
- Migrate the systemd unit to call `sb facet narrate` on a separate cadence from harvest.

### Migration Plan (Cutover)

1. Land v2 schema next to v1 (Phase 2). Both tables exist.
2. New harvest writes to v2; old `judgment_moments` is frozen. `sb facet harvest` can be invoked with `--v1` to fall back during diagnostics, deprecated after one week of soak.
3. Existing prism notes on disk stay as-is until the first v2 render touches them. New v2 renders write to the same path with new body shape (replacing the v1 fencepost content).
4. Old `spectra/{frame,iterate,reject,push-for,sequence,name-the-failure}.md` files stay on disk as historical artifacts. New narrative spectra start appearing alongside them with story-titled slugs. The six legacy files become evergreen spectra produced by Phase 5's discovery pass with "mode-X" axis.
5. `bin/cleanup-v1-tables.sh` drops the old tables once user confirms v2 is healthy.

## Alternatives Considered

### Alternative 1: Extend the existing schema in place

- **Description:** Add `interaction` and `four_parts` JSON columns to `judgment_moments`. Keep the same table.
- **Pros:** No migration script. Backwards compatible.
- **Cons:** The unit of the row is still a moment, not a gem. Schema would be misleading (table named `judgment_moments` containing dialog slices). The mode-bucketed spectra layer can't be unbucketed without a deeper rewrite.
- **Why not chosen:** the unit-of-capture is the actual problem. A field-bolt-on hides the redesign rather than expressing it.

### Alternative 2: Keep one-line moments AND add a separate gems table

- **Description:** Coexist. Moments for the existing mode-spectra; gems for the new narrative spectra.
- **Pros:** Doesn't break the old pipeline. Allows side-by-side evaluation.
- **Cons:** Two extractors running against the same fossil record means double the LLM cost and two sources of truth diverging over time. The operator (Scott) ends up choosing between the two anyway; supporting both indefinitely creates a maintenance burden with no upside once v2 is proven.
- **Why not chosen:** the cutover is cheap (re-extract from JSONL is the fossil-record property in action). The optionality is anti-value.

### Alternative 3: Static narrative templates (e.g., "Hero's Journey of one workitem")

- **Description:** Skip semantic discovery. Pick 3-5 fixed narrative templates and emit one spectrum per workitem per template.
- **Pros:** Deterministic. No clustering overhead.
- **Cons:** Templates are still buckets, just different ones. The user explicitly said spectra should be **organic and flow with what's discovered in the fossil record** — templates contradict that.
- **Why not chosen:** It's the same mistake as the mode-buckets, in a fancier wrapper.

### Alternative 4: Defer dreaming entirely; just do gems + emergent spectra

- **Description:** Skip Phase 6.
- **Pros:** Smaller surface. The dreaming work is genuinely speculative.
- **Cons:** Without dreaming there's no semantic dedup, no stale-spectrum detection, no cross-references. The corpus grows monotonically and quality degrades over time as duplicates accumulate. We already saw this with the slug-suffix duplicates today.
- **Why not chosen:** the corpus needs the maintenance layer to stay teachable as it grows. But Phase 6 is sequenceable — gem+spectra is a viable interim state.

## Technical Considerations

### Dependencies

- `fastembed` (already used by oracle/cortex): embeddings for semantic clustering.
- `hdbscan` or `linfa-clustering`: clustering crate. Likely `linfa-clustering` since it's pure Rust.
- `serde_json` (already in use): JSON parsing for the new extract pattern.
- `chrono`, `rusqlite`, `tokio`, `clap`, `eyre`, `serde_yaml`: unchanged.
- No new external services. All LLM work continues through fabric.

### Performance

The gem extractor consumes more output tokens per slice than the moment extractor (verbatim AI turns are heavier than paraphrases). Approximate scale, recomputed after Architect Round 2 caught the original arithmetic error (the original draft equated 3.2 KB to "~1500-2500 output tokens," which is wrong by ~3x):

- Average moment row: ~60 output tokens (~250 bytes).
- Average gem row: ~800 output tokens (~3.2 KB, computed as 1 token ~= 4 chars).

This is a ~13x increase in output cost per row. Mitigation: gems are coarser-grained, so we produce fewer rows per session. Estimated net cost increase per session: 2-4x (acceptable given the quality jump).

Sonnet's 8k output cap is the real binding constraint, not raw cost. A fabric call returning 4 gems at ~800 tokens each is ~3200 tokens of gem content plus JSON overhead, well inside the cap. But if a single gem's `interaction[].user_says` field contains an un-truncated multi-KB tool-result, that one gem alone can exceed the cap and break the JSON mid-emit. The capture-side truncation rule in "Known Issues" #1 is what keeps the budget honest.

Spectra discovery is the heaviest pass: embed N gems (N up to 1000+ over time), cluster, then one opus call per discovered cluster. Per cluster, opus reads up to ~30 gems = ~24k input tokens; output ~2-3k tokens. Order ~5-20 clusters per discovery pass. Cluster sizes are tuned via HDBSCAN epsilon rather than capped post-hoc (see Phase 5).

Dreaming is comparable in shape but reads pre-embedded gems and runs on a slower cadence.

### Security

No new attack surface. The pipeline reads local JSONL files, writes local markdown files, and shells out to fabric (same as v1). Secrets in user turns are still scrubbed at the `<redacted>` boundary in the extract pattern (carried over).

### Testing Strategy

- **Phase 1 prototype:** manually run extract v2 against one real session. Verify the output has the four parts and verbatim turns. Document the pattern's failure modes.
- **Unit tests** for each new lib module, in the existing `tests.rs` sibling-file convention.
- **Integration test:** synthetic small JSONL → harvest → prism + narrative. The existing `facet/tests/harvest_end_to_end.rs` becomes a v2 test; the v1 test stays as `harvest_v1_legacy.rs` during cutover.
- **Property-style tests on idempotence:** re-running harvest over the same JSONL produces the same gems (no duplicate inserts). Re-running narrate over the same gems produces the same narratives (revision counter only bumps if the gem set changed).
- **Real-corpus regression:** run the full new pipeline against the user's actual JSONL corpus and compare gem counts, narrative counts, and a hand-spot-check of 5 random gems and 3 random narratives. This is a soak test; failures here may bounce us back to Phase 3 prompt tuning.

### Rollout Plan

1. Phase 1 prototype: manual fabric invocation. No code changes shipped.
2. Phases 2-4: ship together. `sb facet harvest` is v2 from this point; `sb facet harvest --v1` accesses the old path during diagnostics.
3. Phase 5: ships solo. New `sb facet narrate` subcommand. Existing static spectra files stay on disk until the discovery pass produces evergreen replacements.
4. Phase 6: ships solo. `sb facet dream` is opt-in; not on a systemd cadence until the operator confirms it's stable.
5. Phase 7: cleanup. Drop v1 tables, remove `--v1` flag.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Extract v2 prompt produces malformed JSON at higher rates than v1 (richer schema, more verbatim text). | Medium | High | Phase 1 prototype ran clean (N=1, curated 200-turn slice). Architect Round 2 correctly noted that N=1 on a curated slice does not validate generalisation; risk stays at **Medium** until a multi-session soak run (N >= 5, varying content density including tool-heavy sessions) lands. The fixture at `facet/tests/fixtures/v2/gems-prototype-rename-slice.json` serves as a regression anchor, not a generalisation proof. If per-session failure rate exceeds 5% during cutover, escalate to direct Anthropic API tool-use (response_format JSON schema) bypassing fabric. |
| **Tool-result blowout:** a multi-KB `git diff` or `sqlite3` dump echoed verbatim in `interaction[].user_says` overflows Sonnet's 8k output cap and breaks the JSON mid-emit. | High pre-fix / Low post-fix | High | Capture-side truncation rule (new): tool-result turns > 800 chars get replaced in `user_says` with a placeholder `<tool-result: N lines, $tool_name>`. The raw text is not echoed by the extractor. See "Known issues" #1 and the `InteractionTurn` schema note. Risk introduced by Architect Round 2; mitigation lands in the `facet-extract-v2.md` pattern before Phase 3 ships. |
| Verbatim AI/user turns leak secrets that the v1 paraphrase-pass had been silently scrubbing. | Medium | High | Prompt-level redaction (`<redacted>` for tokens, keys, URLs with secrets). Plus a post-extract pass that scans gem text for known secret patterns and quarantines failing gems. |
| **Idempotency key shifts on chunker re-tuning:** boundary-UUID key produces duplicate overlapping gems instead of upserts when a chunker change moves the span by one or two turns. | High pre-fix / Low post-fix | Medium | Key on `(workitem_id, content_hash)` where `content_hash = sha256(sorted turn UUIDs in span)`, not the boundary UUIDs. Boundary UUIDs are still stored for inspection but do not participate in uniqueness. `facet::dedupe` provides a content-similarity backstop for cases where the UUID set itself shifts. Risk + fix introduced by Architect Round 2. |
| **Cross-Session clusters are too broad:** HDBSCAN epsilon set too loosely produces a 100-gem cluster that is "Rust" rather than "Tuesday's tokio deadlock." | Medium | Medium | HDBSCAN epsilon tuned for tightness; clusters of 100+ are a tuning signal, not a runtime case to cap. If a tight cluster legitimately reaches 30 gems, Opus handles it (~24k input tokens). No arbitrary `max_cluster_size` cap (Architect Round 2 explicitly rejected this as story-mutilation). |
| **Synthesis produces a changelog rather than a narrative:** a sequence of disconnected events on the same topic gets dressed up as a story by Opus. This is the Q4 "hard question" from Architect Round 1. | Medium | Medium | Strict rejection gate in `facet-narrate.md`: Opus must return `title: ""` for changelog-shaped clusters. Per-archetype split (Session Arc vs Cross-Session Arc, Phase 5) gives the synthesiser two different prompt templates suited to each shape. Chronological ordering inside every cluster makes "thinking evolved over time" the arc by construction. |
| Storage growth: gems are bigger than moments; embeddings are stored per-gem. | Low | Low | SQLite handles 100k+ gems trivially. Embeddings are ~1.5KB each. Even at 10k gems, the embedding store is ~15MB. |
| Operator can't tell which spectra are evergreen (mode-bucketed) vs discovered (narrative). | Medium | Low | Filename convention: `mode-<name>.md` for evergreens, `<narrative-slug>.md` for discovered. Frontmatter `facet-spectrum-archetype: session \| cross-session \| evergreen` makes it queryable. |
| **Operator rejects a discovered narrative but the next narrate pass regenerates it.** | Medium pre-fix / Low post-fix | Medium | Rejection is operator-edits-frontmatter (`facet-spectrum-status: rejected`); narrate pass suppresses candidates whose gem-id set overlaps >= 80% with a rejected spectrum. See Phase 5. Promoted from Open Question to named risk + design decision per Architect Round 2. |
| **Dialog-arc chunker splits a span mid-arc**, fragmenting the four-part anatomy across two chunks. | Medium | Medium | Phase 3 chunker requirements: user-turn boundaries only, max 50 turns/chunk, 4-turn overlap window. The LLM gets two windows to recognise an arc that crossed a boundary. Sub-spec promoted from implicit risk to named Phase 3 requirement per Architect Round 2. |
| ~~Prototype shows the four-part anatomy isn't reliably extractable~~ | ~~Low~~ Resolved | n/a | Prototype confirmed all four parts extract distinguishably. Closed. |
| Dreaming proposes bad mutations to canonical prisms. | Medium | Medium | Dreaming NEVER auto-applies. Output is always proposals under `notes/facet/dreams/`. Apply is a separate confirmed-by-operator step. |

## Open Questions

- [x] ~~How long does the AI verbatim need to be?~~ Prototype settled on 2500 chars for AI turns, 1500 for user turns; truncate mid-sentence with `…`. Revisit if any single gem exceeds 12 KB.
- [x] ~~Where do the four-part anatomy boundaries get computed?~~ Extract does it; LLM handles the boundary detection reliably (prototype confirmed). The cluster step does not need to pre-annotate.
- [ ] Should `present` (Phase 7) produce a single-presentation markdown (one file, slide breaks via `---`) or a slide-deck JSON (for tooling like reveal.js)? Default to markdown; revisit if Scott actually presents from these.
- [ ] Is "evergreen spectrum" the right naming, or should we just have all spectra be discovered (drop the mode-bucket back-compat path entirely)? Lean toward discovered-only after Phase 5 ships; revisit at cutover.
- [x] ~~How does the operator REJECT a discovered narrative they don't like?~~ Resolved in Phase 5: frontmatter `facet-spectrum-status: rejected`; narrate pass suppresses candidates whose gem-id set overlaps >= 80% with a rejected spectrum. Moved from Open Question to named design decision per Architect Round 2.
- [ ] What's the right minimum cluster size for narrative synthesis? Prototype suggests ≥3 gems per cluster, but a powerful 2-gem pair (e.g., a failure-mode that recurred only twice but was costly both times) is worth narrating too. Probably configurable, default 3.
- [ ] When dreaming proposes a semantic-duplicate merge, what's the UX for confirming? A markdown checklist Scott edits in Obsidian? A separate `sb facet dream apply <dream-id>` command? Defer until dreams are producing output.

## References

- `~/repos/scottidler/obsidian/notes/shopify-ceo-reveals-their-secret-ai-developer.md` — source for the four-part anatomy and the apprenticeship-gap framing.
- `~/repos/scottidler/obsidian/notes/agents-that-remember.md` — source for the sessions / memory / dreaming layer model.
- `~/repos/scottidler/second-brain/docs/design/2026-05-26-facet-judgment-harvester.md` — facet v1 design (this is the doc we are partly superseding).
- `~/.local/share/sb/borg/stages/ht-3cda19/distilled.yml` — full Shopify-CEO transcript (raw fossil).
- `~/.local/share/sb/borg/stages/ht-b8c73b/distilled.yml` — full agents-that-remember transcript.
- `~/repos/scottidler/obsidian/notes/jeffrey-emanuel-rule-of-five-agentic-llm.md` — Rule of Five methodology used to refine this doc.

## Reproducing the Prototype

```sh
# 1. Pattern (in repo + synced to runtime)
cat facet/patterns/facet-extract-v2.md

# 2. Build the input slice from a JSONL session
SESSION=~/.claude/projects/-home-saidler-repos-scottidler-second-brain/d81fb682-dc6d-45d4-8385-50e7a23c29bd.jsonl
python3 /tmp/build-session-slice.py "$SESSION" 1100 1300 > /tmp/slice-rename.yaml

# 3. Invoke fabric (needs -V Anthropic because sonnet-4-6 is a recent model)
cat /tmp/slice-rename.yaml | \
  fabric -p ~/.config/sb/patterns/facet-extract-v2.md \
         -m claude-sonnet-4-6 -V Anthropic \
  > /tmp/extract-v2-result.json

# 4. Validate + inspect
python3 -c "import json; d=json.load(open('/tmp/extract-v2-result.json')); print(len(d['gems']), 'gems')"
```

Reference fixtures: `facet/tests/fixtures/v2/{slice-rename-input.yaml, gems-prototype-rename-slice.json}`.
