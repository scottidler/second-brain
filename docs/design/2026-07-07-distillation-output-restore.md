# Design Document: Distillation Output Restore

**Author:** Scott A. Idler
**Date:** 2026-07-07
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

The 2026-05-16 distiller cutover and 2026-07-05 knowledge-extraction work regressed the reader-facing note shape: full raw-VTT transcripts now dominate video/article note bodies (96% of a 130KB note is VTT junk), and the legacy `obsidian-note.md` extraction intelligence (Enumerated Points, Key Ideas) was lost in the port. This design keeps every input-side technical improvement (map-reduce chunking, claim schema, timestamp anchors, readable extraction, staging/trace, eval harness) and re-points them at a restored-and-improved output: the April note shape as the floor, transcripts back to consumed-intermediate status, embeddings fed from staging instead of the note body.

## Problem Statement

### Background

Three eras of the video/article note:

- **Pre-2026-05-16 (baseline):** fabric pattern `obsidian-note.md` -> `tldr callout / What This Is About / Enumerated Points / Key Ideas / Best Quotes / References`. Explicit listicle detection ("extract all N items as a numbered list", commit 418d492). No transcript in the note. 5-10KB notes. Preserved example: vault note `top-10-claude-code-skills-plugins-clis-april-2026.md` (original body demoted under its `## Transcript` by the summarize backfill, `cortex/src/summarize.rs:247-254`).
- **2026-05-16 -> 2026-06-28:** Phase-6 cutover (commit 0315222) to structured distillers rendering `## Summary / ## Claims / ## Links / ## Transcript`. `distill-video.md` carries zero enumeration handling. The transcript regression stayed invisible because the slide path replaced the rendered body wholesale.
- **2026-06-28 -> now:** content-aware slide filtering (24dd7ed/ea29514) routes talking-head videos to text-only -> transcript unmasked; 2026-07-05 work (6d29c65, 28466f7) appends distilled sections on slide notes and adds article transcripts. Every new video/article note now carries its verbatim source: 40-240KB notes.

Compounding quality bug: `parse_vtt_segments` (`borg/src/youtube.rs:191-241`) strips only literal `<c>`/`</c>`/`<i>`/`</i>` -- not per-word timing tags (`<00:00:00.360>`) or classed tags (`<c.colorE5E5E5>`) -- and dedups only verbatim-identical consecutive segments, so rolling captions emit every line twice. The correct overlap collapse already exists in `clean_vtt` (`youtube.rs:552-567`) one function away.

### Problem

- The second brain is flooding with verbatim transcripts the owner explicitly does not want in notes. The vault is for extracted knowledge; the verbatim source belongs in staging (that is what the trace system is for).
- The extraction got *shallower* while the notes got *fatter*: a "Top 10 X" video no longer reliably yields the 10 items; themes (Key Ideas) are gone entirely.
- The 2026-07-05 design justified article transcripts with "video notes already carry full transcripts" -- a claim about the post-May code state, not the owner's actual baseline. Circular authority; the premise was never checked against the vault.

### Goals

- Restore the April note shape as the floor for video, article, and repo notes: tldr, summary prose, Enumerated Points (all N items when the content is a listicle), Key Ideas, links.
- Improve past it with the machinery built since: timestamp anchors on claims AND enumerated items, kind/who/quote claim decoration, map-reduce so a 3-hour video distills fully, capture notes, tag proposals.
- `## Transcript` leaves video and article note bodies. The transcript remains a first-class *intermediate*: staged, embedded, advertised via trace.
- Transcript-chunk embeddings survive the move: cortex reads the staged transcript instead of the note body.
- Fix `parse_vtt_segments` so even the staged transcript is clean.
- One-shot backfill strips `## Transcript` from the notes polluted since 2026-06-28.
- Eval gate: listicle-survival metric + note-size ceiling, scored against April fixtures, so this class of regression cannot ship silently again.

### Non-Goals

- No revert of input-side machinery: map-reduce chunking, reduce-step claim selection, VTT anchor fetch, claim schema, readable extraction, capture notes, tag proposals, staging/trace, receipts, eval harness all stay.
- Verbatim-preservation kinds keep their in-note text: VoiceNote, Idea, Vocabulary, Image (Phase 9c decisions, `vault/src/distilled.rs:42-50`). Thread also unchanged.
- No slide-filtering changes; the content filter is correct. Slide notes keep their appended distilled sections -- minus the transcript, same as text-only notes.
- No oracle retrieval-pipeline changes; only the embedding *source* for transcript chunks moves.
- Re-rendering pre-2026-06-28 notes into the new shape: parked, not excluded. Revisit condition: after the new shape ships and a re-distill verb exists, a separate decision on bulk re-distillation.

## Proposed Solution

### Overview

- The `Distilled` contract grows three serde-defaulted fields (`tldr`, `enumeration`, `key_ideas`) that the distill prompts populate and `render.rs` emits in the April order.
- `render.rs` stops emitting `## Transcript` for Video and Article kinds. `Distilled.transcript` stays populated -- the staged `distilled.yml` (written on all four distiller paths, `borg/src/stages/distill.rs:278,505,616,684`) keeps the full transcript for free.
- Cortex's transcript-chunk embedding loop joins `notes.trace` -> staged `distilled.yml` for Video/Article; the in-note `## Transcript` section remains the source for the verbatim kinds.
- Prompts get the `obsidian-note.md` enumeration language back, threaded through the chunk -> reduce map-reduce path so the enumeration survives chunking.
- A note-size hard gate at publish catches any future verbatim leak.

### Architecture

Transcript custody after this design:

```
fetch (VTT/readable) -> parse/clean -> distiller input
                                   |-> staged distilled.yml   (durable record, embedding source, trace.ref)
                                   |-> note body               (NEVER, for video/article)
claims/enumeration/key-ideas/tldr -> note body -> FTS + Summary/Claim embeddings
staged transcript -> cortex embed (TranscriptChunk rows, via notes.trace join)
```

Note body order (video/article), rendered by `distillers/src/render.rs`:

1. `## Why Captured` (existing, capture-note toggle)
2. `> [!tldr]` callout -- one sentence (`cortex/src/quality.rs:304` already accepts it as a summary marker)
3. `## Summary` -- the What-This-Is-About paragraph. Heading stays `## Summary`: `vault/src/search.rs:162`, quality, and oracle all key on it; conformance beats renaming.
4. `## Enumerated Points` -- only when detected; numbered, bold-named, one line each, timestamp anchor per item (improvement over April)
5. `## Key Ideas` -- thematic insights; must not repeat enumerated items (April rule, enforced in prompt)
6. `## Claims` -- existing shape (kind/who/quote/anchor)
7. `## Links` (April's References == Links; keep the heading the parsers know)
8. `## Transcript` -- verbatim kinds only (VoiceNote/Idea/Vocabulary/Image/Thread)

All body parsers scan "until next `## `" so *adding* sections is back-compat; *removing* `## Transcript` affects only the cortex embed loop, which is being re-pointed in the same design (`cortex/src/embed.rs:601-620` already skips-with-sentinel on a missing section).

**Transcript emission is the caller's decision, not a global kind rule.** `render(distilled)` (`render.rs:34`) takes no kind today, and the two callers need opposite behavior:

- borg publish (Stage 3): no transcript section for Video/Article, transcript for the verbatim kinds.
- cortex summarize backfill (`summarize.rs:247-268`): reads the ENTIRE legacy note body as the transcript input and re-renders it. If render silently dropped transcripts for video/article, a backfill run would destroy legacy bodies -- including the April baseline content this design exists to protect. Backfill must keep emitting the transcript section (that is where the legacy body survives).

Seam: `render` grows an explicit options parameter (e.g. `RenderOptions { include_transcript: bool }`). There are SIX production call sites (panel-verified), not two: `borg/src/pipeline.rs:895` (URL: video/article/repo), `borg/src/pipeline/text.rs:139` (text/idea), `text.rs:287` (vocab), `borg/src/pipeline/handlers.rs:770` (image), `handlers.rs:987` (audio), `cortex/src/summarize.rs:277` (backfill). One per-kind policy table (video/article/repo -> false; VoiceNote/Idea/Vocabulary/Image/Thread -> true; cortex backfill -> always true) threaded through all six, with a test per caller. No sniffing `meta.extractor` strings for behavior -- typed values at seams.

### Data Model

`vault/src/distilled.rs` additions, all serde-defaulted so legacy staged `distilled.yml` files deserialize unchanged (house pattern: transcript, kind/who/quote landed the same way):

```rust
pub tldr: Option<String>,            // one-sentence hook, rendered as > [!tldr]
pub enumeration: Option<Enumeration>,
pub key_ideas: Vec<String>,          // default empty; section omitted when empty

pub struct Enumeration {
    pub lead_in: Option<String>,     // "The creator covers 10 essential tools:"
    pub declared_count: Option<u32>, // N from title/intro when stated; drives the eval metric
    pub items: Vec<EnumeratedItem>,
}
pub struct EnumeratedItem {
    pub name: String,                // "Codex Plugin"
    pub text: String,                // one-line description
    pub anchor: Option<String>,      // timestamp/section anchor, same semantics as Claim.anchor
}
```

- Typed fields, not `kind_specific`: `KindPayload` is per-kind and frontmatter-only (`distillers/src/render.rs:48-123`); these sections are cross-kind body content that must round-trip through the vault body parsers.
- No `best_quotes` field: `Claim.quote` already carries verbatim quotes rendered as blockquotes under claim bullets. A standalone Best Quotes section would be a second signal encoding the same meaning. The prompt instructs the distiller to surface the strongest verbatim lines as claims with quotes.
- `Distilled.transcript` doc comment (`distilled.rs:42-56`) rewritten: the "regression-guarded; do not revert to None" language applied to the *field*, which stays; the *render* changes. Guards updated to pin the new truth: field populated, section absent for video/article.

### Config and CLI Surface

- `DistillConfig` (`borg/src/config.rs:238-267`): the `article-transcript` toggle is removed -- with the section gone from render there is nothing left for it to configure, and config that doesn't configure is pointless. No `video-transcript` sibling is added for the same reason. `slide-append`, `capture-note`, `propose-tags` stay. `DistillConfig` gains `#[serde(deny_unknown_fields)]` (it has none today -- verified) so a stale `article-transcript:` key in an existing `borg.yml` fails loud at config load, naming the unknown field.
- `gate_article_transcript` + the source gate (`borg/src/pipeline.rs:158-190`, 2026-07-05-article-transcript-boilerplate.md) survive re-scoped: they now clear `Distilled.transcript` itself when the source is not clean, so chrome junk never reaches the staged `distilled.yml` or embeddings. Ordering constraint for the implementer: today the gate runs in the pipeline (:823-827) AFTER `write_distilled_yml` has already persisted the staged copy inside `distill_for_publish_article` -- the gate must move ahead of the staged write (or run inside the distill stage) or the gating is cosmetic.
- Note-size ceiling: `const MAX_NOTE_BYTES` with config field `pipeline.max-note-bytes` defaulting to it. Checked on the final composed string just before atomic write (`borg/src/pipeline.rs:984-999`). Failure -> existing `IngestStatus::Failed` + `FailureStage::QualityBlocked` (precedent `pipeline.rs:842-854`; no new receipts enum value). With transcripts gone an oversize note is a bug (verbatim leak), so hard-fail is correct: fail loudly, fail closed -- degraded-but-published would land the fat note in the synced vault, which is the exact failure the gate exists to stop. Ceiling value is measured, not guessed (panel condition): Phase 3 renders the largest live slide-heavy note under the new shape and sets the const with clear headroom above it (floor 65_536). Panel verified every current >64KB note is a fat transcript note this design removes. A false positive is recoverable: visible in receipts, bump the config value, `sb borg replay <trace>`.
- Backfill verb: one-shot, housed as `bin/strip-transcripts` beside `bin/migrate-receipts` (its exact precedent) -- NOT a permanent `sb` subcommand; one-shot surgery does not earn a forever spot on the CLI surface. Date-scoped `ingested >= 2026-06-28`, Video+Article only, atomic writes via `vault::note::write_atomic`, prints every touched path. Safety guards (panel-corrected): the **date scope is the protection** for legacy-body notes -- every summarize-backfilled note carries a pre-June `ingested` (backfill rewrites the body, never `ingested`), so the April baseline is out of scope by date. A missing `ingested` key -> refuse the note. The earlier demoted-heading heuristic is dropped: backfill demotes by 2 levels (`summarize.rs:254`), so legacy headings land at `####`, not `######`, and real article transcripts legitimately contain demoted headings -- the heuristic both misses and false-refuses. Strip runs from `## Transcript` to EOF (render emits it last, always -- no "preserve the footer" case exists in the distilled shape). Additional guards: refuse to run on a dirty vault worktree (git is the rollback only when the tree is clean), and the operator stops the borg daemon first so no new notes are minted mid-sweep.

### Prompts and Parse

- `distill-video.md` / `distill-article.md`: harvest the enumeration block from `obsidian-note.md` verbatim where possible -- count/range detection in title+intro+body, "list ALL N items, do not skip, group, or summarize", "do not force one when absent" -- plus new YAML keys `tldr`, `enumeration`, `key_ideas`. Key Ideas carries April's non-overlap rule.
- Chunk -> reduce: `distill-*-chunk.md` emits per-chunk enumerated candidates (items the chunk saw, with anchors); `distill-*-reduce.md` merges candidates, restores creator order, enforces the all-N rule against `declared_count`. `build_reduce_input` (`distillers/src/parse.rs:103-114`) gains an enumerated-candidates section beside `## Chunk Summaries` / `## Claim Pool`.
- Parse structs `PatternYaml` (`parse.rs:254-264`) / `ReduceYaml` (`parse.rs:86-92`) extended with serde-defaulted fields -- pre-change pattern output still parses (fallback safety).
- `distillers/src/validate.rs`: bounds for the new fields (item count cap, per-item length) alongside `MAX_SUMMARY_CHARS`.

### Embeddings Re-point

- `cortex/src/embed.rs::process_transcript_batch` (:580-693): for Video/Article, resolve the transcript via `notes.trace` (a real indexed column since the 2026-06-20 oracle-trace design, `vault/src/search/schema.rs:43-45`) -> `<staging>/<trace>/distilled.yml` -> `transcript` field. Verbatim kinds keep the in-note `read_section_text` path. Missing/expired staged file -> existing examined-sentinel skip (no error loop).
- Staleness invariant (panel finding, recorded): both the stale-source predicate and sentinel re-selection key on the NOTE's `modified_at` (`embed.rs:632`, `vector.rs:620-623`), never on the staged file. The staged `distilled.yml` is treated as immutable after publish, so this is correct-by-construction for the normal path. Consequence: a sentinel skip (staged file missing at embed time) is final until the note is modified or the model version bumps. Recovery procedure when a staged file appears later: touch/modify the note (bumps `modified_at` -> reselected next pass). Documented here so nobody debugs it as a mystery.
- Read-only cross-ownership is precedented: oracle reads borg's receipts DB read-only for `failure_history`. Cortex stays the sole embeddings *writer*; borg stays the sole staging *writer*. CLAUDE.md one-way-data-flow paragraph gets the new read edge recorded.
- This deliberately crosses the 2026-06-20-oracle-trace-availability.md non-goal "no embedding of staged-source content" -- superseded here, on purpose: the alternative was verbatim text in the vault, which is the exact thing being removed.
- `NoteType::transcript_eligible()` keeps Article+Youtube: the SQL staleness filter (`vault/src/search/vector.rs:558+`) still selects them; only the text source moves.
- Post-60-day custody: staged transcripts are swept by `sb borg retention sweep` (`borg/src/retention.rs:55-103`, whole-trace-dir delete, operator-invoked). Existing TranscriptChunk rows survive expiry; a future model-bump re-embed cannot regenerate them. Decision: accepted -- see Resolved Decisions.

### Eval

- Fixtures: April baseline notes join `config/eval/distill-fixtures/` (transcript source = the recovered legacy content; expected `distilled.yml` = the April sections mapped onto the new fields, `declared_count` set).
- Two new deterministic metrics in `borg/src/eval/calc.rs` (zero new judge calls):
  - **listicle-survival**: `items.len() == declared_count` per enumeration fixture; partial credit `items/declared`.
  - **note-size**: render the fixture's Distilled, assert bytes < ceiling.
- Judge axes (claim-coverage, anchor-validity, summary-faithfulness) unchanged; re-run recorded as the new baseline (prior: composite 1.952, video 0.467).

**Implemented baseline (2026-07-07).** Deterministic metrics wired into `sb borg eval` (Phase 7 + 7b), computed over the 22 committed fixtures with zero new judge calls:
- listicle-survival: **1.000** over 1 applicable fixture (`video/top-10-claude-code-skills-plugins-clis-april-2026`); all other fixtures are N/A (no expected `declared_count`) and excluded from the aggregate, not scored 0.
- note-size: **22/22** fixtures within the 65,536-byte ceiling.
- Break-the-code check confirmed: a listicle fixture that loses its enumeration items scores 0.0 and drags the aggregate.
- The composite **judge** re-run (the `1.952 / 0.467` successor) is an operator live-fabric step, run after the post-Phase-5 `otto deploy`; not yet recorded.

### Implementation Plan

Ship order: single repo (`second-brain`), no cross-repo blast radius. Phases land in order with ONE daemon-host deploy after Phase 5 -- not earlier. Deploying after Phase 3 alone would mint notes with no `## Transcript` while the embed loop still reads the note body: those notes get marked examined-with-sentinel and would never receive transcript embeddings even after Phase 5 lands (the staleness filter would not re-select them). The Phase 6 sweep runs after that deploy, so its date scope also covers any fat notes minted while Phases 1-5 were in flight.

#### Phase 0: Prove the enumeration prompt on the April baseline
**Model:** fable
- Zero product code. Draft the enumeration-aware `distill-video` pattern; run it via the fabric CLI against the recovered April "Top 10" transcript source, both single-call and simulated chunk -> reduce.
- **Success criteria:** reduce-output YAML parses via serde_yaml and contains all 10 items in creator order; a non-listicle transcript (Herdr video) yields `enumeration: null` -- no forced enumeration.

#### Phase 1: Fix parse_vtt_segments
**Model:** sonnet
- Regex-strip `</?c[^>]*>` and `<\d{2}:\d{2}:\d{2}\.\d{3}>`; port the `clean_vtt` rolling-overlap collapse (`youtube.rs:552-567`: extends -> replace, covered-by -> skip, dup -> skip). Fixture from a real rolling auto-caption VTT.
- **Success criteria:** no `<c` or `<HH:MM:SS.mmm>` substring in any parsed segment; the rolling fixture yields each spoken line exactly once; existing `youtube/tests.rs` green.

#### Phase 2: Distilled contract + render
**Model:** opus
- Add `tldr`/`enumeration`/`key_ideas` (serde-defaulted); render the new section order; add `RenderOptions { include_transcript }` and thread it through ALL SIX call sites per the policy table (see Architecture); extend the round-trip tests (`render/tests.rs:269,389`) to every new section; update the Phase-B2 regression guards and the `distilled.rs` doc comment to pin the new truth.
- **Success criteria:** every pre-change `distilled.yml` fixture deserializes unchanged; rendered video/article publish body contains no `## Transcript` while `Distilled.transcript` is `Some`; a backfill-context render of the same Distilled DOES contain it (named test); one test per render call site asserts its policy; round-trip test covers tldr/enumeration/key-ideas.

#### Phase 3: Publish-path wiring + size gate
**Model:** sonnet
- Video/article publish drops the transcript from the note while `write_distilled_yml` keeps staging intact; remove the `article-transcript` toggle and add `#[serde(deny_unknown_fields)]` to `DistillConfig`; re-scope the source gate to staging; measure the largest live slide-heavy note under the new shape, then set `MAX_NOTE_BYTES` with headroom (floor 65_536) + config field + `QualityBlocked` hard-fail before the atomic write; slide-append path inherits the transcript-free render automatically.
- **Success criteria:** ingest of a long video produces a note under the ceiling with the staged `distilled.yml` transcript intact; an artificially oversize composed note lands `failure_stage=quality-blocked` in receipts; a config fixture containing `article-transcript:` fails deserialization with an error naming the unknown field.

#### Phase 4: Prompts + parse (enumeration through map-reduce)
**Model:** opus
- Land the Phase-0-proven patterns for video+article+repo (single, chunk, reduce); extend `PatternYaml`/`ReduceYaml`/`build_reduce_input`/`select_reduce_claims` seams; validate.rs bounds for the new fields; enumeration-shortfall WARN + degraded receipt.
- **Success criteria:** chunked April-fixture transcript survives reduce with all N items in order; pre-change pattern output (no new keys) still parses; enumeration items carry anchors that pass the anchor-honesty rule; a forced shortfall (declared 10, returned 7) publishes with `degraded=true` in receipts.

#### Phase 5: Embeddings re-point
**Model:** opus
- `process_transcript_batch` resolves Video/Article transcript via `notes.trace` -> staged `distilled.yml`; verbatim kinds unchanged; missing file -> examined sentinel; CLAUDE.md invariant paragraph updated.
- **Success criteria:** fresh video ingest yields TranscriptChunk rows with no `## Transcript` in the note; deleting the trace dir then re-running embed degrades to sentinel-skip with a WARN, no error loop; verbatim-kind embedding unchanged (existing tests green).

#### Phase 6: Backfill strip sweep
**Model:** sonnet
- One-shot `bin/strip-transcripts`: remove `## Transcript`-to-EOF from Video+Article notes `ingested >= 2026-06-28`; refuse notes missing `ingested`; refuse on a dirty vault worktree; atomic writes; print a manifest of every candidate with its disposition (stripped | refused + reason).
- **Success criteria:** run manifest reports N candidates / M stripped / K refused, recounted at run time (37 candidates at research time); an integration test feeds an April-shape fixture (pre-06-28 `ingested`) and asserts it is out of scope untouched; stripped notes retain all other sections byte-identically.

#### Phase 7: Eval extension + new baseline
**Model:** sonnet
- April fixtures added; listicle-survival + note-size metrics in `calc.rs`; baseline re-run recorded in the doc.
- **Success criteria:** listicle metric scores 0 against the current shipped video fixture and full marks against the April-shape artifact; re-run makes zero new judge calls on unchanged fixtures (cache intact).

Operator steps (not phase-agent work): ONE `otto deploy` + daemon restart, after Phase 5 (see ship-order note above -- never after Phase 3 alone); stop the borg daemon, run the Phase 6 sweep once on the daemon host (Syncthing propagates), restart; probe one live video + one article ingest end-to-end and read the resulting notes.

## Acceptance Criteria

- [ ] Fresh ingest of a "Top N" YouTube video publishes a note with `## Enumerated Points` carrying all N items (anchored), a tldr callout, `## Key Ideas`, and no `## Transcript`, under the size ceiling; a fresh article note is likewise transcript-free; both staged `distilled.yml` files carry the full clean transcript.
- [ ] TranscriptChunk embeddings exist for the fresh video note sourced from staging (rows in `note_embeddings` for its note id while the note body has no transcript section).
- [ ] The backfill strips exactly the post-2026-06-28 video/article transcript sections; the April baseline note (`top-10-claude-code-skills-plugins-clis-april-2026.md`) is byte-identical after the run.
- [ ] `sb borg eval` reports listicle-survival = 1.0 on April fixtures, enforces the note-size metric, and the new baseline is recorded in this doc.
- [ ] Named tests prove `parse_vtt_segments` emits no timing/classed tags and no rolling duplicates.

## Resolved Decisions

- **2026-07-07 -- transcripts leave video/article note bodies; field and staging stay.** Owner directive. Supersedes the Phase-B2 "do not revert" guard (which now pins the field, not the section) and 2026-07-05 Phase 7.
- **2026-07-07 -- post-60-day custody: accept re-embed loss.** Existing TranscriptChunk rows survive staging expiry; only a future model-bump re-embed loses expired transcripts, and the vault keeps the extracted knowledge either way. A durable transcript store is unrequested scope today; revisit condition: the first model bump that forces a full re-embed.
- **2026-07-07 -- no standalone Best Quotes section.** `Claim.quote` already renders verbatim quotes under claims; a second section encoding the same meaning violates the derived-field rule. Prompt directs strong quotes into claims.
- **2026-07-07 -- Enumerated Points ride body FTS only.** No parse-back into `notes.claims`; the claims column stays 1:1 with `## Claims`. Items are FTS-searchable via the body column.
- **2026-07-07 -- `article-transcript` toggle removed, no video sibling.** Nothing left to configure once the section is gone from render; config that doesn't configure is pointless.
- **2026-07-07 -- tldr callout returns.** April shape; `quality.rs:304` already accepts it.
- **2026-07-07 -- backfill scope is date-scoped (>= 2026-06-28).** The 366 pre-06-28 transcript-bearing video notes are a mix of legacy-body backfill artifacts and May-June real-VTT notes; touching them needs content detection and buys little (most are slide-masked and small). Revisit with the future re-distill verb.
- **2026-07-07 -- BM25/FTS keyword reach over transcripts is an accepted loss.** With the transcript out of the body, `mode=bm25` can no longer keyword-match transcript text for video/article. Semantic reach survives (TranscriptChunk embeddings, re-pointed), the shipped default pipeline is vector-first with BM25 demoted (weight 0.3), and the vault holding extracted knowledge -- not verbatim -- is the point of this design. Exact-wording lookups go through the trace ref, which is what it is for.
- **2026-07-07 -- repos get the new fields too.** The original ask named blogs, repos, AND videos; an awesome-list README is a listicle. `distill-repo.md` gains `tldr`/`enumeration`/`key_ideas` in Phase 4. Repos remain transcript-free (unchanged since 2026-07-05).
- **2026-07-07 -- enumeration shortfall does not block publish.** If `items.len() < declared_count`, publish with a WARN and mark the receipt degraded (the existing silent-quality channel; surfaces via `sb doctor` degraded_24h). Hard-failing on LLM variance would block ingestion on the flakiest component. Wiring note (panel-verified): `degraded` today derives solely from `fallback_reason.is_some()` (`pipeline.rs:1094`); a shortfall is NOT a fallback, so Phase 4 adds an explicit degradation signal into the ingest result rather than piggybacking.
- **2026-07-07 -- staged distilled.yml is immutable after publish; sentinel skips are final until note-modification or model bump.** Panel finding 4. Recovery for a late-appearing staged file: modify the note to bump `modified_at`. See Embeddings Re-point.
- **2026-07-07 -- size ceiling is measured, not guessed; hard-fail stands.** Panel asked for measure-or-soften; measurement accepted (Phase 3 measures the largest slide-heavy render, floor 65_536), softening rejected: WARN+degraded still publishes the fat note into the synced vault, defeating the gate's purpose. False positives are recoverable (config bump + `sb borg replay`).
- **2026-07-07 -- backfill protection is the date scope, not a heading heuristic.** Panel finding 3: backfill demotes 2 levels so legacy headings land at `####` not `######`, and real article transcripts contain demoted headings -- the heuristic both misses and false-refuses. Every summarize-backfilled note carries a pre-June `ingested`; date scope + missing-`ingested` refusal + strip-to-EOF replace the heuristic. Panel-verified closure (consensus round): 316 legacy-body notes live, max `ingested` 2026-05-15, zero >= 2026-06-28, none field-less; `summarize.rs:312` clones `ingested` unchanged and the only `ingested`-bumping path is a fresh reingest (`pipeline.rs:1010`) which produces a real transcript, never a legacy body -- the hazardous combination is unreachable by construction.
- **2026-07-07 -- consensus round closed with zero open pushbacks.** All 7 panel findings folded or dispositioned; both author deviations (date-scope guard, hard-fail size gate) accepted by the panel with rationale on record. Nothing escalated.

## Alternatives Considered

### Alternative 1: Full revert to the obsidian-note.md fabric path
- **Description:** put `summarize_pattern_youtube` back in the publish path; abandon the structured distillers.
- **Pros:** provably produced the shape the owner wants; smallest prompt work.
- **Cons:** loses map-reduce (long videos truncate again), claim anchors, the typed contract, staged distilled.yml, eval harness leverage; regressions are fixed forward, not by reverting to a superseded design.
- **Why not chosen:** owner directive: technical improvements remain, applied to the restored output.

### Alternative 2: Carry the new sections in kind_specific
- **Description:** stuff enumeration/key-ideas into the existing `KindPayload` map.
- **Pros:** no contract change.
- **Cons:** `KindPayload` is per-kind and frontmatter-only; these are cross-kind body sections needing render + parse-back round-trip; stringly-typed payloads fight the schema-is-law invariant.
- **Why not chosen:** typed fields are the house pattern for contract evolution.

### Alternative 3: Keep in-note transcripts behind a default-off toggle
- **Description:** add `video-transcript: false` default and let operators opt back in.
- **Pros:** softer landing; no backfill urgency.
- **Cons:** the owner has ruled the content out of the vault; a toggle keeps the leak path alive and the note-size gate ambiguous (is oversize a bug or a setting?).
- **Why not chosen:** decision is settled; config selects methodology, not whether a ruled-out behavior can return.

### Alternative 4: Durable transcript store outside staging (custody option b)
- **Description:** copy transcripts to a borg-owned unswept dir keyed by trace, so re-embeds never lose them.
- **Pros:** model bumps re-embed everything forever; cheap (text).
- **Cons:** second copy of every transcript; new retention surface; unrequested scope.
- **Why not chosen (parked):** accepted-loss decision above; revisit at the first forced full re-embed.

## Technical Considerations

### Dependencies
- No new crates (regex already a borg dependency, `youtube.rs:21`). All work in-workspace.

### Performance
- Fewer bytes rendered, indexed, and synced per note (40-240KB -> ~5-20KB). Embed loop adds one YAML read per video/article note per staleness pass; staged files are local disk.

### Security
- None new. Staged transcripts already exist on disk; nothing crosses a trust boundary. Cortex gains read-only access to borg-owned files, precedented by oracle's read-only receipts access.

### Testing Strategy
- Unit: parse_vtt fixtures (tags, rolling overlap), render round-trip for every new section, serde back-compat on legacy distilled.yml, reduce enumeration-merge, size-gate boundary.
- Integration: end-to-end ingest fixture asserting note shape + staged transcript + receipts status; embed loop against a temp staging tree (present/missing/expired).
- Eval: April fixtures with deterministic metrics; break-the-code check -- re-point the metric at the current shipped fixture and watch it score 0 (proves the test bites).

### Rollout Plan
- Phases 1-5 ship as normal commits (otto ci green each); ONE deploy to the daemon host, after Phase 5; Phase 6 sweep runs once on the daemon host (daemon stopped); Phase 7 records the new eval baseline. Live verification: one video (a listicle), one article, read both notes. If a mid-flight deploy ever becomes unavoidable, the repair for the sentinel gap is clearing `embedding_examined` marks for the affected video/article rows -- do not deploy early without it.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| LLM fails the all-N rule on long chunked videos | Med | Med | Phase 0 proves the prompt before any code; reduce step gets explicit candidates + declared_count; eval metric catches drift |
| Backfill strips a legacy-body note | Low | High | date scope (backfilled notes are pre-June by `ingested`) + missing-`ingested` refusal + clean-worktree requirement + git-versioned vault |
| Embed loop churns on expired traces | Med | Low | examined-sentinel skip (existing mechanism), WARN once |
| Stale `article-transcript` key breaks daemon start | Med | Low | loud config error naming the key; operator fixes borg.yml once |
| Size ceiling false-positives on legitimately huge enumerations | Low | Med | ceiling is config-tunable; validate.rs bounds cap section sizes first |

## Open Questions

*(empty -- all decisions closed above; panel findings land here if they reopen one)*

## References

- Baseline example: `~/repos/scottidler/obsidian/notes/top-10-claude-code-skills-plugins-clis-april-2026.md`
- Legacy pattern: `borg/patterns/obsidian-note.md` (enumeration rules to harvest)
- Superseded/amended: `docs/design/2026-07-05-distillation-knowledge-extraction.md` (Phase 7), `docs/design/2026-07-05-article-transcript-boilerplate.md` (gate re-scoped), `docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md` Phase B2 (in-note video transcript), `docs/design/2026-06-20-oracle-trace-availability.md` non-goal (staged-source embedding, deliberately crossed)
- Prior eval baseline: composite 1.952, video 0.467 (commit f1572c5 harness)
