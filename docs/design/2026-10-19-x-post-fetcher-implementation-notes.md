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

## Phase 3: Thread pattern framing
### Design decisions
- `distill-thread.md` now states two input shapes: the X shape (`# Post by`, optional `## Quoted post by`, `### Image N` with Description / Extracted Text / OCR Text, `# Article by`) and the Reddit / HN shape, whose OP-plus-comments framing is kept as written. Rules for `summary`, `claims` ordering and `post-count` got X-specific clauses (a quoted post is a post, never a reply; post-count is 1 or 2) (`borg/patterns/distill-thread.md`).
- `-chunk.md` and `-reduce.md` carry a shorter version of the same framing, so chunked long X Articles attribute the same way.
- The 3 X golden `source.md` files were regenerated from live `api.fxtwitter.com/status/<id>` JSON through `x::render` with an empty images slice, using a throwaway test appended to `borg/src/x/tests.rs` for one run and reverted (nothing committed, nothing under `docs/`).
### Deviations
- Fixture sources carry no image sections: two of the three posts (iximiuz, steipete) have a photo, and describing it needs Vision (Anthropic), which this phase was told not to call. The fixtures therefore render post text only.
- `otto ci` was run with `GEMINI_API_KEY` unset (`env -u GEMINI_API_KEY otto ci`), so `bin/promptfoo-check` printed `NOT RUN: GEMINI_API_KEY is not set` instead of regenerating through the distiller (which calls Anthropic). It decided `running: differs from origin/main` for all three thread patterns. The grader has therefore not scored this pattern change.
### Tradeoffs
- Kept the three patterns in sync with short repeated paragraphs rather than a shared include: patterns are standalone prompts with no include mechanism.
### Open questions
- STALE, left untouched: each fixture's `distilled.yml` (the golden expected note, distilled from the old scrape; e.g. steipete and iximiuz say "no replies are shown", vllm says "positive engagement") and `capture-note.txt` (written from the old source; text still true of the posts). `distilled.yml` should be regenerated through the new patterns by someone willing to call the distiller (`sb borg eval --regenerate`), and `bin/promptfoo-check` run with GEMINI_API_KEY set.
- Photo sections for the iximiuz and steipete fixtures are absent; add them when a Vision-enabled regenerate is acceptable.

## Phase 4: Threads render no in-note transcript
### Design decisions
- `RenderOptions::for_url_publish` returns `include_transcript: false` unconditionally and takes `_distilled` unused — `distillers/src/render.rs` — the signature stays so the one borg call site and its callers do not churn; a per-kind seam can return without an API change.
- `NoteType::transcript_from_staging` adds `Social | Reddit` — `vault/src/schema.rs` — cortex's embed source (`cortex/src/embed.rs`, `embed/source.rs`) keys on it, so thread transcript chunks now come from staging/archive.
- `Distilled.transcript` stays set for threads; only the in-note render changed.
- Flipped by name: `distillers` render test's thread arm now asserts no `## Transcript`; `render_options_thread_keeps_its_transcript` renamed `render_options_thread_is_transcript_free` and asserts false.
- New tests: `for_url_publish_thread_omits_transcript` (`distillers/src/render/tests.rs`), `transcript_from_staging_covers_social_and_reddit` (`vault/src/schema/tests.rs`).
- Docs updated: `for_url_publish` and `transcript_from_staging` doc comments, `render_options_for_kind` comment, `distillers/AGENTS.md` transcript bullet, root `CLAUDE.md` one-way-data-flow paragraph (Video/Article/Social/Reddit). `vault/AGENTS.md` named no thread split, so it is unchanged.
### Deviations
- None.
### Tradeoffs
- Unconditional `false` vs removing the `Distilled` parameter — keeps the call sites stable.
### Open questions
- `bin/strip-transcripts` scopes itself by `transcript_from_staging`, so its sweep now also covers Social/Reddit notes if re-run. It is a one-off tool; confirm that is acceptable or that it should not be re-run.
- Rollout steps 4-6 (reingest of the 34 X notes after the release carrying this phase, Acceptance Criteria 3 and 5 on ripr) are post-release and not executed here.

## Implementation audit fold-in (panel round 1, synthesis `~/.cache/review-panel/runs/CvpeNwww/synthesis.md`)

### Design decisions
- Quoted X Article renders its blocks (`borg/src/x.rs`: `render`, `heading`, `body`): the quote now goes through the same `body()` as the post, under `## Quoted article by <name> (@handle): <title>`. `has_text` checks `body()` for both, so a post whose only content is a quoted Article is no longer rejected. Test: `x_post_renders_quoted_article_blocks`. Audit finding: Phase 1 rendered only `quote.text`, dropping a quoted Article's body.
- `bin/strip-transcripts` scope pinned to Youtube/Video/Article (`bin/strip-transcripts/src/lib.rs`: `in_scope_kind`) instead of following `NoteType::transcript_from_staging`. The panel measured that HEAD stripped a `type: social` note's transcript to EOF while v0.18.1 left it alone; 25 of 34 X notes have no staging or archive, so a re-run before the reingest would destroy their only transcript copy. `in_scope_kind_covers_youtube_video_article_only` now asserts social and reddit are out of scope.

### Deviations
- Supersedes Phase 4's "Deviations: None": stale thread-keeps-transcript comments remained at `borg/src/pipeline.rs` (transcript-emission policy), `cortex/src/embed.rs` (embed source comment), `distillers/src/render/tests.rs` (`site1_url_publish_policy_per_kind`). Fixed here.

### Tradeoffs
- Pinning strip-transcripts' kind set vs. adding an archive check to it: the tool is a one-shot for the 2026-07-07 design; pinning restores its original behavior exactly, an archive check would be new behavior on a tool meant never to run again.

### Open questions
- Supersedes Phase 3's remedy for the stale `config/eval/distill-fixtures/thread/x-com-*/distilled.yml` goldens: `sb borg eval --regenerate` grades in memory and writes nothing (`borg/src/eval/regenerate.rs`), so it does not refresh them. They stay stale until rewritten by hand or by a new tool.
- Deferred by the panel, for Scott: `cortex/src/summarize.rs:335` backfill always writes the transcript into the note, while cortex now reads Social/Reddit transcripts only from staging (same as Article since 2026-07-07). Affects the one trace-less Reddit note and X notes until reingested.
- Deferred by the panel, for Scott: `pipeline.fxtwitter-timeout-secs` is absent from `config/templates/borg.yml.example` (as is `jina-timeout-secs`).
- The changed thread patterns were never scored by `bin/promptfoo-check` (`otto ci` ran with `GEMINI_API_KEY` unset); no HN regression check exists for them.

## Post-release fix: fxtwitter User-Agent (v0.18.2 rollout)

### Design decisions
- `x::fetch_post` sends `x::USER_AGENT` (`borg/src/x.rs`), built through `vault::http::builder` like `GitHubFetcher::new`. Cause: the first live reingest on ripr (vic post, v0.18.2) failed `fetch-failed: fxtwitter API returned status 401 Unauthorized`. fxtwitter's body: "You must identify yourself with a User-Agent header". reqwest sends no User-Agent by default; Phase 0 probed with curl, which does, and the axum stubs never checked the header.
- Recurrence guard: the `borg/src/x/tests.rs` stub now answers 401 to a request with no User-Agent, as the live API does, and `x_post_fetch_sends_descriptive_user_agent` pins the exact header. Removing `.user_agent(USER_AGENT)` fails 10 `x_post_` tests (measured).

### Deviations
- None.

### Tradeoffs
- Photo downloads (`pipeline/photos.rs`) keep the plain client: `pbs.twimg.com` returned 200 `image/jpeg` to a request with no User-Agent (measured from ripr).

### Open questions
- None.

## Panel round 2 fold-in (synthesis `~/.cache/review-panel/runs/CvpeNwww/synthesis.md`)

### Design decisions
- Quoted X Article headings nest under the quote (`borg/src/x.rs`: `body(post, level)`, `render_blocks(blocks, level)`): `header-one`/`-two` render at `##` under the post and `###` under `## Quoted article by`, deeper headers one below. The panel listed this as a defer; folded in because it is a two-argument change and deferral is Scott's call.
- New test `x_post_quote_with_empty_text_and_article_blocks_has_text`: the case 25169d3 fixed (quote text empty, Article blocks present: old `has_text` false, new true). The panel showed `x_post_renders_quoted_article_blocks` still passed with the old `has_text`, because the fixture's quote text is a bare Article link the old check already counted. Reverting `has_text` and the heading level now fails both tests (measured).

### Deviations
- Undisclosed in 25169d3, disclosed here: a quote whose text is a bare Article link with no blocks went from `has_text` true (link rendered) to false, so an image-less post of that shape now fails `fetch-failed`. It matches the top-level bare-link rule (a bare link is not content).
- Stale thread-keeps-transcript comments the first fold-in missed: `cortex/src/embed/tests.rs` (`process_transcript_batch_verbatim_kind_still_reads_in_note_section`), `vault/src/distilled.rs` (`transcript` field doc), `distillers/src/article.rs` (transcript comment). Fixed.

### Tradeoffs
- None.

### Open questions
- Correction to the User-Agent section: removing `.user_agent(USER_AGENT)` fails 15 of 26 `x_post_` tests (the panel's count, every fetch-backed test), not 10. The earlier count came from a filtered output that cut off at 10 lines.
