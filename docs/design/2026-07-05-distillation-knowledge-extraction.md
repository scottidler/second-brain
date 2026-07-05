# Design Document: Distillation Knowledge-Extraction Overhaul

**Author:** Scott Idler
**Date:** 2026-07-05
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Borg's distillation pipeline is tuned for cataloging, not knowledge extraction: fixed tiny claim budgets, a global 10-claim cap that discards everything after the first ~2 chunks of a long video, silent 32K-char input truncation for articles/threads, prompts that instruct the model to drop opinions (the synthesis fuel), the user's own capture annotation thrown away at every transport, and claims that are unreachable by the default vector-only retrieval pipeline. This design fixes extraction density, capture durability, annotation threading, and retrieval reach - gated by a distillation-quality eval harness built first so every change is measured, not vibes.

## Problem Statement

### Background

The second brain exists to fill an Obsidian vault with useful, retrievable knowledge for later synthesis. Borg ingests blogs, GitHub repos, YouTube videos, threads, images, and voice notes; the distillers crate runs per-kind Fabric patterns producing the `Distilled { summary, claims, tags, links }` contract; cortex embeds; oracle retrieves. A 2026-07-04 audit of the patterns and the code around them (session-level review, same methodology as the clyde `report.pmt` review) found the extraction layer systematically under-delivering. All findings below are code-verified.

### Problem

Seven verified defects, in severity order:

1. **Long-video claim loss.** `distill_long` merges chunk claims chronologically (`distillers/src/video.rs:224-257`), then `enforce_bounds` truncates to `MAX_CLAIMS = 10` (`distillers/src/validate.rs:13,22-24`). A 6-chunk video keeps only chunks 1-2's claims; a 3-hour podcast keeps roughly its first 20 minutes. The reduce pattern explicitly refuses to produce claims (`borg/patterns/distill-video-reduce.md:11-12`), so nothing selects the *best* N claims across the whole video - the pipeline keeps the *earliest* N. Voicenote has the same defect (`distillers/src/voicenote.rs` map-reduce twin).
2. **Slide-published videos lose all distilled sections.** When the slides path produces a body, it replaces `rendered_distilled.body_markdown` wholesale (`borg/src/pipeline.rs:653-674`), and `obsidian-youtube-slides.md:85` mandates the legacy `obsidian-note.md` shape. Those notes get no `## Summary`/`## Claims`/`## Transcript` - which also means no claims FTS text and no transcript-chunk embeddings (`vault/src/search/index.rs:98-99` parses these from the body).
3. **Articles/threads silently truncate at 32K chars.** All single-call distillers default `max_chars: 32_000`; `vault/src/fabric.rs:216-229` `truncate_input` cuts the tail with only a daemon-log WARN. Video/voicenote have a map-reduce path above 12K tokens; article/thread/repo/image do not, despite the chunking primitives being shared (`distillers/src/parse.rs:67-83`, `video.rs:507`).
4. **Articles are the lossiest kind, permanently.** The article distiller sets `transcript: None` ("origin URL is the recoverable archive", `distillers/src/article.rs:181-183`); the staged copy is deleted by the retention sweep after 60 days (`borg/src/retention.rs:55-102`, `borg/src/config.rs:683`). After day 60, an 8,000-word essay survives as a ≤2000-char summary plus ≤7 sentences and a URL that may rot. Video, voicenote, image, and thread all keep their transcript in-note; articles and repos do not.
5. **The user's capture annotation is dropped at every transport.** Telegram (`borg/src/telegram.rs:587-592`) and Signal (`borg/src/signal.rs:641-645`) extract the URL and discard surrounding prose; ntfy parses Url-vs-Text exclusively; `IngestRequest` has no prose field (`borg/src/types.rs:238-246`); the CLI text path turns prose+URL into an Idea note without ever fetching the URL when the prose is ≥10 chars (`borg/src/pipeline/text.rs:30-37`). "This is how we should fix borg's linker: <url>" is the highest-signal sentence in the capture - the edge between the source and existing work - and it never reaches the note.
6. **Claims are invisible to default retrieval.** Embeddings come in exactly two kinds - `Summary` (title + summary text) and `TranscriptChunk` (`vault/src/search/vector.rs:39-53`); the shipped default retrieval is vector-only with BM25 off (`config/templates/oracle.yml.example:49-66`). Claims are FTS5-indexed (`notes.claims`, `vault/src/search/schema.rs:21`) but the default pipeline never consults FTS. For an article or repo note, default search sees a title and 2-4 sentences; the seven extracted claims are dead weight.
7. **The claims philosophy filters out synthesis fuel, and the claim schema is impoverished.** `distill-article.md` instructs "Drop opinion and authorial reflection" - but captured articles are mostly *arguments*; the position and reasoning are the value. The `Claim` struct is a bare sentence + anchor: no claim kind (fact / position / recommendation / number), no attribution, no verbatim quote (the legacy format had "Best Quotes"; the new contract dropped quotes entirely). Fixed budgets ignore content richness: 7 claims for a 400-word post and for a 10,000-word essay alike.

Plus one hygiene defect: all 8 distill patterns mandate `tags: []` while the code lowercases, dedups, unions, caps, and merges distiller tags into the tag pipeline (`distillers/src/video.rs:268-282`, `borg/src/pipeline.rs:617`) - plumbing that only carries data if the LLM disobeys the pattern.

### Goals

- Claims cover the whole source, not its head: reduce-step selection for chunked kinds, map-reduce for long articles/threads, claim budgets that scale with content size.
- Verbatim capture is durable: article text survives in-note past the 60-day staging retention; slide-published videos regain their distilled sections.
- The user's capture annotation ("why I captured this") is threaded from every transport into the published note.
- Claims are reachable by the default retrieval pipeline (claim embeddings).
- Claims carry synthesis-grade structure: kind, attribution, optional verbatim quote; positions are captured attributed rather than dropped.
- Every behavior change is measured by a distillation-quality eval harness built before the changes, modeled on the existing oracle retrieval eval.

### Non-Goals

- **Slide pipeline internals** (frame extraction, slide filtering, `obsidian-youtube-slides.md` content) - untouched; only the publish splice is changed so distilled sections are appended below the slide body.
- **Retrieval pipeline composition** (stage order, fusion weights, rerank) - owned by `2026-06-06-configurable-retrieval-pipeline.md`. Phase 9 adds a new embedding kind and measures; it does not re-tune the pipeline.
- **Specialized thread fetchers** (X/Reddit/HN JSON APIs). Threads keep the Jina/fabric-u fetch chain; fetch-surface depth is parked - revisit if the eval harness shows thread claim coverage capped by transcript quality rather than by distillation.
- **Repo fetch surface expansion** (release notes, docs/, About description). Parked - revisit after the eval baseline exists; cheap adds, but unmeasured today.
- **PDF/document ingestion** - stays on the legacy `fabric::summarize` path (`borg/src/pipeline/handlers.rs:1082-1089`); migrating it to the distiller contract is a separate effort.
- **Claim-level entity extraction** - entities and triples remain cortex's downstream pipeline (`extract-entities`/`extract-triples`); the richer claim shape (kind/who/quote) feeds it but this design does not touch it.
- **Retention policy changes** for staged artifacts. Durable article capture is solved in-note (Resolved Decisions); `retention.rs` and the trace-availability semantics of `2026-06-20-oracle-trace-availability.md` are untouched.

## Proposed Solution

### Overview

Ten phases in four groups:

- **Measure first** (Phases 0-1): a zero-code spike proving the reduce pattern can select claims, then a distillation eval harness (`judge-distillation` pattern, golden fixtures from staged traces, judgment cache, calibration) with a recorded baseline.
- **Contract** (Phases 2-4): reconcile the tags plumbing and claim-cap constants, upgrade the `Claim` schema (kind / who / quote, serde-defaulted), rewrite the patterns for the new shape and attributed positions.
- **Coverage** (Phases 5-7): reduce-step claim selection for video/voicenote, article/thread map-reduce chunking, durable note content (article transcript in-note + slide-note append).
- **Reach** (Phases 8-9): capture-note threading through every transport, and `EmbeddingKind::Claim` with the `note_embeddings` migration, measured by the retrieval eval.

### Architecture

Existing data flow, with changes marked:

```
transport (telegram/signal/ntfy/http/cli)
  └─ prose+URL → ContentKind::Url { url, note }          [Phase 8: note field NEW]
       └─ Stage 0 fetch (jina / fabric-u / browser-UA)
            └─ Stage 2 distill (distillers crate)
                 ├─ short input  → single pattern call
                 ├─ long video/voicenote → chunk → map → reduce
                 │     └─ reduce SELECTS claims from chunk pool   [Phase 5 NEW]
                 ├─ long article/thread  → chunk → map → reduce   [Phase 6 NEW]
                 └─ Distilled { summary, claims[kind,who,quote], …, transcript }
                                                          [Phase 3: claim fields NEW]
                                                          [Phase 7: article transcript NEW]
            └─ Stage 3 publish
                 ├─ render: ## Why Captured / Summary / Claims / Links / Transcript
                 │                                        [Phase 8: Why Captured NEW]
                 └─ slide body + appended distilled sections      [Phase 7: append NEW]
cortex embed: Summary | TranscriptChunk | Claim           [Phase 9: Claim NEW]
oracle: default vector pipeline now reaches claims        [Phase 9 effect]
sb borg eval: judge-distillation over golden fixtures            [Phase 1 NEW]
```

House invariants preserved: vault stays LLM-free; distillers own fabric calls; cortex remains the only embeddings writer; libraries return typed data; `vault::distilled` remains the schema source of truth; pattern source of truth stays `borg/patterns/` synced by `otto deploy`.

### Data Model

**Claim schema upgrade** (`vault/src/distilled.rs`), all new fields serde-defaulted so existing `distilled.yml` staged artifacts and `cortex summarize --backfill` inputs deserialize unchanged:

```rust
pub struct Claim {
    pub text: String,
    pub anchor: Option<String>,
    /// fact | position | recommendation | number. Default: fact.
    #[serde(default)]
    pub kind: ClaimKind,
    /// Attribution for positions/thread claims: "@simonw", "the author".
    #[serde(default)]
    pub who: Option<String>,
    /// Short verbatim quote (≤200 chars) supporting the claim.
    #[serde(default)]
    pub quote: Option<String>,
}
```

`ClaimKind` is a `vault::distilled` enum (schema is law; consumers import it, never re-string it). Render (`distillers/src/render.rs`) emits, per claim:

```markdown
- **position** (@simonw): Orchestration beats autonomy for coding agents. [00:14:30]
  > "the agents don't need to be smart, the harness does"
```

Kind prefix omitted when `fact` (the default, keeps existing notes' visual shape); `who` omitted when absent; quote as an indented blockquote line only when present.

**Claim budget** becomes size-aware. `MAX_CLAIMS` (currently a flat 10, no consumer outside the distillers crate depends on it) is replaced by:

```rust
/// base 10, +2 per chunk beyond the first, hard ceiling 24
pub fn max_claims(chunk_count: usize) -> usize
```

Single-call kinds pass `chunk_count = 1` (cap stays 10). `enforce_bounds` gains the cap as a parameter (`enforce_bounds(distilled, max_claims)`) since it cannot know chunk count today. Pattern-level caps are made consistent with the code caps in Phase 2 (article pattern says 7 today while the code allows 10).

**Capture note**: `ContentKind::Url(String)` becomes `ContentKind::Url { url: String, note: Option<String> }`. The note travels: transport → `process_content` → `process_url_inner` → `NoteContent` (which already carries the analogous `description: Option<String>` for the YouTube callout) → frontmatter `capture-note:` + body section `## Why Captured` rendered above `## Summary`. It is also appended to the summary-embedding text in Phase 9 (title + capture note + summary) so "why I captured it" is semantically searchable. `IngestRequest` gains an optional `note` field - additive-only, coordinated extension re-sign in the same PR per the existing contract (`borg/tests/extension_body_matches_ingest_request.rs`).

**Embeddings**: `EmbeddingKind` gains `Claim`. The `note_embeddings` CHECK constraint (`vault/src/search/schema.rs:103`) cannot be altered in SQLite; migration is table-rebuild-preserving-rows (create new table with widened CHECK, `INSERT INTO ... SELECT`, drop old, rename) - existing summary/transcript rows survive, avoiding hours of re-embedding ~21K rows on the AVX-only daemon host. Claim text is embedded from `notes.claims` (already populated by the indexer at `vault/src/search/index.rs:99`), following the summary path's no-file-I/O pattern: claims joined into one row per note, split into additional rows by the existing chunk budget when the joined text exceeds the model's token window (24 claims can exceed bge-small's 512-token limit; silent model-side truncation would drop tail claims - the exact defect this design removes elsewhere). `search_vector` has no kind filter and max-pools per note (`vector.rs:139-146,156-248`), so claim rows enter ranking automatically - which is a **risk, not a freebie** (review-panel consensus): up to 24 narrow claim vectors give every note extra chances to win on a tangential sentence. The governing retrieval invariant: **claim rows may only add recall, never displace precision** - claims exist to rescue notes whose summary embedding misses the query, not to outrank notes whose summary answers it. Phase 9 gates on per-query non-regression (not just aggregate nDCG), with kind-weighted pooling in `search_vector` as the named contingency if the gate fails. `semantic_neighbors` keeps reading `kind='summary'` only.

Capture-note reachability requires real plumbing (panel finding): the summary embed path reads `notes.summary` + `notes.title` with no file I/O, and `index_one` does not persist arbitrary frontmatter - so Phase 9 adds a `notes.capture_note` column populated by the indexer from the `capture-note:` frontmatter, threaded through `StaleTarget` into the embed-text assembly. The `## Why Captured` body section is FTS-reachable without any of this.

### API Design

New/changed surfaces (all internal to the workspace):

- `sb borg eval [--fixtures <path>] [--judge-model <m>] [--emit-calibration]` - distillation-quality eval, named parallel to `sb oracle eval` and modeled on it (`oracle/src/eval.rs` structure: retrieve/evaluate split, judgment cache SQLite, calibration panel with trust gate).
- `distillers`: `ReduceYaml` gains `#[serde(default)] claims: Option<Vec<PatternClaim>>`; reduce-input rendering includes the chunk-claim pool with anchors; parse-failure falls back to today's chronological merge.
- `borg/patterns/`: `distill-article-chunk.md`, `distill-article-reduce.md` (new); `distill-video-reduce.md`, `distill-voicenote-reduce.md` rewritten to select claims; all distill patterns updated for the new claim shape; `judge-distillation.md` (new).
- `vault::distilled::{Claim, ClaimKind}` as above; `Distilled.transcript` populated for articles.
- `ContentKind::Url` struct variant; `IngestRequest.note`; `NoteContent` capture-note field.
- `vault::search::vector::EmbeddingKind::Claim`; cortex embed loop gains the claim arm in `stale_embedding_targets`, the CLI `--kind claim` value, and the rollback verb `sb cortex embed --drop-kind claim`; `notes.capture_note` indexed column for embed-text assembly.

### Implementation Plan

Ordering constraints: eval harness before any prompt/behavior change (measurability); claim schema before prompt upgrades and claim embeddings; the `note_embeddings` migration lands with its writer (Phase 9); capture-note threading is independent after Phase 1. Operator steps are called out per phase, never buried.

#### Phase 0: Spike - prove the reduce pattern can select claims
**Model:** sonnet
- Zero code. Take a real >4-chunk video trace from staging; hand-assemble a reduce input containing the chunk summaries plus the pooled chunk claims (with anchors); run `fabric -p` with a prototype selection prompt.
- **Success criteria:** raw fabric output parses as `{summary, claims[]}`; selected claims are verbatim (or near-verbatim) members of the input pool with anchors intact - not paraphrases with invented timestamps; at least one selected claim originates from the final third of the pool.

#### Phase 1: Distillation eval harness + baseline
**Model:** opus
- `sb borg eval` copying the `oracle/src/eval` structure: fixtures harvested from staged traces (`transcript.md`/`fetched.html` + `distilled.yml` pairs, snapshotted into `config/eval/distill-fixtures/` so they outlive retention; curated, not bulk-dumped - a handful per kind, long-transcript exemplars included deliberately; voicenote fixtures are screened or synthesized so no personal audio transcript lands in the repo), a `judge-distillation.md` pattern scoring three axes on a 0-3 rubric - claim coverage (the judge enumerates the transcript's key claims, then scores what fraction the note represents), anchor validity, summary faithfulness - with the composite as their mean; judgment cache keyed on content+model hashes, calibration hooks (`--emit-calibration`).
- Record the pre-change baseline scores in the doc's addendum.
- **Success criteria:** `sb borg eval` produces a scored report over ≥20 fixtures spanning all kinds; a re-run is cache-hit stable (0 new judge calls); baseline numbers recorded.

#### Phase 2: Tags + cap reconciliation
**Model:** sonnet
- Flip all distill patterns from "leave `tags: []`" to "propose up to 7 lowercase candidate tags"; the existing canonical post-filter (`borg` hygiene + `pipeline.rs:617` merge) already gates them. Rationale: the distiller sees the full content; downstream tagging sees less; the plumbing (lowercase/dedup/union/cap) already exists and is currently dead.
- Make pattern claim caps consistent with code caps (article pattern 7 → 10).
- **Success criteria:** a fixture ingest produces distiller-proposed tags that survive the canonical filter; `cargo test --workspace` green; eval score non-regressing vs Phase 1 baseline.

#### Phase 3: Claim schema upgrade
**Model:** opus
- `Claim { kind, who, quote }` + `ClaimKind` in `vault::distilled`, serde-defaulted; mirror leaf structs in `distillers/src/parse.rs`; render shape in `distillers/src/render.rs` (kind prefix, who, quote blockquote); `parse_body_claims` (`vault/src/search/index.rs`) updated to strip the new decoration for FTS text.
- Forward-compat, not just back-compat (panel condition): an unknown `ClaimKind` string from a drifting LLM must NOT hard-fail the whole parse - deserialize shim maps unknown kinds to `Fact` with a WARN, so one bad enum value can't demote an entire distillation to fallback.
- Replace flat `MAX_CLAIMS` with `max_claims(chunk_count)`; `enforce_bounds` takes the cap as a parameter.
- **Success criteria:** named `distilled.yml` fixtures (old-shape, new-shape, unknown-kind) all deserialize with the documented defaults - not "a sampled staging dir"; rendered claims with all fields present parse back via `parse_body_claims`; old-shape YAML produces `kind=fact, who=None, quote=None`.

#### Phase 4: Pattern upgrades for the new claim shape
**Model:** sonnet
- Rewrite `distill-article/video/thread/repo/image/voicenote(+chunks)` for: the new claim fields; attributed positions ("The author argues that..." → `kind: position, who:`) instead of the opinion ban; optional ≤200-char verbatim `quote`; thesis-first summaries ("state the thesis and strongest takeaway", not "what it is and who it is for"); size-aware claim budgets stated in the prompt.
- **Operator step:** `otto deploy` to sync patterns.
- **Success criteria:** eval coverage score improves or holds vs baseline on every kind; no increase in `yaml-parse-error` fallbacks across a 20-fixture replay.

#### Phase 5: Reduce-step claim selection (video + voicenote)
**Model:** opus
- Reduce input becomes two labeled sections: `## Chunk Summaries` (today's blank-line-joined summaries) and `## Claim Pool` (every chunk claim, one per line, anchor prefixed). Rewrite both reduce patterns to select up to `max_claims(chunk_count)` from the pool, spanning the whole timeline; extend `ReduceYaml` with `claims` (carrying the full Phase 3 shape - kind/who/quote - not just text/anchor); parse selected claims back with an anchor-honesty rule that tolerates paraphrase without permitting invention (panel condition): a selected claim WITH an anchor must match a pool anchor or the anchor is stripped and counted; a selected claim WITHOUT an anchor is accepted as a synthesis across pool claims - no text-match gate, so legitimate consolidation is never dropped as "invented". Fall back to the current chronological merge on parse failure or empty selection, recorded as a distinct `fallback_reason` (`reduce-selection-failed`) - NOT buried in `bounds_truncations` - because the fallback silently reintroduces the head-bias this phase removes; the eval harness watches this rate.
- **Success criteria:** a >4-chunk video fixture AND a >4-chunk voicenote fixture yield published claims whose anchors land in the final third; fallback-path unit test (malformed reduce output → chronological merge, `fallback_reason=reduce-selection-failed` recorded); eval coverage for long videos improves vs baseline.

#### Phase 6: Article + thread map-reduce chunking
**Model:** opus
- Above a 12K-token threshold (matching video), chunk via the shared `chunk_transcript`/`find_boundary`, map with new `distill-article-chunk.md`, reduce with `distill-article-reduce.md` (summary synthesis + claim selection per Phase 5's mechanics). Threads reuse the chunk/map mechanics with a thread reduce variant that additionally emits `author`/`post-count` - the reduce input includes the transcript head, where thread metadata lives, so `KindPayload::Thread` fields survive the long path. Make sub-threshold truncation loud: if `truncate_input` would cut a single-call input, log WARN with the trace id and record a `bounds_truncations` entry.
- **Operator step:** `otto deploy`.
- **Success criteria:** a >32K-char article fixture containing a unique fact only beyond the 32K-char mark distills with zero `truncate_input` cuts and that fact appears in the published claims (articles carry no anchors, so provenance is asserted by content, not timestamp); a >32K-char thread fixture publishes with `author`/`post-count` intact through the long path; single-call path unchanged for short articles (fixture diff). Impl notes record the measured reduce-input size for the largest fixture (panel: lost-in-the-middle risk is real for pathological inputs; measure, don't block - the video path already survives the same shape).

#### Phase 7: Durable note content
**Model:** opus
- Articles: populate `Distilled.transcript` with the fetched markdown (mirroring `thread.rs:192-202`), rendered under `## Transcript`. Repos stay transcript-free (the README is already summarized structurally; staged copy + origin URL suffice).
- Slide-published videos: change the publish splice (`borg/src/pipeline.rs:653-674`) from replace to slide-body-then-append - distilled `## Summary`/`## Claims`/`## Links`/`## Transcript` follow the slide sections, restoring claims-FTS and transcript-chunk embeddings for that class. Some slide-vs-summary redundancy in those notes is accepted; reach wins.
- Article transcripts are the Jina/fabric-u markdown as fetched - heading demotion already guards the splice (`render.rs` `push_transcript`); residual nav junk is the same quality class threads accept today.
- Amend `transcript_eligible()` to include `Article` and `Youtube` (panel finding, code-verified): the embed loop filters transcript targets by `note_type IN (transcript_eligible)`, and the current list excludes exactly the two note classes this phase targets - without this change the new `## Transcript` sections are FTS-indexed but never embedded.
- **Success criteria:** article fixture note carries `## Transcript` with the full fetched markdown; a slide-path fixture note contains both slide sections and `## Claims`; `index_vault` produces claims FTS text for both AND `sb cortex embed` produces `transcript-chunk` rows for an article note and a slide-path youtube note (FTS parsing and embedding creation asserted separately - they are different code paths).

#### Phase 8: Capture-note threading
**Model:** opus
- `ContentKind::Url { url, note }`; one extraction rule applied at every transport (telegram, signal, ntfy, CLI): capture note = the message text with the first URL token removed, whitespace-collapsed; empty → `None` (additional URLs stay in the note text as plain links - the first URL remains the capture target, as today); attachment captions (the Signal `caption:` tag hack, `borg/src/signal.rs:663-668`) migrate into the same field; CLI text rule change: prose+URL always becomes an annotated URL ingest, `idea:` prefix forces an Idea note (replaces the `<10 chars` heuristic at `pipeline/text.rs:30-37`); `IngestRequest.note` (additive) + extension re-sign in the same PR; `NoteContent` carry-through; render frontmatter `capture-note:` + `## Why Captured` section; thread `capture_note` into `DistillInputs` so patterns may use it as context (patterns treat it as trusted operator text - it is the user's own words).
- **Operator step:** extension re-sign + `otto deploy`.
- **Success criteria:** prose sent with a URL lands in the published note's `## Why Captured` for ALL five transports (telegram, signal, ntfy, HTTP `IngestRequest`, CLI text) - one fixture each, since each transport has its own extraction site; `extension_body_matches_ingest_request` passes; bare-URL ingests render no empty section.

#### Phase 9: Claim embeddings + measurement
**Model:** opus
- `EmbeddingKind::Claim`; `note_embeddings` table-rebuild migration preserving existing rows. Migration spec (panel condition): one transaction; idempotent via old-CHECK detection (inspect `sqlite_master.sql` for the `'claim'` literal before rebuilding); indexes and `embedding_config` recreated/preserved; crash mid-migration leaves the old table intact (rebuild into a temp name, swap last).
- `stale_embedding_targets` claim arm reading `notes.claims`; cortex CLI `--kind claim` + daemon tick inclusion; `notes.capture_note` column + `StaleTarget` field so summary-embedding text becomes title + capture-note + summary (notes without a capture note produce byte-identical embedding text, so staleness detection re-embeds nothing retroactively).
- Rollback is a first-class verb, not hand-run SQL (panel condition - reverting cortex code does NOT stop oracle reading claim rows, since `search_vector` scans all kinds): `sb cortex embed --drop-kind claim` deletes the rows.
- Measurement enforces the retrieval invariant: `sb oracle eval` on the `configured` target before backfill and after; gate is **no per-query nDCG regression beyond noise on the calibration set AND aggregate non-negative** - not aggregate alone. If the per-query gate fails: `--drop-kind claim`, implement the kind-weighted pooling contingency in `search_vector`, re-backfill, re-measure.
- **Operator step:** one-time `sb cortex embed --backfill` on the daemon host; `sb oracle eval` before/after as above.
- **Success criteria:** claim rows exist for backfilled notes (SQL count > 0 for `kind='claim'`); existing summary/transcript rows survive the migration byte-identical (row-count + spot-hash check); per-query + aggregate eval gate passes (a null aggregate result with no per-query regressions is accepted and recorded); `sb cortex embed --drop-kind claim` empties the kind and a re-run of the eval reproduces the pre-backfill baseline.

## Acceptance Criteria

- [ ] A >4-chunk video fixture publishes at least one claim anchored in the final third of its duration (Phase 5 integration test).
- [ ] A >32K-char article fixture distills with no `truncate_input` cut, a fact present only beyond the 32K-char mark appears in its published claims, and its note carries the full fetched markdown under `## Transcript` (Phases 6-7).
- [ ] Prose accompanying a URL through telegram and HTTP ingestion appears in the published note under `## Why Captured`, and `extension_body_matches_ingest_request` passes (Phase 8).
- [ ] With claim embeddings backfilled, a default-pipeline (vector-only) `knowledge_search` for a phrase that appears only in a note's claims returns that note in the top 10, AND no calibration-set query's nDCG regresses beyond noise vs the pre-backfill baseline (Phase 9 - recall added, precision protected).
- [ ] `sb borg eval` runs pre- and post-change; the post-change composite score is ≥ the recorded baseline on every content kind (Phases 1, 4-7 gate).

## Resolved Decisions

All five author-proposed decisions were reviewed 2026-07-05 by the panel (Architect/Gemini + Staff Engineer/Codex): **unanimous AGREE on all five**, with conditions folded into the phases as noted.

- **2026-07-05 - Durable article capture is in-note, not retention-exemption.** In-note `## Transcript` matches the video/voicenote/thread precedent and restores FTS reach; embedding reach additionally requires the `transcript_eligible()` amendment (panel condition, folded into Phase 7). Cost: larger article notes - accepted; video notes already carry full transcripts of comparable size. Acknowledged consequence (panel condition): making verbatim article text searchable in-vault partially supersedes the `2026-06-20` trace-availability stance that verbatim sources are advertised-but-not-searched - deliberate, because the vault is the synced, durable store and staging is not. (Author + panel, confirmed.)
- **2026-07-05 - Slide notes get distilled sections appended, not replaced.** Cheap splice change with high value (restores claims/transcript for the whole slide class); slide pipeline internals stay out of scope; same `transcript_eligible()` condition covers the youtube note type. (Author + panel, confirmed.)
- **2026-07-05 - `note_embeddings` migration is rebuild-preserving-rows.** Drop-and-recreate would force re-embedding ~21K rows on the AVX-only host; rebuild keeps existing vectors and only the new claim rows need inference. Panel condition folded into Phase 9: single-transaction, idempotent, index/`embedding_config`-preserving, with `sb cortex embed --drop-kind claim` as the first-class rollback verb (code revert alone does not stop oracle reading claim rows). (Author + panel, confirmed.)
- **2026-07-05 - Prose+URL becomes an annotated URL ingest; `idea:` prefix forces an Idea.** Replaces the `<10 chars` heuristic. The existing explicit prefix remains the escape hatch, so no capability is lost, and the ambiguous case now defaults to the interpretation that fetches the source. Panel condition: acceptance covers all five transports. (Author + panel, confirmed.)
- **2026-07-05 - Distillers propose candidate tags; canonical filter stays the gate.** The distiller sees the full content; the downstream autotag surface sees less; the union/dedup/cap plumbing already exists. Reverses the `tags: []` instruction from the extractor-contract design - the canonical-vocabulary post-filter that motivated it is unchanged. (Author + panel, confirmed, no conditions.)
- **2026-07-05 - Retrieval invariant for claim embeddings: recall-only, never precision-displacing.** Set in response to the panel's hardest question ("claims rescue invisible notes" vs "summary dominance" - pick one): claim rows exist to rescue notes whose summary embedding misses the query; they must not outrank notes whose summary answers it. Enforced by the Phase 9 per-query gate; kind-weighted pooling is the named contingency. (Author, in response to panel; both reviewers requested exactly this decision be made explicit.)

## Alternatives Considered

### Alternative 1: Raise MAX_CLAIMS instead of reduce-step selection
- **Description:** keep the chronological merge, lift the cap to ~50 for long videos.
- **Pros:** trivial; no new pattern behavior to validate.
- **Cons:** floods notes with per-chunk filler claims; no quality ranking; claim count stops meaning anything; embedding and FTS noise scales with it.
- **Why not chosen:** the defect is selection, not budget. Budget scaling alone keeps the head-bias for any video longer than the budget covers.

### Alternative 2: Retention exemption for staged transcripts (vs in-note)
- **Description:** spare `transcript.md`/`fetched.html` from the 60-day sweep so verbatim text survives in staging.
- **Pros:** notes stay small; no vault churn.
- **Cons:** changes `retention.rs` from dir-level to file-level deletion; breaks the `trace-expires`/`within-window` semantics oracle advertises (`2026-06-20` doc); the text is durable but still invisible to FTS and embeddings; staging is per-host, not synced - the laptop never has it.
- **Why not chosen:** durability without reachability misses the point; the vault is the synced, indexed store.

### Alternative 3: BM25 default-on instead of claim embeddings
- **Description:** claims are already FTS5-indexed; enabling BM25 in the default pipeline makes them keyword-reachable with zero schema work.
- **Pros:** config-only change.
- **Cons:** measured regression - the eval that shaped the default showed hybrid at nDCG 0.799 vs vector-only 0.876 because the weak BM25 list dilutes the strong vector signal; keyword reach ≠ semantic reach.
- **Why not chosen:** it re-litigates a settled, eval-backed decision and still doesn't give semantic access to claims. Claim embeddings extend the winning modality instead.

### Alternative 4: One combined "extract everything" mega-pattern
- **Description:** replace per-kind patterns with a single large extraction prompt emitting summary, claims, quotes, entities, and triples in one call.
- **Pros:** one pattern to maintain; densest single-call extraction.
- **Cons:** loses per-kind anchoring rules (timestamps vs post IDs); one parse failure loses everything; contradicts the per-kind distiller architecture (`2026-05-16` extractor contract) and the cortex-owned entities/triples pipeline.
- **Why not chosen:** the per-kind contract is load-bearing across borg, cortex backfill, and replay; incremental schema evolution preserves it.

## Technical Considerations

### Dependencies
- No new crates (verified: serde/serde_yaml/rusqlite/fastembed paths all in place).
- Fabric CLI + configured model, as today. Pattern edits require `otto deploy` to take effect (source of truth `borg/patterns/`, synced to `~/.config/sb/patterns/`).
- Extension re-sign (Phase 8) rides the existing `sb borg extension sign` flow; additive-only `IngestRequest` change per the standing contract.

### Performance
- Reduce-step claim selection adds claim text to the reduce input: bounded by pool size (≤5/chunk) × ~1 sentence, well under the reduce call's input budget.
- Article chunking multiplies fabric calls for long articles exactly as video already does (bounded by `chunk_concurrency`, default 4).
- Claim embeddings: one row per note in the common case, a small number when the joined claims exceed the model's token window (consistent with the Data Model split rule); inference cost is one short text per note on backfill; incremental thereafter. The migration itself is SQL-only (no inference).
- Article transcripts in-note: vault file growth comparable to existing video notes; Obsidian already renders those without issue. FTS/index size grows accordingly (same class as transcript chunks today).

### Security
- Capture note is the operator's own text - rendered verbatim in-note; it is NOT wrapped in the injection guard because it is trusted input, but it IS passed to the distiller inside a labeled block so a pasted hostile string can't masquerade as pattern instructions (same "treat as content" guard applies).
- All existing prompt-injection guards in patterns are preserved verbatim in the rewrites (explicit success criterion of the Phase 4 review).

### Testing Strategy
- Unit: fallback paths (malformed reduce output → chronological merge), serde back-compat (old-shape `distilled.yml` fixtures), anchor-pool validation, `max_claims` scaling, `parse_body_claims` round-trip.
- Integration: fixture ingests per kind through `MemArtifactStore` + mock fabric (house pattern); slide-append splice; capture-note end-to-end per transport; extension body test.
- Eval: `sb borg eval` baseline vs post-phase scores is the quality gate for every prompt-touching phase; `sb oracle eval` gates Phase 9.
- Migration: row-preservation assertion over a copied production DB before first daemon start on the new schema (house rule: snapshot before first run of a schema change).

### Rollout Plan
- Single repo; phases land as individual PRs in order; each prompt-touching phase ends with `otto deploy` on the daemon host.
- Phase 9 backfill is a one-time operator command on the daemon host (embeddings DB is per-host, only the daemon host owns one that matters).
- No coordinated cross-repo shipping. The extension re-sign (Phase 8) is in-repo but user-visible (Firefox updates the .xpi).

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Reduce pattern paraphrases instead of selecting (invented anchors) | Med | Med | Phase 0 spike proves selection; parse-back validates anchors against the input pool; fallback to chronological merge |
| Phase 1 harness scope-creeps (largest phase, easiest to gold-plate) | Med | Med | copy `oracle/src/eval` structure verbatim; curated ≥20 fixtures and three rubric axes, nothing more; anything else is a follow-up |
| New claim fields raise `yaml-parse-error` fallback rate | Med | Med | serde-defaulted fields; fallback preserves transcript (existing behavior); eval replay watches fallback counts (Phase 4 criterion) |
| Judge miscalibration makes the eval gate meaningless | Med | High | copy the oracle eval's calibration panel + trust gate verbatim; hand-labeled calibration subset before trusting scores |
| `note_embeddings` migration corrupts on a deployed DB | Low | High | rebuild-preserving-rows in one transaction, temp-name-then-swap; idempotent old-CHECK detection; snapshot before first run; row-count + spot-hash assertion |
| Claim vectors degrade retrieval precision (max-pool, no kind filter) | Med | High | recall-only invariant; per-query eval gate before accepting the backfill; `sb cortex embed --drop-kind claim` rollback; kind-weighted pooling contingency |
| Article notes bloat the vault / Obsidian rendering | Low | Low | same class as existing video transcripts; measure median note size before/after in Phase 7 impl notes |
| Slide-append changes note shape consumed by Bases/dashboards | Low | Low | append-only below existing sections; `system/views` queries key on frontmatter, not body layout |
| Capture-note behavior change surprises muscle memory (prose+URL no longer makes an Idea) | Med | Low | `idea:` prefix escape hatch documented in the phase; release note in impl notes |

## Open Questions

All *design* decision points were confirmed by the 2026-07-05 pre-implementation review panel (unanimous, conditions folded into phases); every panel finding is dispositioned in Resolved Decisions, the phases, or the risk table.

### Post-implementation audit follow-ups (2026-07-05 Implementation Audit)

A second panel (Architect/Gemini + Staff Engineer/Codex) audited the committed implementation. The high-risk mechanics (Phase 9 migration crash-safety/idempotency, Phase 3 forward-compat shim, Phase 5 anchor-honesty + distinct fallback, the recall-only vector invariant, byte-identical no-capture-note embed text, injection-guard preservation) were independently verified correct. Two defects were fixed in the audit follow-up commit; the remainder are tracked here:

- **FIXED - `judge-distillation.md` was orphaned from install.** The eval judge pattern shipped in `borg/patterns/` but was absent from the `PATTERNS` array in `sb/src/cli/bootstrap.rs` (the only install mechanism), and the guardrail test asserted a hardcoded count instead of comparing the tree, so `sb borg eval` could not find its judge on any provisioned machine. Added to `PATTERNS`; the guardrail test now globs `borg/patterns/*.md` and compares by name so an omission cannot recur.
- **FIXED - schema source-of-truth comment drift.** `Distilled.transcript`'s doc comment claimed Article leaves it `None`; Phase 7 populates it. Corrected.
- **TRACKED (observability) - sub-threshold truncation lacks a trace-bearing breadcrumb.** Phase 6 records a `bounds_truncations` entry in the in-memory distilled meta and WARNs at the distiller layer with `source_url`, but `bounds_truncations` is not persisted to receipts/frontmatter and `degraded` is derived only from `fallback_reason`, so a truncated single-call article/thread is not surfaced by `sb borg log --degraded` and no `trace_id`-bearing WARN is emitted at the pipeline layer (which is the layer that has the trace). Follow-up: either persist `bounds_truncations` into receipts or emit a `trace_id`-bearing WARN at the pipeline distill sites. Deferred rather than patched hastily during finalization.
- **DEFERRED (disclosed) - capture-note reaches the distiller only on the single-call path** of the four URL distillers; the long map-reduce paths and Signal attachment captions render the note in `## Why Captured` but do not feed it to the distiller. Reasoned in the Phase 8 impl notes (prepending to every chunk duplicates context, which the design forbids); the reduce step is the correct future seam if operator-note-informed distillation is wanted for large inputs.

### Post-deploy live-gate checklist (operator, daemon host)

`Status: Implemented` reflects code-complete + unit/integration gates green. The design's measurement gates are operator-run and remain outstanding (done-means-live):

1. Calibrate the eval judge: `sb borg eval --emit-calibration`, hand-label a subset, confirm the judge reports TRUSTWORTHY (else every coverage/per-query gate below measures an unvalidated judge).
2. `otto deploy` (syncs rewritten + new patterns, incl. the now-listed `judge-distillation.md`; restarts daemons).
3. Extension re-sign for the additive `IngestRequest.note` (Phase 8).
4. Phase 4-7 quality gate: live re-distill/replay of fixtures + `sb borg eval`; confirm composite holds-or-improves per kind vs the 1.952 baseline and no increase in `yaml-parse-error` fallbacks.
5. Phase 5/6 coverage: confirm a >4-chunk video/voicenote publishes a final-third-anchored claim, and a >32K article surfaces its late-body fact in published claims.
6. Phase 9 retrieval: `sb oracle eval` on `configured` before backfill (record per-query + aggregate baseline) → `sb cortex embed --backfill` → `sb oracle eval` after; accept only if no per-query nDCG regression beyond noise AND aggregate non-negative. On failure: `sb cortex embed --drop-kind claim`, implement the kind-weighted-pooling contingency (comment sited at the `search_vector` max-pool), re-backfill, re-measure.

## References

- `docs/design/2026-05-16-extractor-contract-and-l2-summaries.md` - Distilled contract (parent)
- `docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md` - note_embeddings owner
- `docs/design/2026-06-06-configurable-retrieval-pipeline.md` - retrieval defaults, config carve-out
- `docs/design/2026-06-06-oracle-eval-baseline.md`, `2026-06-06-oracle-eval-relevance-lift.md` - eval precedent
- `docs/design/2026-06-20-oracle-trace-availability.md` - retention/trace semantics constraint
- `docs/design/2026-04-29-frame-aware-youtube-ingestion.md`, `2026-06-28-content-aware-slide-filtering.md` - slides path owners
- 2026-07-04 session audit of distill patterns (this doc's problem statement) and the clyde `report.pmt` review (b6e485dc) it was modeled on

## Addendum

### Rejected drafts / deferred options
- Retention-exemption durability (Alternative 2) - deferred permanently in favor of in-note.
- BM25 default-on (Alternative 3) - rejected; settled by prior eval.
- Specialized thread fetchers, repo fetch expansion, PDF migration - parked in Non-Goals with revisit conditions.

### Phase 1 baseline scores

Recorded 2026-07-05 from a live `sb borg eval` run over the 21 committed golden
fixtures (`config/eval/distill-fixtures/`, 7 kinds), judged by the
`judge-distillation` pattern against fabric's configured default model. A
re-run made 0 new judge calls (cache-hit stable). Scores are the 0-3 rubric
means; composite is their mean.

```
kind            n   coverage     anchor    summary  composite
------------------------------------------------------------
article         5      1.200      3.000      1.400      1.867
idea            1      3.000      3.000      3.000      3.000
image           2      2.000      3.000      3.000      2.667
repo            3      2.000      3.000      3.000      2.667
thread          3      2.333      2.000      3.000      2.444
video           5      0.400      0.600      0.400      0.467
voicenote       2      2.500      3.000      3.000      2.833
------------------------------------------------------------
ALL            21      1.571      2.286      2.000      1.952

judgments: 21 scored, 21 new, 11 from truncated sources, 0 on a distill fallback
calibration: UNCALIBRATED - no hand labels yet (run `sb borg eval --emit-calibration`)
```

The baseline confirms the problem statement quantitatively: **video is the worst
kind** (composite 0.467 - claim coverage 0.400, anchor validity 0.600), the
signature of long-video claim loss (chunks after the first two are dropped, so
the judge sees mostly missing late-timeline claims). **Articles** score low on
coverage (1.200) and summary faithfulness (1.400) - consistent with the 32K-char
truncation defect and the opinion-dropping prompt. 11 of 21 sources exceeded the
24K-char judge budget and were truncated (the long-transcript exemplars), so the
video/article coverage numbers are, if anything, generous. The panel-condition
calibration subset is not yet hand-labeled; the run is reported UNCALIBRATED
until labels land (Phase 1 ships the `--emit-calibration` hook for this).

The judge model, char budget, and rubric are pinned in the cache key
(`rubric_version = v1`), so these numbers are the fixed comparison point every
prompt-touching phase (2, 4-7) must hold or beat.
