# Design Document: YouTube Description Extraction & Tag Harvesting

**Author:** Scott Idler
**Date:** 2026-03-22
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Borg currently drops YouTube video descriptions on the floor during ingestion. Descriptions contain valuable content - the author's own summary, resource links, referenced repos, series episode lists, and hashtags - mixed with boilerplate (subscribe CTAs, social links, merch, sponsorships). This design adds a filtered description section to YouTube notes and harvests author-provided tags/hashtags into frontmatter.

## Problem Statement

### Background

Borg ingests YouTube videos by fetching metadata (title, channel, duration) and transcripts via Fabric CLI or yt-dlp, then generating an LLM summary via a Fabric pattern. The resulting note contains: frontmatter, an iframe embed, a `## Summary` section (LLM-generated), and a source footer.

The YouTube video description - often the richest source of author-curated context - is never captured. Neither are the author's tags or inline hashtags.

### Problem

Analysis of 10 ingested YouTube notes against their actual yt-dlp metadata reveals:

1. **Lost context:** Every video has an opening paragraph written by the creator describing the content. This is dropped entirely.
2. **Lost resources:** Links to referenced videos, articles, courses, tools, and git repos appear in descriptions but are never captured.
3. **Lost taxonomy:** 8/10 videos had meaningful `tags` arrays (e.g., "youth Air raid", "Pass Protection in the Spread Offense"). Description hashtags like `#claudecode #HomeLab #Kubernetes` are also ignored.
4. **Tags are purely LLM-generated:** The current tag pipeline runs the summary through Fabric's `create_tags` pattern. The author's own categorization is never consulted.

### Goals

- Capture the author's description text in a filtered, collapsed callout section in YouTube notes
- Extract hashtags from description text and merge into note tags
- Extract the `tags` array from yt-dlp metadata and merge into note tags
- Filter out boilerplate noise (subscribe CTAs, social links, merch, sponsorships)
- Preserve resource links, referenced content, series/episode lists

### Non-Goals

- Changing the LLM summary generation (the `youtube_summary` Fabric pattern is untouched)
- Fetching descriptions for non-YouTube content types
- Retroactively updating existing notes (cortex could do this later)
- Configuring filter rules per-user (hardcoded heuristics are fine for now)
- Parsing structured data from descriptions (e.g., timestamps/chapters)

## Proposed Solution

### Overview

Three-tier extraction from YouTube metadata (fabric preferred, yt-dlp fallback):

1. **Tags** - hashtags from description + `tags` array from metadata (fabric or yt-dlp), merged into frontmatter tags
2. **Filtered description** - boilerplate stripped, rendered in a collapsible Obsidian callout
3. **Metadata expansion** - both `YouTubeContent` (fabric) and `VideoMetadata` (yt-dlp) structs gain `description` and `tags` fields, ensuring either path can supply the data

### Architecture

The change flows through the existing pipeline without new components:

```
    fabric -y <url> --metadata  (YouTube Data API v3, preferred)
        │
        │  fails?  ──► yt-dlp --dump-json  (fallback, no API key needed)
        │                    │
        ▼                    ▼
    ┌──────────────────────────┐
    │ title, channel, duration │  (existing)
    │ description              │  (NEW)
    │ tags[]                   │  (NEW)
    └──────────────────────────┘
                │
                ▼
    ┌─────────────────┐
    │ description      │──► filter_description() ──► filtered text
    │ filter module    │──► extract_hashtags()   ──► Vec<String>
    └─────────────────┘
                │
                ▼
    pipeline.rs
    ├── merge hashtags + metadata tags + LLM tags into all_tags
    ├── pass filtered description to NoteContent
    └── render_note() outputs callout section
```

### Data Model

#### Struct changes

**`youtube.rs` - `VideoMetadata`:**
```rust
pub struct VideoMetadata {
    pub title: String,
    pub uploader: String,
    pub duration_secs: f64,
    pub description: String,  // NEW
    pub tags: Vec<String>,    // NEW
}
```

**`fabric.rs` - `YouTubeContent`:**
```rust
pub struct YouTubeContent {
    pub title: String,
    pub channel: String,
    pub duration_secs: f64,
    pub published_at: String,
    pub transcript: String,
    pub video_id: String,
    pub description: String,  // NEW
    pub tags: Vec<String>,    // NEW
}
```

**`markdown.rs` - `NoteContent`:**
```rust
pub struct NoteContent {
    pub title: String,
    pub source_url: Option<String>,
    pub asset_path: Option<String>,
    pub tags: Vec<String>,
    pub summary: String,
    pub description: Option<String>,  // NEW - filtered description for callout (YouTube only, None for all other types)
    pub content_type: ContentType,
    pub embed_code: Option<String>,
    pub method: Option<IngestMethod>,
    pub trace_id: Option<String>,
}
```

**`pipeline.rs` - return type for YouTube processing functions:**

The `process_youtube_fabric()` and `process_youtube_legacy()` functions currently return `Result<(String, String, ContentType)>` (title, summary, content_type). To carry description and yt-dlp tags, introduce a struct:

```rust
struct YouTubeResult {
    title: String,
    summary: String,
    content_type: ContentType,
    description: String,  // raw description from yt-dlp
    yt_tags: Vec<String>, // tags array from yt-dlp
}
```

Both `process_youtube_fabric()` and `process_youtube_legacy()` return `Result<YouTubeResult>`. The caller in `ingest_url()` calls `extract_hashtags()` and `filter_description()` on the raw description, merges yt_tags into the tag list, and passes the filtered description to `NoteContent`.

### Description Filter Design

New module: `borg/src/description.rs`

Two public functions:

#### `extract_hashtags(description: &str) -> Vec<String>`

Regex: `#[\w][\w-]*` (must start with word char after `#`, can contain hyphens).

Returns lowercase, deduplicated tags with `#` stripped.

#### `filter_description(description: &str) -> Option<String>`

Line-by-line filter with two mechanisms and a state machine:

**State machine:** The filter tracks an `in_killed_section: bool` flag. When a section killer fires, **all remaining lines are dropped to the end of the description**. No recovery.

This is deliberately aggressive but correct: in every observed sample (10 videos across 5 channels), boilerplate sections ("Let's connect", "For business inquiries", "FOLLOW ME ON") appear at the bottom of descriptions. Valuable content (opening paragraph, resource lists, episode links) is always above them. Kill-to-end avoids false recovery inside social blocks (e.g., a Zoom meeting link inside a "Let's connect" section would incorrectly trigger a URL-based recovery signal).

**Section killers** (case-insensitive) - trigger killed-section state:

| Pattern | What it catches |
|---------|----------------|
| `follow me on` | Social media blocks |
| `let's connect` | Social media blocks |
| `connect with me` | Social media blocks |
| `social media links` | Social media blocks |
| `for business inquiries` | Contact blocks |
| `my main.*channel` | Channel self-promotion |

**Line killers** - individual lines matching any of these are dropped (regardless of state):

| Pattern | What it catches |
|---------|----------------|
| `sub_confirmation` | Subscribe links |
| `subscribe for more` (case-insensitive) | Subscribe CTAs |
| `watch my most recent upload` (case-insensitive) | Channel self-links |
| `consider becoming a patron` (case-insensitive) | Patron CTAs |
| `patreon.com` | Patron links |
| `sponsored by` (case-insensitive) | Sponsorship blocks |
| `affiliate` (case-insensitive) | Affiliate disclaimers |
| `promo=` | Promo tracking params |
| `teespring.com` or `merch` (case-insensitive) | Merch |
| Line is only emoji + whitespace + dashes (no alphanumeric) | Decorative separators |
| `if you find my content helpful` (case-insensitive) | Subscribe CTAs |

**Always keep:**
- The opening paragraph(s) - all text before the first killed section/line
- Lines containing non-social URLs (resource links, articles, repos, tools)
- Lines that are part of a numbered/bulleted list (episode lists, resource lists)

**Post-processing:**
- Strip hashtags from the text (they've already been extracted into tags)
- Collapse runs of 3+ blank lines to 2
- Trim leading/trailing whitespace
- Return `None` if the filtered result is empty or only whitespace

### Rendered Note Template

```markdown
---
title: "Video Title"
date: 2026-03-22
source: "https://youtube.com/watch?v=abc"
type: youtube
origin: assisted
method: telegram
tags:
  - claudecode
  - kubernetes
  - homelab
creator: "Channel Name"
duration: 24
---

# Video Title

<iframe ...></iframe>

> [!info]- Video Description
> Kubernetes homelab tour after 3 years - here's every app, every cluster,
> and the exact hardware running it all.
>
> RESOURCES MENTIONED:
> - Homelab guide & n8n code: https://go.kubecraft.dev/k8s-homelab-n8n
> - Talos Linux: https://www.talos.dev
> - CloudNativePG: https://cloudnative-pg.io
> - Cilium: https://cilium.io

## Summary

{LLM-generated summary via Fabric youtube_summary pattern}

---

*Source: [https://youtube.com/watch?v=abc](https://youtube.com/watch?v=abc)*
```

The callout is collapsed by default (`-` suffix on `[!info]-`) so it doesn't dominate the note but is one click to expand.

### Concrete Example: Filter in Action

**Raw description** (ThePrimeagen - "The harsh reality of good software"):
```
Recorded live on twitch, GET IN

https://twitch.tv/ThePrimeagen

Become a backend engineer.  Its my favorite site
https://boot.dev/?promo=PRIMEYT

This is also the best way to support me is to support yourself becoming a better backend engineer.

Reviewed video: https://www.youtube.com/watch?v=1UEMXDSh8Og
By: https://www.youtube.com/@awesome-coding

MY MAIN YT CHANNEL: Has well edited engineering videos
https://youtube.com/ThePrimeagen

Discord
https://discord.gg/ThePrimeagen

Have something for me to read or react to?: https://www.reddit.com/r/ThePrimeagenReact/

Kinesis Advantage 360: https://bit.ly/Prime-Kinesis

Hey I am sponsored by Turso, an edge database.  I think they are pretty neet.  Give them a try for free and if you want you can get a decent amount off (the free tier is the best (better than planetscale or any other))
https://turso.tech/deeznuts
```

**After `filter_description()`:**
```
Recorded live on twitch, GET IN

https://twitch.tv/ThePrimeagen

Reviewed video: https://www.youtube.com/watch?v=1UEMXDSh8Og
By: https://www.youtube.com/@awesome-coding
```

**What was removed and why:**
- `boot.dev/?promo=PRIMEYT` - line killer: `promo=`
- "Become a backend engineer..." and "This is also the best way..." - line killer: surrounding the promo link
- "MY MAIN YT CHANNEL..." - section killer: `my main.*channel` (kills to end)
- Discord, Reddit, Kinesis, Turso sponsor block - all below the section killer

**Tags extracted:** `["programming", "software-engineer", "software-engineering", "developer", "web-design", "web-development", "programmer-humor"]` (from yt-dlp `tags` array - no description hashtags in this video).

### Implementation Plan

#### Phase 1: Data capture

1. Add `description` and `tags` fields to `VideoMetadata` in `youtube.rs`
2. Parse both from yt-dlp `--dump-json` output in `fetch_metadata()`
3. Add same fields to `YouTubeContent` in `fabric.rs`
4. In `parse_youtube_metadata()`, extract `description` and `tags` from the JSON

**Description fetch strategy:** Fabric metadata is the preferred path, yt-dlp is the fallback.

- **Preferred: Fabric (`process_youtube_fabric`):** `fabric -y <url> --metadata` uses the YouTube Data API v3 (requires `YOUTUBE_API_KEY` in `~/.config/fabric/.env`). Returns `description`, `tags`, `title`, `channelTitle`, `publishedAt`, `viewCount`, `likeCount` in one call. This is faster and richer than yt-dlp (includes view/like counts). Extract `description` and `tags` from `YouTubeContent` directly.
- **Fallback: yt-dlp (`process_youtube_legacy`):** When fabric metadata fails (invalid/missing API key, network error), the existing fallback to `youtube::fetch_metadata()` fires. This calls `yt-dlp --dump-json` which returns the same `description` and `tags` fields. No API key required.
- Both sources return identical `description` and `tags` data, so the filter logic is the same regardless of which path produced the data.

#### Phase 2: Description filtering

5. Create `borg/src/description.rs` with `extract_hashtags()` and `filter_description()`
6. Add `mod description;` to `borg/src/main.rs` (or `lib.rs`)
7. Unit tests against real description samples from our research (Air Raid Warden, ThePrimeagen, El Estepario Siberiano patterns)

#### Phase 3: Pipeline integration

8. Add `description: Option<String>` to `NoteContent` in `markdown.rs`
9. Update `render_note()` to emit the callout between embed and summary. Rendering logic:
   - If `description` is `Some(text)`, emit `> [!info]- Video Description\n` followed by each line of the text prefixed with `> `
   - Blank lines within the callout become `>`
   - If `description` is `None`, emit nothing (no empty callout)
10. Create `YouTubeResult` struct in `pipeline.rs` and update `process_youtube_fabric()` and `process_youtube_legacy()` to return it
11. In `process_youtube_fabric()`: extract description+tags from `YouTubeContent` (fabric metadata, preferred). When fabric metadata fails and falls back to yt-dlp via `youtube::fetch_metadata()`, that call also captures description+tags. The caller sees the same `YouTubeResult` regardless of which source provided the data.
12. In the main `ingest_url()` flow: call `extract_hashtags()` on the raw description, merge with yt-dlp `yt_tags` and LLM-generated tags
13. Pass `filter_description()` output as `description` field on `NoteContent`
14. Update all existing `NoteContent` construction sites (non-YouTube paths) to pass `description: None`

#### Phase 4: Testing

15. Unit tests for `extract_hashtags` - various hashtag patterns, edge cases
16. Unit tests for `filter_description` - real description samples
17. Integration test: full `render_note` with description callout
18. Manual end-to-end test: ingest a YouTube video and verify the note

## Alternatives Considered

### Alternative 1: Verbatim description (no filtering)

- **Description:** Include the full YouTube description as-is in the note
- **Pros:** Zero data loss, simplest implementation, no risk of over-filtering
- **Cons:** Notes get polluted with subscribe CTAs, merch links, sponsorship blocks. The Air Raid Warden descriptions are ~2KB of boilerplate per video. This noise would dominate the note.
- **Why not chosen:** Defeats the purpose of curated knowledge. The collapsed callout helps, but even inside the callout, boilerplate wastes attention.

### Alternative 2: LLM-filtered description

- **Description:** Pass the description through a Fabric pattern to extract only valuable content
- **Pros:** More intelligent filtering, could summarize/restructure, handles edge cases better
- **Cons:** Additional LLM call per ingestion (cost + latency), non-deterministic output, harder to debug when filtering goes wrong, may hallucinate or drop important links
- **Why not chosen:** Heuristic filtering is good enough for the boilerplate patterns observed. The descriptions are structured enough that regex/pattern matching handles 90%+ of cases. LLM filtering can be a future enhancement if heuristics prove insufficient.

### Alternative 3: Description as separate linked note

- **Description:** Create a companion note (e.g., `video-title-description.md`) linked from the main note
- **Pros:** Keeps the main note clean, preserves full description, enables separate search
- **Cons:** File bloat (doubles the note count for YouTube), adds complexity to the template, fragile linking
- **Why not chosen:** A collapsed callout achieves the same "out of the way but accessible" goal without file proliferation.

## Technical Considerations

### Dependencies

- No new crate dependencies. The filter uses `regex` (already in the dependency tree) and string manipulation.
- yt-dlp already returns `description` and `tags` in its `--dump-json` output; we just need to parse them.

### Performance

- `filter_description()` is a single-pass line-by-line scan. Negligible cost.
- `extract_hashtags()` is a single regex scan. Negligible cost.
- No additional network calls in either path. Both fabric metadata and yt-dlp already return description and tags in their existing responses - we just need to parse the fields we were ignoring.

### Security

- Description text is user-generated content from YouTube. It's rendered inside an Obsidian callout (markdown), not executed. No injection risk.
- URLs in descriptions are preserved as-is. Obsidian renders them as clickable links, which is the desired behavior.

### Testing Strategy

- Unit tests for `extract_hashtags()`: empty input, no hashtags, mixed hashtags, hashtags with hyphens, duplicate hashtags, hashtags at start/middle/end of lines
- Unit tests for `filter_description()`: use real description samples from our 10-video research as test fixtures. Verify boilerplate is removed and valuable content is preserved.
- Unit test for `render_note()`: verify callout is rendered correctly, verify `None` description produces no callout
- Manual smoke test: ingest a known YouTube video and inspect the resulting note

### Rollout Plan

Ship as a single commit. The change is additive - existing notes are unaffected. New YouTube ingestions get the description callout and richer tags. No migration needed.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Over-filtering strips valuable content | Medium | Low | Conservative filter - only strip well-known boilerplate patterns. When in doubt, keep the line. Unit tests against real samples. |
| Under-filtering leaves noise | Low | Low | Collapsed callout means noise is hidden by default. Filter can be tightened incrementally. |
| yt-dlp JSON schema changes `description`/`tags` field names | Very Low | Medium | Fields have been stable for years. Fallback to empty string/vec if missing. |
| Very long descriptions bloat note file size | Low | Low | Filtering removes most bulk. Could add a max-length cap later if needed. |
| Hashtags in description are not real tags (e.g., `#1` in a numbered list) | Low | Low | Regex requires word char after `#`. sanitize_tag() filters garbage. |
| Tag explosion from combining 3 sources (yt-dlp tags + hashtags + LLM) | Medium | Low | Dedup handles overlap. 20-30 tags is acceptable in Obsidian. Can add a cap later if needed. |
| Empty or missing description (Shorts, old videos) | Medium | None | `filter_description` returns `None`, no callout rendered. Tags still extracted from yt-dlp `tags` array. |
| Non-English descriptions with non-English boilerplate | Low | Low | Filter patterns are English-only. Non-English boilerplate passes through (acceptable - under-filter is better than over-filter). |
| Description contains Obsidian-conflicting markdown (`>`, `---`, `[[`) | Low | Low | Inside a callout, `>` creates nested blockquotes (harmless). `---` renders as horizontal rule within callout (fine). `[[` would create wikilinks - unlikely in YouTube descriptions but if present, they'd be broken links (acceptable). |

## Open Questions

- [x] Should the yt-dlp `tags` array be sanitized differently from description hashtags? **No.** Both go through `sanitize_tag()` which lowercases and replaces spaces/invalid chars with hyphens. Multi-word phrases like "youth Air raid" become `youth-air-raid`, which is consistent with the vault's tag conventions.
- [x] Should we store the raw (unfiltered) description anywhere for future use, or is filtered-only sufficient? **Filtered-only.** The raw description can always be re-fetched from YouTube via `yt-dlp --dump-json`. No need to store it in the note.
- [x] Should the description callout type be configurable? **No, hardcode `[!info]` for now.** Callout type is a cosmetic choice with no functional impact. Can be made configurable later if needed.

## References

- Borg YouTube pipeline: `borg/src/pipeline.rs` (lines 305-456)
- Note template: `borg/src/markdown.rs` (lines 56-177)
- YouTube metadata: `borg/src/youtube.rs` (lines 32-56)
- Fabric integration: `borg/src/fabric.rs` (lines 20-63)
- Obsidian callout syntax: https://help.obsidian.md/callouts
