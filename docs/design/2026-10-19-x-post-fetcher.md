# Design Document: X Post Fetcher

**Author:** Scott Idler
**Date:** 2026-10-19
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

X (Twitter) post ingests publish the logged-out x.com page: login wall, "Relevant people", "Trending now", bare engagement counters, footer links. The attached image, often the actual content, is lost. This design reads X posts from the fxtwitter JSON API (mirroring the Hacker News items-API fetcher), describes each attached photo through borg's existing Vision + OCR path, fails closed when the API fails, and drops the in-note `## Transcript` from every thread kind.

## Problem Statement

### Background

- A URL on an X/Reddit/HN host is a thread (`is_thread_url`, `borg/src/stages/raw.rs:62-77`). Readability extraction is skipped for threads.
- HN got a structured fetcher (d3c5653: `borg/src/hackernews.rs`, `process_hn_thread_from` at `borg/src/pipeline/handlers.rs:619`) because the rendered page carried vote arrows and nav links into the transcript.
- X never got one. It falls to `fabric -u` -> Jina (`borg/src/pipeline.rs:812-822`).
- This was a parked decision:
  - `docs/design/2026-05-16-extractor-contract-and-l2-summaries.md:872` judged rendered markdown "sufficient"
  - `docs/design/2026-07-05-distillation-knowledge-extraction.md:45` parked specialized thread fetchers, revisit if transcript quality caps claims
  - It does (below). The revisit condition is met.
- `docs/design/2026-07-07-distillation-output-restore.md` moved Video/YouTube/Article transcripts out of notes into staging. Its line 43 says "Thread also unchanged", with no stated reason.

### Problem

Measured on trace `ht-957555b4` (`notes/vicvijayakumar-on-x-vic-vijayakumar-argues-that-every-org-is-independently.md`):

- **Chrome in the note.** `fetched.yml` says `extractor: jina`, 5651 bytes. The page scrape renders verbatim as `## Transcript` because:
  - `distill_for_publish_thread` (`borg/src/stages/distill.rs:820`) has no source gate (the article gate `gate_article_transcript` at `distill.rs:391` is article-only)
  - the thread distiller keeps its input as `Distilled.transcript` (`distillers/src/thread.rs:486`)
  - `RenderOptions::for_url_publish` renders `## Transcript` for any `KindPayload::Thread` (`distillers/src/render.rs:37-41`)
- **Wrong claims.** The scrape interleaves the quoted post with replies, and `borg/patterns/distill-thread.md` frames every non-OP post as a reply:
  - the note says @boristane "replied"; fxtwitter shows @boristane is the **quoted post**
  - claims cite replies from @staysaasy and @nkohari
  - `cortex-thread-post-count: 4` counts replies
- **Lost image.** The diagram (the quoted post's photo) reached the distiller as `![](https://pbs.twimg.com/media/...)`. The summary guesses at it from the text.
- **Scope across the vault:** 34 `type: social` notes, all x.com. 21 carry the `Log in](/i/` chrome. 34 carry `## Transcript`.

### Goals

Every requirement traces to Scott's direction in the 2026-10-19 session.

- An X note is built from ONLY ("on an X post this is probably the only relevant part", screenshot of post + quote + diagram):
  - the post text and its author
  - the quoted post's text and its author
  - each attached photo, described by the same Vision + OCR path image ingest uses
- No replies ("i dont give a fuck about replies").
- No counters, views, timestamps, avatars, bios, login wall, trending, footer ("all of this is trash", "NOBODY GIVES A SHIT ABOUT THIS CRAP").
- No in-note `## Transcript` on thread notes ("what is the purpose of keeping transcript in the note at all. I thought we had eradicated it entirely").
- Existing X notes rebuilt to the new shape.

### Non-Goals

Excluded:

- Replies of any kind, including a reply-post's parent (1 of 34 posts is itself a reply; it ingests as a standalone post).
- Engagement metrics, view counts, post timestamps as content.

Parked, with revisit condition:

- **Video in posts** (2 of 34). Not described; the post text still ingests. Revisit if a video-only post lands an empty note.
- **Images inside X Articles** (`article.media_entities`). Article text is rendered (Resolved Decision 3), its images are not. Revisit if an Article note's claims show the same image-guessing failure.
- **Storing photos as vault assets.** Describe only. Revisit if Scott wants the image visible in Obsidian.
- **Deterministic `author` / `post-count` from JSON.** Labeled input lets the LLM get them right; no distiller seam change. Revisit if the post-rollout vic criterion (Acceptance Criteria 4) shows misattribution.
- **`?s=46` in `source:`.** `TRACKING_PARAMS` (`borg/src/hygiene.rs:23-74`) has no `s`; 1 of 34 sources carries it. The fetcher parses the id from the path, so it is unaffected. Revisit as its own targeted fix.
- **Non-status x.com URLs** (profiles, `x.com/i/article/<id>` links). `status_id` returns `None`; they keep today's fabric/Jina chain. Revisit when one lands in the vault.
- **`reingest --type` misses renamed notes; `--source` help says "substring" but matching is exact** (`borg/src/client.rs:267`, `vault/src/ledger.rs:327`). Rollout works around both (step 3). Revisit as their own targeted fix.
- **A Reddit fetcher.** 1 `type: reddit` note exists (2024-08-05, no trace). No measured problem.

## Proposed Solution

### Overview

```
x.com status URL
  -> x::status_id(url)                         (new)
  -> GET api.fxtwitter.com/status/<id>         (new, borg/src/x.rs)
       Err / 404 / non-JSON -> Err -> receipt fetch-failed  (no scrape fallback)
  -> download each photo (post + quote)
  -> per photo, ocr_extract (spawn_blocking) || vision_extract (VISION_PERMITS)
       -> shared image-section builder (same merge as image ingest)
       photo failed -> section omitted, receipt degraded
       every photo failed AND no post text -> Err -> fetch-failed
  -> x::render -> labeled markdown                         (new)
  -> distill_for_publish_thread (pattern taught post/quote/image)
  -> render with NO ## Transcript (all threads)
  -> transcript lives in staging/archive; cortex embeds from there
```

### Architecture

- **`borg/src/x.rs`** (new), a copy of the `hackernews.rs` shape:
  - `pub const API_BASE: &str = "https://api.fxtwitter.com"`
  - `pub fn status_id(url: &str) -> Option<u64>`: `/<user>/status/<id>` on x.com/twitter.com/mobile.twitter.com, query ignored
  - `pub async fn fetch_post(api_base, id, timeout) -> Result<Post>`: rejects non-2xx, rejects a content-type that does not start with `application/json` (observed: `application/json`), rejects `tweet: null`
  - `pub fn render(post: &Post, images: &[DescribedImage]) -> (String /*title*/, String /*md*/)`
  - tests in `borg/src/x/tests.rs` (no inline test modules: `bin/source-lint`)
- **`borg/src/pipeline/handlers.rs`**: `process_x_post` + `process_x_post_from(api_base, ...)`, mirroring `process_hn_thread_from` (`:619`):
  1. fetch
  2. download + describe photos
  3. render
  4. `stages::raw::persist_fetched_if_staging(.., "fxtwitter-api", 200, Some("text/markdown"))`
  5. `run_gate_1`
  6. return `(title, md, None)` plus a `degraded: bool`
- **`borg/src/pipeline.rs:798`**: a dispatch arm beside the HN arm. An X status URL takes it; on `Err` it returns `Err` (fail closed). It never reaches the fabric/Jina chain.
- **Shared image-section builder**: extract the merge + build from `process_image_inner` (`handlers.rs:738-815`) into one fn, `image_section(vision: Option<&VisionResult>, ocr_text: &str) -> String`, used by both `process_image_inner` and the X path. It carries image ingest's existing rules unchanged: vision preferred for extracted text, OCR as fallback, `## Description` only when vision returned one, `Extracted Text` vs `OCR Text` label by whether vision ran. Siblings cannot drift.
- **Photo description** runs per photo exactly as image ingest does: `ocr::ocr_extract` under `tokio::task::spawn_blocking` (temp file, `pipeline.ocr-timeout-secs`) joined with `ocr::vision_extract` when `vision.enabled`. Only the vision call takes `VISION_PERMITS` (`borg/src/ocr.rs:38-71`, acquired in `vision_call`); OCR takes no permit.
- **Permit hazard**: the X path must NOT re-enter `process_content`, which takes a GENERAL permit while the URL trace already holds one (`pipeline.rs:240`). Calling the `ocr` functions directly avoids it.
- **Photo outcomes** (each photo independently):

  | case | section | degraded |
  |---|---|---|
  | vision + OCR ok | Description + Extracted Text | no |
  | `vision.enabled: false`, OCR ok | OCR Text | no (operator choice, same as image ingest) |
  | vision enabled but `Err`, or `Ok` with empty description (`vision_call` returns `Ok("")` on a response with no text block, `ocr.rs:189-196`), OCR ok | OCR Text | yes |
  | builder returns empty (vision and OCR both empty) | omitted | yes |
  | download fails, non-2xx, or Content-Type not `image/*` | omitted | yes |

- **Image-only post with every photo failed**: if the post's and quote's `text` are both empty after trim (and no Article blocks) and no photo produced a section, `process_x_post` returns `Err` -> `fetch-failed`. On a reingest the old note stays untouched. Publishing an empty note over a useful one is the failure this prevents.
- **Degraded flag**: rename `youtube_fell_back` (`pipeline.rs:744`) to `fetch_degraded`, set by either the YouTube engine fallback or any degraded photo above. `receipt_degraded` (`pipeline.rs:1223`) is unchanged in shape.
- **Transcript policy (all threads)**:
  - `RenderOptions::for_url_publish` (`distillers/src/render.rs:37-41`): `include_transcript: false` for every URL kind
  - `NoteType::transcript_from_staging` (`vault/src/schema.rs:210`): add `Social | Reddit`
  - HN notes are `type: article`, already staging-sourced for embeddings (`schema.rs:210`)
  - cortex already reads archive -> staging (`cortex/src/embed/source.rs:41`)
  - **Accepted trade: BM25/FTS keyword reach.** Oracle's FTS indexes `note.body` (`vault/src/search/index.rs:304`). Words that appear only in a thread's transcript stop keyword-matching. This is the same trade the 2026-07-07 design accepted for Video/Article (its Resolved Decision "BM25/FTS keyword reach over transcripts is an accepted loss"): the shipped retrieval default is vector-first, transcript-chunk embeddings survive from staging/archive, and exact wording goes through the trace ref. For X specifically, the clean post text is what the distiller quotes into Claims, so most of it stays in the body

### Data Model

fxtwitter response, only the fields read (all `Option` where the API omits or nulls them; top-level `media` is JSON `null` when absent):

```rust
struct Envelope { code: u16, tweet: Option<Post> }
struct Post {
    id: String,
    text: String,                 // complete for long posts (is_note_tweet), t.co already expanded
    author: Author,               // name, screen_name
    quote: Option<Box<Post>>,
    media: Option<Media>,         // photos: Vec<Photo { url, width, height }>
    article: Option<Article>,     // X Articles: title + content.blocks[] (Draft.js {type, text})
}
```

Rendered markdown handed to the distiller (the vic post):

```
# Post by Vic 📠 (@VicVijayakumar)

every single org is building this loop right now.

if you're at a bigco, ...

## Quoted post by boris (@boristane)

the only sane code review loop

### Image 1 (in @boristane's post)

#### Description
A flowchart depicting an AI-assisted code review process. ...

#### Extracted Text
Agent generates code
...
```

X Article post: `# Article by <name> (@handle): <article.title>` then block text in order (`header-*` blocks become `##`/`###`, `atomic` blocks skipped).

### API Design

- Config: `pipeline.fxtwitter-timeout-secs` (kebab-case under the existing `rename_all`), default 30. HN reuses `jina-timeout-secs`; that name lies for a non-Jina API, so X gets its own.
- Photo URL: fxtwitter's `?name=orig`. MIME from the response `Content-Type`, not the extension (`ocr::mime_from_extension` parses `jpg?name=orig` as an extension).
- `extractor:` label in `fetched.yml`: `fxtwitter-api`.

### Implementation Plan

#### Phase 0: Prove fxtwitter and the Vision path on the 34 vault posts (DONE, 2026-10-19)
**Model:** sonnet
- Zero code. Probes run against the 34 vault X posts; JSON kept in the session scratchpad, not committed.
- **Results:**
  - all 34 posts: HTTP 200 JSON from `/status/<id>` (handle-less path works); 34 back-to-back calls, 0 non-200, no rate-limit headers
  - 17 are long posts: `text` complete (up to 7255 chars)
  - 3 are X Articles: `text` empty or a bare `x.com/i/article/` link; content only in `article.content.blocks` (20, 93, 643 blocks)
  - missing post: HTTP 404 JSON `{"code":404,"message":"NOT_FOUND","tweet":null}`
  - malformed id (`/x/status/1`): HTTP 200 **text/html**, so content-type must be checked
  - 34 unique photos, all `image/jpeg`, largest `orig` 750069 bytes (Anthropic image cap is 5 MB)
  - the HU82 diagram, `orig` and `large` both 65889 bytes
  - borg's exact `vision_extract` prompt on `claude-sonnet-5` against the diagram returned every node and edge label in TEXT, and DESCRIPTION: "A flowchart depicting an AI-assisted code review process. Code generated by an agent goes through self-verification, a second agent's review, and automated checks before reaching a decision point. If all checks pass, the code ships; if not, it either loops back to code generation (for resolvable issues) or goes to human review (for novel issues), with human review feeding back into code generation."
  - tesseract on the same image: node labels recovered, layout scrambled (vision TEXT is the better extract)
- **Not proven:** protected/suspended accounts (no test id available). The fetcher treats any non-2xx or `tweet: null` as `Err`, so the unknown case fails closed.

#### Phase 1: fxtwitter client and renderer, no images
**Model:** sonnet
- `borg/src/x.rs`: `API_BASE`, `status_id`, serde types, `fetch_post`, `render` (images slice empty)
- `pipeline.fxtwitter-timeout-secs` config key, default 30
- Fixtures: the vic post JSON, one X Article JSON, one long-post JSON, the 404 body, an HTML body
- Tests (axum on `127.0.0.1:0`, the `hackernews/tests.rs:80-98` pattern):
  - `x_post_renders_post_and_quote`
  - `x_post_renders_no_chrome_or_counts`
  - `x_post_renders_article_blocks`
  - `x_post_404_is_err`, `x_post_html_body_is_err`
  - `x_post_status_id_ignores_query_and_host_variant`
  - `x_post_title_passes_quality_gate`: `detect_blocked_content` returns `None` for the render's title
  - `x_post_tweet_null_is_err` (HTTP 200, `tweet: null`), `x_post_malformed_json_is_err`, `x_post_timeout_is_err` (stub server sleeps past the timeout)
- **Success criteria:**
  - `cargo test --workspace -- x_post_` runs >= 10 tests, all pass (filter is `x_post_` with the underscore: the existing `router::tests::test_social_x_post` matches bare `x_post`)
  - `x_post_renders_post_and_quote` asserts the vic render contains `@VicVijayakumar` and `@boristane` and none of `Log in`, `Trending`, `Relevant people`, `Views`

#### Phase 2: Photo description and dispatch
**Model:** opus
- Extract the shared image-section builder from `handlers.rs:803-815`; `process_image_inner` calls it
- Download each photo (post, then quote), `vision_extract` + `ocr_extract` concurrently under `VISION_PERMITS`
- `process_x_post` / `process_x_post_from` in `handlers.rs`; dispatch arm at `pipeline.rs:798`; X status URLs never reach fabric/Jina
- `youtube_fell_back` -> `fetch_degraded`; a failed photo description sets it and omits that photo's section
- Vision test seam: `ocr.rs:171` hardcodes the Anthropic URL. Add a base-URL seam (task-local scope, the `borg/src/youtube/engine/bases.rs` pattern) so no test calls Anthropic
- Tests:
  - `x_post_image_description_under_quoted_post`: stubbed vision; the description lands under `## Quoted post by`
  - `x_post_fetch_error_fails_closed`: driven through the URL dispatcher (`process_url_inner`), not only `process_x_post_from`. API 500 -> receipt `fetch-failed`; stub fabric and Jina servers record zero hits; a pre-existing note at the target path is byte-identical afterward
  - `x_post_vision_error_degrades`: vision 500, OCR stub ok -> section is `OCR Text`, `fetch_degraded == true`
  - `x_post_vision_empty_text_degrades`: vision 200 with no text block -> `fetch_degraded == true`
  - `x_post_photo_download_error_omits_section`: photo 404 -> no `### Image` section, `fetch_degraded == true`
  - `x_post_image_only_all_failed_is_err`: empty `text`, every photo 404 -> `Err`
  - existing image-ingest tests pass unchanged
- **Success criteria:**
  - `cargo test --workspace -- x_post_` runs >= 13 tests, all pass
  - `cargo test --workspace -- image` passes >= 15 tests (15 on main before the refactor), 0 failed

#### Phase 3: Thread pattern framing
**Model:** sonnet
- `borg/patterns/distill-thread.md` (+ `-chunk.md`, `-reduce.md`): an X input is a post, an optional quoted post, and image descriptions; a quoted post is not a reply; image descriptions are evidence for claims; HN input (OP + comments) keeps its current framing
- Regenerate the 3 X golden fixtures (`config/eval/distill-fixtures/thread/x-com-*/source.md`) from fxtwitter through `x::render`
- **Success criteria:**
  - `rg -c 'Log in|Trending' config/eval/distill-fixtures/thread/x-com-*/source.md` returns no matches (observed on main: 6-8 per fixture)
  - `rg -c -i 'quoted post' borg/patterns/distill-thread.md` returns >= 1 (observed on main: no match)
  - the LLM-side check (vic note attributes the quote correctly) is an acceptance criterion after rollout, not a phase test: no test calls Anthropic

#### Phase 4: Threads render no in-note transcript
**Model:** sonnet
- `distillers/src/render.rs:37-41`: `for_url_publish` sets `include_transcript: false` for every URL kind
- `vault/src/schema.rs:210`: `transcript_from_staging` adds `Social | Reddit`
- Update the doc comments and the `distillers/AGENTS.md` / `vault/AGENTS.md` lines that say threads keep an in-note transcript; update the root `CLAUDE.md` one-way-data-flow paragraph if it names the split
- `Distilled.transcript` stays SET for threads (`distillers/AGENTS.md:24` "regression-guarded, do NOT revert"): it is what lands in `distilled.yml` and what cortex embeds. Only the in-note render changes, the same split Video already has
- Flip the existing assertion at `distillers/src/render/tests.rs:841-854` ("thread publish body must keep its transcript") to assert the thread body has NO `## Transcript`; grep for any other test pinning the old thread behavior and flip it the same way
- Tests: `for_url_publish_thread_omits_transcript`; `transcript_from_staging_covers_social_and_reddit`
- **Success criteria:**
  - `cargo test --workspace -- for_url_publish_thread_omits_transcript transcript_from_staging_covers_social_and_reddit` runs 2 tests, both pass
  - `otto ci` exits 0 (catches any other test still pinning the old thread render)

### Rollout (operator steps, after the release lands)

1. `otto deploy` on ripr (the daemon host), then confirm the daemon runs the new build: `sb --version` on ripr prints the released tag
2. `sb borg reingest --type social` (`borg/src/client.rs:303`): re-POSTs each X source with `force`, re-fetches, overwrites in place (the pipeline finds the old note by `source:` first, `pipeline.rs:671`). Dry-run on main selects 30 of the 34
3. The other 4 are skipped by `--type` because `note_has_type` (`client.rs:267`) looks the note up by the ledger's filename, and these 4 were renamed since (a `lütke` -> `lutke` diacritic slug, two bare-id slugs `[[2073625424585015596]]`, `[[2073850909684293780]]`, one `dörr` -> `dorr`). Reingest each by exact source (`--source` is exact-match, `vault/src/ledger.rs:327`); each dry-runs to 1 entry on main:
   - `sb borg reingest --source https://x.com/tobi/status/1909251946235437514`
   - `sb borg reingest --source https://x.com/tom_doerr/status/2067473155988332909`
   - `sb borg reingest --source https://x.com/GithubProjects/status/2073625424585015596`
   - `sb borg reingest --source https://x.com/omarsar0/status/2073850909684293780`
4. `sb borg wait`; exit 0 = drained clean, 3 = some failed (inspect `sb borg log --stage fetch-failed --since 1h`)
5. Embeddings: the rewrite advances `modified_at`, which clears the examined sentinel (`vault/src/search/vector.rs` `stale_embedding_targets`); cortex re-embeds from the new archive. `sb borg wait` proves borg drained, not that cortex caught up: after the next cortex embed tick, Acceptance Criterion 5 checks every X note has transcript-chunk rows
7. Recovery for a failed X ingest: `sb borg replay <trace>` needs the staged trace (`borg/src/replay.rs:438`). After staging expires, recover with `sb borg reingest --source <exact URL>`
6. HN notes keep their old in-note `## Transcript` until reingested. Reingest is optional (FTS-only duplicate)

Ordering: the reingest must run after the release that carries Phase 4. Of the 34 X notes, 25 have no staging and no archive on ripr; reingesting rebuilds both, so none lose their transcript embedding.

## Acceptance Criteria

Criteria 2-5 run after rollout. Each starts from one file list and fails loudly if the list is not the 34 X notes, so an empty glob cannot pass as a false 0:

```
cd ~/repos/scottidler/obsidian
S=$(rg -l '^type: social' notes inbox); test "$(echo "$S" | wc -l)" -eq 34 || echo "BAD CORPUS"
V=$(fd -t f 'vicvijayakumar-on-x' notes inbox); test -n "$V" || echo "BAD CORPUS"
```

- [ ] 1. `cargo test --workspace -- x_post_` runs >= 13 tests, all pass
  - Observed on main: 0 tests (Phases 1-2 not built; `router::tests::test_social_x_post` does not match `x_post_`)
- [ ] 2. No X note carries the login-wall chrome: `echo "$S" | xargs rg -q 'Log in\]\(/i/'` exits 1
  - Observed on main: exits 0 (21 of 34 files match)
- [ ] 3. No X note renders a transcript: `echo "$S" | xargs rg -q '^## Transcript'` exits 1
  - Observed on main: exits 0 (34 of 34 match)
- [ ] 4. The vic note carries the diagram and the right attribution, and no replies:
  - `rg -q -i 'self-verif' $V` exits 0 (the diagram reached Summary or Claims)
  - `rg -q -i 'quot(ed|es|ing).{0,40}boristane|boristane.{0,40}quot' $V` exits 0 (boris is the quoted author)
  - `rg -q 'staysaasy|nkohari' $V` exits 1
  - `rg -q -i 'repl(y|ies|ied|ying)' $V` exits 1
  - Observed on main: `self-verif` exits 1; quoted-boristane exits 1; `staysaasy|nkohari` exits 0; `repl(...)` exits 0
- [ ] 5. Every X note has transcript-chunk embeddings after the cortex tick, run on ripr (the cortex host; db path `vault::paths::oracle_db_path`, `vault/src/paths.rs:408`; kind string `EmbeddingKind::TranscriptChunk.as_str()`, `vault/src/search/vector.rs:60`):
  ```
  IN=$(echo "$S" | sed "s/.*/'&'/" | paste -sd,)
  sqlite3 -readonly ~/.local/share/sb/oracle/oracle.db "SELECT COUNT(DISTINCT note_path) FROM note_embeddings WHERE kind='transcript-chunk' AND note_path IN ($IN)"
  ```
  returns 34
  - Observed on main (desk's oracle.db, in-note transcript source): 34

## Resolved Decisions

1. **2026-10-19, fallback when fxtwitter fails: fail closed.** Any non-2xx, non-JSON, or `tweet: null` -> `Err` -> receipt `fetch-failed`; retry via `sb borg replay <trace>`. No Jina fallback for X status URLs. HN's silent fallback is the pattern not to copy: a fallback publishes exactly the trash this design removes. (Author, from Scott's "all of this is trash"; taste: fail loudly, fail closed.)
2. **2026-10-19, in-note transcript dropped for ALL thread kinds**, not only X. Scott's "I thought we had eradicated it entirely" plus siblings-behave-identically. HN's in-note copy is already FTS-only.
3. **2026-10-19, X Articles render their block text.** For an Article post the body IS the post text; rendering only `text` publishes an empty note for 3 of 34 existing posts.
4. **2026-10-19, vision failure on one photo degrades, does not fail.** The post text still carries signal; the receipt is `degraded=true`, `sb doctor` warns, replay trues it up.
5. **2026-10-19, image-only post with every photo failed fails closed** (panel round 1, staff engineer). An empty note must never overwrite a useful one on reingest.
6. **2026-10-19, in-note transcript drop accepts the FTS trade** (panel round 1, staff engineer: "not a duplicate"). Named in Architecture; same trade as 2026-07-07 for Video/Article; Scott's eradication ask stands.
7. **2026-10-19, panel round 1 deferrals, each with its reason:**
   - VISION permit wait is not bounded by the 120 s HTTP timeout and the pool is shared: at an observed max of 3 photos per post this sits inside `hard_timeout_secs`; revisit if a receipt shows `pipeline-timed-out` on an X trace
   - fxtwitter rate limits under bulk reingest and failure-rate observability: no observed problem (34/34 back to back); `fetch-failed` receipts and `sb borg wait` exit 3 surface it if it happens
   - Phase 3's semantic check is LLM-side and `bin/promptfoo-check` is report-only (`bin/promptfoo-check:10`): moved to Acceptance Criterion 4, measured on the live note
8. **2026-10-19, `orig` photo size.** Measured max 750069 bytes across all 34 photos, far under the 5 MB image cap.

## Alternatives Considered

### Gate the scrape instead of replacing it
- **Description:** Keep fabric/Jina; drop the transcript and strip known X chrome lines.
- **Pros:** No new external dependency.
- **Cons:** The distiller still sees replies mixed with the quote (wrong claims); the image stays a bare URL; chrome stripping is a regex arms race against x.com markup.
- **Why not chosen:** Fixes the look, not the content.

### Official X API
- **Description:** `api.x.com/2/tweets/:id`.
- **Pros:** First-party.
- **Cons:** Paid tier for read access; a credential to custody.
- **Why not chosen:** fxtwitter returns everything needed, keyless, measured 34/34.

### Route photos through `process_image`
- **Description:** Publish each photo as its own image note and link it.
- **Cons:** Re-enters a GENERAL permit while holding one (deadlock risk); spawns notes Scott did not ask for.
- **Why not chosen:** Call `vision_extract` directly.

## Technical Considerations

### Dependencies
- External: `api.fxtwitter.com` (third-party, Cloudflare Workers), `pbs.twimg.com` (photos), Anthropic Messages API (existing).
- Crates: `reqwest`, `serde_json`, `axum`, `base64` already direct deps of borg. No new crate.

### Performance
- One JSON GET plus every photo on the post and its quote. X allows 4 photos per post, so up to 8 per ingest (observed max 3). Vision calls run concurrently under the shared `VISION_PERMITS` pool; each HTTP call is capped at 120 s, but the permit wait is not, so a saturated pool lengthens an X ingest (deferral in Resolved Decision 7). Bounded overall by `hard_timeout_secs` (1800).

### Security
- No credentials added. fxtwitter sees which post ids are ingested; acceptable for public posts.
- Photo bytes go to Anthropic, same as image ingest today.

### Testing Strategy
- Unit + axum-served integration tests per phase; no test touches the network or Anthropic (vision base seam, Phase 2).
- Fixtures from the Phase 0 probe JSON live under `borg/src/x/` test fixtures, never under `docs/`.

### Edge cases
- **Gate-1 and the quality gate:** `run_gate_1` scans the rendered markdown for block pages, and `detect_blocked_content` (`pipeline.rs:916`) checks the summary and `scraped_title`. The render's title is `@<handle> on X`, which matches none of `BLOCKED_TITLE_INDICATORS` (`borg/src/quality.rs:32`); a Phase 1 test pins it. `BLOCKED_CONTENT_INDICATORS` (`quality.rs:42`, e.g. `captcha`) can block a short summary that mentions one; that is today's behavior for every kind and this design leaves it alone.
- **Short posts:** a one-line post with no image renders a few hundred bytes. Neither gate has a minimum length; the distiller runs on it as it does on short HN items.
- **Deleted post on reingest:** 404 -> `Err` -> `fetch-failed`; the old note is untouched (the old note is deleted only after the new note's atomic write, `pipeline.rs:692`).
- **Quote of a quote:** not observed (neither quoting post in the 34 has a nested `quote`). Render one level; deeper nesting is ignored.

### Blast radius / ship order
- Single repo (second-brain). One release carries all phases; operator reingest follows.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| fxtwitter goes down or changes schema | Med | X ingests fail | Fail closed with `fetch-failed` receipts; `sb doctor` surfaces; replay after fix. All-`Option` serde absorbs additive changes |
| fxtwitter rate-limits a bulk reingest | Low | Some reingests fail | Measured 34 sequential with no 429; `sb borg wait` exit 3 names failures; replay them |
| Vision describes a photo poorly | Low | Weaker claims | Same prompt as image ingest; measured on the diagram |
| Thread-pattern edit regresses HN distillation | Med | HN claims change | Pattern keeps HN framing explicitly; promptfoo-check runs on pattern change |

## Open Questions

(none)

## References
- `docs/design/2026-07-07-distillation-output-restore.md`
- `docs/design/2026-05-16-extractor-contract-and-l2-summaries.md:872`
- `docs/design/2026-07-05-distillation-knowledge-extraction.md:45`
- d3c5653 `fix(borg): read Hacker News threads from the items API, not the page`
- b64500b `fix(borg): HN thread notes carry Hacker News as creator`
