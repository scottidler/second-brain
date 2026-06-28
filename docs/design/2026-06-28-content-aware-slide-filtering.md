# Design Document: Content-Aware Slide Filtering

**Author:** Scott Idler
**Date:** 2026-06-28
**Status:** Implemented
**Review Passes Completed:** 5/5

**Builds on:** [2026-04-29-frame-aware-youtube-ingestion.md](2026-04-29-frame-aware-youtube-ingestion.md) (the slide pipeline this filters) and [2026-04-19-staged-ingestion-pipeline.md](2026-04-19-staged-ingestion-pipeline.md) (trace-id artifact foundation).

## Summary

The frame-aware YouTube pipeline embeds extracted video frames ("slides") into Obsidian notes, but selects them by structure alone (frame counts, unique-slide ratio) with zero understanding of what is on the slide. The result was visual noise: title cards, talking-head frames, charts, memes, and channel thumbnails embedded into 208 notes across 287 images. This design adds an inline per-slide **vision classification** step that keeps only frames that are genuine **code snippets or software-architecture/system diagrams** and drops everything else, reusing the existing Anthropic vision call in `borg/src/ocr.rs`. The feature is currently disabled in production (`youtube.slides.enabled: false`) pending this filter.

## Problem Statement

### Background

borg's frame-aware YouTube pipeline (shipped v0.5.32, 2026-04-29) runs four stages on every YouTube ingest:

- **Stage 0** (`youtube.rs::extract_frames:386`) - ffmpeg `fps -> mpdecimate -> scale` extracts budget-bounded JPEGs.
- **Stage 1** (`slides.rs::segment_with_pairs:341`) - `cluster_frames:120` collapses near-duplicate frames into unique slides by perceptual hash, `drop_transitions:167` removes transition artifacts, `ocr_slides:291` runs Tesseract OCR per slide, transcript segments are bound per slide.
- **Stage 2** (`pipeline/handlers.rs:333`) - a Fabric pattern (`obsidian-youtube-slides.md`) receives the slide manifest as **text** and returns a summary plus an `embed_slides` list naming which slides to publish.
- **Stage 3** (`slides/publish.rs::publish_slides:44`) - copies the selected slides into `system/attachments/images/<YYYY-MM>/` and inserts `![[...]]` wikilink embeds.

The note-shape decision - whether to embed nothing (`TextOnly`), one hero image, or a section-by-section set - is made by `propose_note_shape` (`slides.rs:179`) from `unique_slides`, `frames_after_mpdecimate`, and their ratio against `SlideThresholds`.

### Problem

**Slide selection has no idea what is on a slide, and the LLM that selects `embed_slides` never sees a single pixel.**

Three verified facts:

1. **Note-shape is frame-count arithmetic only.** `propose_note_shape:179` keys entirely off counts and a ratio. A meme deck and an architecture deck with the same compression ratio get the same shape.
2. **The "vision" pattern is text-only.** `fabric::run_pattern` (`vault/src/fabric.rs:55`) is a CLI subprocess that pipes only text on stdin - no image channel. `render_pattern_input` (`slides.rs:467`) writes `![]({frame_path})` markdown links into that text, but they are dead references the LLM cannot fetch. The LLM picks `embed_slides` from **OCR text + transcript only**. A code screenshot with little OCR-able text and a talking-head frame are indistinguishable to the selector.
3. **`vision_per_slide` was never wired.** Declared at `config.rs:442`, defaulted `false` at `:459`, and read nowhere. The 2026-04-29 doc's claim that vision is "wired but disabled by default" (its Phase 1.2) is incorrect; `Slide.caption` (`slides.rs:51`) is always `None`.

The downstream cost was concrete: 208 notes carried 287 embedded frames, the large majority of which were not code or diagrams. Those have since been stripped and the feature disabled.

### Goals

- **Classify every unique slide by content** during ingestion as one of `CodeSnippet | Diagram | Other`, with a confidence score.
- **Embed only `CodeSnippet`/`Diagram` slides** above a confidence threshold; drop everything else before the note shape is decided.
- **Reuse the existing vision seam** (`ocr::vision_extract:75`, Anthropic Messages API) rather than inventing a new LLM integration.
- **Stay within the ingestion budget** (5-10 min): one vision call per unique slide (typically 5-20 per video after dedupe), run with a bounded concurrency cap.
- **Degrade gracefully:** any classification failure drops that slide (fail-closed) and the note still publishes its text/transcript shape.
- **Be config-gated** so the filter can be turned off to fall back to the legacy structural behavior.

### Non-Goals

- **Replacing the structural shape heuristic wholesale.** Classification gates the *candidate set*; the existing hero-vs-section logic runs on the survivors (with one threshold adjustment, below).
- **The deferred cortex-sweep approach.** Documented as an alternative; it is strictly more work (see Alternatives) and is not chosen.
- **Persisting frames for replay.** The slide work dir is deleted at publish (`pipeline.rs:679`); frames are not staged. Fixing that is a separate concern flagged below, not part of this filter.
- **Moving slide selection into Fabric.** Fabric cannot carry images; classification stays in Rust.
- **New non-YouTube image classification.** `Image` ingests already have their own `ocr.rs` path.
- **Per-video tuning knobs / `--focus`.** Out of scope.

## Proposed Solution

### Overview

Insert a **classification pass** into Stage 1, immediately after `ocr_slides`, as a peer step. Each unique slide gets a vision call that returns a typed `SlideClass`. Slides classified `Other` (or below the confidence threshold) are dropped from the candidate set. The surviving slides feed the existing shape decision and the Stage 2 Fabric input, so the LLM only ever sees - and can only embed - genuine code/diagram slides.

```
handlers.rs::try_extract_slides  (async, holds `config`)
  segment_with_pairs (pure) -> manifest{ slides, proposed_note_shape (structural) }
        │
        ├─ if content-filter disabled: use proposed_note_shape as today  ──┐
        │                                                                  │
        └─ if content-filter enabled:                                      │
              classify_slides(slides, &config.content_filter, &config.llm) │  (NEW, slides/classify.rs)
                  per-slide vision call, capped concurrency                │
              keep kind in {CodeSnippet, Diagram} && confidence >= min     │
              shape = shape_from_kept_count(kept.len())  (NEW, pure)       │
              override manifest slides + shape with the filtered set  ─────┤
                                                                           ▼
              if shape == TextOnly: bail to prose-only (existing handlers.rs:323)
              else render_pattern_input(kept) -> Stage 2 Fabric (text) -> embed_slides ⊆ kept
                                                                           ▼
                              Stage 3 publish (unchanged)
```

Classification is a network call, so it lives in the async orchestrator `try_extract_slides`, **not** inside the pure `segment_with_pairs`. When the filter is disabled, the pipeline behaves exactly as today.

### Architecture

**New module: `borg/src/slides/classify.rs`** (`slides.rs` is already 629 lines; per `rust.md` a new single-word submodule is preferred over growing it).

- `classify_slide(frame, filter, llm) -> Result<SlideClass>` - one vision call. Reads the JPEG bytes + mime (`ocr::mime_from_extension:196`) and calls the new `ocr::vision_classify` sibling, with a **classification prompt** that forces a structured `KIND: <code|diagram|other>` + `CONFIDENCE: <0.0-1.0>` response, parsed into the enum. `classify.rs` owns the prompt and parsing.
- `classify_slides(slides, filter, llm) -> Vec<Result<SlideClass>>` - the capped-parallel fan-out, mirroring `ocr_slides:291` but async (since `vision_extract`/`vision_classify` are async). **Concurrency is bounded** by `content-filter.concurrency` via a `tokio::sync::Semaphore` (required by `feedback-no-unbounded-fanout`).

**Orchestration (`handlers.rs::try_extract_slides:216`):** `segment_with_pairs:341` stays pure and still computes its structural `proposed_note_shape:421`. When the filter is enabled, `try_extract_slides` (which already holds `config` and is async) calls `classify_slides` on the manifest's slides, attaches each `SlideClass`, partitions kept/dropped, and **overrides** the manifest's slide list and shape with the filtered result before the existing `TextOnly` bail at `handlers.rs:323`.

**Shape decision (`shape_from_kept_count`, NEW pure fn in `slides.rs`):** the filter bypasses `propose_note_shape:179` entirely (its `min_unique_slides` talking-head suppressor was a structural *proxy* for "is this noise," which the vision filter now answers directly). Because `Hero` embeds exactly one image (`publish.rs:100`) and `SlideSection` embeds all selected, the rule is simply by kept count:
- `0` kept -> `TextOnly`
- `1` kept -> `Hero`
- `>= 2` kept -> `SlideSection`

This guarantees a talk with even a single genuine architecture diagram embeds it, rather than being suppressed under the legacy `min_unique_slides = 4` threshold. When the filter is disabled, `propose_note_shape`'s ratio logic is untouched.

**Stage 2/3:** `render_pattern_input:467` is given only kept slides (the dead `![]()` links become irrelevant since non-kept slides never reach the pattern). `enforce_shape:588` remains the downgrade-only Stage 3 safety net. Publish/cleanup are unchanged.

### Data Model

```rust
// borg/src/slides/classify.rs
pub enum SlideKind {
    CodeSnippet,  // source code / terminal on screen
    Diagram,      // architecture / system / flow diagram
    Other,        // talking head, title card, chart, meme, thumbnail, ...
}

pub struct SlideClass {
    pub kind: SlideKind,
    pub confidence: f32,  // 0.0..=1.0 from the vision model
}
```

`Slide` (`slides.rs:41`) gains `pub class: Option<SlideClass>`, replacing the dead `caption: Option<String>` field at `:51` (never populated, folding it removes naming drift). Both fields are `#[serde(skip_serializing_if)]`, and `caption` was always `None`, so it was never written to `slides.yml` - removing it is a schema-neutral change.

### API Design

`ocr::vision_extract:75` is `async fn(image_data: &[u8], mime_type, &VisionConfig, &LlmConfig) -> Result<VisionResult>` with a fixed OCR-style prompt (TEXT/DESCRIPTION/TITLE/TAGS). Classification needs a different prompt and parse, so Phase 2 adds a sibling in `ocr.rs` that shares the HTTP/auth/base64 logic but takes a caller-supplied prompt:

```rust
// borg/src/ocr.rs  (new sibling, factored out of vision_extract's shared core)
pub async fn vision_classify(
    image_data: &[u8],
    mime_type: &str,
    vision_config: &VisionConfig,   // model override; classify builds this from content-filter.model
    llm_config: &LlmConfig,         // api-key + default model resolution
) -> Result<String>;                // raw model text; classify.rs parses KIND/CONFIDENCE

// borg/src/slides/classify.rs
pub async fn classify_slide(
    frame: &Path,                   // reads bytes + mime via ocr::mime_from_extension:196
    filter: &ContentFilterConfig,
    llm: &LlmConfig,
) -> Result<SlideClass>;
pub async fn classify_slides(
    slides: &[Slide],
    filter: &ContentFilterConfig,
    llm: &LlmConfig,
) -> Vec<Result<SlideClass>>;        // capped-parallel; Err per slide -> dropped (fail-closed)
```

Config additions to `YoutubeSlidesConfig` (`config.rs:420`, serde kebab-case, Defaults at `:447`):

```yaml
youtube:
  slides:
    enabled: true
    content-filter:
      enabled: true            # master switch for the vision filter
      model: ""                # empty -> inherit LlmConfig.model (mirrors VisionConfig.model)
      concurrency: 4           # cap on parallel vision calls (no unbounded fan-out)
      min-confidence: 0.6      # keep a slide only at/above this confidence
```

Shape follows kept count directly (0 -> TextOnly, 1 -> Hero, >= 2 -> SlideSection), matching publish semantics, so no `hero-max` knob is needed.

The dead `vision_per_slide` stub (`config.rs:442`) is **removed** and folded into `content-filter.enabled` - one concept, one flag (per `general.md`).

### Implementation Plan

#### Phase 1: Config surface
**Model:** sonnet
- Add the `content-filter` block to `YoutubeSlidesConfig` (`config.rs:420`) and its `Default` (`:447`); remove the `vision_per_slide` stub.
- Mirror the additions into the repo config template (`config/templates/borg.yml`) and `~/.config/sb/borg.yml` documentation.
- Unit test: config round-trips with defaults and with overrides.

#### Phase 2: Vision classifier
**Model:** opus
- Factor `ocr::vision_extract:75` so its HTTP/auth/base64 core is shared, and add `ocr::vision_classify` taking a caller-supplied prompt and returning raw model text.
- New `borg/src/slides/classify.rs`: `SlideKind`, `SlideClass`, `classify_slide`, plus the code/diagram-detection prompt and the `KIND`/`CONFIDENCE` parser.
- Keep the prompt strict; force the structured response. Distinguish architecture/system diagrams from ordinary charts/graphs explicitly.
- Unit tests for the parser: well-formed, malformed, and ambiguous model responses (fail-closed -> `Err`, which drops the slide).

#### Phase 3: Orchestration + shape gate
**Model:** opus
- `classify_slides` capped-parallel pass (tokio `Semaphore`, `content-filter.concurrency`).
- Add pure `shape_from_kept_count` to `slides.rs`.
- In `handlers.rs::try_extract_slides:216`: when the filter is enabled, classify the manifest's slides, attach `SlideClass`, partition kept/dropped, override the slide list + shape via `shape_from_kept_count`, then fall through to the existing `TextOnly` bail at `:323`.
- Restrict `render_pattern_input:467` to kept slides.

#### Phase 4: Tests + re-enable
**Model:** sonnet
- Tests mirroring `slides/tests.rs` (fixture JPEGs via `write_solid_jpeg`/`write_gradient_jpeg`, mocked classifier) covering: all-Other -> TextOnly; one Diagram -> Hero; many code slides -> SlideSection; classifier error -> slide dropped, note still publishes.
- Flip `youtube.slides.enabled: true` (and `content-filter.enabled: true`) once `otto ci` is green.

## Alternatives Considered

### Alternative 1: Deferred cortex sweep over staged frames
- **Description:** Stage every unique frame under `raw/<trace_id>/frames/`, publish the note text-only, then run an `sb cortex` vision sweep later that classifies staged frames and drops selected images into already-published notes.
- **Pros:** Moves vision cost off the ingestion critical path; enables re-running the filter without re-ingesting.
- **Cons:** Requires building two things that **do not exist**: (a) frame persistence into the staged `ArtifactStore` with retention - today frames live in `/tmp/borg-youtube-frames/<id>` and are `remove_dir_all`'d at `pipeline.rs:679`; and (b) a brand-new cortex vision verb (`sb cortex` has no `render` or vision command; the `cortex::Command` enum at `sb/src/cli/cortex.rs:57` is Classify/Lint/Link/Intel/State/Daemon/Migrate/Sweep/Summarize/Embed/Graph/Hub/Entities).
- **Why not chosen:** Strictly more work for no quality gain. Inline reuses `vision_extract` directly and the unique-slide count (5-20) keeps inline cost well inside budget. Frame-staging for replay is independently desirable but is its own design.

### Alternative 2: OCR-only heuristic classification
- **Description:** Classify from Tesseract OCR text alone (e.g. code keywords, monospace density, box/arrow glyphs).
- **Pros:** No vision API cost; fully local.
- **Cons:** Architecture diagrams are mostly lines and boxes with sparse text - OCR sees almost nothing. Code screenshots OCR poorly (syntax coloring, ligatures). This is exactly the signal-poor path that produced the current noise.
- **Why not chosen:** It cannot reliably distinguish a diagram from a title card; the whole point is content understanding OCR does not provide.

### Alternative 3: Move selection into the Fabric pattern with a vision-capable model
- **Description:** Have the Stage 2 LLM see the actual frames and select.
- **Pros:** One LLM call for both summary and selection.
- **Cons:** `fabric::run_pattern` is a text-only CLI subprocess (`vault/src/fabric.rs:55`); adding an image channel means re-architecting the Fabric integration for all patterns.
- **Why not chosen:** Disproportionate blast radius for a per-slide yes/no judgment that a small Rust vision call handles cleanly.

## Technical Considerations

### Dependencies
- Reuses `borg/src/ocr.rs::vision_extract` (Anthropic Messages API) - no new crate. `tokio::sync::Semaphore` already available.
- Requires the Anthropic API key the OCR vision path already resolves.

### Performance
- One vision call per unique slide. After `mpdecimate` + pHash clustering, typical videos yield 5-20 unique slides. With `concurrency: 4`, classification adds seconds, not minutes - inside the 5-10 min budget. The daemon host has an old CPU (`project-desk-old-cpu`), but classification is a network call, so CPU is not the constraint; API spend and rate limits are, which the concurrency cap bounds.

### Security
- Frame JPEGs are sent to Anthropic, same trust boundary as the existing OCR vision path. No new exposure.

### Testing Strategy
- Mirror `slides/tests.rs`: synthetic fixture JPEGs, a mocked classifier injected at the `classify_slides` seam so tests do not hit the network. Assert shape outcomes per kept-count and the fail-closed drop on classifier error.

### Rollout Plan
- Ship behind `content-filter.enabled` (and the existing `slides.enabled`). Both default off in the live config until Phase 4 is green; flip on after a manual ingest of a known code-talk and a known talking-head video confirms correct embed/skip.
- Re-enabling only affects **new and reingested** notes. The 208 notes already stripped of frames stay text-only unless explicitly reingested - which is the desired state, not a regression.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Vision misclassifies a diagram as Other (under-embed) | Med | Low | Fail-closed bias is acceptable; tune `min-confidence`; replay after re-enable on a sample. |
| Vision misclassifies a chart/meme as Diagram (re-introduces noise) | Med | Med | Strict prompt distinguishing system/architecture diagrams from charts; raise `min-confidence`. |
| Unbounded vision fan-out pegs API/rate limits | Low | High | Hard `concurrency` cap via Semaphore (`feedback-no-unbounded-fanout`). |
| API outage during ingest drops all slides | Low | Low | Fail-closed: note still publishes text/transcript; replay later. |
| `slides.rs`/`handlers.rs` bloat over 1500 lines | Med | Med | Classifier lives in new `slides/classify.rs`; only the call site touches `slides.rs`. |

## Open Questions
- [ ] Confidence threshold default - start at `0.6` and tune after a labeled sample of real ingests?
- [ ] Should `Diagram` include data-flow/sequence diagrams, or strictly architecture/system? (Affects the prompt's positive class.)
- [ ] Picture-in-picture frames (code editor with a talking-head inset) - instruct the prompt to classify by the dominant/primary content region, so a code-with-webcam slide still scores `CodeSnippet`. Confirm this is the desired behavior.
- [ ] Is the dropped `vision_per_slide` stub referenced in any external config the user maintains? (grep says only the two declaration lines - safe to remove.)

## References
- [2026-04-29-frame-aware-youtube-ingestion.md](2026-04-29-frame-aware-youtube-ingestion.md) - the pipeline being filtered (note: its Phase 1.2 vision-wiring and `raw/<trace>/frames/` staging claims are not reflected in shipped code; this design corrects the record).
- `borg/src/ocr.rs:75` `vision_extract` - the reused vision seam.
- `borg/src/slides.rs:179` `propose_note_shape` - the structural decision point being gated.
