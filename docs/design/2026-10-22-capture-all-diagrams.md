# Design Document: Capture All Diagrams

**Author:** Scott Idler
**Date:** 2026-10-22
**Status:** Approved
**Review Passes Completed:** 5/5

## Summary

Borg sees the diagrams in X posts and web articles and throws them away. X photos get downloaded and described, then the bytes die; X Article figures and web-article images never get downloaded at all. This doc stores every X post photo, and every diagram in an X Article or web article, as a vault asset, embeds it in the note under an `## Images` section with its description, and rebuilds the existing X notes.

## Problem Statement

### Background

- `docs/design/2026-10-19-x-post-fetcher.md` built X notes from post text, quoted text, and each photo's Vision + OCR description. It parked two items:
  - **Storing photos as vault assets** (line 64): "Describe only. Revisit if Scott wants the image visible in Obsidian."
  - **Images inside X Articles** (line 63).
- Thread and article notes carry no in-note `## Transcript` (2026-07-07 and 2026-10-19 Resolved Decision 2). The description of a photo lives only in staging (`transcript.md`, `distilled.yml`).
- Image ingest (`borg/src/pipeline/handlers.rs:771-937`) and YouTube slides (`borg/src/slides/publish.rs:44-112`) already store images under `system/attachments/images/YYYY-MM/` and embed them as `![[basename]]`.

### Problem

Scott, 2026-10-22, on the vic note (trace `cl-8c531660`): "why didnt the resulting obsidian note get the image, diagram, graphic that came with this post?" then "we definitely want to capture all diagrams", scope "X plus web articles".

Measured on desk, 2026-10-22:

- **X post photos**: `describe_photo` (`borg/src/pipeline/photos.rs:80-125`) downloads the bytes at `:81`, hands them to OCR and vision, and returns only `section: String`. `DescribedImage { owner, section }` (`borg/src/x.rs:100-103`) has no slot for bytes. The URL note is built with `asset_path: None` (`borg/src/pipeline.rs:1031`). 0 of 34 `type: social` notes embed an image; 27 of the 34 posts carry 34 photos.
- **X Article figures**: `render_blocks` drops `atomic` blocks (`borg/src/x.rs:274`). The serde types (`x.rs:69-88`) never deserialize `entityRanges`, `entityMap`, or `media_entities`. The UberEng Article (`2106071967619322330`) has 6 figures, 0x_kaize's (`2048059417661174052`) has 2.
- **Web articles**: readability (`dom_smoothie` -> `htmd`, `borg/src/readability.rs:200-230`) keeps `![alt](url)`. Nothing downloads, describes, or embeds them, and the distiller sees only a bare URL. Across staged `fetched.html` (latest trace per source): 638 sources, 358 with images, 2696 image refs; per note p50 4, p90 14, max 120. The 4 article notes with a `![` embed are 3 `[yaml-parse-error]` fallbacks plus 1 hand-written note, so no code path produces an article embed today.

### Goals

Every goal traces to Scott, 2026-10-22.

- Every diagram attached to an X post, inside an X Article, or inside a web article lands in the vault as a file and shows in the note ("capture all diagrams", "the image, diagram, graphic that came with this post").
- One path for all three sources: download -> filter -> describe -> store -> embed. Siblings cannot drift.
- Existing X notes rebuilt to the new shape (the 2026-10-19 rollout precedent).

### Non-Goals

Excluded:

- Decorative images in web articles: avatars, logos, icons, tracking pixels, ads, hero photos. "Diagrams" is the ask; storing every article image would multiply the vault's binary weight for no value.
- Video in X posts (stays parked by the 2026-10-19 doc).

Parked, with revisit condition:

- **X Article `cover_media`.** A header image, decorative in all 3 Articles probed. Revisit if one is a diagram.
- **AVIF images** (2 of 2696 refs). The `image` crate's `avif` feature here is encode-only; decoding needs `avif-native` (dav1d, a C dep). Dropped with reason `unsupported-format`. Revisit if the drop count in `sb borg log` grows.
- **`data:` URI images** (64 refs). Overwhelmingly lazy-load placeholders. Dropped by the prefilter. Revisit if a probe finds a diagram inlined this way.
- **Orphan-attachment lint in cortex.** No such lint exists. This doc removes its own orphans at reingest (Phase 1). Revisit if `system/attachments` grows files no note references.
- **The `[yaml-parse-error]` fallback that leaks raw page markdown into `## Summary`** (3 notes). A separate bug in `fallback_distilled`; its own targeted fix.

## Proposed Solution

### Overview

```
  X post photos ─┐
  X Article figs ├─> figures::collect ─> prefilter ─> classify ──> cap ──> describe ─> FigureSet
  article ![](u) ┘   (download, byte    (format;     (vision;     (max-   (vision +      │
                      ceiling)           articles:    X Article    per-    OCR)           │
                                         heuristics)  + articles)  note)                  │
          distiller input <── description spliced in position ────────────────────────────┤
          (claims ground in the diagram; lands in distilled.yml transcript)                │
                                                                                          │
  plan paths ─> render full note (images: + ## Images) ─> size gate ─> write assets ─> publish
                                                                        (no-clobber)     │
  after durable publish:  old images: minus new images: -> rkvr                         │
  publish fails:          files THIS run created -> rkvr                                ┘
```

### Architecture

**One module, `borg/src/pipeline/figures.rs`,** grown out of today's `pipeline/photos.rs` (which it replaces). It owns:

- `Candidate { url, alt, caption, origin }`: `origin` is `XPost(Owner) | XArticle | Article`.
- `Figure { candidate, bytes, mime, width, height, category: Option<SlideCategory>, section }`: the kept image, its bytes, and its built Description / Extracted Text section (`image_section`, unchanged).
- `collect(candidates, cfg) -> FigureSet`: download (`max-candidates`, each capped at `max-download-bytes`), prefilter, classify, cap at `max-per-note`, **then** describe. The cap runs before describe, so vision calls per note are at most `max-candidates` (classify) + `max-per-note` (describe). Drops are counted by reason and written as one `info` log line per trace (`figures: kept=N dropped={prefilter:a, not-diagram:b, cap:c, error:d}`). Receipts keep their single `degraded` boolean; no schema change.
- `plan(&FigureSet, trace_id, now) -> Vec<PlannedFigure>`: assigns each kept figure its vault path. Pure, no I/O.
- `store(vault_root, &[PlannedFigure]) -> Vec<PathBuf>`: the only writer. Runs after the size gate passes on the fully rendered note. Returns only the files it created.

**Candidates per source:**

- **X post photos**: `x::photos()` (`x.rs:163`) as today, `origin: XPost(owner)`.
- **X Article figures**: deserialize `blocks[].entityRanges`, `entityMap` (in fxtwitter a **list** of `{key, value}`, not a Draft.js dict), and `media_entities`. Resolve each `atomic` block: `entityRanges[0].key` -> `entityMap` entry with the same key (as a string) -> `value.type == "MEDIA"` -> `value.data.mediaItems[].mediaId` -> `media_entities[].media_id` -> `media_info.original_img_url`, caption from `value.data.caption`. Proven 8 of 8 on the two live Articles. An unresolvable atomic block is skipped and logged, never an error.
- **Web articles**: parse `![alt](url)` from the fetched markdown (readable, fabric, jina all emit it). Substack's lazy-image markup comes through as `![alt](<https://host/p/{"src":"https://substack-post-media...",...}>)` (19 refs over 4 traces): extract the inner `src`. Relative URLs resolve against the page URL. HN (`hn-api`) carries no images.

**Prefilter (deterministic, no network beyond the download):**

Format and safety checks apply to every source:

- Keep only png, jpeg, gif, webp, and svg: the formats the vision API accepts plus svg (rasterized below). `data:` URIs, avif, bmp, tiff, and anything else are dropped with reason `unsupported-format`. bmp/tiff were 0 of 2696 refs; not worth a transcode path.
- Downloads stop at `max-download-bytes` (default 20 MiB); over it is a drop, reason `too-large`. `download_photo` today reads the whole body with no ceiling (`photos.rs:148`).
- Exact duplicates within the note by sha256 are dropped. No perceptual-hash near-dup: two charts that differ only in their numbers would collide. Revisit if Phase 0 or a live note shows duplicates.

Content heuristics apply to **web-article images only** (X photos and X Article figures are author-placed; see Classify):

- Drop on URL tokens that only ever name decoration. Match whole tokens of the host and path split on non-alphanumerics, never substrings (`icon` must not hit `silicon`): `avatar`, `avatars`, `gravatar`, `authors`, `logo`, `logos`, `icon`, `icons`, `favicon`, `emoji`, `pixel`, `badge`, `badges`; hosts `yt3.ggpht.com`, `img.shields.io`.
- Drop under `min-side-px` (default 200) on either axis, read from the decoded header.
- **Alt text is not a signal.** martinfowler: 7 of 7 diagrams have empty alt; chroma context-rot: 43 charts, all empty alt.

**SVG:** 151 refs. Many are diagrams (mermaid and draw.io exports), many are icons. The vision API takes jpeg/png/gif/webp only. Rasterize with `resvg` (`cargo add resvg`), scaled so the longer side is at most `max-side-px` (an SVG's viewBox can be arbitrarily large), classify the raster, and store the **original `.svg`** (Obsidian renders it). The prefilter's size and keyword rules apply to the raster.

**Classify:**

- `ocr::vision_classify` (`borg/src/ocr.rs:252`) with a new `FIGURE_TAXONOMY_PROMPT` beside `TAXONOMY_PROMPT` (`borg/src/slides/classify.rs:76`). The slide prompt talks about webcam insets and video frames; a figure on a page needs its own wording. Same `SlideCategory` enum, same output schema, same fail-closed `ClassifyError`. The prompt is new; Phase 0 is its eval.
- Keep if the category is in `images.content-filter.keep` and confidence >= `min-confidence`. A classify error drops the image and marks the receipt degraded.
- **Web-article images and X Article figures are classified.** An X Article has no 4-image bound (UberEng carries 6) and its figures can be cover-style art, the same population as a web article.
- **X post photos skip classify.** A post has at most 4 photos, each chosen by the author, and they are often the whole content (the vic flowchart is a quoted-post photo). The 2026-10-19 doc already describes every one; this doc only stops discarding them.

**Describe:** the existing `describe_photo` path (OCR under `spawn_blocking` joined with `vision_extract`, `image_section` builder), run only on images that survived the cap. Two vision calls per kept classified image (classify, then describe) over one combined call: `vision_extract`'s prompt is proven, the classify prompt is evaluated in Phase 0, and only kept images pay the second call.

- Each figure's section is capped at `max-section-bytes` (default 1500), truncated at a line boundary with a trailing `…`. `image_section` has no cap today, and an OCR dump of a dense chart would otherwise spend the note budget.

**Vision concurrency:** all vision calls share the one process-wide `VISION_PERMITS` pool, sized once at startup from `youtube.slides.content-filter.max-vision-concurrency` (`borg/src/startup.rs:41-42`; `init_vision_permits` ignores later calls). The `images:` filter therefore carries no concurrency knob of its own: a second knob over the same pool would be a name that lies.

**Distiller input:** each kept figure's section is spliced into the source markdown **at the image's position** (X: under its owning post, as `push_images` does today; X Article: at its atomic block; article: replacing its `![alt](url)` line). The distiller grounds claims in the diagram.

- `gate_article_transcript` (`borg/src/stages/distill.rs:391`) runs `transcript_quality_ok` on the transcript. Short OCR lines from figures could push a good readable article over the short-line ratio and clear its whole transcript. So the quality check runs on the source **before** the splice; the stored transcript is the spliced one.
- A non-clean article source (fabric, jina, browser) has its transcript cleared by that gate today. The figure descriptions still reach the note body (`## Images`) and so FTS; they do not reach transcript embeddings for those sources. Accepted: same as every other word of those pages.

**Store and publish ordering** (fixes the order the size gate sees):

1. `plan` assigns every kept figure its path before anything is written.
2. The full note renders with `images:` and `## Images`.
3. `note_size_gate` (`pipeline.rs:181`, called right before `archive_then_publish`) checks the full rendered note. A fail publishes nothing and writes no asset.
4. `store` writes each file with `vault::note::write_atomic_new` (`vault/src/note.rs:182`, `persist_noclobber`, returns `Ok(false)` on an existing path).
5. `archive_then_publish`. If it fails, only the files `store` created in step 4 go through `rkvr::remove`; a reused path is never touched.

- **Path:** `system/attachments/images/YYYY-MM/<trace-id>-<n>-<sha8>.<ext>`, `YYYY-MM` from the ingest time. The trace id is unique per ingest, so two notes never share a file and one note's cleanup can never remove another's image. A note title is not unique (`hygiene::note_filename`, `pipeline.rs:983`; `publish_unique` exists because different sources share titles), so the slug cannot carry ownership.
- **Oversize:** an image over `max-bytes` (default 2 MiB) is downscaled to `max-side-px` (default 2048) and re-encoded: jpeg -> jpeg q85, everything else -> png (the `image` webp encoder is lossless-only; a gif keeps its first frame). Still over the cap -> dropped, receipt degraded. An SVG is never resized.
- `rkvr::remove` falls back to a plain delete when `rkvr` is not on PATH (`borg/src/rkvr.rs`), the same as slides today.

**Note shape:** a new `## Images` section at the end of the distilled body, before the `---` / Source footer:

```markdown
## Images

![[cl-8c531660-1-3fa2c8d1.jpg]]

*In @boristane's post.*

#### Description
A flowchart depicting a code review and deployment process...

#### Extracted Text
Agent generates code
...
```

- One sub-block per figure, in source order. The provenance line is the X owner (`In @handle's post`), the X Article caption, or the article alt text when non-empty.
- The description stays in the body so oracle FTS (`vault/src/search/index.rs:304` indexes the body only) can keyword-match text inside a diagram. Cost: at most `max-per-note` x `max-section-bytes` (20 x 1500 = 30 KB) against the 64 KiB note gate (`MAX_NOTE_BYTES`, `config.rs:384`), and the gate now sees it.
- The section name is `## Images`, not `## Figures`: an X photo is often a screenshot, and the name has to be true for every source.
- **Truncation is intended.** A page with more kept diagrams than `max-per-note` keeps the first `max-per-note` in source order; the rest are a `cap` drop in the log line, not degraded. chroma context-rot's 43 charts keep 20.

**Reingest cleanup:** generalize `borg/src/slides/cleanup.rs` (`read_old_slides_frontmatter` -> `compute_orphans` -> `rkvr::remove`) to read both `slides:` and `images:`, captured before replace (`pipeline.rs:684-686`) and run after the new note is durable (`:1182-1195`). Slides keep their own key and behavior.

**A degraded reingest never replaces a note that owns images.** If the old note has a non-empty `images:` and this run's image stage is degraded (a candidate that passed the prefilter failed download, classify, describe, or store), the trace fails `fetch-failed` and the old note and its files stay untouched. A fresh ingest (no old note) publishes degraded as usual. Same rule as the 2026-10-19 doc's image-only post: never publish a worse note over a useful one.

### Data Model

- **Frontmatter `images:`**: a list of vault-relative paths (`system/attachments/images/2026-10/...`), the files this note owns. Mirrors `slides:`.
  - Added to `RENDER_NOTE_KEYS` (`borg/src/markdown.rs:119`); the `render_note_keys_matches_the_writer` test enforces it.
  - Not in `BORG_PRESERVE_KEYS` (`markdown.rs:149`): a reingest regenerates it.
  - Not `asset:`: that key is single-valued and means "this note IS the asset."
- **`NoteContent`** (`borg/src/markdown.rs:9-48`) gains `images: Vec<String>`.
- **`FetchedPost`** (`handlers.rs:649-653`) carries `figures: FigureSet` instead of pre-rendered sections only.
- **Receipt:** `fetch_degraded` (`pipeline.rs:744`) is set when any candidate that passed the prefilter fails download, classify, describe, or store. Prefilter drops and classifier "not a diagram" verdicts are expected and do not degrade.

### API Design

Config, a new top-level `images:` block in `borg.yml`, documented in `config/templates/borg.yml.example` beside `youtube.slides`:

```yaml
images:
  enabled: true
  max-candidates: 40          # images downloaded per note, in source order
  max-download-bytes: 20971520  # one image over this is dropped before decode
  max-per-note: 20            # images kept per note, cap applied before describe
  max-section-bytes: 1500     # description + extracted text per image in the note
  min-side-px: 200            # web articles: either axis under this is decoration
  max-bytes: 2097152          # stored file over this is downscaled to max-side-px
  max-side-px: 2048           # also the SVG raster bound
  content-filter:             # web-article images and X Article figures; X post photos are always kept
    enabled: true
    keep: [architecture-diagram, sequence-diagram, flowchart, chart, infographic]
    model: ""                 # empty = inherit llm.model
    min-confidence: 0.6
  # Vision concurrency is the shared pool, youtube.slides.content-filter.max-vision-concurrency.
```

- `content-filter` is a new `FigureFilterConfig { enabled, keep, model, min-confidence }` that reuses `SlideCategory` and its parser (`borg/src/config.rs:650-700`): one taxonomy, unknown categories are a load error. It does not reuse `ContentFilterConfig`: that struct's defaults are `enabled: false, keep: [architecture-diagram]` (`config.rs:~770`) and it carries the pool knob. `FigureFilterConfig` has its own serde default fns equal to the block above, so an omitted or partial `images:` block yields exactly these values.
- `keep` default includes `chart` and `infographic`: "all diagrams" covers chroma's 43 data charts and the explanatory one-pagers; `app-ui`, `webpage`, `code`, `terminal`, `talking-head`, `b-roll`, `title-card`, `other` stay out.
- `enabled: false` restores today's behavior: X photos described but not stored, articles untouched. A reingest under `enabled: false` writes `images: []`, so cleanup moves the old note's images to rkvr: the frontmatter and the files never disagree.

### Implementation Plan

#### Phase 0: Measure the article filter
**Model:** sonnet
- Zero repo code. Throwaway harness in the session scratchpad.
- Sample 40 recent `readable` article stages with image refs. Download every ref with `BROWSER_UA`. Record fetch success, MIME mix, dimension distribution, and the bytes the Phase 4 prefilter keeps.
- Add the 8 X Article figures (UberEng 6, 0x_kaize 2) to the sample.
- Hand-label 60 prefilter survivors as keep-category or not. Run a draft `FIGURE_TAXONOMY_PROMPT` through `vision_classify` on them.
- Rasterize 10 sampled SVGs with `resvg`'s CLI and classify them.
- Paste the numbers into this doc under Phase 0 Results. No files under `docs/` besides this one.
- **Success criteria:** keep-category precision >= 0.9 and recall >= 0.8 on the 60 labels with `min-confidence: 0.6`; fetch success, MIME mix, and kept-bytes p50/p90 per note recorded in this doc. A miss stops the build: the prompt is revised and re-measured before Phase 5.

#### Phase 1: Config and owned-images plumbing
**Model:** sonnet
- The whole `images:` config block (`ImagesConfig`, `FigureFilterConfig` with its own defaults) and its annotated example. Every later phase reads it.
- `images:` frontmatter on `NoteContent`, `RENDER_NOTE_KEYS`, `render_note`.
- Generalize `slides::cleanup` to `images:` + `slides:`.
- `figures::plan` and `figures::store` (`write_atomic_new`, downscale over `max-bytes`); the publish ordering above (plan -> render -> size gate -> store -> publish); rkvr only the files `store` created on publish failure.
- The degraded-reingest rule: old note owns images + degraded image stage -> `fetch-failed`, nothing replaced.
- **Success criteria:**
  - `render_note_keys_matches_the_writer` passes with `images` added; an omitted `images:` block and a block with only `enabled: true` both deserialize to the documented defaults; `keep: [diagram]` fails config load.
  - Publish-path tests through the note writer, not `process_x_post_from`: a reingest that drops one of two images leaves the kept file and moves the other to rkvr; a note over the size gate because of `## Images` writes 0 files under `system/attachments`; a publish failure removes the created file and leaves a pre-existing reused path in place.
  - A reingest over a note with `images:` whose image stage is degraded returns `fetch-failed` and the old note's bytes and files are unchanged.

#### Phase 2: X post photos stored and embedded
**Model:** sonnet
- Move `photos.rs` into `figures.rs`; `describe_photo` returns the bytes and MIME with the section; downloads honor `max-download-bytes`.
- `FetchedPost` carries the `FigureSet`; `process_url_inner` plans, renders `## Images`, gates, stores, publishes.
- **Success criteria:** a publish-path test on the vic fixture yields a note with exactly one `![[` line under `## Images`, `images:` of length 1, and that file present under `system/attachments/images/`; a photo-download-error test yields no `## Images` section and `fetch_degraded == true`.

#### Phase 3: X Article figures
**Model:** opus
- serde for `entityRanges`, `entityMap` (list), `media_entities`; atomic -> media resolution; captions; `render_blocks` emits the figure section at its atomic block; X Article figures go through classify.
- Fixture trimmed from the live UberEng JSON under `borg/src/x/` (the existing `article.json` lost these fields in its trim).
- **Success criteria:** the UberEng fixture yields 6 candidates in block order, each with its `Figure N` caption; an atomic block with no matching entity is skipped and the render still succeeds; with stubbed vision labeling one figure `title-card`, that figure is not kept.

#### Phase 4: The deterministic prefilter and article candidates
**Model:** sonnet
- Pure `prefilter(source, url, mime, dims, bytes) -> Keep | Drop(reason)`: format checks for every source, content heuristics for web articles only; sha256 dedupe.
- Candidate parsing from fetched markdown: `![alt](url)`, Substack `src` extraction, relative URL resolution against the page URL.
- SVG rasterize via `resvg`, raster bounded by `max-side-px`, href resolver locked down.
- **Success criteria:**
  - A table test drops the 90x90 XDA avatar, a 420px `logo` URL, a `data:` URI, an `.avif`, and a `.bmp`; keeps the martinfowler `platform-teams-stuff-done` diagram and a URL containing `silicon`; keeps a 150px X post photo (content heuristics do not apply to X).
  - An SVG embedding a PNG fixture by local path renders that PNG under usvg's default resolver and renders it absent under the locked resolver (the test fails if the lockdown is removed).
  - An SVG with a 100000-unit viewBox rasterizes at <= `max-side-px` on its longer side.

#### Phase 5: Web-article figures
**Model:** opus
- Parse candidates from fetched markdown on the readable, fabric, and jina paths; `FIGURE_TAXONOMY_PROMPT` (the Phase 0 version); classify -> cap -> describe -> splice in position before distill; `transcript_quality_ok` runs on the pre-splice source.
- Vision seam stubbed in tests through `ocr::bases`.
- **Success criteria:**
  - With stubbed vision labeling 3 images `flowchart`, `app-ui`, `chart`, the note keeps exactly the flowchart and the chart.
  - With 30 candidates all labeled `flowchart` and `max-per-note: 20`, the stub records exactly 30 classify and 20 describe calls.
  - A classify error drops that image and sets `fetch_degraded`; a readable article fixture whose transcript passes the quality gate still passes it after 20 figure sections are spliced in.

#### Phase 6: Distill patterns treat figures as evidence
**Model:** sonnet
- `borg/patterns/distill-article*.md` and `distill-thread*.md`: a figure's Description / Extracted Text is source content; claims may cite it.
- Add an eval fixture with a diagram (the vic post).
- **Success criteria:** `sb borg eval` on the new vic fixture produces at least one claim whose text matches a line from the figure's Extracted Text, recorded in the implementation notes. `bin/promptfoo-check` is report-only by the 2026-10-17 ruling and is not a gate here.

## Acceptance Criteria

Run on desk from the vault root (`~/repos/scottidler/obsidian`) after the rollout. Each "Observed on main" line was run 2026-10-22 before any phase shipped.

`owns N` below means all three agree: N `![[` lines, N entries under `images:`, and N files present. The check, for a note `$n`:

```bash
emb=$(rg -c '^!\[\[' "$n" || echo 0); fm=$(sed -n '/^images:/,/^[a-z]/p' "$n" | rg -c '^  - ' || echo 0)
ex=$(sed -n '/^images:/,/^[a-z]/p' "$n" | rg -o 'system/attachments/\S+' | while read -r p; do [ -f "$p" ] && echo ok; done | wc -l)
echo "$emb $fm $ex"
```

- [ ] 1. The vic note owns exactly one image: the check prints `1 1 1` for `notes/vicvijayakumar-on-x-vic-vijayakumar-argues-that-every-org-is-independently.md`.
  Observed on main: `0 0 0`.
- [ ] 2. Every X post with a photo owns its photos: for each of the 27 photo-bearing posts enumerated in the 2026-10-19 Phase 0, the check prints three equal numbers >= 1. Aggregate guard: `rg -l '^type: social$' notes | xargs -r rg -l '^!\[\[' | wc -l` returns >= 27.
  Observed on main: `0`.
- [ ] 3. The UberEng X Article note owns every figure the classifier keeps: the check prints three equal numbers >= 1 and <= 6 for `notes/ubereng-on-x-uber-engineering-describes-the-design-of-its-mcp-gateway-a.md`, and `sqlite3 -readonly ~/.local/share/sb/oracle/oracle.db "SELECT count(*) FROM notes_fts WHERE notes_fts MATCH '\"auto crawler\"'"` (text from the Figure 2 caption, nowhere in the note today) returns >= 1 after the oracle reindex.
  Observed on main: `rg -c` no match (exit 1); FTS count `0`.
- [ ] 4. A web article owns its diagrams: after `sb borg reingest --source https://martinfowler.com/articles/platform-teams-stuff-done.html`, the check prints three equal numbers >= 1 for `notes/how-platform-teams-get-stuff-done.md` (staged fetch `cl-1d3513ff` has 7 image refs; how many are diagrams is Phase 0's call, not this criterion's).
  Observed on main: `rg -c` no match (exit 1); no `images:` key.
- [ ] 5. No new broken asset embeds: `sb cortex lint --rule broken-links --format json | jq -c '[.[]|select(.rule=="broken-links.asset")|.path]|sort'` prints a list that is a subset of the baseline.
  Observed on main: `["notes/claude-usage-report-2026-02-25-to-2026-03-27.md","notes/claude-usage-report-2026-02-25-to-2026-03-27.md"]`.

## Resolved Decisions

- 2026-10-22, Scott: scope is X posts plus web articles (option B over X-only).
- 2026-10-22, author + staff engineer (round 1): X post photos skip the classifier (author-chosen, at most 4, often the whole content); X Article figures and web-article images are classified. Architect dissent recorded: classify everything (memes, selfies). Not taken: storing X photos is exactly what Scott asked for on the vic post, and the 2026-10-19 doc already describes every one. Round 2: two of the architect's three examples (steipete's OpenClaw terminal screenshot carrying the 458MB/503MB/173MB breakdown, and the RepoBar app screenshot) are post content that the `keep` list (no `terminal`, no `app-ui`) would delete; the cost of the skip is decorative art, at most 4 per post.
- 2026-10-22, author: one module for all three sources (`figures.rs`), extending the 2026-10-19 shared `image_section` rule.
- 2026-10-22, author + both seats: descriptions stay in the note body under each embed, for FTS reach, capped by `max-section-bytes`; the in-note transcript stays eradicated.
- 2026-10-22, author + staff engineer: two vision calls per kept classified image, with the cap before describe. Architect dissent recorded: one combined call. Not taken: see Alternative 4.
- 2026-10-22, round 1 must-fixes folded: size gate sees the full note; trace-id ownership; degraded reingest fails closed; cap before describe; no second vision-pool knob; quality gate on the pre-splice source.
- 2026-10-22, both seats: no new staging-based reingest selector for the backfill.
- 2026-10-22, Scott: backfill every article note after a canary (option B over forward-only); steps in the Rollout Plan.

## Alternatives Considered

### Alternative 1: Hotlink the remote URL
- **Description:** embed `![](https://...)` instead of storing a file.
- **Pros:** zero vault weight.
- **Cons:** rots when the host moves or deletes; pbs.twimg and Substack URLs are not permanent; no offline view on the phone.
- **Why not chosen:** the vault is the durable record; a hotlink is a capture that expires.

### Alternative 2: Store every article image, no classifier
- **Description:** prefilter only, keep everything that survives.
- **Pros:** no vision cost, no classifier precision risk.
- **Cons:** the prefilter only cuts 2696 -> about 2228 refs (17%); XDA-style pages are mostly 1650-2900px app screenshots that no size rule separates from diagrams; the vault is git-tracked (848 attachment files, 129 MB `.git`) and synced to the phone.
- **Why not chosen:** Scott asked for diagrams, not every image.

### Alternative 3: Reuse `process_image` per figure
- **Description:** feed each image through the existing image-ingest handler.
- **Cons:** it takes a GENERAL permit while the URL trace already holds one (`pipeline.rs:240`), the deadlock the 2026-10-19 doc avoided; it publishes a standalone image note per figure.
- **Why not chosen:** wrong unit (a note per image) and a permit hazard.

### Alternative 4: One combined classify + describe vision call
- **Description:** one prompt returns category, confidence, description, and text.
- **Pros:** one call per image instead of two for kept images.
- **Cons:** a second new prompt with no eval (the describe half would replace `vision_extract`'s proven one, shared with image ingest); describes images that the classifier then drops.
- **Why not chosen:** only the classify prompt is new and Phase 0 evaluates it; the describe prompt stays the proven sibling; only kept images pay the second call. Revisit if vision cost shows up in `sb borg log`.

## Technical Considerations

### Dependencies

- New crate: `resvg` (SVG rasterize), added with `cargo add`.
- Existing: `image` 0.25.10 (decode png/jpeg/gif/webp/bmp/tiff), `image_hasher` 3.1.1, `ocr::vision_classify`, `ocr::vision_extract`, `VISION_PERMITS`, `rkvr`.
- No cross-repo blast radius. Ship order: second-brain release -> `otto deploy` on ripr (daemon host) and desk -> reingest.

### Performance

- Vision calls take `VISION_PERMITS` (`ocr.rs:38-71`), the process-wide pool slides already share. An article with 40 candidates and 15 kept costs at most 55 vision calls; Phase 0 records the observed distribution.
- Downloads run concurrently up to `max-candidates`, bounded by `pipeline.fxtwitter-timeout-secs` per image (the existing photo timeout).

### Security

- Image bytes are untrusted input. Decode with `image` (pure Rust) and `resvg` (pure Rust, no script execution).
- `usvg`'s default `ImageHrefResolver` reads `<image href>` paths from the local filesystem relative to `resources_dir`. An SVG from the web could pull a local file into the raster sent to vision. Parse with `resources_dir: None` and a resolver that returns `None` for every href. Phase 4 has a test: an SVG whose `<image href="/etc/hostname">` rasterizes with the image absent.
- The stored SVG is the original bytes. Obsidian renders SVG through an `<img>` tag, which runs no script.
- Stored files keep only the extension implied by the decoded MIME, never the URL's.

### Testing Strategy

- Unit: prefilter table, entity resolution, Substack `src` extraction, store atomicity and non-overwrite, orphan diff.
- Integration: vic and UberEng fixtures through `process_x_post_from` with stubbed HTTP and vision; an article fixture with mixed images through the readable path.
- Tests must bite: each named success criterion test is broken once (e.g. remove the `images:` write) and shown failing before the phase commits.

### Rollout Plan

1. Release, `otto deploy` on ripr and desk.
2. X notes: `sb borg reingest --type social`, plus the 4 renamed notes by exact `--source` (the 2026-10-19 rollout list); `sb borg wait`.
3. Web articles, backfilling every article note:
   - `--source` is a substring match, so every per-note run passes the note's full URL.
   - The martinfowler note for Acceptance Criterion 4, by `--source`.
   - `sb borg reingest --type article --dry-run`, and reconcile its count against the vault's article notes. Observed on main 2026-10-22: dry run lists 615; `rg -l '^type: article'` finds 643 notes.
   - A canary of 3 notes from the 358 with image refs, each by `--source`; `sb borg wait`; run the ownership check on each.
   - `sb borg reingest --type article`, then the notes missing from the dry run by `--source`, as in the 2026-10-19 social rollout; `sb borg wait`.
   - A page that no longer fetches fails `fetch-failed` and keeps its old note; `sb borg log --status failed --since 1d` lists them.
4. Scott commits the new attachments in the vault repo.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Classifier keeps screenshots or drops diagrams | Med | Med | Phase 0 precision/recall gate; `keep` and `min-confidence` are config |
| Vault and its git history grow fast | Med | Med | diagram-only filter, `max-per-note`, `max-bytes` downscale; Phase 0 records bytes per note |
| Asset written, publish blocked -> orphan | Med | Low | store after every gate; rkvr on publish failure (Phase 1 test) |
| Reingest in a later month leaves the old copy | High | Low | generalized cleanup diffs `images:` before/after and rkvrs orphans |
| Image host 403s or rate-limits | Low | Low | per-image drop, receipt degraded; 56 of 56 probed images returned 200 |
| Note crosses 64 KiB with descriptions | Low | Med | `max-per-note` x `max-section-bytes` = 30 KB worst case; the size gate sees the full note and writes no asset on a fail |
| Degraded reingest replaces a note that owns images | Med | High | fail `fetch-failed` and leave the old note and files untouched |
| Two notes share an image file | Low | High | filenames carry the trace id, unique per ingest |

## Open Questions

None.

## References

- `docs/design/2026-10-19-x-post-fetcher.md` (parked items, lines 63-64)
- `docs/design/2026-06-28-content-aware-slide-filtering.md` (taxonomy and `content-filter`)
- `borg/src/slides/publish.rs`, `borg/src/slides/cleanup.rs` (owned-images precedent)
