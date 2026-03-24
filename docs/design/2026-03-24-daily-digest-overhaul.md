# Design Document: Daily Digest Overhaul

**Author:** Scott Idler + Claude
**Date:** 2026-03-24
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Replace the current Fabric-based daily digest synthesis in cortex with a direct Anthropic API call, restructure the output format for readability (collapsed note list, conversational themes, curated highlights, breadcrumbs), and default to Opus 200k for higher quality thematic synthesis.

## Problem Statement

### Background

Cortex generates a daily digest note each morning summarizing the previous day's ingested notes. It gathers all notes with yesterday's date, lists them, shows top tags, stats, and optionally runs a Fabric pattern for an "AI Summary" section. The digest lands in `notes/ai/daily-{today}.md` in the Obsidian vault.

### Problem

The current daily digest has two distinct problems:

1. **Format is ugly and unhelpful.** The raw note list (often 20+ items) dominates the note. Stats and tag lists are filler. Nothing is collapsed or visually prioritized.

2. **AI synthesis quality is poor.** The Fabric-piped summary oscillates between two failure modes: either way too verbose (wall of text) or generic and useless (reads like a book report). The current output - a single paragraph like "Your reading focused heavily on AI coding tools and productivity systems" - tells the user nothing they didn't already know from glancing at the note titles.

The purpose of the daily digest is to remind the user what they thought was important yesterday, surface thematic threads across notes, and tease enough detail to invite re-exploration. It is currently failing at all three.

### Goals

- Produce a daily digest that the user actually wants to read
- Surface thematic connections across yesterday's notes, not just summarize titles
- Highlight 3-5 standout notes with context about why they matter
- Include "breadcrumbs" - provocative questions or connections that invite deeper exploration
- Collapse the raw note list so it doesn't dominate the view
- Use a direct Anthropic API call for better prompt control and model selection
- Default to Opus 200k for synthesis quality (one call/day, cost is negligible)

### Non-Goals

- Changing the weekly review (separate concern, future work)
- Replacing Fabric in classify or process_new_notes (those stay on Fabric)
- Building a general-purpose LLM client library (keep it minimal)
- Changing how notes are gathered (the date-filtering logic is fine)
- Changing the output path or frontmatter schema

## Proposed Solution

### Overview

1. Add a lightweight `llm.rs` module to cortex that calls the Anthropic Messages API directly via `ureq`
2. Rewrite `generate_daily_digest()` in `intel.rs` to use the new LLM module with a carefully crafted prompt
3. Restructure the output note format: Themes up top, Highlights with wikilinks, Breadcrumbs for curiosity, collapsed callout for the raw note list

### Architecture

```
intel.rs (daily digest)
    |
    +-- gathers yesterday's notes (unchanged)
    |
    +-- builds prompt with note titles, bodies, and file stems
    |
    +-- calls llm::complete() with system + user prompt
    |
    +-- assembles final note: frontmatter + LLM output + collapsed note list
    |
    +-- writes to notes/ai/daily-{today}.md (unchanged)

llm.rs (new module)
    |
    +-- complete(system, user, model, max_tokens, timeout) -> Result<String>
    |
    +-- POSTs to https://api.anthropic.com/v1/messages
    |
    +-- reads ANTHROPIC_API_KEY from env
    |
    +-- uses ureq (already a dependency)
```

The `llm.rs` module is intentionally minimal - one function, one API call. No streaming, no tool use, no conversation history. Just a completion.

### New Module: `llm.rs`

```rust
pub fn complete(
    system: &str,
    user: &str,
    model: &str,
    max_tokens: u32,
    timeout_secs: u64,
) -> Result<String>
```

- Reads `ANTHROPIC_API_KEY` from environment
- POSTs to `https://api.anthropic.com/v1/messages` with `anthropic-version: 2023-06-01`
- Request body: `{ model, max_tokens, system, messages: [{ role: "user", content: user }] }`
- Parses response, extracts `content[0].text`
- Returns error on missing API key, HTTP failure, or unexpected response shape
- Timeout via ureq's built-in timeout mechanism

### Config Changes

`IntelConfig` updates:

| Field | Old | New | Default |
|-------|-----|-----|---------|
| `batch-daily` | `Option<String>` (Fabric pattern) | Removed | - |
| `model` | (new) | `String` | `"claude-opus-4-0-20250514"` |
| `max-output-tokens` | (new) | `u32` | `1024` |
| `llm-timeout-secs` | (new) | `u64` | `120` |
| `fabric-timeout-secs` | `u64` | unchanged (used by weekly review) | `120` |
| `max-input-tokens` | `usize` | unchanged | `50000` |
| `batch-weekly` | `Option<String>` | unchanged (still Fabric) | `"weekly_digest"` |
| `output-path` | `String` | unchanged | `"notes/ai"` |

`fabric-timeout-secs` is kept because the weekly review still uses Fabric. `llm-timeout-secs` is a new field for the direct API call.

### Output Format

```markdown
---
title: Daily Digest 2026-03-24
date: 2026-03-24
type: digest
tags: [digest]
---

# Daily Digest - 2026-03-24

## Themes

[3-5 sentence conversational synthesis in second person. What threads
connected yesterday's reading? What was the user gravitating toward?
Written by the LLM.]

## Highlights

- [[note-slug|Note Title]] - one-liner about why it stood out
- [[note-slug|Note Title]] - how it connects to a theme
- [[note-slug|Note Title]] - what made it interesting
[3-5 notes, selected and written by the LLM]

## Breadcrumbs

- [provocative question or tension noticed across notes]
- [connection the user might not have spotted]
- [thread worth pulling on]
[2-3 items, written by the LLM]

> [!notes]- Yesterday's Notes (23)
> - [[note-one|Title One]]
> - [[note-two|Title Two]]
> ...
```

Key changes from current format:
- **Removed:** Stats section (filler)
- **Removed:** Active Topics section (replaced by Themes with actual context)
- **Removed:** Flat "AI Summary" paragraph
- **Added:** Themes (conversational synthesis)
- **Added:** Highlights (curated picks with reasoning)
- **Added:** Breadcrumbs (curiosity hooks)
- **Changed:** Raw note list moved into collapsed `> [!notes]-` callout

### Prompt Design

**System prompt:**

```
You are a sharp, well-read colleague reviewing someone's daily reading and notes.
You've read everything they ingested yesterday and you're giving them a morning
briefing - conversational, second-person, concise. Not a summary bot. Not a book
report. You notice patterns, connections, and tensions they might have missed.

Output exactly three markdown sections. No frontmatter, no title heading.
Never use em dashes. Use regular dashes, commas, or semicolons instead.

## Themes
3-5 sentences. What threads connected yesterday's reading? What was the user
gravitating toward? Be specific - name concepts, tools, people. Don't just list
topics.

## Highlights
3-5 bullet points. Each starts with a wikilink in the format [[slug|Title]] using
the exact slugs provided, followed by a dash and a one-liner about why this note
stood out or how it connects to a broader theme. Pick the most interesting or
connective notes, not just the longest.

## Breadcrumbs
2-3 bullet points. Provocative questions, surprising connections between notes, or
tensions you noticed. These should make the reader want to go back and look at
specific notes. Reference notes by their wikilink when relevant.
```

**User prompt construction:**

For each note, provide:
```
=== [[{file_stem}|{title}]] ===
{body}
```

Separated by dividers. The explicit wikilink format in each note header gives the LLM the exact syntax to use when referencing notes in its output.

### Implementation Plan

**Phase 1: Add `llm.rs` module**
- Single `complete()` function
- Reads `ANTHROPIC_API_KEY` from env
- Uses ureq with timeout
- Unit test: verify error on missing API key

**Phase 2: Update `IntelConfig`**
- Add `model`, `max-output-tokens` fields
- Rename `fabric-timeout-secs` to `llm-timeout-secs`
- Remove `batch-daily`
- Update `Default` impl and test fixtures

**Phase 3: Rewrite `generate_daily_digest()`**
- Build prompt from yesterday's notes with file stems
- Call `llm::complete()` instead of Fabric
- Assemble output: frontmatter + LLM sections + collapsed callout with note list
- Fallback: if LLM call fails, still produce the note with just the collapsed list and a warning
- Edge case: if no notes were ingested yesterday, skip the LLM call entirely and produce a minimal note with "No notes ingested on {yesterday}" (same as current behavior)

**Phase 4: Update tests**
- Update `test_daily_digest_on_vault` to check for new format
- Update `test_resolve_output_path_default` to use `notes/ai`
- Add test for prompt construction
- Add test for LLM failure fallback (graceful degradation)

## Alternatives Considered

### Alternative 1: Keep Fabric, improve the pattern

- **Description:** Write a better Fabric pattern (daily_digest) with more specific instructions
- **Pros:** No new dependencies, consistent with existing LLM integration
- **Cons:** Fabric adds indirection (shell out to CLI that shells out to API), harder to control model selection, harder to iterate on prompts (pattern files vs. inline), Fabric's own prompt wrapping may interfere with output quality
- **Why not chosen:** The user has already iterated on this multiple times with Fabric and the results have been consistently disappointing. Direct API access gives full control over model, prompt, and output parsing.

### Alternative 2: Use the `anthropic` Rust SDK crate

- **Description:** Add the official `anthropic` crate as a dependency
- **Pros:** Typed request/response, handles auth, versioning
- **Cons:** Adds a heavy dependency tree for one API call per day, SDK may lag behind API features, cortex is sync-first and the SDK may be async-oriented
- **Why not chosen:** Overkill. A single ureq POST with manual JSON construction is ~30 lines of code, no new dependencies, and fully sufficient for this use case.

### Alternative 3: Use Sonnet as default, Opus as upgrade path

- **Description:** Default to Sonnet for cost savings, let user opt into Opus
- **Pros:** Cheaper per call
- **Cons:** The whole motivation for this redesign is synthesis quality. Sonnet has already produced mediocre results through Fabric. One Opus call per day costs ~$0.10-0.30.
- **Why not chosen:** Cost is negligible for a once-daily call. Quality is the entire point.

## Technical Considerations

### Dependencies

- **ureq:** Already in Cargo.toml, used for HTTP
- **serde_json:** Already in Cargo.toml, used for JSON construction/parsing
- **ANTHROPIC_API_KEY:** Must be set in the environment where cortex runs (systemd user daemon)

### Performance

- One API call per day, expected latency 5-30 seconds depending on input size and model
- Input truncation at `max-input-tokens` (default 50k) prevents runaway costs
- Output capped at `max-output-tokens` (default 1024)

### Security

- API key read from environment only, never logged or written to disk
- Note content sent to Anthropic API - same trust boundary as existing Fabric usage

### Testing Strategy

- Unit tests for `llm.rs`: error on missing key, request construction
- Unit tests for prompt building: verify wikilink format, note inclusion
- Integration test for digest format: verify collapsed callout, section headers
- Manual verification: run `cortex intel --daily` and review output in Obsidian

### Rollout Plan

- Build and install: `cargo install --path cortex && systemctl --user restart cortex`
- Set `ANTHROPIC_API_KEY` in cortex's systemd environment if not already present
- Run `cortex intel --daily` manually to verify before letting the daemon schedule it

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| LLM output doesn't include valid wikilinks | Medium | Medium | Prompt includes exact slug format; fallback appends collapsed note list regardless |
| LLM outputs extra sections or formatting | Medium | Low | Post-process: strip any frontmatter or title heading from LLM output |
| API key not set in systemd environment | Low | High | Log clear error message, fall back to note-list-only digest |
| LLM hallucinates note titles not in the input | Low | Medium | Could post-validate wikilinks against known stems; v1 accepts the risk |
| Opus model ID changes | Low | Low | Configurable via `model` field |

## Open Questions

- [ ] Should the fallback (LLM unavailable) digest include the old-style tag list, or just the collapsed note list?
- [ ] Should we post-validate that wikilinks in LLM output match actual note stems?

## References

- Cortex intel module: `cortex/src/intel.rs`
- Cortex fabric module: `cortex/src/fabric.rs`
- Cortex config: `cortex/src/config.rs`
- Anthropic Messages API: https://docs.anthropic.com/en/api/messages
- Obsidian callout syntax: https://help.obsidian.md/callouts
