## Phase 0: Measure the article filter
### Design decisions
- The draft `FIGURE_TAXONOMY_PROMPT` reuses the `TAXONOMY_PROMPT` category set and output schema verbatim and rewords only the framing (a figure on a web page, not a video frame): design doc "Phase 0 Results": so the same `SlideCategory` parser fits and Phase 5 copies the text unchanged.
- `app-ui` and `webpage` both cover website screenshots, and `b-roll` is widened to cover logos, icons, mascots, memes, and product photos: same prompt: the sample's dominant false-keep risks were XDA screenshots and decorative art, and none of those categories are in `keep`.
- Labels were fixed before the classifier ran and were not edited after: hand-labeling method: so the precision/recall are not tuned to the prompt.
- The prompt passed on its first draft, so there was no revision loop: Phase 0 gate: nothing was tuned.

### Deviations
- The classify call was made from a Python harness posting the same request body to the Messages API, not through Rust `ocr::vision_classify`: the phase said zero repo code and the Rust fn is not drivable without a harness crate; the endpoint, model (`claude-sonnet-5`), `max_tokens` (64), and body shape are identical. Same effect, different caller.
- Images over 3.5 MB or 2048 px were downscaled to a 2048 px jpeg before classify: the API's 5 MB per-image limit; 10 of 143 survivors exceed 2 MiB, max 3.2 MB, so this touched only the large-pixel-count images. Borg's real path will downscale at `max-bytes` (Phase 1).
- The prefilter was reimplemented in the harness (format, URL tokens, `min-side-px`, per-note sha256): the Rust prefilter is Phase 4; the harness mirrors the design's rules and does not claim to match that code.
- 40 sample notes are the 40 most recent readable stages with image refs (by mtime), not a random draw; the 60 labeled survivors are a seeded random draw from those.

### Tradeoffs
- 60 labels from 17 notes vs a draw across more notes: the 40-note sample yields 143 survivors from 27 notes, concentrated in a few image-heavy articles (XDA, stateof.ai, a jev post), so the labeled set is clustered. Recorded; the precision margin (one image) is flagged in the doc.
- Borderline labels (#30 Obsidian canvas screenshot, #41 portrait slide, #56 graph view) were called by eye and left as first labeled, against re-labeling to fit: the gate is only honest if labels are not adjusted.

### Open questions
- Is a precision margin of one image (0.909 vs the 0.9 gate) enough to proceed to Phase 5, or should the labeled set be widened with a second draw first? The criterion as written is met.
- Phase 4 SVG rasterization: the `resvg` CLI draws no text without a loaded font and skips CSS class styling on some SVGs (2 of 10 rendered black). Should Phase 4 load a `fontdb` fallback? The doc's Phase 4 does not mention fonts; this is recorded in Phase 0 Results.

## Phase 1: Config and owned-images plumbing
### Design decisions
- `ImagesConfig` / `FigureFilterConfig` live in their own `borg/src/config/images.rs`, with `#[serde(default)]` structs whose `Default` carries the documented values (not `ContentFilterConfig`): an omitted block, a block with only `enabled: true`, and a partial `content-filter` all land on the same defaults; tests pin each.
- `slides::cleanup` reads both keys through one `Owned { slides, images }` and `read_old_owned`, diffed per key by `compute_all_orphans` — `borg/src/slides/cleanup.rs` — slides keep their own key and behavior; a path that moves between keys is still an orphan of its old key.
- `read_old_owned` is called with `?` in `process_url_inner` where `read_old_slides_frontmatter` was `unwrap_or_default()` — `borg/src/pipeline.rs` — a swallowed read error would read as "owns no images", which defeats the degraded-reingest rule and orphans files; `read_preserved_fields` just above already fails closed on the same file.
- The ordering lives in one writer, `publish::publish_url_note` (degraded-reingest refusal -> size gate -> `figures::store` -> `archive_then_publish` -> orphan cleanup), with a typed `UrlPublishError` carrying the failure stage — `borg/src/pipeline/publish.rs` — so the success-criterion tests run through the real writer, not `process_x_post_from`, and `process_url_inner` cannot reorder the steps.
- `process_url_inner` already runs the final ordering with an empty `FigureSet` (nothing produces figures until Phase 2): `plan` -> render (`images:` from the plan) -> `publish_url_note`. `fetch_degraded` is OR-ed with the image stage's degraded flag.
- Store-failure rollback: `figures::store` removes the files it already created before returning its error — `borg/src/pipeline/figures.rs:store` — so a half-stored set never leaks into the caller.
- `images:` stays out of `BORG_PRESERVE_KEYS` and `CORTEX_PRESERVE_KEYS`; a test pins both.

### Deviations
- The design says `store` downscales over `max-bytes`; `plan` does it. Same effect, correct seam: the downscale can change the extension (gif -> png) and can drop the figure (still over the cap), and both must be known when `plan` assigns the path the note embeds and when the note renders, which is before `store` runs. `plan` stays free of I/O (decode + encode only). `plan` therefore takes `&ImagesConfig` and returns `Plan { planned, degraded }`, not a bare `Vec<PlannedFigure>`.
- `figures::Figure` carries only `bytes`, `mime`, `section`, not the design's full `Figure { candidate, bytes, mime, width, height, category, section }`: `Candidate`, `SlideCategory`, and dimensions belong to the phases that produce them. `Gif`/`Webp` variants and `section` carry `#[expect(dead_code)]` with a reason until Phase 2 constructs and reads them.
- A figure-file write failure fails the publish (`publish-failed`, created files removed) instead of "drop that figure, degrade the receipt": by the time `store` runs the note is already rendered with that figure embedded, so dropping it would publish a dangling embed. Download, classify, describe, and oversize-after-downscale failures (all before render) do degrade.
- `FigureSet`'s `## Images` section render is not in this phase: the design puts it in Phase 2 ("renders `## Images`"). Phase 1 renders only the `images:` frontmatter.
- Old `publish_failure_result` was replaced by `url_publish_failure_result`, since the one production caller now goes through `UrlPublishError`; its existing test was ported, not removed.

### Tradeoffs
- One writer function with a spec struct vs adding the gate/store/cleanup inline in `process_url_inner`: the inline form cannot be tested without driving the whole URL pipeline, which the phase's criteria forbid.
- `plan` hashes the final stored bytes (post-downscale) for `sha8` vs the downloaded bytes: the name then matches the file's content, so a reused path means identical bytes.

### Open questions
- Should a figure-file write failure degrade-and-drop instead of failing the publish? That needs `store` to run before render, which would put file writes ahead of the size gate and break the "gate fails write 0 files" criterion. Left as fail-closed.

## Phase 2: X post photos stored and embedded
### Design decisions
- `photos.rs` and its tests moved into `figures.rs` / `figures/tests.rs` (the old files are gone); `describe_x_photos` now returns `XPhotos { images, figures, degraded }`: `images` is the per-photo section list the distiller source markdown still splices (`x::render` unchanged), `figures` is the `FigureSet` to store — `borg/src/pipeline/figures.rs` — so the distiller input and the stored note cannot disagree about which photos were described.
- `describe_photo` returns a `DescribedPhoto { bytes, mime: Option<FigureMime>, section }`; the MIME is the response `Content-Type` mapped by `FigureMime::from_content_type`, never the URL — `figures.rs:describe_photo`.
- `download_photo` enforces `images.max-download-bytes` twice: a declared `Content-Length` over the ceiling bails before any read, and the body is read in chunks and bails the moment the running total crosses it (a missing or lying header cannot defeat it) — `figures.rs:download_photo`.
- The section is capped once, at describe time (`cap_section`, line boundary, trailing `…`), so the distiller input and the `## Images` block carry the same text — `figures.rs`.
- `Plan` gained `images_section` (built inside `plan` from the same loop that assigns names), and `with_images_section` appends it to the distilled body after plan and before `render_note` adds the Source footer — `figures.rs:plan`, `pipeline.rs:process_url_inner`. A dropped figure (oversize after downscale) leaves no embed, so the embed list and `images:` come from one pass and cannot diverge.
- `Figure` gained `provenance` (`In @handle's post`); the quoted post's photo carries the quoted author's handle (the vic flowchart is `@boristane`'s).
- `FetchedPost.degraded` stays the describe-stage flag (it holds with `images.enabled: false`, as before); `FigureSet.degraded` is the image-stage flag and is only set when images are enabled. `process_url_inner` ORs `FetchedPost.degraded` and the plan's flag into `fetch_degraded`, and the writer gets `figure_set.degraded || plan.degraded`.
- Success-criteria tests run through the real writer, not `process_x_post_from` alone: `vic_photo_is_stored_embedded_and_listed` and `photo_download_error_yields_no_images_section_and_degrades` (`borg/src/pipeline/tests/x.rs`) fetch through the stubs, then `plan` -> `with_images_section` -> `render_note` -> `publish_url_note`. Each was broken once (figure push skipped; download error returning non-degraded) and shown failing before restore.
- Phase 1's `#[expect(dead_code)]` markers on `Gif`/`Webp` and `Figure.section` are removed (now constructed and read).

### Deviations
- The publish-path tests assemble plan -> render -> `publish_url_note` by hand instead of driving `process_url`: a full `process_url` run needs the distiller LLM, tags, and the real XDG ledger (`ledger::append_entry`), which a test must not write to. Same helpers as production (`figures::plan`, `with_images_section`, `render_note`, `publish_url_note`); `process_url_inner`'s own glue (`figure_set = post.figures`, the `with_images_section` call) has no direct test.
- A photo that downloaded but is in a format borg does not store (avif, bmp, tiff, ...) is described for the distiller as before but not stored or embedded, and does not degrade: the design's prefilter makes those a drop, not an error. The full prefilter is Phase 4; this is the minimum Phase 2 needs to name a file extension from the MIME.
- A photo whose vision and OCR both produce nothing is still omitted entirely (not stored), as before: its section would be empty and the doc says a dropped image never publishes an empty heading.

### Tradeoffs
- Cap at describe time vs cap in `plan`: describe-time keeps one text for both the distiller and the note, at the cost of the distiller seeing the truncated form of a very long OCR dump (the design wants the cap to protect the note budget; the 1500-byte default is also what the model needs).
- Chunked read vs `response.bytes()` with a post-hoc length check: the chunked read stops a hostile body at the ceiling instead of buffering 20 MiB+ first.

### Open questions
- None.

## Phase 3: X Article figures
### Design decisions
- Serde for `entityRanges`, `entityMap` (a list of `{key, value}`), and `media_entities` on `x::Block` / `x::ArticleContent` / `x::Article`; every key and media id is read through one `key_as_string` deserializer, since a block's range key is a JSON number and the entity map's is a string — `borg/src/x.rs` — one comparison type, and a non-scalar key is a parse error (fail loudly), pinned by `x_article_entity_key_rejects_non_scalar`.
- `x::article_figures(post)` resolves `atomic` block -> `entityRanges[0].key` -> entity -> `MEDIA` -> each `mediaItems[].mediaId` -> `media_entities[].media_info.original_img_url`, caption from the entity; any break in that chain skips the block with an `info` log line and returns the rest — `x.rs:article_figures` / `resolve_atomic`.
- `DescribedImage` gained `place: Place { Photo | Figure(n) }`, `n` indexing `article_figures` of the owning post; `render_blocks` emits a placed figure at its `atomic` block as `<sub> Image N`, `*caption*`, then the section, and an atomic block with nothing placed still renders nothing — `x.rs:render_blocks` — so the distiller sees the figure where the author put it.
- `FIGURE_TAXONOMY_PROMPT` sits beside `TAXONOMY_PROMPT` with `classify_figure(bytes, mime, &FigureFilterConfig, &LlmConfig)`, same `parse_classification` and fail-closed `ClassifyError`; the prompt text was checked byte-equal against the doc's Phase 0 block (2050 bytes) — `borg/src/slides/classify.rs`.
- `keep_outcome` now delegates to `keep_outcome_for(result, keep, min_confidence)`, which the figure path calls with `images.content-filter` — `slides/classify.rs` — one keep rule for slides and figures.
- `figures::collect_classified(candidates, config, ocr, trace_id) -> Collected` is the source-agnostic classified path Phase 5 reuses: first `max-candidates` downloaded concurrently -> classify concurrently (vision pool bounds it) -> keep -> cap at `max-per-note` in source order -> describe concurrently; returns one slot per candidate, positionally aligned. Drops are tallied `prefilter / not-diagram / cap / error` and written as the design's one `info` line per call; only `error` degrades — `borg/src/pipeline/figures.rs`.
- `describe_photo` split into `download_photo` + `describe_bytes` so the classified path describes bytes it already downloaded (no second fetch) through the same OCR + vision + `image_section` + `cap_section` code as X photos.
- `describe_x_photos`/`XPhotos` became `describe_x_media`/`XMedia`: photos (unclassified, as before) and both posts' Article figures (classified) run concurrently; `FigureSet` is assembled in source order, per owner the Article's figures then its photos, post before quote. A kept figure's provenance is its caption, else `In @<handle>'s article`.
- With `images.enabled: false` no Article figure is downloaded, classified, or placed (today's behavior); with `content-filter.enabled: false` every downloaded figure is kept unclassified up to the cap. Classify uses `content-filter.model` and ignores `vision.enabled`, as `classify_slide` does.
- Success-criterion tests, each broken once and seen failing before restore: (1) `x::tests::x_article_ubereng_yields_six_figures_in_block_order_with_captions` (broken: caption dropped); (2) `x::tests::x_article_unresolvable_atomic_block_is_skipped_and_render_succeeds` (broken: an unresolvable block aborts resolution); (3) `pipeline::tests::x::article::x_article_title_card_figure_is_not_kept` (broken: `DroppedNotInKeep` treated as keep; 6 describe calls instead of 5).

### Deviations
- Criterion 1 says "each with its `Figure N` caption"; the live Article's sixth figure (block 84, the Omni MCP diagram) has no caption in fxtwitter's JSON (Phase 0 Results already calls it "uncaptioned"). The test asserts captions `Figure 1`..`Figure 5` plus `None` for the sixth, and that figure's provenance falls back to `In @UberEng's article`.
- `Candidate` is `{ url, provenance }`, not `{ url, alt, caption, origin }`: the only thing `origin` decided was "X post photos skip classify", and X photos never enter `collect_classified`, so every candidate there is classified by construction. Phase 5 adds what web articles need (alt text into provenance fits the same field).
- The fixture replaces `borg/src/x/article.json` rather than adding a second UberEng file: one Article fixture, now trimmed from the live JSON with all 93 blocks (`type`, `text`, `entityRanges`), the full `entityMap`, and `media_entities` cut to `media_id` + `media_info.{original_img_url,width,height}`. Every pre-existing test on `ARTICLE` still passes against it.
- SVG reaching the classified path is dropped as `prefilter` (`unsupported-format`): the vision API takes no svg and the rasterizer is Phase 4. X Article media is pbs JPEG, so nothing hits this today.
- `Figure` still carries no `category`/`width`/`height`: nothing reads them yet.
- Overflow past `max-candidates` is counted in the `cap` bucket of the log line (the design names only the four buckets).

### Tradeoffs
- Concurrent classify and describe fan-out via `join_all` vs a `JoinSet` like `classify_slides`: the vision permit pool already bounds in-flight calls and `join_all` keeps results in order without an index-reassembly step; a panic in one future would take the trace down rather than one figure, accepted since the futures do no unchecked indexing.
- Placing figures by index into a fresh `article_figures` call at render time vs threading block indices through `DescribedImage`: the resolution is pure and cheap, and one function stays the only place the entity chain is walked.

### Open questions
- Should X post photos also route through a shared `collect` (classify skipped) so they emit the same `figures: kept=… dropped={…}` line? Phase 2's path is unchanged and logs per photo only.
