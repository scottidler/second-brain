# Design Document: Capture All Diagrams

**Author:** Scott Idler
**Date:** 2026-10-22
**Status:** Draft
**Review Passes Completed:** 5/5

## Summary

Borg sees the diagrams in X posts and web articles and throws them away. X photos get downloaded and described, then the bytes die; X Article figures and web-article images never get downloaded at all. This doc stores every diagram as a vault asset, embeds it in the note under an `## Images` section with its description, and rebuilds the existing X notes.

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
  X Article figs ├─> figures::collect ─> prefilter ─> classify ─> describe ─> FigureSet
  article ![](u) ┘    (download, cap)    (cheap)      (vision,    (vision +    │
                                                       articles    OCR)        │
                                                       only)                   │
          distiller input <── description spliced in position ─────────────────┤
          (claims ground in the diagram; lands in distilled.yml transcript)     │
                                                                               │
  after every publish gate:  store_figures ─> images: [paths] ─> ## Images ────┘
                             (atomic, never overwrite)            ![[file]] + description
  after durable publish:     orphans of the old images: list -> rkvr
```

### Architecture

**One module, `borg/src/pipeline/figures.rs`,** grown out of today's `pipeline/photos.rs` (which it replaces). It owns:

- `Candidate { url, alt, caption, origin }`: `origin` is `XPost(Owner) | XArticle | Article`.
- `Figure { candidate, bytes, mime, width, height, category: Option<SlideCategory>, section }`: the kept image, its bytes, and its built Description / Extracted Text section (`image_section`, unchanged).
- `collect(candidates, cfg) -> FigureSet`: download (cap `max-candidates`), prefilter, classify (articles only), describe, cap `max-per-note`. Each drop is counted by reason in the result so a receipt can say why.
- `store(vault_root, &FigureSet, now) -> Vec<StoredFigure>`: the only writer. Runs after every publish gate.

**Candidates per source:**

- **X post photos**: `x::photos()` (`x.rs:163`) as today, `origin: XPost(owner)`.
- **X Article figures**: deserialize `blocks[].entityRanges`, `entityMap` (in fxtwitter a **list** of `{key, value}`, not a Draft.js dict), and `media_entities`. Resolve each `atomic` block: `entityRanges[0].key` -> `entityMap` entry with the same key (as a string) -> `value.type == "MEDIA"` -> `value.data.mediaItems[].mediaId` -> `media_entities[].media_id` -> `media_info.original_img_url`, caption from `value.data.caption`. Proven 8 of 8 on the two live Articles. An unresolvable atomic block is skipped and logged, never an error.
- **Web articles**: parse `![alt](url)` from the fetched markdown (readable, fabric, jina all emit it). Substack's lazy-image markup comes through as `![alt](<https://host/p/{"src":"https://substack-post-media...",...}>)` (19 refs over 4 traces): extract the inner `src`. Relative URLs resolve against the page URL. HN (`hn-api`) carries no images.

**Prefilter (deterministic, no network beyond the download):**

- Drop `data:` URIs, `avif`, and any MIME the pipeline cannot decode (`image` here decodes png, jpeg, gif, webp, bmp, tiff).
- Drop on URL tokens that only ever name decoration. Match whole tokens of the host and path split on non-alphanumerics, never substrings (`icon` must not hit `silicon`): `avatar`, `avatars`, `gravatar`, `authors`, `logo`, `logos`, `icon`, `icons`, `favicon`, `emoji`, `pixel`, `badge`, `badges`; hosts `yt3.ggpht.com`, `img.shields.io`.
- Drop under `min-side-px` (default 200) on either axis, read from the decoded header.
- Drop exact duplicates within the note by sha256 and near-duplicates by perceptual hash (`image_hasher`, already a direct borg dep, `borg/Cargo.toml:39`).
- **Alt text is not a signal.** martinfowler: 7 of 7 diagrams have empty alt; chroma context-rot: 43 charts, all empty alt.

**SVG:** 151 refs. Many are diagrams (mermaid and draw.io exports), many are icons. The vision API takes jpeg/png/gif/webp only. Rasterize with `resvg` (`cargo add resvg`) at the original viewBox, classify the raster, and store the **original `.svg`** (Obsidian renders it). The prefilter's size and keyword rules apply to the raster.

**Classify (web articles only):**

- `ocr::vision_classify` (`borg/src/ocr.rs:252`) with a new `ARTICLE_TAXONOMY_PROMPT` beside `TAXONOMY_PROMPT` (`borg/src/slides/classify.rs:76`). The slide prompt talks about webcam insets and video frames; an article figure needs its own wording. Same `SlideCategory` enum, same output schema, same fail-closed `ClassifyError`.
- Keep if the category is in `images.content-filter.keep` and confidence >= `min-confidence`. A classify error drops the image and marks the receipt degraded.
- **X photos skip classify.** A post has at most 4 photos, each chosen by the author, and they are often the whole content (the vic flowchart is a quoted-post photo). The 2026-10-19 doc already describes every one; this doc only stops discarding them.
- **X Article figures skip classify.** Same reasoning: the author placed each one in the body with a caption.

**Describe:** the existing `describe_photo` path (OCR under `spawn_blocking` joined with `vision_extract`, `image_section` builder). Two vision calls per kept article image (classify, then describe) over one combined call: both prompts are proven, and only kept images pay the second call.

**Distiller input:** each kept figure's section is spliced into the source markdown **at the image's position** (X: under its owning post, as `push_images` does today; X Article: at its atomic block; article: replacing its `![alt](url)` line). The distiller grounds claims in the diagram, and the description reaches `distilled.yml` `transcript:`, which cortex embeds from staging (`cortex/src/embed/source.rs:41`).

**Store:** after the note-size gate (`pipeline.rs:1112`) and right before `archive_then_publish`.

- Path: `system/attachments/images/YYYY-MM/<slug>-fig-<n>-<sha8>.<ext>`, `YYYY-MM` from the ingest time. Atomic write (temp + rename), never overwrite: an existing path with the same sha is reused, matching `slides/publish.rs` (`pick_filename`, `:119`).
- Oversize: an image over `max-bytes` (default 2 MiB) is downscaled to `max-side-px` (default 2048) and re-encoded: jpeg -> jpeg q85, everything else -> png (the `image` webp encoder is lossless-only; a gif keeps its first frame). Still over the cap -> dropped, receipt degraded. An SVG is never resized.
- The filename carries the note slug, so two notes never share a file and one note's cleanup can never remove another's image. A reingest in the same month with the same bytes reuses the path, so it is not an orphan.
- If publish fails, the files written by this call go through `rkvr::remove`.

**Note shape:** a new `## Images` section at the end of the distilled body, before the `---` / Source footer:

```markdown
## Images

![[vicvijayakumar-on-x-...-fig-1-3fa2c8d1.jpg]]

*In @boristane's post.*

#### Description
A flowchart depicting a code review and deployment process...

#### Extracted Text
Agent generates code
...
```

- One sub-block per figure, in source order. The provenance line is the X owner (`In @handle's post`), the X Article caption, or the article alt text when non-empty.
- The description stays in the body so oracle FTS (`vault/src/search/index.rs:304` indexes the body only) can keyword-match text inside a diagram. Cost: about 0.5-1 KB per figure against the 64 KiB note gate (`MAX_NOTE_BYTES`, `config.rs:384`); `max-per-note: 15` caps it at about 15 KB.
- The section name is `## Images`, not `## Figures`: an X photo is often a screenshot, and the name has to be true for every source.

**Reingest cleanup:** generalize `borg/src/slides/cleanup.rs` (`read_old_slides_frontmatter` -> `compute_orphans` -> `rkvr::remove`) to read both `slides:` and `images:`, captured before replace (`pipeline.rs:684-686`) and run after the new note is durable (`:1182-1195`). Slides keep their own key and behavior.

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
  max-candidates: 40      # images downloaded per note, in source order
  max-per-note: 15        # images kept per note
  min-side-px: 200        # either axis under this is decoration
  max-bytes: 2097152      # over this, downscale to max-side-px
  max-side-px: 2048
  content-filter:         # web articles only; X photos and X Article figures are always kept
    enabled: true
    keep: [architecture-diagram, sequence-diagram, flowchart, chart, infographic]
    model: ""             # empty = inherit llm.model
    max-vision-concurrency: 4
    min-confidence: 0.6
```

- `content-filter` reuses `ContentFilterConfig` and `SlideCategory` (`borg/src/config.rs:650-788`): one taxonomy, one parser, unknown categories are a load error.
- `keep` default includes `chart` and `infographic`: "all diagrams" covers chroma's 43 data charts and the explanatory one-pagers; `app-ui`, `webpage`, `code`, `terminal`, `talking-head`, `b-roll`, `title-card`, `other` stay out.
- `enabled: false` restores today's behavior: X photos described but not stored, articles untouched. A reingest under `enabled: false` writes `images: []`, so cleanup removes the old note's images: the frontmatter and the files never disagree.

### Implementation Plan

#### Phase 0: Measure the article filter
**Model:** sonnet
- Zero repo code. Throwaway harness in the session scratchpad.
- Sample 40 recent `readable` article stages with image refs. Download every ref with `BROWSER_UA`. Record fetch success, MIME mix, dimension distribution, and the bytes the Phase 4 prefilter keeps.
- Hand-label 60 prefilter survivors as diagram-category or not. Run a draft `ARTICLE_TAXONOMY_PROMPT` through `vision_classify` on them.
- Rasterize 10 sampled SVGs with `resvg`'s CLI and classify them.
- Paste the numbers into this doc under Phase 0 Results. No files under `docs/` besides this one.
- **Success criteria:** diagram precision >= 0.9 and recall >= 0.8 on the 60 labels with `min-confidence: 0.6`; fetch success, MIME mix, and kept-bytes p50/p90 per note recorded in this doc.

#### Phase 1: Config and owned-images plumbing
**Model:** sonnet
- The whole `images:` config block (`ImagesConfig`, reusing `ContentFilterConfig`) and its annotated example. Every later phase reads it.
- `images:` frontmatter on `NoteContent`, `RENDER_NOTE_KEYS`, `render_note`.
- Generalize `slides::cleanup` to `images:` + `slides:`.
- `figures::store`: atomic, never overwrite, downscale over `max-bytes`, after all gates; rkvr on publish failure.
- **Success criteria:** `render_note_keys_matches_the_writer` passes with `images` added; a reingest test that drops one of two images leaves exactly one file and the other went through `rkvr::remove`; a gate-blocked publish leaves 0 new files under `system/attachments`; `images.content-filter.keep: [diagram]` (not a `SlideCategory`) fails config load.

#### Phase 2: X post photos stored and embedded
**Model:** sonnet
- Move `photos.rs` into `figures.rs`; `describe_photo` returns the bytes and MIME with the section.
- `FetchedPost` carries the `FigureSet`; `process_url_inner` stores after the gates and appends `## Images`.
- **Success criteria:** the vic fixture test asserts one `![[` under `## Images` and `images:` of length 1; a photo-download-error test asserts no `## Images` section and `fetch_degraded == true`.

#### Phase 3: X Article figures
**Model:** opus
- serde for `entityRanges`, `entityMap` (list), `media_entities`; atomic -> media resolution; captions; `render_blocks` emits the figure section at its atomic block.
- Fixture trimmed from the live UberEng JSON under `borg/src/x/` (the existing `article.json` lost these fields in its trim).
- **Success criteria:** the UberEng fixture yields 6 figures in block order, each with its `Figure N` caption; an atomic block with no matching entity is skipped and the render still succeeds.

#### Phase 4: The deterministic prefilter and article candidates
**Model:** sonnet
- Pure `prefilter(url, mime, dims, bytes) -> Keep | Drop(reason)`; sha256 + phash dedupe.
- Candidate parsing from fetched markdown: `![alt](url)`, Substack `src` extraction, relative URL resolution against the page URL.
- SVG rasterize via `resvg` with the href resolver locked down.
- **Success criteria:** a table test drops the 90x90 XDA avatar, a 420px `logo` URL, a `data:` URI, and an `.avif`, keeps the martinfowler `platform-teams-stuff-done` diagram, and keeps a URL containing `silicon`; an SVG with `<image href="/etc/hostname">` rasterizes with that image absent.

#### Phase 5: Web-article figures
**Model:** opus
- Parse candidates from fetched markdown on the readable, fabric, and jina paths; `ARTICLE_TAXONOMY_PROMPT` (the Phase 0 version); classify -> describe -> splice in position before distill; store and `## Images` after the gates.
- Vision seam stubbed in tests through `ocr::bases`.
- **Success criteria:** with stubbed vision labeling 3 images `flowchart`, `app-ui`, `chart`, the note keeps exactly the flowchart and the chart; vision calls per note <= `max-candidates` + `max-per-note`; a classify error drops that image and sets `fetch_degraded`.

#### Phase 6: Distill patterns treat figures as evidence
**Model:** sonnet
- `borg/patterns/distill-article*.md` and `distill-thread*.md`: a figure's Description / Extracted Text is source content; claims may cite it.
- Add an eval fixture with a diagram (the vic post).
- **Success criteria:** `rg -i 'figure|diagram' borg/patterns/distill-article.md` returns >= 1 line; `bin/promptfoo-check` passes in `otto ci`.

## Acceptance Criteria

Run on desk from the vault root (`~/repos/scottidler/obsidian`) after the rollout. Each "Observed on main" line was run 2026-10-22 before any phase shipped.

- [ ] 1. The vic note embeds the flowchart and owns exactly one image:
  `rg -c '^!\[\[' notes/vicvijayakumar-on-x-vic-vijayakumar-argues-that-every-org-is-independently.md` returns `1`.
  Observed on main: no match (exit 1).
- [ ] 2. Every X post with a photo has an embed: `rg -l '^type: social$' notes | xargs rg -l '^!\[\[' | wc -l` returns >= 27.
  Observed on main: `0`.
- [ ] 3. The UberEng X Article note embeds its 6 figures: `rg -c '^!\[\[' notes/ubereng-on-x-uber-engineering-describes-the-design-of-its-mcp-gateway-a.md` returns `6`.
  Observed on main: no match (exit 1).
- [ ] 4. A web article keeps its diagrams: after `sb borg reingest --source https://martinfowler.com/articles/platform-teams-stuff-done.html`, `rg -c '^!\[\[' notes/how-platform-teams-get-stuff-done.md` returns >= 5 (its staged fetch has 7 image refs, trace `cl-1d3513ff`).
  Observed on main: no match (exit 1).
- [ ] 5. No new broken asset embeds: `sb cortex lint --rule broken-links --format json | jq '[.[]|select(.rule=="broken-links.asset")]|length'` returns <= `2`.
  Observed on main: `2`.

## Resolved Decisions

- 2026-10-22, Scott: scope is X posts plus web articles (option B over X-only).
- 2026-10-22, author: X post photos and X Article figures skip the diagram classifier; only web-article images are filtered. Basis: author-chosen, at most 4 per post, often the whole content.
- 2026-10-22, author: one module for all three sources (`figures.rs`), extending the 2026-10-19 shared `image_section` rule.
- 2026-10-22, author: descriptions stay in the note body under each embed, for FTS reach; the in-note transcript stays eradicated.
- 2026-10-22, author: two vision calls per kept article image (classify, describe) over one combined prompt.

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
- **Cons:** a new prompt with no eval; describes images that the classifier then drops.
- **Why not chosen:** two proven prompts, and only kept images pay the second call. Revisit if vision cost shows up in `sb borg log`.

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
3. Web articles: forward-only from the release, plus the martinfowler note for Acceptance Criterion 4. The 640-note article backfill is Open Question 1.
4. Scott commits the new attachments in the vault repo.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Classifier keeps screenshots or drops diagrams | Med | Med | Phase 0 precision/recall gate; `keep` and `min-confidence` are config |
| Vault and its git history grow fast | Med | Med | diagram-only filter, `max-per-note`, `max-bytes` downscale; Phase 0 records bytes per note |
| Asset written, publish blocked -> orphan | Med | Low | store after every gate; rkvr on publish failure (Phase 1 test) |
| Reingest in a later month leaves the old copy | High | Low | generalized cleanup diffs `images:` before/after and rkvrs orphans |
| Image host 403s or rate-limits | Low | Low | per-image drop, receipt degraded; 56 of 56 probed images returned 200 |
| Note crosses 64 KiB with descriptions | Low | Med | `max-per-note: 15` at about 1 KB each |

## Open Questions

- [ ] 1. Article backfill: forward-only, all 640 via `--type article`, or a new reingest selector for notes whose staged fetch has image refs (358 sources)?

## References

- `docs/design/2026-10-19-x-post-fetcher.md` (parked items, lines 63-64)
- `docs/design/2026-06-28-content-aware-slide-filtering.md` (taxonomy and `content-filter`)
- `borg/src/slides/publish.rs`, `borg/src/slides/cleanup.rs` (owned-images precedent)
