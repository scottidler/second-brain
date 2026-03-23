# Design Document: Enumerated Points in Ingested Notes

**Author:** Scott Idler
**Date:** 2026-03-23
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Add a conditional `## Enumerated Points` section to borg's ingestion output when content creators explicitly enumerate items (e.g., "10 CLI Tools", "7 Auth Concepts"). Move the Fabric pattern into the repo as a version-controlled flat file. Key Ideas must not duplicate enumerated points.

## Problem Statement

### Background

Borg ingests YouTube videos, blog posts, and other content into Obsidian notes using a custom Fabric pattern (`obsidian_note`). The pattern produces structured markdown with sections: Summary (tldr callout), What This Is About, Key Ideas, Best Quotes, and References. The pattern currently lives outside version control at `~/.config/fabric/patterns/obsidian_note/system.md`.

### Problem

When creators explicitly enumerate items in their content - "10 CLI Tools I Use With Claude Code", "7 Authentication Concepts Every Developer Should Know" - the ingested notes do not preserve that structure. Key Ideas produces thematic bullets that may group, omit, or reframe the creator's numbered items. A video titled "10 CLI Tools" might produce only 6 Key Ideas bullets covering some tools thematically while missing others entirely. The creator's intentional structure is lost.

Secondary problem: both custom pattern files (`obsidian_note` and `obsidian_classify`) are unversioned, living in fabric's global config directory rather than the repo that depends on them.

### Goals

- When a creator explicitly enumerates N items, capture all N as a numbered list in a dedicated `## Enumerated Points` section
- Key Ideas remains present but must be distinct from enumerated points - no duplication
- When no enumeration is present, the note format is unchanged from today
- Move both custom patterns (`obsidian_note` and `obsidian_classify`) into the borg crate under version control, renamed with hyphens

### Non-Goals

- Changing the existing section format (Summary, What This Is About, Key Ideas, Best Quotes, References)
- Multi-pass LLM detection pipeline (deferred as Approach B for future consideration)
- Modifying any Rust code in the borg pipeline
- Handling implicit or subjective enumerations (only explicit creator-stated counts)

## Proposed Solution

### Overview

Three changes: (1) move both custom Fabric patterns from `~/.config/fabric/patterns/` to `borg/patterns/` as flat files with hyphenated names, (2) update the summarize pattern prompt to conditionally detect and emit an Enumerated Points section, and (3) update borg config to use file paths.

Fabric natively supports file paths as pattern names. When `-p` receives a value starting with `~`, `/`, or `.`, it reads the file directly instead of looking up a named pattern in its patterns directory (see `loadPattern()` in `fabric/internal/plugins/db/fsdb/patterns.go:58-82`). This means no symlinks, no custom patterns directory config, and no Rust code changes - just point the borg config at the file paths.

**Patterns to move:**

| Old location | New location |
|---|---|
| `~/.config/fabric/patterns/obsidian_note/system.md` | `borg/patterns/obsidian-note.md` |
| `~/.config/fabric/patterns/obsidian_classify/system.md` | `borg/patterns/obsidian-classify.md` |

### Architecture

No architectural changes. The pipeline remains:

```
content -> fabric -p <pattern> -> summary text -> markdown.rs render_note() -> vault
```

The only difference is pattern names in config change from fabric named patterns to direct file paths.

### Data Model

No schema changes. The summary field in `NoteContent` already accepts arbitrary markdown. The new section is just additional markdown content within the summary.

### Note Output Format

**When enumeration is detected:**

```markdown
> [!tldr]
> One or two sentence takeaway.

## What This Is About

3-5 sentence plain-language summary.

## Enumerated Points

The creator covers 10 CLI tools:

1. **Yazi** - Terminal file manager with Vim keybinds and plugin system
2. **Zoxide** - Smart directory jumping with interactive mode
3. **TLDR (Tealdeer)** - Concise, example-focused man page summaries
4. **Bat** - Enhanced cat with syntax highlighting
5. **Tmux** - Terminal multiplexer for persistent sessions
6. **PixelMuse CLI** - AI image generation from the terminal
7. **Mole** - Mac deep clean and optimization
8. **Jolt** - Battery and hardware monitoring
9. **TTYper** - Terminal typing test
10. **Taproom** - Homebrew package explorer

## Key Ideas

- **Package manager ecosystem mapping** - Different tools live in different systems (Homebrew, npm, crates), each with their own discovery methods
- **Tool discovery as a skill** - Knowing how to find tools is as valuable as knowing specific tools
- **Custom CLI development** - Building your own tools can fill gaps in existing tooling

## Best Quotes
...

## References
...
```

**When no enumeration is detected:**

Identical to today's output. The Enumerated Points section and heading are omitted entirely.

### Updated Pattern

The full pattern for `borg/patterns/obsidian-note.md`:

```
# IDENTITY and PURPOSE

You are an expert knowledge distiller. You take transcripts or articles and produce a clean, visually appealing Obsidian note that a human actually wants to read. You value clarity, brevity, and insight over exhaustive coverage.

# STEPS

1. Read the entire input carefully.
2. Identify the core thesis or argument.
3. Detect whether the creator explicitly enumerates a set of items (e.g., "10 tools", "7 concepts", "5 steps"). Look for an explicit count in the title, introduction, or body, AND content structured around that count. If detected, extract all N items as a numbered list. If not detected, skip the Enumerated Points section entirely.
4. Extract only the most important and surprising ideas - quality over quantity. These MUST NOT duplicate any enumerated points from step 3. Focus on cross-cutting themes, meta-observations, or insights that go beyond the creator's list.
5. Pick the 2-3 quotes that best capture the spirit of the content.
6. Note any tools, books, people, or projects mentioned that are worth following up on.
7. Write a note that someone could read in 2-3 minutes and walk away informed.

# OUTPUT FORMAT

Return ONLY the following markdown structure. No preamble, no commentary, no extra sections.

> [!tldr]
> One or two sentence takeaway that captures the essential insight.

## What This Is About

A 3-5 sentence summary written in plain language. Cover what the content is, who made it, what their main argument or finding is, and why it matters. Write it like you're explaining it to a smart friend, not writing an abstract.

## Enumerated Points

(ONLY include this section if the creator explicitly enumerates N items. Otherwise, omit it entirely - do not include the heading.)

A one-line lead-in sentence stating what the creator enumerates and how many (e.g., "The creator covers 10 CLI tools:").

1. **Item name** - One sentence describing what it is or why it matters.
2. **Item name** - Continue for all N items the creator enumerates.
(list all N items - do not truncate or group them)

## Key Ideas

- **Idea name or theme** - A sentence explaining the idea and why it's interesting or useful.
- **Another idea** - Keep these to 5-7 bullets maximum. Each one should teach the reader something.
- (continue as needed, max 7)

IMPORTANT: If Enumerated Points is present, Key Ideas MUST NOT repeat any of those items. Key Ideas should contain only thematic insights, cross-cutting observations, or meta-commentary that goes beyond the enumerated list.

## Best Quotes

> "The most impactful quote from the content."

> "A second great quote, if one exists."

> "A third quote only if it adds something the others don't."

## References

- **Name of thing** - what it is, one line (only include if actually mentioned in content)
- (only real references - tools, books, people, projects, links)

# OUTPUT INSTRUCTIONS

- Only output Markdown.
- Do NOT include wrapping code fences - output the markdown directly.
- Do NOT add sections beyond what is specified above.
- Do NOT pad with filler. If there are only 3 key ideas, write 3. If there's only 1 great quote, write 1.
- Do NOT start every bullet with the same word.
- Do NOT use generic filler like "The speaker discusses..." - be specific and direct.
- Write in a natural, conversational tone. Not academic, not corporate.
- The `> [!tldr]` callout is Obsidian syntax - keep it exactly as shown.
- References should only include things explicitly named in the content - do not fabricate.
- If the content is shallow or has little substance, say so honestly in the summary rather than inflating it.
- When Enumerated Points is present, list ALL items the creator mentions - do not skip, group, or summarize multiple items into one entry.
- When Enumerated Points is absent, do not force one. Only include it when the creator explicitly states a count.

# INPUT

INPUT:
```

### Implementation Plan

1. Copy `~/.config/fabric/patterns/obsidian_note/system.md` to `borg/patterns/obsidian-note.md`
2. Copy `~/.config/fabric/patterns/obsidian_classify/system.md` to `borg/patterns/obsidian-classify.md`
3. Apply the pattern changes to `obsidian-note.md` (enumeration detection step, conditional section, deduplication constraint)
4. Update `~/.config/borg/borg.yml` to point all three pattern configs at the new file paths
5. Manual validation with enumerated and non-enumerated content
6. Remove old pattern directories from `~/.config/fabric/patterns/` after validation

### Config Change

`~/.config/borg/borg.yml` before:

```yaml
fabric:
  summarize-pattern-youtube: obsidian_note
  summarize-pattern-article: obsidian_note
  classify-pattern: obsidian_classify
```

After:

```yaml
fabric:
  summarize-pattern-youtube: ~/repos/scottidler/second-brain/borg/patterns/obsidian-note.md
  summarize-pattern-article: ~/repos/scottidler/second-brain/borg/patterns/obsidian-note.md
  classify-pattern: ~/repos/scottidler/second-brain/borg/patterns/obsidian-classify.md
```

## Alternatives Considered

### Alternative A: Two-pass LLM pipeline (Approach B)

- **Description:** First LLM call detects enumeration and extracts the list. Second call generates the full note with the detection result as structured input.
- **Pros:** More reliable detection, separation of concerns, detection prompt can use a cheap model
- **Cons:** Double the LLM calls per ingestion (latency + cost), requires Rust pipeline changes in `fabric.rs` and `pipeline.rs`
- **Why not chosen:** Single-pass approach should handle obvious cases well. Revisit if detection proves unreliable in production.

<!-- FUTURE: Approach B implementation notes
If single-pass detection proves unreliable, implement two-pass:
  Pass 1: Small/cheap model detection prompt - "Does this content explicitly
           enumerate N items? If yes, output JSON: {count: N, items: [...]}.
           If no, output: {count: 0}"
  Pass 2: Feed detection JSON into obsidian-note pattern via fabric variable
           substitution so it knows definitively whether to emit the section.
Requires:
  - New detection pattern file (borg/patterns/detect-enumeration.md)
  - Rust changes in fabric.rs to chain two fabric calls
  - Pipeline changes to pass detection result to summarize call
Revisit threshold: if >20% of enumerated videos miss the section, or if
hallucinated enumerations appear in >5% of non-enumerated content. -->

### Alternative B: Rust post-processing

- **Description:** Keep pattern as-is, add Rust code to parse LLM output and detect/reformat enumeration sections
- **Pros:** Deterministic formatting guarantees
- **Cons:** Fragile parsing of LLM markdown output, significant Rust code for marginal benefit, the deduplication constraint still needs to be in the prompt anyway
- **Why not chosen:** The core problem is what the LLM generates, not how we format it. Prompt-level solution addresses the root cause.

### Alternative C: Custom fabric patterns directory

- **Description:** Set `CUSTOM_PATTERNS_DIR` in fabric's `.env` to point at `borg/patterns/`, keep using named pattern resolution
- **Pros:** Pattern name stays short in config
- **Cons:** Requires fabric config change outside the repo, still needs `obsidian-note/system.md` directory structure to match fabric's naming convention
- **Why not chosen:** Direct file path approach is simpler, requires no fabric config changes, and allows flat file naming

## Technical Considerations

### Dependencies

- **Fabric CLI** (`/home/saidler/go/bin/fabric`) - must support file path patterns. Verified in source code: `loadPattern()` in `patterns.go:58-82` routes paths starting with `~`, `/`, `.` to `getFromFile()` which handles `~` expansion.
- No new dependencies introduced.

### Performance

No change. Same single LLM call per ingestion. Pattern is marginally longer (additional instructions) but well within token limits.

### Testing Strategy

Manual validation before removing old pattern:

1. Run with a YouTube transcript that has explicit enumeration (e.g., "10 CLI Tools"):
   ```
   echo "<transcript>" | fabric -p ~/repos/scottidler/second-brain/borg/patterns/obsidian-note.md
   ```
2. Run with a transcript that has no enumeration
3. Confirm: Enumerated Points appears only in case 1, Key Ideas has no duplication, format matches spec
4. Ingest a real URL through borg and verify end-to-end

### Rollout Plan

1. Create the pattern file in the repo
2. Update borg config to point at it
3. Restart borg daemon (`systemctl --user restart borg`)
4. Ingest a few test URLs, verify output
5. Back up old pattern, then remove from fabric patterns directory

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| LLM hallucinates enumeration where none exists | Low | Low | Prompt explicitly requires creator-stated count; validate with test cases |
| LLM misses obvious enumeration | Medium | Low | Monitor ingested notes; upgrade to two-pass (Approach B) if unreliable |
| Key Ideas duplicates enumerated points | Medium | Low | Deduplication constraint stated in both STEPS and OUTPUT FORMAT; review initial outputs |
| Fabric file-path resolution breaks on update | Low | Medium | Pin fabric version; path resolution is stable, core feature |
| Old unversioned pattern lost | Low | High | Back up to `borg/patterns/obsidian-note.md.bak` before removing |

## Open Questions

- (none - all resolved during brainstorming)

## References

- `docs/design/2026-03-22-youtube-metadata-pipeline-redesign.md` - Related borg pipeline design
- `~/.config/fabric/patterns/obsidian_note/system.md` - Current pattern (to be moved)
- `fabric/internal/plugins/db/fsdb/patterns.go` - Fabric pattern loading source code
- `borg/src/fabric.rs` - Borg's fabric invocation code
- `~/.config/borg/borg.yml` - Runtime config
