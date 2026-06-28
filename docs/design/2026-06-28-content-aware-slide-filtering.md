# Design Document: Content-Aware Slide Filtering

**Author:** Scott Idler
**Date:** 2026-06-28
**Status:** Implemented
**Review Passes Completed:** 5/5 + 2 revisions (empirical validation against the 287 captured frames; cross-model review-panel pass that caught and fixed the classify/best-frame ordering bug)

**Builds on:** [2026-04-29-frame-aware-youtube-ingestion.md](2026-04-29-frame-aware-youtube-ingestion.md) (the slide pipeline this filters) and [2026-04-19-staged-ingestion-pipeline.md](2026-04-19-staged-ingestion-pipeline.md) (trace-id artifact foundation).

## Summary

The frame-aware YouTube pipeline embeds extracted video frames ("slides") into Obsidian notes, but selects them by structure alone (frame counts, unique-slide ratio) with zero understanding of what is on the slide. The result was visual noise: 208 notes carried 287 embedded frames, the majority of which were talking-head shots, b-roll, webpages, app UIs, and title cards rather than anything worth keeping. This design adds an inline per-slide **vision classification** step that assigns each unique slide a category from a fixed taxonomy and embeds only the categories named in a config `keep` array (default: `architecture-diagram` only), plus a **best-frame capture** step so the embedded image is the most-complete rendering of a diagram rather than the first (least-complete) frame the clustering happens to pick. The feature is currently disabled in production (`youtube.slides.enabled: false`) pending this filter.

## Problem Statement

### Background

borg's frame-aware YouTube pipeline (shipped v0.5.32, 2026-04-29) runs four stages on every YouTube ingest:

- **Stage 0** (`youtube.rs::extract_frames:386`) - ffmpeg `fps -> mpdecimate -> scale` extracts budget-bounded JPEGs (`frame_NNNN.jpg` + a `frames.yml` sidecar with per-frame timestamps).
- **Stage 1** (`slides.rs::segment_with_pairs:341`) - `cluster_frames:120` collapses near-duplicate frames into unique-slide clusters by perceptual hash, `drop_transitions:167` removes transition artifacts, `materialize_slides:312` copies each cluster's canonical frame to `slide-NNN.jpg`, `ocr_slides:291` runs Tesseract OCR, transcript segments are bound per slide.
- **Stage 2** (`pipeline/handlers.rs:333`) - a Fabric pattern (`obsidian-youtube-slides.md`) receives the slide manifest as **text** and returns a summary plus an `embed_slides` list naming which slides to publish.
- **Stage 3** (`slides/publish.rs::publish_slides:44`) - copies the selected slides into `system/attachments/images/<YYYY-MM>/` and inserts `![[...]]` wikilink embeds.

The note-shape decision (embed nothing, one hero image, or a section-by-section set) is made by `propose_note_shape:179` from `unique_slides`, `frames_after_mpdecimate`, and their ratio against `SlideThresholds`.

### Problem

**Selection has no idea what is on a slide, the LLM that picks `embed_slides` never sees a pixel, and even when a slide is worth keeping the published frame is the wrong one.**

Verified causes:

1. **Note-shape is frame-count arithmetic only.** `propose_note_shape:179` keys entirely off counts and a ratio. A meme deck and an architecture deck with the same compression ratio get the same shape.
2. **The "vision" pattern is text-only.** `fabric::run_pattern` (`vault/src/fabric.rs:55`) pipes only text on stdin. `render_pattern_input:467` writes `![]({frame_path})` markdown links into that text, but they are dead references the LLM cannot fetch. The LLM picks `embed_slides` from **OCR text + transcript only**, so a code screenshot with sparse text and a talking-head frame are indistinguishable.
3. **`vision_per_slide` was never wired.** Declared at `config.rs:442`, defaulted `false`, read nowhere. `Slide.caption` (`slides.rs:51`) is always `None`. (The 2026-04-29 doc's claim that vision is "wired but disabled" is incorrect.)
4. **The published frame is the first-seen, least-complete one.** `cluster_frames:120` sets each cluster's `canonical` to `frames[0]` and `materialize_slides:312` copies that. For a diagram that is *drawn live* on screen (common with the YouTubers this targets), the first frame is a near-blank canvas; worse, as the diagram grows it drifts past the pHash threshold and **fragments into several clusters**, each canonical = an incomplete stage. So even a perfect classifier would embed half-drawn diagrams.

**Empirical confirmation.** A stratified sample of 18 of the 287 captured frames (spanning all three months), eyeballed directly: ~10 were pure noise (presenter-at-podium, talking-head studio shots, two-person podcast sets, hands-on-hardware b-roll, blog webpages, the Gemini slides-app UI, a half-rendered title-card animation), ~4 were genuine diagrams, ~4 were code/terminal screens. Five of the technical frames had a **picture-in-picture webcam inset** - the presenter's face in a corner over a diagram or webpage. This is the real distribution the filter must handle.

### Goals

- **Classify every unique slide** into one category from a fixed taxonomy, with a confidence score, using a vision model.
- **Embed only categories named in a config `keep` array.** Default `keep: [architecture-diagram]`; the array accepts one or more taxonomy strings so the keep-set is widened (e.g. add `code`, `sequence-diagram`) by config edit, no code change.
- **Classify by dominant content region**, so a diagram-with-webcam-inset is kept and a webpage-with-webcam-inset is dropped. (Picture-in-picture is the norm, not an edge case.)
- **Publish the most-complete frame** of a kept slide: re-select the best frame within the cluster's time window, and merge a progressive-drawing run into one slide keyed on its terminal/most-complete frame.
- **Reuse the existing vision seam** (`ocr::vision_extract:75`, Anthropic Messages API).
- **Stay within the ingestion budget** (5-10 min): one classification call per unique slide (typically 5-20), bounded concurrency.
- **Degrade gracefully:** any classification failure drops that slide (fail-closed); the note still publishes its text/transcript shape.

### Non-Goals

- **Replacing the structural shape heuristic wholesale.** Classification gates the *candidate set*; shape then follows kept count.
- **The deferred cortex-sweep approach** (documented as an alternative; strictly more work, not chosen).
- **Persisting frames for replay.** The slide work dir is deleted at publish (`pipeline.rs:679`). Best-frame selection happens *during* ingest while frames still exist on disk; cross-ingest replay is a separate concern.
- **Moving selection into Fabric** (it cannot carry images).
- **A user-editable taxonomy.** The category *vocabulary* is fixed in code (the model needs a closed label set); only which categories to `keep` is config.
- **Region-cropping the diagram out of a screen-share.** Vision tolerates surrounding chrome; cropping is a possible later refinement, not in scope.

## Proposed Solution

### Overview

**Ordering is load-bearing and was wrong in the first draft.** Today's Stage 1 (`segment`/`segment_with_pairs`) is *not* pure: it already runs `cluster_frames -> drop_transitions -> materialize_slides` (which copies the first-seen canonical `frames[0]` to `slide-NNN.jpg`) `-> ocr_slides`, all at `slides.rs:417-426`, before the orchestrator sees the manifest. Classifying that manifest means classifying the *first-seen* frame - for a live-drawn diagram, a near-blank canvas - and `drop_transitions` would already have discarded short diagram fragments. Best-frame selection and classification must therefore run **before** transition-drop, materialize, and OCR, not after.

The corrected pipeline, when the filter is enabled, splits Stage 1 so the orchestrator drives selection on the *most-complete* frame:

```
slides.rs (pure, no I/O, no network):
  clusters = cluster_frames(frames, hashes, threshold, dur)        // raw, pre-transition-drop
  runs     = collapse_runs(clusters, frames)                       // structural: stitch progressive-drawing
                                                                    //   runs (monotonic pHash growth / small gap)
  best[run] = best_frame(run.start, run.end, frames)               // most-complete frame in each run's window

handlers.rs::try_extract_slides (async, holds `config`):
  classes = classify_slides(best[], &config.content_filter, &config.llm)   // NEW slides/classify.rs
              one vision call per RUN (not per raw cluster -> bounded), by dominant region,
              process-wide concurrency cap
  kept    = runs where class.category ∈ config.keep && class.confidence >= min-confidence
  // transition-drop is now subsumed: a true transition is a short run that classifies
  // title-card/other and is dropped by the keep-filter; no separate drop_transitions pass.
  materialize kept best-frames -> slide-NNN.jpg ; ocr_slides(kept best-frames)   // OCR matches the embedded image
  shape   = shape_from_kept_count(kept.len())                      // NEW pure fn
  if shape == TextOnly: bail to prose-only (existing handlers.rs:323)
  else render_pattern_input(kept) -> Stage 2 Fabric -> embed_slides ⊆ kept -> Stage 3 publish (unchanged)
```

`collapse_runs` is **structural** (monotonic-growth / small-gap merge of adjacent clusters), so it runs *before* classification - this both stitches a live-drawn diagram back into one slide and bounds the vision-call count to the number of distinct content runs, not the number of growth fragments. When the filter is disabled, `segment` runs exactly as today (classify/best-frame/collapse skipped).

### Architecture

**New module `borg/src/slides/classify.rs`** (`slides.rs` is 629 lines; per `rust.md` a new single-word submodule beats growing it).

- `classify_slide(frame, filter, llm) -> Result<SlideClass>` - reads JPEG bytes + mime (`ocr::mime_from_extension:196`), calls the new `ocr::vision_classify` sibling with a taxonomy prompt, parses `CATEGORY`/`CONFIDENCE`.
- `classify_slides(best_frames, filter, llm) -> Vec<Result<SlideClass>>` - capped-parallel fan-out mirroring `ocr_slides:291` but async, **one call per run** (post `collapse_runs`). Concurrency bounded by a **process-wide** vision permit pool (below), not a per-video semaphore.

**Pure helpers in `slides.rs`** (no I/O, no network, unit-testable): `collapse_runs(clusters, frames)`, `best_frame(start, end, frames)`, `shape_from_kept_count(kept)`.

**Restructuring Stage 1.** Today `segment`/`segment_with_pairs:341` does cluster -> transition-drop -> materialize -> OCR -> `propose_note_shape` inline (`:417-426`); it is *side-effectful* (materialize copies `frames[0]` to disk, OCR shells out). The filter path must not feed off that. The fix factors the **pure prefix** (`cluster_frames` + the new `collapse_runs`/`best_frame`) out so `try_extract_slides` can interleave classification before the **effectful suffix** (materialize the *chosen* frames, OCR them, shape). Concretely, when the filter is enabled `try_extract_slides:216` calls the pure prefix, classifies the per-run best frames, keep-filters, then materializes + OCRs only the kept best-frames - so the embedded image and its OCR text are the same frame. When disabled, the existing `segment_with_pairs` path runs unchanged.

**Capture stage (best-frame).** `cluster_frames:120` retains only `canonical = frames[0]` and a `[start,end]` range, but the caller still holds the full `frames: Vec<FrameRef>` (timestamps in `frames.yml`), so `best_frame` filters that slice by the run's `[start,end]` window - **no change to the `Cluster` type needed** (it is an internal intermediate). Within the window it re-selects the most-complete frame: largest JPEG byte size (more ink/detail at fixed encode quality), falling back to the last frame. `collapse_runs` merges temporally adjacent clusters whose content monotonically grows (a small inter-cluster gap + pHash superset), stitching a live-drawn diagram into one run *before* classification - this is structural, needs no category, and bounds the vision-call count.

**Transition-drop.** The legacy `drop_transitions:167` (drops clusters shorter than `transition_min_seconds=5`) is **not run on the filter path**: a genuine transition is a short run that classifies `title-card`/`other` and is dropped by the keep-filter, while a briefly-shown diagram fragment that the old rule would have wrongly discarded now survives because `collapse_runs` stitches it into its run. This avoids both the false-drop and the redundant pass.

**Shape (`shape_from_kept_count`, NEW pure fn in `slides.rs`):** the filter bypasses `propose_note_shape:179` (its `min_unique_slides` suppressor was a structural *proxy* for "is this noise," now answered directly). Because `Hero` embeds exactly one image (`publish.rs:100`) and `SlideSection` embeds all:
- `0` kept -> `TextOnly`
- `1` kept -> `Hero`
- `>= 2` kept -> `SlideSection`

So a talk with a single genuine architecture diagram embeds it, instead of being suppressed under the legacy `min_unique_slides = 4`. When disabled, `propose_note_shape`'s ratio logic is untouched.

**Stage 2/3:** `render_pattern_input:467` is given only kept slides. `enforce_shape:588` remains the downgrade-only Stage 3 safety net. Publish/cleanup unchanged.

### Data Model

```rust
// borg/src/config.rs - the taxonomy is config-owned (config.rs already has no
// dependency on slides/; putting it here avoids config.rs importing classifier
// types, which would be backwards layering). classify.rs and slides.rs import it.
// Deserialized from kebab-case strings with case-insensitive aliases (serde
// has no `ignore_case`; a small custom Deserialize or per-variant #[serde(alias)]
// gives the case-insensitive behavior cli.md expects).
pub enum SlideCategory {
    ArchitectureDiagram,  // components/systems wired by arrows, data flow
    SequenceDiagram,      // UML/interaction sequence
    Flowchart,            // flow / decision diagram
    Code,                 // source code on screen
    Terminal,             // terminal / TUI / CLI output
    Infographic,          // framework / maturity-model / quadrant slide (NOT a real diagram)
    Chart,                // bar / line / scatter / plot
    AppUi,                // application GUI screenshot
    Webpage,              // browser / blog / docs page
    TalkingHead,          // person(s) on camera
    BRoll,                // physical objects, hardware, scenery
    TitleCard,            // title / intro / transition frame
    Other,
}

pub struct SlideClass {
    pub category: SlideCategory,
    pub confidence: f32,  // 0.0..=1.0
}
```

`Slide` (`slides.rs:41`) gains `pub class: Option<SlideClass>`, replacing the dead `caption: Option<String>` (`:51`); both are `skip_serializing_if`, and `caption` was always `None`, so dropping it is schema-neutral for `slides.yml`. **`Cluster` (`slides.rs:58`) is left unchanged** - it is an internal intermediate, and `best_frame` re-selects from the caller's `frames: &[FrameRef]` slice (which carries timestamps) filtered by the run's `[start,end]` window, so there is no need to thread member-frame refs through the `Cluster` type.

### API Design

```rust
// borg/src/ocr.rs - new sibling factored from vision_extract's shared HTTP/auth/base64 core.
// A process-wide vision permit pool (OnceCell<Semaphore>) gates ALL vision calls
// (this classifier + the existing image-ingest vision_extract path) so total
// in-flight Anthropic calls are capped across concurrent videos, not per-video.
pub async fn vision_classify(
    image_data: &[u8],
    mime_type: &str,
    vision_config: &VisionConfig,   // model override; built from content-filter.model
    llm_config: &LlmConfig,         // api-key + default model
) -> Result<String>;                // raw model text; classify.rs parses CATEGORY/CONFIDENCE

// borg/src/slides/classify.rs
pub async fn classify_slide(frame: &Path, filter: &ContentFilterConfig, llm: &LlmConfig) -> Result<SlideClass>;
pub async fn classify_slides(best_frames: &[PathBuf], filter: &ContentFilterConfig, llm: &LlmConfig) -> Vec<Result<SlideClass>>;

// borg/src/slides.rs (pure)
pub fn collapse_runs(clusters: &[Cluster], frames: &[FrameRef]) -> Vec<Run>;     // structural, pre-classification
pub fn best_frame(start: f64, end: f64, frames: &[FrameRef]) -> Option<PathBuf>; // max-jpeg-size, fallback last
pub fn shape_from_kept_count(kept: usize) -> NoteShape;                          // 0->TextOnly,1->Hero,>=2->SlideSection
```

Config additions to `YoutubeSlidesConfig` (`config.rs:420`, serde kebab-case, Defaults at `:447`):

```yaml
youtube:
  slides:
    enabled: true
    content-filter:
      enabled: true
      keep: [architecture-diagram]   # taxonomy categories to embed; add more strings to widen
      model: ""                      # empty -> inherit LlmConfig.model
      max-vision-concurrency: 4      # PROCESS-WIDE cap on in-flight vision calls (all videos + image ingest)
      min-confidence: 0.6            # keep a run only at/above this confidence
```

`keep` is a `Vec<SlideCategory>` deserialized from kebab-case strings; **each entry is validated against the taxonomy at config load - an unknown string is a hard parse error**, not a silent no-op. `max-vision-concurrency` sizes the **process-wide** permit pool, so 4 concurrent videos cannot multiply into 16+ simultaneous Anthropic calls (it bounds the existing image-ingest vision path too). The dead `vision_per_slide` stub is removed and folded into `content-filter.enabled`. Shape follows kept count directly, so no `hero-max` knob is needed.

### Observability

Per `borg/AGENTS.md` function-level logging and the `degraded_24h` philosophy, an all-noise video and an Anthropic outage must not both silently read as "zero kept slides." `try_extract_slides` logs, per ingest, a structured tally: `{classified, kept-by-category, dropped-low-confidence, dropped-not-in-keep, dropped-api-error, dropped-parse-error}`. A nonzero `dropped-api-error` count is the operator signal that filtering degraded (publishing fewer images than the content warranted), distinct from a legitimately image-free note.

### Implementation Plan

Phases are sequenced so the data-flow order is honored: capture (best-frame/collapse) is built and tested **before** classification consumes it, and the orchestrator wiring lands last.

#### Phase 1: Config surface + taxonomy
**Model:** sonnet
- Add `SlideCategory` to `config.rs` (config-owned), with case-insensitive kebab-case deserialization via a custom `Deserialize` or per-variant `#[serde(alias)]` (serde has no `ignore_case`).
- Add the `content-filter` block; `keep: Vec<SlideCategory>` with taxonomy validation (unknown string -> hard error); `max-vision-concurrency`, `min-confidence`, `model`, `enabled`. Remove the `vision_per_slide` stub.
- Mirror into `config/templates/borg.yml` and the live `~/.config/sb/borg.yml` docs.
- Tests: round-trip defaults + overrides; mixed-case `keep` entries parse; unknown string errors.

#### Phase 2: Capture stage (pure, no network)
**Model:** opus
- `collapse_runs(clusters, frames)` - structural monotonic-growth/small-gap merge of adjacent clusters into runs (no `Cluster` change; reads the caller's `frames` slice).
- `best_frame(start, end, frames)` - max-JPEG-size, fallback last-in-window.
- `shape_from_kept_count`.
- Tests: synthetic growing-diagram fixture -> run stitched, terminal frame chosen; gap beyond threshold -> two runs.

#### Phase 3: Vision classifier
**Model:** opus
- Factor `ocr::vision_extract:75` to share its HTTP/auth/base64 core; add `ocr::vision_classify(prompt)` plus the **process-wide** vision permit pool (`OnceCell<Semaphore>` sized by `max-vision-concurrency`) gating both classify and the existing image-ingest path.
- `slides/classify.rs`: `SlideClass`, `classify_slide`, `classify_slides` (one call per run), the taxonomy prompt, and the `CATEGORY`/`CONFIDENCE` parser.
- Prompt: classify by **dominant content region** (ignore small webcam insets); sharp line between `architecture-diagram`/`sequence-diagram`/`flowchart` and `infographic`/`chart`; force the structured response.
- Tests: parser on well-formed/malformed/ambiguous responses (fail-closed -> `Err` -> run dropped).

#### Phase 4: Orchestration + shape gate
**Model:** opus
- In `handlers.rs::try_extract_slides:216`, filter-enabled path: pure prefix (`cluster_frames` -> `collapse_runs` -> per-run `best_frame`) -> `classify_slides` on the best frames -> keep-filter by `keep`/`min-confidence` -> materialize + OCR the kept best-frames -> `shape_from_kept_count` -> existing `TextOnly` bail at `:323`. Skip `drop_transitions` on this path. Restrict `render_pattern_input:467` to kept runs. Emit the observability tally.
- Filter-disabled path: `segment_with_pairs` unchanged.

#### Phase 5: Tests + re-enable
**Model:** sonnet
- End-to-end tests mirroring `slides/tests.rs` (fixture JPEGs, mocked classifier): all-noise -> TextOnly; one diagram -> Hero; many diagrams -> SlideSection; classifier error -> dropped; `keep` widened to `[architecture-diagram, code]` keeps a code frame.
- Flip `youtube.slides.enabled: true` + `content-filter.enabled: true` once `otto ci` is green; validate on a known diagram-talk and a known talking-head video.

## Alternatives Considered

### Alternative 1: Deferred cortex sweep over staged frames
- **Description:** Stage every unique frame under `raw/<trace_id>/frames/`, publish text-only, then run an `sb cortex` vision sweep later to classify and inject images.
- **Pros:** Moves vision cost off the critical path; re-runnable without re-ingest.
- **Cons:** Requires building two things that do not exist: frame persistence into the staged `ArtifactStore` with retention (frames live in `/tmp/borg-youtube-frames/<id>`, `remove_dir_all`'d at `pipeline.rs:679`), and a new cortex vision verb (`cortex::Command` at `sb/src/cli/cortex.rs:57` has no render/vision command).
- **Why not chosen:** Strictly more work, no quality gain. Inline reuses `vision_extract`; the unique-slide count (5-20) keeps inline cost inside budget.

### Alternative 2: Fixed `CodeSnippet | Diagram` enum instead of a config keep-array
- **Description:** Hardcode the positive class to code-or-diagram.
- **Pros:** Simpler config.
- **Cons:** Locks the policy in code; widening or narrowing (e.g. diagrams-only now, add terminals later) needs a rebuild.
- **Why not chosen:** A fixed code-side taxonomy + a config `keep` array gives both a reliable closed label set for the model and a one-line policy change. Default `[architecture-diagram]` matches the immediate intent.

### Alternative 3: OCR-only heuristic classification
- **Description:** Classify from Tesseract text alone.
- **Cons:** Architecture diagrams are mostly lines/boxes with sparse text; OCR sees almost nothing, and cannot tell a diagram from a title card. This is the signal-poor path that produced the current noise.
- **Why not chosen:** No content understanding.

### Alternative 4: Move selection into a vision-capable Fabric pattern
- **Cons:** `fabric::run_pattern` is text-only (`vault/src/fabric.rs:55`); adding an image channel re-architects Fabric for all patterns.
- **Why not chosen:** Disproportionate blast radius for a per-slide judgment a small Rust call handles.

## Technical Considerations

### Dependencies
Reuses `ocr::vision_extract` (Anthropic Messages API) - no new crate. `tokio::sync::Semaphore` already available. Requires the Anthropic API key the OCR path resolves.

### Performance
One vision call per **run** (5-20 typical after `collapse_runs` stitches progressive-drawing fragments - critically, the count is bounded by distinct content runs, not by growth fragments, so a 30s live-drawn diagram is one call, not fifteen). The **process-wide** `max-vision-concurrency` cap bounds total in-flight calls across all concurrent videos plus the image-ingest path. The daemon host has an old CPU (`project-desk-old-cpu`), but classification is a network call, so CPU is not the constraint; API spend/rate limits are, bounded by the pool. `best_frame` reads a handful of local JPEG sizes - negligible.

### Security
Frame JPEGs go to Anthropic, same trust boundary as the existing OCR vision path. No new exposure.

### Testing Strategy
Mirror `slides/tests.rs`: synthetic fixture JPEGs (`write_solid_jpeg`/`write_gradient_jpeg`), a mocked classifier injected at the `classify_slides` seam so tests never hit the network. Assert keep-filtering by category, shape by kept count, best-frame selection on a growing-diagram fixture, and fail-closed drop on classifier error.

### Rollout Plan
- Ship behind `content-filter.enabled` (and `slides.enabled`), both off in the live config until Phase 5 is green; flip on after validating embed/skip on a known diagram-talk and a known talking-head video.
- Re-enabling only affects **new and reingested** notes. The 208 already-stripped notes stay text-only unless explicitly reingested - the desired state.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Vision misclassifies a diagram as Other/Infographic (under-embed) | Med | Low | Fail-closed bias acceptable; tune `min-confidence`; widen `keep` if needed; replay a sample. |
| Infographic/chart misread as architecture-diagram (re-introduces noise) | Med | Med | Prompt draws a sharp diagram-vs-infographic/chart line; raise `min-confidence`; `infographic`/`chart` are not in default `keep`. |
| PiP webcam confuses dominant-region judgment | Med | Med | Prompt classifies by dominant region; sampled data shows models handle insets well. |
| best_frame picks a busy-but-wrong frame (e.g. a transient overlay) | Low | Low | Max-JPEG-size proxy + last-in-window fallback; tune if a labeled sample shows misses. |
| Vision fan-out pegs API/rate limits across concurrent videos | Low | High | **Process-wide** permit pool (`max-vision-concurrency`), not per-video; caps the image-ingest path too. |
| Classified frame, embedded image, and OCR text disagree | Low | Med | Resolved by ordering: best-frame selection precedes both classification and OCR, so all three are the same frame. |
| `collapse_runs` over-merges two distinct diagrams shown back-to-back | Low | Med | Merge requires monotonic pHash growth + small gap; distinct diagrams are not supersets, so they stay separate. |
| `slides.rs`/`handlers.rs` bloat past 1500 lines | Med | Med | Classifier lives in `slides/classify.rs`; pure helpers are small; only call sites touch `slides.rs`. |

## Open Questions
- [ ] `best_frame` heuristic: is max-JPEG-size a good enough completeness proxy, or is edge/stroke density worth the extra cost? Decide after a labeled sample.
- [ ] Should `architecture-diagram` absorb `flowchart`/`sequence-diagram` for the default `keep`, or keep them distinct so the user opts each in? (Currently distinct; default keeps only `architecture-diagram`.)
- [ ] `min-confidence` default - start `0.6`, tune against a labeled sample of real ingests.
- [ ] `collapse_runs` gap threshold for stitching a progressive-drawing run - seconds-scale; tune empirically.

## References
- [2026-04-29-frame-aware-youtube-ingestion.md](2026-04-29-frame-aware-youtube-ingestion.md) - the filtered pipeline (its Phase 1.2 vision-wiring and `raw/<trace>/frames/` staging claims are not in shipped code; this design corrects the record).
- `borg/src/ocr.rs:75` `vision_extract` - the reused vision seam.
- `borg/src/slides.rs:120` `cluster_frames` / `:179` `propose_note_shape` / `:312` `materialize_slides` - the segmentation + selection points being gated and re-pointed.
- Empirical sample: 18 of 287 captured frames, archived at `/var/tmp/rmrf/2026-06-28-130548-*`.
