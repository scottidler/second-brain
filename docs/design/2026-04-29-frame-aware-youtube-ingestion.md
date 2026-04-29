# Design Document: Frame-Aware YouTube Ingestion

**Author:** Scott Idler
**Date:** 2026-04-29
**Status:** Implemented (components ready; production wiring awaits YoutubeUrl staged-pipeline migration. Phase 3 reliability improvements ship live.)
**Review Passes Completed:** 5/5

**Builds on:** [2026-04-19-staged-ingestion-pipeline.md](2026-04-19-staged-ingestion-pipeline.md) (staged pipeline with `trace_id`-keyed artifacts is the foundation; this doc adds visual extraction to the `YoutubeUrl` shape)

**Related prior art:** [bradautomates/claude-video](https://github.com/bradautomates/claude-video) - an in-session Claude skill that extracts frames + transcripts. Different architecture (synchronous, agent-driven), but several of its engineering choices are directly portable to borg's offline daemon. Where applicable, citations point to specific files in that repo.

## Summary

Extend borg's `YoutubeUrl` ingestion pipeline so that visual content - slides, terminal demos, charts, whiteboards, code on screen - participates in summarization and, for slide-heavy content, is published into the Obsidian note as visual aids. The transcript-only ingestion that exists today loses roughly half the signal on talks where the structure lives on screen, not in the audio. The design adds a frame-extraction step at Stage 0, a slide-segmentation step at Stage 1, and a conditional vault-embedding step at Stage 3, with cost bounded by an auto-fps frame budget and a perceptual-hash dedupe pass that collapses held slides into single canonical representatives.

The work splits cleanly into two ship boundaries: **Phase 1** delivers richer Fabric input and better summaries with no vault-shape change (slides feed the LLM but don't appear in the note). **Phase 2** adds the published vault embedding (slides as visual aids in the note body). Each phase is independently shippable and independently valuable; Phase 1 is the natural place to validate that visual context measurably improves summary quality before committing to the larger vault-shape change.

## Problem Statement

### Background

borg's YouTube pipeline today (`borg/src/pipeline.rs::process_youtube`) is text-only:

1. `yt-dlp --dump-json` for metadata
2. Fabric `--transcript` for the spoken transcript (falling back to `yt-dlp` subtitles, then audio extraction + Groq Whisper)
3. Fabric pattern (`obsidian-note.md`) for the LLM summary
4. Stage 3 writes a markdown note to the vault with the iframe embed and the prose summary

The video itself - the actual frames - is never seen by anything. yt-dlp downloads it briefly to extract audio when subtitles fail, then discards it.

### Problem

For the content shapes most worth ingesting into a second brain - tech talks, lectures, conference presentations, screencasts, tutorials, walkthroughs - the visual track is the primary signal. A slide-heavy talk:

- Organizes ideas as discrete slides; the deck is the talk's outline
- Shows code, diagrams, terminal output, charts; speech references but does not reproduce these
- Builds arguments via slide transitions; "as you can see on the next slide" is meaningless without the slide

A 30-minute lecture transcript reads as undifferentiated prose. Fabric does its best to chunk it, but the section structure that was native to the slide deck has to be re-discovered by the LLM from word-level cues - and is often lost.

A concrete recent example sitting in the vault: `notes/my-claude-code-can-instantly-watch-any-video-heres-how.md` (the very video about claude-video itself, ingested 2026-04-29). The note's summary is fine but flattened. The original video walks through a clear deck - title slide, "how it works" pipeline diagram, frame budget table, token cost table, demo screencap - none of which appear in the note. The pipeline diagram in particular is the kind of artifact the second brain *should* be capturing: a 30-second visual that's worth more than the surrounding two paragraphs of speech describing it. This design exists so that note, re-ingested under the new pipeline, comes out with the diagram embedded and the summary structured around the slide deck's own argument.

Three structural problems:

1. **Visual content is dropped.** Anything on screen that's not also said aloud is invisible to the summarizer. Code demos, charts, slide titles, on-screen text - all gone.
2. **Talk structure is lost.** Slide changes are the natural section boundaries of any presentation. Without them, Fabric reconstructs structure from prose alone.
3. **Notes are visually thin.** Obsidian notes for video sources are walls of text plus an iframe. There is no visual scaffolding of the talk's argument.

### Goals

- **Capture frames at Stage 0** alongside the existing transcript + metadata, bounded by a duration-aware frame budget so cost scales sublinearly with video length.
- **Segment frames into unique slides at Stage 1** via pixel-diff dedupe (`ffmpeg mpdecimate` at extraction) plus perceptual-hash post-pass, producing a manifest of `[start, end]` ranges per unique slide with bound transcript segments.
- **Per-slide OCR and vision-API captioning at Stage 1**, using the existing `ocr.rs` plumbing, so slide content is searchable in oracle/Obsidian and feeds Stage 2 with a richer signal than transcript alone.
- **Slide-aware Fabric summarization at Stage 2**, where the pattern receives the slide manifest and produces a summary structured by slide, plus an explicit list of which slides should be embedded in the published note.
- **Conditional vault embedding at Stage 3.** For slide-heavy content, copy the LLM-selected slide subset into the existing vault attachment area and emit Obsidian wikilink embeds in the note body. For text-only/talking-head content, publish the existing prose-only shape - no spurious image overhead.
- **Replayable.** The trace-id-keyed staging artifacts let any pipeline tweak (different mpdecimate threshold, different vision model, different Fabric pattern, different note shape decision) be retroactively applied to already-ingested videos.
- **Bounded cost.** The frame budget caps token spend on visual processing; the dedupe collapse means the actual vision-API call count is roughly the slide count, not the frame count.

### Non-Goals

- Frame extraction for non-`YoutubeUrl` kinds. `Image` already has its own pipeline (`ocr.rs`); article/GitHub/thread URLs have no video to process.
- Real-time / streaming ingestion. Stage 0 remains a batch fetch.
- Audio chunking for videos longer than the Whisper upload limit (~50 min mono). Captions cover the majority case; chunking is a separate design.
- Replacing the legacy in-memory `process_youtube` ahead of the staged-pipeline migration. This design assumes the `YoutubeUrl` pipeline runs on the staged path. If the legacy path is still live for some inputs at delivery time, frame-aware ingestion is a no-op there.
- Scraping past auth walls or DRM. yt-dlp public-content only.
- Building a video player in Obsidian. The iframe embed already covers playback.
- Bypassing the auto-fps budget for "high-importance" videos. If a user wants more detail on a section, the replay command (with `--focus`) is the path.

## Proposed Solution

### Overview

The `YoutubeUrl` pipeline gains three new artifact families and one new gate:

```
 ┌────────────────────────────┐   ┌──────────────────────────┐   ┌──────────────────────┐   ┌──────────────────────┐
 │ STAGE 0                    │   │ STAGE 1                  │   │ STAGE 2              │   │ STAGE 3              │
 │ raw/<trace_id>/            │   │ transcripts/<trace_id>/  │   │ summaries/           │   │ vault/notes/         │
 │  envelope.yml              │   │  transcript.md           │   │  summary.md          │   │  <slug>.md           │
 │  body.txt                  │   │  slides.yml          NEW │──▶│  + embed_slides      │──▶│  + ![[slide-NN.jpg]] │
 │  fetched.* (info+subs)     │──▶│  slides/                 │   │                      │   │  + slides: [...]     │
 │  frames/         NEW       │   │   slide-001.jpg      NEW │   │                      │   │   in frontmatter NEW │
 │  audio.*                   │   │   slide-002.jpg      NEW │   │                      │   │                      │
 └────────────────────────────┘   └──────────────────────────┘   └──────────────────────┘   └──────────────────────┘
              │                              │                            │                          │
           gate-0                         gate-1                        gate-2                  (existing
        domain blocklist            block-page detection           failed-fetch                 quality
        (no change)                 (no change)                    paraphrase                   checks)
                                                                   detection
                                                                   + zero-slides  NEW
                                                                   warning
```

The four new artifact concepts:

- **Stage 0 `frames/`** - all extracted frames after `ffmpeg mpdecimate` runs at extraction time. Pixel-diff filter drops obviously-identical neighbors; ~30-100 frames survive depending on video length.
- **Stage 1 `slides.yml`** - the manifest. Each entry is a unique slide with its frame path, `[start, end]` timestamps, OCR text, vision caption, and the transcript segments bound to its time range.
- **Stage 1 `slides/slide-NN.jpg`** - the canonical first-seen frame for each unique slide cluster. These are the candidates for vault embedding.
- **Stage 3 frontmatter `slides:`** - a list of vault-owned slide attachment paths, used for cleanup on replay/reingest.

Stage 2 and Stage 3 acquire branching behavior driven by a "compression ratio" (unique-slides / extracted-frames). High-compression videos are slide-heavy; low-compression videos are motion / talking-head and revert to the existing text-only note shape.

### Architecture

#### Stage 0 - Frame extraction

Stage 0 runs alongside the existing video download and audio extraction. After yt-dlp produces `raw/<trace_id>/fetched.video.mp4`, an additional ffmpeg invocation produces frames:

```
ffmpeg \
  -hide_banner -loglevel error -y \
  -i raw/<trace_id>/fetched.video.mp4 \
  -vf "fps=<auto_fps>,mpdecimate=hi=64*32:lo=64*16:frac=0.33,scale=512:-2" \
  -frames:v 100 \
  -q:v 4 \
  raw/<trace_id>/frames/frame_%04d.jpg
```

The filter chain is read left-to-right and the **order matters**: `fps=<auto_fps>` resamples the source to the budget rate first, then `mpdecimate` drops near-identical neighbors from the downsampled stream, then `scale=512:-2` resizes for token efficiency. The reverse order (mpdecimate first, then fps) is broken: ffmpeg's `fps` filter forces a constant framerate by duplicating the previous frame to fill any gap mpdecimate left, undoing the dedupe. Running `fps` first means mpdecimate operates on a sparse already-budget-rate stream and its drops are kept. The auto-fps target is duration-aware (table below). Combined with the frame budget cap, this produces a small, semantically meaningful frame set even for slide-heavy long videos at zero additional code complexity.

Two terms used throughout this doc:

- **frame** - any JPEG written by Stage 0's ffmpeg invocation. `frames_after_mpdecimate` in `slides.yml` is the count of these.
- **slide** - a unique cluster of visually-similar consecutive frames identified by Stage 1's pHash pass. `unique_slides` is the count of these. Each slide has one canonical representative frame.

The **compression ratio** is `unique_slides / frames_after_mpdecimate`. It quantifies how slide-like the visual content is - lower means the content held still on a small number of distinct images for long stretches.

**Auto-fps frame budget** (lifted from `bradautomates/claude-video/scripts/frames.py:94-110`, lightly adapted):

| Duration   | Frame budget | Effective fps (pre-mpdecimate) |
|------------|--------------|--------------------------------|
| ≤30 s      | up to 30     | ~1 fps                         |
| 30 s – 1 min | 40         | ~0.7 fps                       |
| 1 – 3 min  | 60           | ~0.4 fps                       |
| 3 – 10 min | 80           | ~0.2 fps                       |
| > 10 min   | 100 (cap)    | <0.2 fps, sparse-scan warning  |

Hard caps: 2 fps, 100 frames, 512px width. `mpdecimate` reduces this further whenever the video is genuinely static; slide-heavy content typically drops to 30-60% of the budget.

For each retained frame, the source-video timestamp is computed as `index / effective_fps + start_seconds` (start defaults to 0). Stage 0 writes a sidecar `raw/<trace_id>/frames.yml` listing each frame's path and timestamp.

**Non-canonical frames** (the ~80-90% of `frames/` that didn't become canonical slide representatives) stay in `raw/<trace_id>/frames/` until staging retention ages them off (30-90 days, per the staged-pipeline retention policy). They are not orphaned - they are the audit trail. A future replay with tightened pHash thresholds may promote previously-merged frames into distinct slides, which is only possible if the underlying frame data is still on disk.

**Gate-0 unchanged.** Domain blocklist still runs on the URL before fetch.

#### Stage 1 - Slide segmentation, OCR, transcript binding

Stage 1 reads bytes from disk, never the network. It produces:

1. **`transcript.md`** - the existing transcript artifact (unchanged).
2. **`slides.yml`** - the new slide manifest.
3. **`slides/slide-NNN.jpg`** - the canonical first-seen frame per slide cluster, copied from `raw/<trace_id>/frames/`.

The slide-segmentation algorithm:

1. **Hash every frame.** Compute a 64-bit perceptual hash (dHash via the `image_hasher` Rust crate) for each `frame_NNNN.jpg`. Microseconds per frame.
2. **Cluster by Hamming distance.** Walk frames in chronological order; if the current frame's hash is within 6 bits of the previous canonical frame, it joins that cluster (extending its `end` timestamp). Otherwise it starts a new cluster as the canonical first-seen frame.
3. **Drop transitions.** Clusters with `end - start < 5 seconds` are dropped as transition artifacts (slide change in progress, brief animation, fade-through).
4. **Emit canonical frames.** Each surviving cluster's canonical frame is copied to `slides/slide-NNN.jpg` (3-digit zero-padded, sequential).
5. **Per-slide OCR + vision** (parallel, like existing `Image` ingestion):
   - Tesseract via `ocr::ocr_extract` for raw on-slide text (free, fast)
   - Anthropic vision via `ocr::vision_extract` for a caption / description (cost-gated; opt-out via config flag)
6. **Bind transcript.** For each slide with range `[t_start, t_end]`, filter VTT segments where `seg.start >= t_start && seg.start < t_end`. Each slide carries its own bound transcript snippet.
7. **Compute compression ratio.** `unique_slides / total_frames_extracted`. This drives Stage 2's note-shape decision.

`slides.yml` shape:

```yaml
trace_id: ht-0fafb6
video:
  url: https://www.youtube.com/watch?v=QZMljuD10sU
  duration_seconds: 540
extraction:
  frames_after_mpdecimate: 38    # ffmpeg dropped pixel-identical neighbors
  unique_slides: 7               # canonical clusters after pHash
  transitions_dropped: 2         # < 5s clusters discarded
  compression_ratio: 0.18        # unique_slides / frames_after_mpdecimate
  proposed_note_shape: slide-section  # Stage 1's recommendation; Stage 2 confirms via embed_slides
slides:
  - id: s001
    frame_path: slides/slide-001.jpg
    start: 0.0
    end: 42.3
    duration: 42.3
    ocr: |
      My Claude Code Can INSTANTLY Watch Any Video
      Brad | AI & Automation
    caption: "Title slide with 'INSTANTLY' set in bold yellow over a dark background"
    transcript:
      - "[00:00] welcome everyone to this video"
      - "[00:05] I'm going to show you a free Claude skill"
  - id: s002
    frame_path: slides/slide-002.jpg
    start: 42.3
    end: 135.8
    duration: 93.5
    ocr: |
      How it works
      yt-dlp -> ffmpeg -> Whisper
    caption: "Three-step pipeline diagram with arrows"
    transcript:
      - "[00:42] the way this works is yt-dlp downloads"
      - "[01:00] then ffmpeg extracts frames"
      ...
```

**Gate-1 extended.** Existing block-page detection unchanged. Add a soft gate: if `frames_extracted == 0` (ffmpeg failed) or `frames_after_phash == 0` (post-dedupe collapse failed), log a warning and fall back to text-only note shape. Not a hard rejection - the transcript path is still viable.

**Note-shape proposal** (computed at Stage 1, used as input to Stage 2's Fabric pattern):

A static slide deck collapses 100 frames to ~10 slides (ratio ~0.1); a talking-head video collapses 100 frames to ~3 (ratio ~0.03 - even lower!); an animated/motion video barely collapses (ratio ~0.7). The ratio alone doesn't disambiguate slide-heavy from talking-head - both have low ratios - so a unique-slide-count tiebreaker handles the talking-head case:

| Condition                                                       | Proposed shape | Behavior                                                       |
|-----------------------------------------------------------------|----------------|----------------------------------------------------------------|
| `unique_slides ≤ 3`                                             | text-only      | Talking-head / static-camera; embed nothing                    |
| `compression_ratio ≥ 0.50`                                      | text-only      | Motion / animated content; pHash didn't find clean slides      |
| `0.10 ≤ compression_ratio < 0.50` and `unique_slides ≥ 4`       | hero           | Mixed; embed one representative slide as note header           |
| `compression_ratio < 0.10` and `unique_slides ≥ 4`              | slide-section  | Slide-heavy; embed LLM-selected subset as section markers      |

Stage 1 writes the proposed shape into `slides.yml`. Stage 2's Fabric pattern is given the proposal as input and produces the final shape via its `embed_slides` output: an empty list means text-only regardless of proposal; one slide means hero; multiple means slide-section. The pattern is instructed it may downgrade (slide-section → hero, hero → text-only) but not upgrade beyond what Stage 1 detected was viable.

**Enforcement is in Stage 3 Rust, not in the prompt.** Stage 3 reads `slides.yml` for the proposed shape and validates the LLM's `embed_slides` output against it before writing anything to the vault. If the LLM proposes more slides than the proposal allows (e.g. proposed `text-only` but returned three slides), Stage 3 truncates to the cap implied by the proposed shape and logs a warning to the trace's audit record. The prompt instruction is the LLM's first chance to do the right thing; the Rust check is the authoritative gate. Prompt-level rules are advisory, not enforceable.

The asymmetry is deliberate: Stage 1's analysis is mechanical and grounded in the actual frame data, while Stage 2's analysis is the LLM's interpretation. If the LLM thinks the talk has more visual structure than dedupe found, it's almost certainly hallucinating; if it thinks the talk has less, the LLM is likely right that the slides aren't worth embedding (e.g. a deck where every slide is the same template with one bullet changed). Downgrades therefore reflect taste; upgrades would reflect confabulation.

Thresholds are tunable via `borg.yml` config (`youtube.slides.thresholds.{text_only_max_ratio, slide_section_max_ratio, min_unique_slides}`).

#### Stage 2 - Slide-aware summarization

A new Fabric pattern, `obsidian-youtube-slides.md`, consumes `slides.yml` and the transcript. The pattern's input is a flattened markdown render of `slides.yml`:

```markdown
# Slide-aware video summary input

Video: <title> by <uploader> (<duration>)
URL: <url>
Note shape: slide-section

## Slide s001 - 00:00 -> 00:42

![](slides/slide-001.jpg)

On-slide text (OCR):
> My Claude Code Can INSTANTLY Watch Any Video
> Brad | AI & Automation

Visual caption:
> Title slide with 'INSTANTLY' set in bold yellow over a dark background

Transcript while this slide was on screen:
- [00:00] welcome everyone to this video
- [00:05] I'm going to show you a free Claude skill

## Slide s002 - 00:42 -> 02:15

...
```

The pattern instruction directs the LLM to:

- Produce a per-slide summary (or a single hero summary, or a text-only summary, depending on `note_shape`)
- Return a structured response with: `embed_slides: [s001, s002, s005]` (which slides should be copied to vault), `sections: [{slide: s001, title: "Introduction", body: "..."}]`, etc.
- For `text-only` shape, return only the prose summary (existing format).

Output shape (YAML frontmatter at the top of the Fabric output, body below):

```yaml
---
shape: slide-section
embed_slides: [s001, s002, s005]
sections:
  - slide: s001
    title: Introduction
  - slide: s002
    title: How the pipeline works
  - slide: s005
    title: Cost and limits
---

## Introduction
The talk opens with a title card and a one-line pitch...

## How the pipeline works
The presenter walks through three components: yt-dlp downloads...
```

Stage 2's existing quality gate (failed-fetch paraphrase detection on the summary text) runs unchanged on the body.

#### Stage 3 - Conditional vault embedding

Stage 3 reads the Stage 2 output and the `slides.yml` manifest, and produces the published note.

For `text-only` shape: existing behavior. No attachments copied. No frontmatter changes.

For `hero` shape: copy one slide (`embed_slides[0]`) into the vault attachment area, emit the wikilink embed near the top of the note body, record the path in the `slides:` frontmatter list.

For `slide-section` shape:

1. For each slide in `embed_slides`, copy `transcripts/<trace_id>/slides/slide-NNN.jpg` to the vault attachment area at the path:
   ```
   <vault_root>/system/attachments/images/<YYYY-MM>/<slug>-slide-NNN.jpg
   ```
   This matches the existing image-attachment convention used by borg's `Image` ingestion (path layout decided in `pipeline.rs:794`, storage implemented in `assets::store_asset` at `borg/src/assets.rs`). `<slug>` is the note's filename stem.
2. Atomic file writes: write to `<filename>.tmp` then `rename` to final path, so a partially-synced vault never sees half-written binary content.
3. Emit the note body as a sequence of `## <Section Title>` blocks, each with the wikilink embed at the top:
   ```markdown
   ## Introduction
   ![[<slug>-slide-001.jpg]]

   The talk opens with...
   ```
4. Record all owned attachment paths in the note's frontmatter `slides:` list, so cleanup on replay can find and remove them.

Frontmatter additions (only when `shape != text-only`):

```yaml
---
title: My Claude Code Can INSTANTLY Watch Any Video
trace: ht-0fafb6
slides:
  - system/attachments/images/2026-04/my-claude-code-can-instantly-watch-any-video-heres-how-slide-001.jpg
  - system/attachments/images/2026-04/my-claude-code-can-instantly-watch-any-video-heres-how-slide-002.jpg
  - system/attachments/images/2026-04/my-claude-code-can-instantly-watch-any-video-heres-how-slide-005.jpg
---
```

**On replay/reingest of the same trace_id (or same URL, fresh trace_id):**

1. Read the old note's `slides:` frontmatter list. Hold it in memory.
2. Compute the new owned-files set. If any new path collides with an old path (same `<slug>-slide-NNN.jpg`), use a different sequence number for the new file - never overwrite. This means a transiently-broken state where both old and new files briefly coexist is preferred over a state where readers see a half-overwritten JPG.
3. Write all new slide JPGs to vault (atomic tmpfile + rename per file).
4. Write the new note (atomic tmpfile + rename) with the new `slides:` list.
5. Only after the new note is durable on disk: delete the old slide attachments via `rkvr rmrf` (per the safety rule - archived for recovery, not destructive).

**Crash recovery:** if borg crashes between steps 4 and 5, the vault contains the new note plus orphaned old slides. The orphans are harmless (not referenced by any note's frontmatter) and are caught by the periodic cortex sweep (open question below). If borg crashes between steps 3 and 4, the vault contains the old note plus extra unreferenced new slides; on next replay these are detected as orphans and cleaned. Crashes between earlier steps leave the old state intact.

This lets pipeline tweaks (different Fabric pattern, different mpdecimate parameters, different note-shape thresholds) be retroactively re-applied via `borg replay <trace_id>` without leaking orphaned attachment files at steady state.

### Data Model

New types in `borg/src/types.rs`:

```rust
/// One unique slide identified after pHash dedupe.
#[derive(Debug, Serialize, Deserialize)]
pub struct Slide {
    pub id: String,                  // "s001", zero-padded
    pub frame_path: PathBuf,         // relative to transcripts/<trace_id>/
    pub start: f64,                  // seconds, source-video timeline
    pub end: f64,
    pub ocr: String,                 // tesseract output
    pub caption: Option<String>,     // vision API caption
    pub transcript: Vec<String>,     // bound VTT segments, formatted
}

/// Stage 1 manifest emitted alongside transcript.md.
#[derive(Debug, Serialize, Deserialize)]
pub struct SlideManifest {
    pub trace_id: String,
    pub video: VideoMetaSnippet,     // url, duration, etc.
    pub extraction: ExtractionStats, // frame counts + compression ratio
    pub slides: Vec<Slide>,
    pub note_shape: NoteShape,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum NoteShape {
    TextOnly,
    Hero,
    SlideSection,
}
```

New config in `borg/src/config.rs`:

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct YoutubeSlidesConfig {
    pub enabled: bool,                          // master switch; default true
    pub max_frames: u32,                        // default 100
    pub max_fps: f32,                           // default 2.0
    pub mpdecimate_hi: u32,                     // default 64*32 = 2048
    pub mpdecimate_lo: u32,                     // default 64*16 = 1024
    pub mpdecimate_frac: f32,                   // default 0.33
    pub phash_hamming_threshold: u32,           // default 6
    pub transition_min_seconds: f32,            // default 5.0
    pub frame_resolution_px: u32,               // default 512
    pub vision_per_slide: bool,                 // default false in Phase 1; flipped to true in Phase 2
    pub slide_thresholds: SlideThresholds,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SlideThresholds {
    pub text_only_max_ratio: f32,               // default 0.50; ratio >= this = text-only
    pub slide_section_max_ratio: f32,           // default 0.10; ratio < this = slide-section
    pub min_unique_slides: u32,                 // default 4; below this = text-only regardless
}
```

Defaults live in `borg/src/config/youtube.rs` (new module per the rust.md rule on single-word file names).

### API Design

No public HTTP API change. Internal Rust API additions:

```rust
// borg/src/youtube.rs - extend
pub fn extract_frames(
    video_path: &Path,
    out_dir: &Path,
    config: &YoutubeSlidesConfig,
) -> Result<Vec<FrameRef>>;

// borg/src/slides.rs - new module
pub fn segment(frames: &[FrameRef], config: &YoutubeSlidesConfig) -> Result<SlideManifest>;
pub fn ocr_and_caption_slides(
    manifest: &mut SlideManifest,
    config: &Config,
) -> Result<()>;
pub fn bind_transcript(manifest: &mut SlideManifest, vtt: &str);
pub fn write_manifest(manifest: &SlideManifest, out_dir: &Path) -> Result<()>;

// borg/src/stages/extract.rs - extend
impl Extractor for YoutubeExtractor {
    fn extract(&self, raw: &RawCapture) -> Result<Transcript>;
}
// (returns transcript + writes slides.yml as a side artifact)
```

CLI additions:

- `borg replay <trace_id>` (existing) automatically re-runs frame-aware steps if `slides.yml` is missing or older than current code version.
- `borg replay <trace_id> --focus 02:15-02:45` (new) re-runs Stage 0 frame extraction at focused-mode budget over a subrange. Slide segmentation runs on the focused frames.
- `borg ingest --no-frames <url>` (new) skips frame extraction entirely for one ingestion. Useful when a user knows up-front that frames are not relevant.

### Implementation Plan

Each phase is shippable on its own. Phase 1 alone delivers the richer Fabric input (better summaries) without any vault-shape change. Phase 2 adds vault embedding.

#### Phase 1.1: Stage 0 frame extraction
**Model:** sonnet
- Add `extract_frames` to `borg/src/youtube.rs` (ffmpeg invocation with mpdecimate + auto-fps + budget cap)
- Auto-fps table copied from `claude-video/scripts/frames.py:94-110`
- Stage 0 writes `raw/<trace_id>/frames/frame_%04d.jpg` and `raw/<trace_id>/frames.yml` sidecar
- Config wiring: `youtube.slides.*` defaults; master `enabled: true` flag
- Tests: golden-file test on a sample slide-deck mp4 fixture, asserting frame count and timestamp accuracy

#### Phase 1.2: Slide segmentation + OCR + binding
**Model:** opus
- New module `borg/src/slides.rs` (segment, OCR-and-caption, bind, manifest IO)
- pHash via `image_hasher` crate; cluster by Hamming distance ≤ 6
- Transition drop at `< 5s`
- OCR via existing `ocr::ocr_extract` per slide (parallel, rayon) - free, local, always on
- Vision via existing `ocr::vision_extract` per slide is wired but **disabled by default in Phase 1** (`vision_per_slide: false`). Until Phase 2 lands the vault-embed payoff, paying for ~15 vision-API calls per video to marginally enrich a text-only summary is poor unit economics. Tesseract is the only OCR signal feeding Stage 2 in Phase 1. Vision flips on in Phase 2 alongside vault embedding, where the captions also become useful as wikilink alt text.
- Transcript binding by VTT segment range filter
- Emits `transcripts/<trace_id>/slides.yml` + canonical `slides/slide-NNN.jpg` files
- Tests: fixture-based segmentation test (synthetic 5-slide deck), OCR mock, VTT binding correctness

#### Phase 1.3: Slide-aware Fabric pattern
**Model:** opus
- New pattern `borg/patterns/obsidian-youtube-slides.md`
- Pattern input: rendered markdown from `slides.yml`
- Pattern output: YAML frontmatter (`shape`, `embed_slides`, `sections`) + body
- Branching by `note_shape` is in the pattern (output structure differs by shape); enforcement of the downgrade-only rule is in Stage 3 Rust (validates `embed_slides` against `proposed_note_shape` before any vault write)
- Stage 2 wiring: when `shape != text-only`, route summarization through the new pattern; when `text-only`, existing pattern unchanged
- Tests: deterministic-input pattern run against a captured `slides.yml`, asserting output shape

After Phase 1.3, frame-aware ingestion produces better summaries via Fabric. The note in the vault is still text-only. This is a clean ship boundary - measure summary quality lift before committing to Phase 2.

#### Phase 2.1: Vault attachment publish path
**Model:** sonnet
- Stage 3 reads the Fabric output's frontmatter; for `hero` and `slide-section`, copies selected slides into `<vault>/system/attachments/images/<YYYY-MM>/<slug>-slide-NNN.jpg`
- Atomic write (tmpfile + rename)
- Wikilink embeds in note body
- `slides:` frontmatter list of vault-relative attachment paths
- Tests: end-to-end fixture from `slides.yml` through to a published note + attachment files in a tempdir vault

#### Phase 2.2: Replay/reingest cleanup
**Model:** sonnet
- On reingest of an existing slug (existing reingest-domain-preservation flow): read old note's `slides:` list, delete those files via `rkvr rmrf`, write new note + new slides
- Tests: replay test that asserts orphan-free vault state after re-ingestion

#### Phase 3: Side improvements (independent of frame work)
**Model:** sonnet
- Port Whisper retry + Cloudflare-WAF UA workaround from `claude-video/scripts/whisper.py:148-217` into `borg/src/transcription.rs::try_groq`
- Port rolling-prefix VTT dedupe from `claude-video/scripts/transcribe.py:55-67` into `borg/src/youtube.rs::clean_vtt`
- Switch borg's Whisper-targeted audio extraction to `-vn -ac 1 -ar 16000 -b:a 64k` (claude-video's tuning) for size-bounded uploads
- Tests: retry behavior with mocked HTTP server returning 429+Retry-After, dedupe test on rolling-prefix VTT input

These three are independent of Phases 1-2 and can ship anytime.

## Alternatives Considered

### Alternative 1: Vision API on raw frames, no dedupe

**Description:** Send all 100 frames per video to Anthropic's vision API; let the LLM see everything and figure out what mattered.

**Pros:** Simplest implementation. No mpdecimate / pHash code path. No threshold tuning.

**Cons:** ~5x more vision API calls per video on slide-heavy content. Many of those calls operate on near-duplicate frames competing for the model's attention, with no information gain. Cost scales linearly with video length, defeating the cost-bounding goal.

**Why not chosen:** Cost discipline. The dedupe step is cheap (microseconds per frame) and dramatically reduces both cost and noise on the content shapes most worth ingesting. Brad's claude-video repo punts on this because it's a synchronous user-facing skill and the user is paying per invocation; borg is a daemon and over hundreds of ingestions the savings compound.

### Alternative 2: Time-anchored YouTube embeds instead of stills

**Description:** Instead of extracting and embedding frames, emit Obsidian-flavored YouTube embeds with `?t=<timestamp>` per slide range, letting the user click to play that section.

**Pros:** No vault binary growth. Always the source-of-truth video.

**Cons:** Offline reading breaks (no internet, no slides). Mobile reading breaks (Obsidian iframe rendering is iffy). Search is gone (oracle and Obsidian full-text search can't index frames they don't have). No graceful degradation when YouTube takes the video down.

**Why not chosen:** The vault is the second brain's persistent memory; it must work offline and survive source-link rot. Stills are the durable form.

### Alternative 3: Per-note folder layout

**Description:** Vault layout `vault/notes/<slug>/<slug>.md` + `vault/notes/<slug>/slide-NNN.jpg`, instead of `vault/system/attachments/images/<YYYY-MM>/`.

**Pros:** Cleanup is one folder removal. Self-contained note unit. Easy to copy or share a single note + its assets.

**Cons:** Diverges from borg's existing image-attachment convention (`assets::store_asset` at `borg/src/assets.rs`, called from `pipeline.rs:794`). Existing 100+ ingested-image notes live under `system/attachments/images/<YYYY-MM>/`; mixing two conventions is worse than either alone. Obsidian's "default attachment folder" setting in `app.json` would need a per-note override that Obsidian doesn't support.

**Why not chosen:** Consistency with existing convention. Cleanup-via-frontmatter-list is only marginally more complex than cleanup-via-folder-rm.

### Alternative 4: All slides embedded, no LLM selection

**Description:** Skip the "embed_slides" output from Fabric; embed every unique slide.

**Pros:** Simpler pipeline; deterministic note shape.

**Cons:** A 30-min talk with 25 unique slides produces a note with 25 images inline. Reading load is heavy; the note loses the "summary as scaffolding" property and becomes a slide-show transcription.

**Why not chosen:** The LLM-selected subset (typically 5-10 slides) is the right granularity for a notes-app summary. The slides not embedded are still in staging for replay if a richer note is wanted later.

### Alternative 5: Defer slide work entirely; stay text-only

**Description:** Don't build this. Transcript-only is good enough for most purposes; users who want visual context can click through to YouTube.

**Pros:** Zero work. No vault size growth. No threshold tuning.

**Cons:** Concedes the central insight - that visual content carries roughly half the signal on slide-heavy content. Once a user sees a frame-aware note next to a transcript-only note for the same talk, the gap is obvious. The existing claude-video skill's reception in the AI/automation YouTube community confirms the demand signal.

**Why not chosen:** The marginal lift is large for the content shapes most worth ingesting. Phase 1 alone (without vault embedding) is low-cost and ships incremental value.

## Technical Considerations

### Dependencies

External:
- `ffmpeg` (already required) - mpdecimate filter is built-in, no new install
- `tesseract` (already required) - OCR for slides reuses existing path
- `image_hasher` Rust crate (new) - ~5 KB pure-Rust, no native deps

Internal:
- Existing `ocr.rs` (tesseract + Anthropic vision) - reused unchanged
- Existing staged-pipeline modules (`stages/raw`, `stages/extract`, `stages/summarize`, `stages/artifact`) - extended

External APIs:
- Anthropic vision API - existing config/secret path; cost-gated per slide

### Performance

Per ingestion of a 30-min slide-heavy video:

- Stage 0 video download: today's pipeline uses `--no-download` for metadata and `--skip-download` for subtitles; the staged-pipeline shape requires the full video on disk so frames can be extracted (and so replay works offline against persisted bytes). At 720p that is roughly 100-300 MB depending on encoding, ~30-90s wall time on a typical home connection. This is the dominant cost of Stage 0, not the ffmpeg work.
- Stage 0 frame extraction: ~3-5s of ffmpeg wall time once the video is on disk (mpdecimate is cheap; the real cost is the video decode)
- Stage 1 segmentation: pHash over ~80 frames ≈ 50 ms; clustering ≈ negligible
- Stage 1 OCR (tesseract) over ~15 unique slides: ~3-8s wall time, parallelized across rayon threads
- Stage 1 vision API over ~15 slides: ~10-20s wall time, parallelized via tokio (single-shot per slide)
- Stage 2 Fabric pattern: ~5-15s, single LLM call as today
- Stage 3 publish: ~50 ms (file I/O)

Total added latency per slide-heavy ingestion: ~15-30s. Acceptable for a daemon doing background work.

For text-only / talking-head content: the dedupe collapses to ~1-3 unique slides, vision/OCR adds 1-3s, and Stage 3 emits no attachments. Effectively no overhead beyond Stage 0's frame extraction (~3-5s).

### Storage

- Stage 0 frames: ~100 frames × 30 KB ≈ 3 MB per video in staging. Ages off per the existing 30-90 day retention.
- Stage 1 canonical slides: ~15 × 30 KB ≈ 450 KB per video in staging.
- Vault attachments: ~5-10 selected slides × 30 KB ≈ 150-300 KB per slide-heavy note. At 100 such notes ≈ 15-30 MB total. Manageable; vault remains under 100 MB for the foreseeable future even with significant growth.

### Security

- No new external API surface. Vision API path reuses existing Anthropic-key handling.
- All frames live under user's home directory; no network exposure.
- Atomic file writes prevent half-synced binary content from leaking through Obsidian Sync / iCloud / Syncthing during partial writes.

### Testing Strategy

- Unit tests for each Stage 1 helper (pHash clustering, transition drop, transcript binding, manifest serialization)
- Golden-file tests for Stage 0 ffmpeg invocation against a synthetic slide-deck fixture (`tests/fixtures/slide-deck-30s.mp4` containing 5 known slides at known timestamps)
- Integration test: full pipeline run from a fixture mp4 + fixture VTT through to a published note in a tempdir vault, asserting:
  - `slides.yml` has the expected slide count and timestamps
  - Vault contains exactly the expected slide JPGs
  - Note frontmatter `slides:` list matches the vault attachment paths
  - On reingest, old slides are removed and new ones are present
- Cost-mocked Fabric runs (`fabric --dry-run` or fixture-replay) for Stage 2

### Rollout Plan

1. Phase 1.1 / 1.2 / 1.3 ship behind `youtube.slides.enabled: true` in config (default true). The flag exists primarily as a safety lever: if Stage 0 ffmpeg-on-decode causes systemd-borg memory issues on long videos, set to `false` and revert to text-only behavior without rolling back the binary.
2. After Phase 1 ships, observe Fabric summary quality lift via spot-checks on 5-10 newly ingested videos. If insufficient lift, tune the Fabric pattern before proceeding to Phase 2.
3. Phase 2 ships behind `youtube.slides.publish_to_vault: true` (default false). Enable per-user once Phase 1 is stable for ~2 weeks.
4. Phase 3 (side improvements) ships independently and immediately - all are pure reliability improvements with no vault-shape impact.

### Cortex compatibility

The new `slides:` frontmatter list is a list of strings (paths). cortex's existing field handling treats unknown fields as opaque pass-through; nothing in cortex parses or validates `slides:` today and the addition is non-breaking. Add a note to the schema doc that `slides:` is a borg-owned field (cortex must not modify it).

oracle's full-text search indexes note bodies and frontmatter; embedded wikilinks and the `slides:` list are searchable as text. The OCR'd text from slides lives in the body (rendered into the slide-section summary), so on-slide content becomes findable via oracle's `knowledge_search` without any oracle change.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| ffmpeg OOMs on a long 4K video | Low | Medium | Frame budget cap (100 frames) + scale to 512px in the same filter chain bounds memory. Set ulimit on the daemon if needed. |
| pHash misclusters animated slides as N distinct slides | Medium | Low | Threshold is config-tunable. Worst case is over-counting unique slides, which inflates vision cost on that one video; LLM still selects representatives. |
| Anthropic vision API cost balloons | Medium | Medium | Per-slide cost gate (`vision_per_slide: false` falls back to OCR-only). Compression ratio bounds slide count; frame budget bounds frame count. Daily-spend telemetry already exists. |
| Wrong note shape from threshold edge case | Low | Low | Thresholds are config-tunable. Replay command can re-run with overridden thresholds. |
| Reingest cleanup fails halfway, leaving orphan attachments | Low | Low | rkvr archive, not destructive delete. Quarterly cortex sweep can find unreferenced attachments. |
| Vault binary growth surprises user | Low | Low | Default thresholds err toward fewer embeds. Operator can set `publish_to_vault: false` to disable Phase 2 entirely. |
| Vision API returns hallucinated captions for low-text slides | Medium | Low | Captions are advisory input to Fabric, not user-facing text. Fabric's prose summary is what users read. |
| Existing legacy `process_youtube` path stays live and skips frame work | Medium | Medium | Make this design's enablement contingent on staged-pipeline migration of `YoutubeUrl` (already in flight). Document the dependency in Phase 1.1. |
| mpdecimate misses motion-graphic slides; budget burns on near-dupes | Low | Low | pHash post-pass catches what mpdecimate misses. Worst case is wasted vision calls on one ingestion; bounded by frame cap. |

## Open Questions

- [ ] Does `image_hasher` produce sufficiently stable hashes across re-encodes (yt-dlp's mp4 vs original webm)? Empirical test on 3-5 real videos before committing to threshold.
- [ ] Should `caption` (vision API) gate on tesseract returning empty/low-confidence output, or always run? Argument for always-run: vision and OCR capture different signals (caption describes layout/diagrams, OCR captures text). Argument for gated: cost. Default to always-run with a config flag.
- [ ] What happens when a video has no audio at all (silent screencast)? Transcript path returns nothing. Fabric pattern needs to handle "summarize from slides + OCR alone." Probably falls out for free, but worth a fixture test.
- [ ] For `hero` shape, which slide does the LLM pick - the first content slide or the most "representative"? Pattern instruction needs to be specific. Probably "the slide that best represents the talk's central topic" with a fallback to "the first non-title slide."
- [ ] The 5s transition-drop threshold: tunable via config but is the default right? On fast-paced content (lightning talks), real slides may dwell <5s. Watch for false negatives on 1-2 minute videos.
- [ ] Does cortex need a new "slide attachment integrity" check (note's `slides:` list refers to files that exist)? Probably yes, eventually, but out of scope for this doc. Logged for cortex backlog.

## References

- [2026-04-19-staged-ingestion-pipeline.md](2026-04-19-staged-ingestion-pipeline.md) - the staged pipeline this builds on
- [2026-03-22-youtube-metadata-pipeline-redesign.md](2026-03-22-youtube-metadata-pipeline-redesign.md) - the unified `process_youtube` path being extended
- [bradautomates/claude-video](https://github.com/bradautomates/claude-video) - prior art for in-session frame extraction; specific files cited inline
  - `scripts/frames.py:94-131` - auto-fps budget tables (lifted)
  - `scripts/transcribe.py:55-67` - rolling-prefix VTT dedupe (lifted in Phase 3)
  - `scripts/whisper.py:148-217` - retry policy + UA workaround (lifted in Phase 3)
- `notes/my-claude-code-can-instantly-watch-any-video-heres-how.md` - the ingested source video that prompted this design
- ffmpeg mpdecimate filter docs: <https://ffmpeg.org/ffmpeg-filters.html#mpdecimate>
- `image_hasher` crate: <https://crates.io/crates/image_hasher>
