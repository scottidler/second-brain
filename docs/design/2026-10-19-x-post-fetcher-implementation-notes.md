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
