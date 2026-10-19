# X Post Fetcher: Implementation Notes

## Phase 1: fxtwitter client and renderer, no images
### Design decisions
- Fixtures trimmed to the fields read, plus `replies`/`likes`/`views` kept on purpose (`borg/src/x/{vic,article,long}.json`), so the no-chrome/no-counts tests fail if the renderer ever emits them. Fetched fresh from api.fxtwitter.com on 2026-10-19; the article fixture is cut to its first 24 of 93 blocks.
- `render` already accepts `&[DescribedImage]` (owner + prebuilt section) and places them as `### Image N (in @handle's post)` under their owning post, numbered per owner (`borg/src/x.rs:render`). The signature is in the design's Architecture; tested with hand-built sections, no image fetching.
- An X Article's post text is dropped when it is only the bare `https://x.com/i/article/<id>` link; a real comment on an Article is kept (`x.rs:render`).
- `status_id` matches the host exactly against x.com, www.x.com, twitter.com, www.twitter.com, mobile.twitter.com (not `ends_with`), so `notx.com` cannot match (`x.rs:status_id`). Extra path segments after the id (`/photo/1`) still yield the id.
- `EXTRACTOR = "fxtwitter-api"` constant added for the Phase 2 `persist_fetched_if_staging` call.
- Markdown opens at `# Post by ...` with no `Title:` / `URL Source:` header lines (the design's rendered example); the title is returned separately as `@<handle> on X`.
### Deviations
- None from the spec. `borg/AGENTS.md` module map gained an `x.rs` line (repo convention, not in the phase bullets).
### Tradeoffs
- The config default and override are asserted inside the two existing pipeline-config tests, not a new test: `borg/src/config/tests.rs` sat at 1498 of the 1500-line `bloat` limit, and a separate test failed `otto ci`.
- `Post.id` is a `String` per the design's Data Model; `fetch_post` takes the numeric id from the caller and does not cross-check it against the response.
### Open questions
- None.

## Phase 2: Photo description and dispatch
### Design decisions
- Photo download + describe lives in a new `borg/src/pipeline/photos.rs` (`describe_x_photos`, `describe_photo`, `download_photo`), with the shared builder `photos::image_section` beside it; `process_image_inner` (`handlers.rs`) now calls `image_section(vision, ocr_text, "##")` in place of its inline merge. `handlers.rs` sits at ~1390 of the 1500-line `bloat` limit, so the photo code went to its own module rather than growing it.
- Vision "Ok with empty description" is treated as a vision failure in the X path and passed to the builder as `None`, so the section is labeled `OCR Text` as the outcomes table says (`photos.rs:describe_photo`). Image ingest's own behavior (label by whether vision returned at all) is unchanged.
- An X post with no text and no photos at all also fails (`x::has_text` false, no section). The design states the rule as "no photo produced a section"; zero photos satisfies it, and an empty note is the thing the rule prevents.
- OCR is a port: `photos::OcrFn = fn(&Path, u64) -> Result<String>`; `process_x_post` passes `ocr::ocr_extract`, tests pass a canned reader. OCR runs on a temp file (`tempfile::NamedTempFile`) inside `spawn_blocking`, joined with the vision call; only vision takes a `VISION_PERMITS` permit. Photos of one post are described concurrently (`join_all`), order preserved.
- Three task-local test seams, all the `youtube/engine/bases.rs` shape and all failing outside a scope in a test: `ocr::bases` (Anthropic base for `vision_call`), `x::bases` (fxtwitter base for `process_x_post` via `x::api_base`), `jina::bases` (Jina Reader base). The Jina seam exists so the fail-closed test's "Jina records zero hits" is a real stub count.
- `x.rs` gained `photos(post)` (post's photos, then the quote's, with owner) and `has_text(post)` (post / quote / Article blocks, bare Article link excluded, matching `render`).
- `process_x_post` returns `FetchedPost { title, markdown, degraded }`; the dispatcher builds the `(title, md, None)` triple from it. No creator byline: social notes carry no creator (`ContentType::Social`).
- `borg/src/pipeline/AGENTS.md` gained invariant 13 (X arm fails closed, photo path, test seams).
### Deviations
- `image_section` takes a third argument, the heading marks (`"##"` for image ingest, `"####"` under an X `### Image N`): the design's two-argument signature cannot produce both the image note's `## Description` and the X render's `#### Description` (Phase 1's render tests pin `####`). Same builder, correct seam.
- `process_x_post_from` takes `(api_base, ocr, ...)`, one more parameter than the design's `process_x_post_from(api_base, ...)`, for the OCR port above.
- The fail-closed test calls `process_url` (which wraps `process_url_inner` in the hard timeout and maps a bubbled `Err` to `fetch-failed`) rather than `process_url_inner` directly: the receipt stage the bullet asserts is assigned in that wrapper.
- Photo bullet says "concurrently under `VISION_PERMITS`": only the vision call takes the permit (as Architecture states and image ingest does); OCR takes none.
### Tradeoffs
- The fail-closed test cannot seed a ledger entry: `ledger::ledger_path()` is the real XDG ledger and changing `XDG_DATA_HOME` is process-global and races parallel tests. So it proves the source's note and the temp vault's file list are byte-identical, not the reingest lookup path. To keep a regressed implementation from publishing into the real ledger, the stub fabric prints a block-page title (`Just a moment...`): a fallback is counted at fabric, then refused by the quality gate before publish, and `archive.root` points at the temp dir. A mutation run (X arm changed to fall back) fails the test with `QualityBlocked != FetchFailed`, writing nothing outside the temp dir.
- Vision-failure detection is "description empty after trim", not "raw response empty": `parse_vision_response` fills `description` from the first lines whenever the output is non-empty, so an empty description means the model returned no text.
### Open questions
- An earlier mutation run of `x_post_fetch_error_fails_closed` (before the block-page stub hardening) published through the real ledger: it appended one row `| 2026-10-19 | 07:35 | cli | ✅ | [[x-thread]] | https://x.com/poster/status/100 | xp-closed |` to `~/.local/share/sb/borg/borg-ledger.md` (line 17) and created `~/.local/share/sb/borg/sources/xp-closed/` (`distilled.yml`, `transcript.md`). Removing them was denied by the auto-mode classifier; Scott should delete that ledger line and `rkvr rmrf` that directory.
