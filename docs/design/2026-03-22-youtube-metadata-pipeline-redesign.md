# Design Document: YouTube Metadata Pipeline Redesign

**Author:** Scott Idler
**Date:** 2026-03-22
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Redesign the YouTube ingestion pipeline so that yt-dlp is the single authoritative source for all video metadata (title, creator, duration, description, tags), and fabric is used only for what it uniquely provides: transcript extraction and LLM summarization. This fixes the duration=0 bug, eliminates the fragile field-name mapping between fabric and yt-dlp, and simplifies the pipeline from a tangled primary/fallback model to a clear separation of concerns.

## Problem Statement

### Background

Borg's YouTube pipeline has two paths:

1. **Fabric path** (`process_youtube_fabric`): calls `fabric -y <url> --metadata` for metadata, `fabric -y <url> --transcript` for transcript, then a fabric pattern for LLM summary. Falls back to yt-dlp only when fabric returns "Unknown" title.

2. **Legacy path** (`process_youtube_legacy`): calls `yt-dlp --dump-json` for metadata and `yt-dlp` for subtitles. Used only when fabric binary is unavailable.

The code treats fabric as the primary metadata source and yt-dlp as a fallback. This mental model is wrong.

### Problem

Fabric's `--metadata` flag calls the YouTube Data API v3 requesting only `snippet` + `statistics` parts. It structurally cannot return duration, which lives in the `contentDetails` part. Fabric's Go source has a `GrabDuration()` function that requests `contentDetails`, but the CLI never wires it through to `--metadata` output.

This means:

1. **Duration is always 0** when fabric metadata succeeds (most ingestions). Duration was only correct when fabric's title lookup failed, triggering the yt-dlp fallback.

2. **Field name mismatches** between fabric (YouTube Data API v3 naming: `channelTitle`, `publishedAt`) and yt-dlp (`channel`/`uploader`, `upload_date`) required a fragile mapping layer in `parse_youtube_metadata()` that was already wrong once (v0.5.5 fix).

3. **Redundant calls.** The fabric path shells out to `fabric --metadata` and `fabric --transcript`. When fabric's title fails, it also shells out to `yt-dlp --dump-json` which returns everything fabric's metadata provides plus duration. Even in the happy path, yt-dlp for metadata alone would replace fabric's metadata call entirely.

4. **Two code paths** (`process_youtube_fabric` and `process_youtube_legacy`) that produce the same `YouTubeResult` but with different bugs and different field sources.

### Goals

- Duration is always correct for YouTube notes
- Single, complete metadata source with no field mapping complexity
- One YouTube processing function instead of two
- Fabric used only for what it uniquely provides (transcript, LLM summary)
- No change to note output format (frontmatter, callout, summary all stay the same)

### Non-Goals

- Calling the YouTube Data API v3 directly from borg (future option, not this change)
- Changing fabric's source code or submitting a PR to fabric (planned separately)
- Changing the transcript fallback behavior (audio extraction + Groq transcription stays as-is, just moves into the unified function)
- Changing how non-YouTube content is processed

## Proposed Solution

### Overview

Replace the two-path architecture with a single `process_youtube()` function that:

1. Calls `yt-dlp --dump-json` for all metadata (title, creator, duration, description, tags)
2. Calls `fabric -y <url> --transcript` for transcript (falling back to yt-dlp subtitles, then audio transcription)
3. Calls a fabric pattern for LLM summary

Each tool is used for exactly what it's best at. No overlap, no mapping, no fallback chains for metadata.

### Architecture

**Current (tangled):**
```
if fabric available:
  process_youtube_fabric():
    fabric --metadata ──► title, creator, description, tags (NO duration)
    │
    ├─ title == "Unknown"? ──► yt-dlp --dump-json ──► title, creator, duration
    │                          (but fabric description/tags preferred if non-empty)
    │
    ├─ title OK? ──► use fabric metadata as-is ──► duration = 0 (BUG)
    │
    fabric --transcript ──► transcript
    │  empty? ──► yt-dlp subtitles fallback
    │
    fabric pattern ──► LLM summary

else:
  process_youtube_legacy():
    yt-dlp --dump-json ──► all metadata (works correctly)
    yt-dlp subtitles ──► transcript
    │  fails? ──► audio extraction + Groq transcription
```

**Proposed (clean separation, parallel where possible):**
```
process_youtube():
                    ┌─ yt-dlp --dump-json ──► ALL metadata ──────────┐
                    │   (title, creator, duration, description, tags) │
  tokio::join! ─────┤                                                ├──► merge
                    │                                                │
                    └─ fabric --transcript ──► transcript ────────────┘
                         fallback: yt-dlp subtitles                  │
                         fallback: audio extraction + Groq           │
                                                                     ▼
                                                    fabric pattern ──► LLM summary
```

Metadata and transcript are independent - they run concurrently via `tokio::join!`. The LLM summary depends on the transcript, so it runs after both complete. This saves 1-3 seconds per ingestion (the yt-dlp call overlaps with the slower fabric transcript call).

### Data Model

No struct changes needed. `VideoMetadata` already has all fields from yt-dlp. `YouTubeResult` stays the same.

**Remove or simplify:**
- `YouTubeContent` struct in `fabric.rs` (no longer needed for metadata; fabric only returns transcript)
- `parse_youtube_metadata()` in `fabric.rs` (no longer needed; yt-dlp JSON is parsed in `youtube.rs`)
- `parse_iso8601_duration()` in `fabric.rs` (dead code; fabric never returns duration)

**fabric.rs changes:**
- `fetch_youtube()` becomes `fetch_transcript()` - only calls `fabric -y <url> --transcript`
- Remove `--metadata` call entirely
- Remove `YouTubeContent` struct (replace with simple `String` transcript return)

**youtube.rs - no changes needed.** `fetch_metadata()` already parses everything correctly.

### Implementation Plan

#### Pre-work: Revert uncommitted changes

There is an uncommitted change in `process_youtube_fabric()` that added a `ytdlp_meta` call as a duration supplement. This should be discarded - the redesign replaces the entire function.

#### Phase 1: Simplify fabric.rs

1. Replace `fetch_youtube()` with `fetch_transcript(url, config) -> Result<String>`
2. Remove `YouTubeContent` struct
3. Remove `parse_youtube_metadata()`
4. Remove `parse_iso8601_duration()`

#### Phase 2: Unify pipeline.rs

5. Merge `process_youtube_fabric()` and `process_youtube_legacy()` into a single `process_youtube()`
6. New function flow:
   - Run metadata and transcript concurrently via `tokio::join!`:
     - `youtube::fetch_metadata(url)` - yt-dlp for all metadata (wrap blocking call in `spawn_blocking`)
     - `fabric::fetch_transcript(url, config)` - fabric for transcript if available.
       Fabric uses the YouTube captions API which returns cleaner, properly punctuated text
       compared to yt-dlp's raw subtitle scraping. This is fabric's real value for YouTube.
   - If fabric transcript is empty or unavailable, fall back to `youtube::fetch_subtitles(url)`
   - If subtitles unavailable, fall back to audio extraction + Groq
   - Call `fabric::summarize()` for LLM summary (depends on transcript, runs after join)
7. Convert blocking `std::process::Command` calls to async-friendly `tokio::process::Command` or wrap in `spawn_blocking` to avoid blocking the tokio runtime
7. In `ingest_url()`, replace the `if use_fabric { process_youtube_fabric } else { process_youtube_legacy }` branch with a single `process_youtube()` call
8. Inside `process_youtube()`, check `fabric::is_available()` to gate transcript and summary calls only - metadata always comes from yt-dlp regardless
9. Convert `youtube::fetch_metadata()` and the new `fabric::fetch_transcript()` to async (either `tokio::process::Command` or `spawn_blocking` around the existing `std::process::Command`)

#### Phase 3: Clean up

10. Remove the fabric/yt-dlp field mapping table comment from `fabric.rs` (no longer relevant)
11. Update the description extraction design doc to reflect the new architecture
12. Update tests

#### Phase 4: Fix reingest dedup - find old note by source URL, not ledger path

The current dedup logic in `ingest_url()` reads the old note's file path from the ledger entry, then tries to delete it. This breaks when cortex has moved the file (e.g., `inbox/foo.md` -> `notes/foo.md`) because the ledger still stores the original inbox path.

**Fix:** When replacing an existing note, scan the vault for any `.md` file whose `source:` frontmatter matches the canonical URL, and delete that file regardless of where it lives. The ledger is still used for fast dedup detection ("have I ingested this URL before?"), but file location comes from the vault itself.

13. Add a `find_note_by_source(vault_root, canonical_url) -> Option<PathBuf>` function to search the vault for a note matching the given source URL
14. In the dedup/replace block of `ingest_url()`, use `find_note_by_source()` instead of (or in addition to) the ledger's stored path to locate and delete the old note
15. Preserve the original note's `date:` field - read it from the old note before deleting, then patch it into the new note after writing

This directly enables the 150-note YouTube backport reingest, where old notes may be in `inbox/` or `notes/` and the original ingestion date must be preserved.

## Alternatives Considered

### Alternative 1: Fabric metadata + yt-dlp duration supplement

- **Description:** Keep fabric `--metadata` as primary for title/creator/description/tags/published, add a separate yt-dlp call just for duration.
- **Pros:** Minimal code change from current state. Half-implemented already (the uncommitted code in `process_youtube_fabric`).
- **Cons:** Still two metadata sources. Still need the field-name mapping layer (`channelTitle` vs `channel`, `publishedAt` vs `upload_date`). Three shell-outs per ingestion. The yt-dlp call already returns everything fabric provides, making fabric's metadata call pure waste.
- **Why not chosen:** Adds complexity to solve a problem that disappears entirely if we just use yt-dlp for all metadata.

### Alternative 2: Direct YouTube Data API v3 calls from borg

- **Description:** Bypass fabric for metadata entirely. Use the YouTube API key (already configured for fabric) to call the API directly from borg, requesting `snippet` + `contentDetails` + `statistics` in one call.
- **Pros:** Fastest option (single HTTP call vs shell-out to yt-dlp). Clean, well-defined JSON schema. Full control over requested fields. Gets view count, like count, category if we ever want them.
- **Cons:** New dependency (google-youtube3 crate or raw HTTP). API key management in borg config. YouTube API quota limits (10,000 units/day, each Videos.list costs 1 unit). More code to write and maintain.
- **Why not chosen now:** yt-dlp already works, is already integrated, and solves the problem today. This is the right long-term architecture if we need richer metadata (view counts, like counts) or hit yt-dlp reliability issues. Could be implemented later without changing the pipeline structure - just swap `youtube::fetch_metadata()` internals from yt-dlp to API calls.

### Addendum: Fabric PR (separate work item)

A PR to the fabric project (github.com/danielmiessler/fabric) should add `contentDetails` to the `--metadata` call and include duration in the `VideoMetadata` struct. The Go code already has `GrabDuration()` calling `contentDetails` - it just needs to be wired into `GrabMetadata()` or added as a combined call. This benefits all fabric users, not just borg.

This is tracked separately and does not block or change this redesign. Even after such a PR lands, borg's pipeline should still use yt-dlp as the metadata source - the simplicity of a single metadata source outweighs any minor performance gain from switching back to fabric.

## Technical Considerations

### Dependencies

- No new dependencies. yt-dlp and fabric are both already used.
- Shell-outs per YouTube ingestion stays at 2, but composition changes: `fabric --metadata` + `fabric --transcript` becomes `yt-dlp --dump-json` + `fabric --transcript`. The yt-dlp call replaces fabric's metadata call and adds duration.

### Performance

- **Current:** 3 sequential shell-outs: fabric metadata (1-2s) + fabric transcript (5-15s) + fabric summary (3-10s) = ~10-27s total. (yt-dlp only fires on fabric title failure.)
- **Proposed:** yt-dlp metadata (1-3s) runs in parallel with fabric transcript (5-15s), then fabric summary (3-10s) runs after. Total: ~8-25s. The yt-dlp call is fully hidden behind the slower transcript call.
- On a 150-note reingest batch, the per-note savings compound to 2-7 minutes total.

### Security

No change. Same tools, same data sources, same trust model.

### Testing Strategy

- Unit tests for `fetch_transcript()` (mock fabric binary output)
- Verify `process_youtube()` produces correct `YouTubeResult` with all fields populated
- Integration test: ingest a YouTube video and verify duration, creator, description callout, tags all present
- Verify legacy transcript fallback chain still works (subtitles -> audio extraction -> Groq)

### Rollout Plan

Ship as a single commit. The change is internal refactoring - note output format is unchanged. Existing notes are unaffected. New YouTube ingestions use the simplified pipeline.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| yt-dlp breaks due to YouTube page changes | Low | High | yt-dlp is actively maintained (updates within days of breakage). Fabric transcript is unaffected. Can fall back to Alternative 2 (direct API) if yt-dlp becomes unreliable. |
| yt-dlp is slower than YouTube API for metadata | Low | Low | Difference is ~1-2 seconds per ingestion. Negligible compared to transcript + summary time. |
| Removing fabric metadata path loses data we didn't know we needed | Very Low | Low | yt-dlp returns a superset of fabric's metadata fields. We're gaining fields (duration), not losing any. |
| Fabric transcript depends on YouTube API key; if key is invalid, transcript fails | Medium | Medium | Existing fallback to yt-dlp subtitles already handles this. No change. |
| yt-dlp fails (video deleted, geo-blocked, age-restricted) | Low | Medium | Ingestion fails with an error, same as current behavior. Fabric would also fail for the same reasons. Not a regression. |

## Open Questions

- [x] Does yt-dlp return all fields fabric provides? **Yes.** yt-dlp returns title, channel, uploader, duration, description, tags, upload_date, and 100+ other fields. It's a strict superset.
- [x] Will removing fabric metadata break anything else? **No.** `fabric::fetch_youtube()` is only called from `process_youtube_fabric()`. No other code depends on it.
- [x] Should we keep the ISO 8601 duration parser? **No.** It was written to handle a format fabric never sends. Dead code. Remove it.
- [x] Do we need `published_at` from yt-dlp? **Not currently.** The note's `date:` field uses ingestion time, not publish date. `VideoMetadata` doesn't have it. If needed later, add `upload_date` (format `YYYYMMDD`) to `VideoMetadata`.

## References

- Borg YouTube pipeline: `borg/src/pipeline.rs` (process_youtube_fabric, process_youtube_legacy)
- Fabric integration: `borg/src/fabric.rs` (fetch_youtube, parse_youtube_metadata)
- yt-dlp metadata: `borg/src/youtube.rs` (fetch_metadata)
- Fabric YouTube source: `~/repos/danielmiessler/fabric/internal/tools/youtube/youtube.go`
- Previous design: `docs/design/2026-03-22-youtube-description-extraction.md`
