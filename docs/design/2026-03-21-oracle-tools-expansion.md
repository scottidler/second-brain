# Design Document: Oracle MCP Tools Expansion

**Author:** Scott Idler
**Date:** 2026-03-21
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Expand oracle from 8 MCP tools to 18+ by exposing tag queries, wikilink graph traversal, similarity search, inbox/classification status, cortex governance metadata, creator/source browsing, cross-domain connections, and activity timelines. The data already exists in the vault and SQLite index - oracle just needs tools to surface it.

## Problem Statement

### Background

Oracle is the MCP knowledge retrieval server for the second-brain vault. After the workspace consolidation and cortex classify work, the vault now has rich metadata: 917 notes across 10 domains, 600 with tags, 252 with wikilinks, cortex quality/classification/duplicate fields, source URLs on 711 notes, and creators on 542. The SQLite FTS5 index stores all of this.

The current 8 tools cover basic search, browse, read, stats, and ingest history. But the vault's real value is in the connections, patterns, and governance metadata that the current tools can't surface.

### Problem

1. **Tags are the primary organizational signal** but oracle can't search or browse by tag
2. **Wikilinks create a knowledge graph** but oracle can't traverse it - no "what links here" or "what does this link to"
3. **Cortex writes governance metadata** (quality scores, classification confidence, duplicate groups, review flags) but oracle can't query any of it
4. **The inbox/classify pipeline** produces notes needing review but there's no way to see inbox status via MCP
5. **vault::search already has `find_similar` and `tag_domain_map`** but they're not exposed as MCP tools
6. **711 notes have source URLs and 542 have creators** but there's no way to browse by source or creator
7. **No cross-domain discovery** - can't find notes that bridge domains or surface unexpected connections

### Goals

- Expose all queryable vault data through MCP tools
- Make tags a first-class query dimension (search, browse, aggregate)
- Surface the wikilink graph (inbound, outbound, orphans)
- Expose cortex governance metadata (quality, classification, duplicates, review queue)
- Enable content discovery (similar notes, cross-domain connections, activity timelines)
- Keep tools focused - each does one thing well
- Maintain the detail level pattern for consistent verbosity control

### Non-Goals

- Write operations - oracle is read-only (cortex handles mutations)
- Replacing Obsidian's UI - oracle augments Claude Code, not Obsidian
- Real-time streaming - oracle is request/response
- Exposing raw SQL - all queries go through typed SearchIndex methods

## Proposed Solution

### Overview

Add 10 new MCP tools in three tiers, plus extend the SearchIndex in vault::search with the backing queries.

**Tier 1 - Tag & Content Discovery (highest value):**
- `tag_search` - find notes by tag, list tags with counts
- `find_similar` - find notes similar to given content or a note path
- `recent_activity` - cross-domain timeline of vault activity

**Tier 2 - Graph & Relationships:**
- `find_links` - wikilink graph traversal (outbound + inbound links for a note)
- `creator_browse` - browse notes by creator/channel
- `source_browse` - browse notes by source domain/URL

**Tier 3 - Governance & Health:**
- `inbox_status` - what's in inbox, what needs review, classify pipeline health
- `quality_report` - notes by quality score, common issues, improvement candidates
- `duplicate_groups` - browse duplicate clusters, compare duplicates
- `classify_status` - classification metadata, confidence distribution, domain assignment stats

### Architecture

```
vault::search::SearchIndex (shared library)
  |
  |-- Existing methods (8 tools already use these)
  |-- New methods:
  |     tag_search(tag, domain, limit) -> Vec<NoteRow>
  |     tag_stats() -> Vec<(String, u64)>
  |     tag_cooccurrence(tag) -> Vec<(String, u64)>
  |     find_similar(content, limit) -> Vec<NoteRow>  [already exists]
  |     recent_notes(days, domain, limit) -> Vec<NoteRow>
  |     find_outbound_links(path) -> Vec<String>
  |     find_inbound_links(stem) -> Vec<NoteRow>
  |     orphan_notes() -> Vec<NoteRow>
  |     creator_stats() -> Vec<(String, u64)>
  |     notes_by_creator(creator, limit) -> Vec<NoteRow>
  |     source_domain_stats() -> Vec<(String, u64)>
  |     notes_by_source_domain(host, limit) -> Vec<NoteRow>
  |     inbox_notes() -> Vec<NoteRow>
  |     notes_needing_review() -> Vec<NoteRow>
  |     quality_distribution() -> HashMap<String, u64>
  |     notes_by_quality(quality, limit) -> Vec<NoteRow>
  |     duplicate_groups() -> Vec<DuplicateGroup>
  |     classify_stats() -> ClassifyStats
  |
  v
oracle::server (MCP tool handlers)
  |-- 8 existing tools
  |-- 10 new tools (wrappers around SearchIndex methods)
```

### Data Model

#### New SearchIndex Methods

Most new queries are simple SQL against the existing `notes` table. The wikilink graph queries parse `[[...]]` patterns from the body column. Cortex governance fields are in the frontmatter (stored in body text), so they need either:
- **Option A:** Parse from body text at query time (simple but slow for aggregates)
- **Option B:** Add columns to the notes table for cortex fields (fast queries, requires schema migration)

**Decision: Option B** - add columns for the most-queried cortex fields during indexing. The fields are in the YAML frontmatter which `index_vault` already parses. Add these columns:

```sql
ALTER TABLE notes ADD COLUMN quality TEXT DEFAULT '';
ALTER TABLE notes ADD COLUMN classified INTEGER DEFAULT 0;
ALTER TABLE notes ADD COLUMN classified_by TEXT DEFAULT '';
ALTER TABLE notes ADD COLUMN confidence TEXT DEFAULT '';
ALTER TABLE notes ADD COLUMN needs_review INTEGER DEFAULT 0;
ALTER TABLE notes ADD COLUMN duplicate_group TEXT DEFAULT '';
```

The `index_vault` function already parses frontmatter - it just needs to also extract `extra` fields with the `cortex-` prefix and map them to columns.

#### New Result Types

```rust
pub struct TagStats {
    pub tag: String,
    pub count: u64,
    pub domains: Vec<String>,  // Which domains this tag appears in
}

pub struct LinkInfo {
    pub outbound: Vec<String>,    // [[targets]] this note links to
    pub inbound: Vec<NoteRow>,    // Notes that link to this one
}

pub struct DuplicateGroup {
    pub group_id: String,
    pub notes: Vec<NoteRow>,
}

pub struct ClassifyStats {
    pub total_classified: u64,
    pub by_method: Vec<(String, u64)>,      // deterministic vs llm
    pub by_confidence: Vec<(String, u64)>,   // high vs medium vs low
    pub by_domain: Vec<(String, u64)>,       // domain distribution of classified notes
    pub pending_review: u64,
    pub inbox_count: u64,
}

pub struct QualityReport {
    pub by_quality: Vec<(String, u64)>,      // high/medium/low distribution
    pub common_issues: Vec<(String, u64)>,    // most common quality issues
    pub improvement_candidates: Vec<NoteRow>, // low-quality notes worth fixing
}
```

### API Design - New MCP Tools

#### 1. tag_search

Find notes by tag, or list all tags with their counts.

```
Parameters:
  tag: Option<String>      - Specific tag to search for (exact or prefix match)
  domain: Option<Domain>   - Filter to tags within a domain
  detail: Option<Detail>   - Detail level for returned notes (default: metadata)
  limit: Option<u32>       - Max results (default: 20)

Response (when tag provided):
  { count, results: [NoteRow...] }

Response (when no tag - list mode):
  { count, tags: [{ tag, count, domains }...] }
```

#### 2. find_similar

Find notes similar to given content or another note.

```
Parameters:
  content: Option<String>  - Text to find similar notes for
  path: Option<String>     - Path to a note (uses its body as content)
  domain: Option<Domain>   - Restrict to domain
  detail: Option<Detail>   - Default: tldr
  limit: Option<u32>       - Default: 5

Response:
  { count, results: [NoteRow...] }
```

#### 3. recent_activity

Cross-domain timeline of recent vault activity.

```
Parameters:
  days: Option<u32>        - How many days back (default: 7)
  domain: Option<Domain>   - Filter to domain
  note_type: Option<Type>  - Filter to type
  detail: Option<Detail>   - Default: tldr
  limit: Option<u32>       - Default: 20

Response:
  { count, span: "2026-03-14 to 2026-03-21", results: [NoteRow...] }
```

#### 4. find_links

Wikilink graph traversal for a note.

```
Parameters:
  path: String             - Note path to inspect
  direction: Option<String> - "outbound", "inbound", or "both" (default: both)
  detail: Option<Detail>   - Default: metadata

Response:
  {
    note: { path, title },
    outbound: [ { target, resolved_path, exists } ... ],
    inbound: [ NoteRow... ],
    orphan: bool  // true if no inbound links
  }
```

#### 5. creator_browse

Browse notes by creator/channel.

```
Parameters:
  creator: Option<String>  - Filter to specific creator (substring match)
  domain: Option<Domain>   - Filter to domain
  detail: Option<Detail>   - Default: metadata
  limit: Option<u32>       - Default: 20

Response (when creator provided):
  { creator, count, results: [NoteRow...] }

Response (when no creator - list mode):
  { count, creators: [{ name, count, domains, types }...] }
```

#### 6. source_browse

Browse notes by source URL domain.

```
Parameters:
  host: Option<String>     - Source domain to filter (e.g., "youtube.com")
  domain: Option<Domain>   - Filter vault domain
  detail: Option<Detail>   - Default: metadata
  limit: Option<u32>       - Default: 20

Response (when host provided):
  { host, count, results: [NoteRow...] }

Response (when no host - list mode):
  { count, sources: [{ host, count, domains }...] }
```

#### 7. inbox_status

View inbox contents and classification pipeline health.

```
Parameters:
  detail: Option<Detail>   - Default: tldr
  limit: Option<u32>       - Default: 50

Response:
  {
    inbox_count: 18,
    needs_review: 14,
    classified: 4,
    notes: [NoteRow...],
    review_candidates: [NoteRow...]  // notes with cortex-needs-review
  }
```

#### 8. quality_report

Notes by quality score and common issues.

```
Parameters:
  quality: Option<String>  - Filter: "low", "medium", "high"
  detail: Option<Detail>   - Default: tldr
  limit: Option<u32>       - Default: 20

Response:
  {
    distribution: { low: 45, medium: 310, high: 200 },
    common_issues: [ { issue: "no-inbound-links", count: 120 }, ... ],
    results: [NoteRow...]  // filtered by quality if provided
  }
```

#### 9. duplicate_groups

Browse duplicate note clusters.

```
Parameters:
  group_id: Option<String> - Specific group to inspect
  detail: Option<Detail>   - Default: summary
  limit: Option<u32>       - Default: 10

Response (when group_id provided):
  { group_id, notes: [NoteRow...] }

Response (when no group_id - list mode):
  { count, groups: [{ group_id, note_count, titles: [...] }...] }
```

#### 10. classify_status

Classification pipeline health and metadata.

```
Parameters:
  domain: Option<Domain>   - Filter to domain

Response:
  {
    total_classified: 281,
    by_method: { deterministic: 270, llm: 11 },
    by_confidence: { high: 260, medium: 21 },
    by_domain: { ai: 80, tech: 65, ... },
    inbox_count: 18,
    pending_review: 14,
    unclassified: 636  // notes without domain (excludes daily/system types which legitimately omit domain)
  }
```

### Implementation Plan

#### Phase 1: Schema Extension + Tag Tools

- Add cortex governance columns to notes table schema in vault::search
- Update `index_vault` to extract cortex-* fields from frontmatter.extra
- Add `tag_search`, `tag_stats`, `tag_cooccurrence` methods to SearchIndex
- Implement `tag_search` MCP tool
- Tests for tag queries

#### Phase 2: Content Discovery Tools

- Expose `find_similar` as MCP tool (method already exists)
- Add `recent_notes` method to SearchIndex
- Implement `find_similar` and `recent_activity` MCP tools
- Tests

#### Phase 3: Graph & Relationship Tools

- Add wikilink parsing: `find_outbound_links`, `find_inbound_links`, `orphan_notes`
- Add `creator_stats`, `notes_by_creator`, `source_domain_stats`, `notes_by_source_domain`
- Implement `find_links`, `creator_browse`, `source_browse` MCP tools
- Tests for wikilink regex, creator/source aggregation

#### Phase 4: Governance & Health Tools

- Add quality/classify/duplicate query methods using new columns
- Implement `inbox_status`, `quality_report`, `duplicate_groups`, `classify_status` MCP tools
- Tests for governance queries

## Alternatives Considered

### Alternative 1: Parse Cortex Fields from Body at Query Time

- **Description:** Instead of adding columns, parse cortex-* fields from the YAML frontmatter in the body column using regex/YAML parsing in SQL queries or Rust.
- **Pros:** No schema migration. Simpler indexing.
- **Cons:** Slow for aggregates (parsing YAML per row). Can't use SQL WHERE clauses. Body column doesn't contain frontmatter - it's the post-frontmatter content.
- **Why not chosen:** The body column stores post-frontmatter content (no YAML). The frontmatter is parsed into the Frontmatter struct during indexing, with cortex-* fields landing in the `extra` HashMap, but that HashMap isn't stored in the DB. Must add columns and extract during indexing.

### Alternative 2: Separate Governance Table

- **Description:** Create a `cortex_metadata` table that tracks governance fields separately from the notes table.
- **Pros:** Clean separation. Doesn't bloat the notes table.
- **Cons:** Requires JOINs for every governance query. More complex schema management. Cortex metadata is per-note, same cardinality as notes.
- **Why not chosen:** Adding columns to the notes table is simpler and the fields are 1:1 with notes. No benefit to a separate table.

### Alternative 3: Fewer, Broader Tools

- **Description:** Instead of 10 focused tools, add 3-4 broad tools with many parameters.
- **Pros:** Fewer tools in the MCP schema. Less code.
- **Cons:** Each tool becomes complex with many optional parameters. Harder for the LLM to choose the right tool. Descriptions become vague.
- **Why not chosen:** Focused tools with clear names are easier for Claude to select. "tag_search" is unambiguous; "advanced_query" with 15 optional params is not.

## Technical Considerations

### Dependencies

- **vault::search** - add new methods and schema columns
- **oracle** - add new MCP tool handlers and request types
- No new external dependencies

### Performance

- Tag queries: tags are stored as JSON arrays (e.g., `["rust","cli"]`). Use `serde_json::from_str` in Rust to parse, then filter. SQLite's `json_each()` is an alternative but Rust-side parsing is simpler and already used elsewhere.
- Wikilink parsing uses regex on body text: `\[\[([^\]|#]+)(?:[|#][^\]]+)?\]\]` to capture target stem. Must skip fenced code blocks. Scan all notes for inbound links is O(n) but notes are small (~2KB average body).
- Cortex field queries use indexed columns after schema extension - fast
- `find_similar` already exists and uses FTS5 - fast
- Consider caching tag_stats and creator_stats if they become slow at scale

### Security

- Read-only - no mutation risk
- All queries are parameterized (no SQL injection)
- No external network access

### Testing Strategy

- Unit tests for each new SearchIndex method with in-memory SQLite
- Test wikilink regex against various patterns: `[[simple]]`, `[[with|alias]]`, `[[path/to/note]]`
- Test tag JSON parsing edge cases: empty tags, null tags, malformed JSON
- Test cortex field extraction from frontmatter during indexing
- Integration: verify each MCP tool returns correct JSON structure

### Rollout Plan

1. Phase 1: Schema extension + tag tools. Rebuild index with `oracle index`. Verify existing tools still work.
2. Phase 2: Content discovery tools. These are low-risk additions.
3. Phase 3: Graph + relationship tools. Wikilink parsing is new logic - test carefully.
4. Phase 4: Governance tools. Depends on Phase 1 schema extension.
5. After all phases: reinstall oracle, restart MCP server, verify all 18 tools appear.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Schema migration breaks existing index | Low | Medium | SQLite ALTER TABLE doesn't support IF NOT EXISTS. Use PRAGMA table_info to check columns before adding. Wrap in a try/catch pattern. Existing data preserved - new columns get defaults. |
| Wikilink regex misses edge cases | Medium | Low | Start with common patterns, iterate. False negatives are harmless. |
| Tag JSON parsing failures | Low | Low | Already handled in existing code with unwrap_or_default. |
| Too many tools overwhelm Claude's tool selection | Low | Medium | Tool descriptions are specific and unique. 18 tools is within MCP norms. |
| Performance degrades with wikilink scanning | Low | Low | Only find_links does full vault scan. Cache results if needed. |
| Cortex field column names diverge from actual field names | Low | Medium | Map explicitly in code: `cortex-quality` -> `quality` column. Document mapping. |

## Open Questions

- [x] Schema extension vs runtime parsing: add columns (decided above)
- [ ] Should tag_search support prefix matching ("rust*") or exact match only? Recommendation: support both - exact by default, prefix with trailing `*`.
- [x] Should find_links resolve wikilink targets to actual note paths? YES - resolve to paths and report whether the target exists. Raw link text alone isn't useful.
- [ ] Should we index wikilinks into a separate `links` table for faster graph queries at scale? Not now - parse at query time. Revisit if the vault exceeds 5K notes.

## References

- [Oracle MCP Design Doc](2026-03-21-oracle-mcp.md)
- [Cortex Classify Design Doc](2026-03-21-cortex-classify-promote.md)
- vault::search implementation: `vault/src/search.rs`
- Oracle server: `oracle/src/server.rs`
- Oracle tools: `oracle/src/tools.rs`
- Vault stats: 917 notes, 600 tagged, 252 with wikilinks, 711 with source URLs
