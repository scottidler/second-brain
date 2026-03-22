# Design Document: Cortex Classify & Promote

**Author:** Scott Idler
**Date:** 2026-03-21
**Status:** Draft
**Review Passes Completed:** 5/5

## Summary

Restructure the second-brain pipeline so that domain classification is exclusively cortex's responsibility. This involves three coordinated changes:

1. **Strip domain classification from borg** - borg becomes a pure capture daemon (ingest, render, write to inbox)
2. **Extract search/index into vault crate** - move Oracle's FTS5 indexing and search into the shared vault library
3. **Add Classify action to cortex** - vault-contextual domain classification using the shared search index, with promotion from `inbox/` to `notes/`

The result: borg captures fast with zero LLM overhead, cortex classifies with full vault context (via shared search index), and oracle remains a thin MCP wrapper.

## Problem Statement

### Background

The second-brain workspace has four crates: vault (shared library), borg (ingestion), cortex (governance), and oracle (MCP knowledge server). After the workspace consolidation (2026-03-20) and oracle addition (2026-03-21), borg writes all ingested notes to `inbox/`.

Currently, borg performs 3-tier domain routing at ingest time (URL config, LLM via Fabric, fallback). This made sense when borg was the only daemon, but now:

- Borg's domain classification is redundant - cortex will reclassify anyway
- Borg's LLM call adds 5-30s latency to every ingest for classification that may be overridden
- Borg lacks vault context (existing notes, tag clusters, link graph) - cortex and oracle have it
- Borg's fallback domain is `"inbox"` (not a valid Domain enum variant), so fallback notes are effectively unclassified
- Oracle already has the vault indexed with FTS5 search, but this capability is locked inside an MCP server binary

### Problem

1. Notes accumulate in `inbox/` indefinitely because no automated path to `notes/` exists
2. Borg wastes time on LLM classification that cortex will redo with better context
3. Oracle's search index is inaccessible to cortex (trapped in a binary, not a library)
4. The vault's organizational model depends on notes in `notes/` with correct `domain` for Dataview queries

### Goals

- Make borg a pure capture daemon - no domain classification, no Fabric dependency for routing
- Extract Oracle's FTS5 search/index into the vault crate as a shared library
- Add cortex Classify action that uses vault search for rich contextual classification
- Promote high-confidence notes from `inbox/` to `notes/`
- Leave low-confidence notes in `inbox/` for manual triage
- Integrate with the existing cortex daemon event loop

### Non-Goals

- Changing borg's content type detection (YouTube vs article vs social) - borg still needs this for rendering
- Changing the vault folder structure - still exactly four top-level dirs
- Auto-generating note content or rewriting summaries
- Building a UI for manual triage - that's Obsidian's job via Dataview views
- Migrating oracle to a different database engine

## Proposed Solution

### Overview

Three coordinated changes across the workspace:

**Borg simplification:** Remove the 3-tier domain routing system (`classify_topic`, `RoutingConfig`, `obsidian_classify` Fabric pattern usage, confidence thresholds). Borg writes notes to inbox with `type` (still needed for rendering) but no `domain` field. The `domain` field is exclusively cortex's to assign.

**Vault search extraction:** Move Oracle's SQLite FTS5 indexing and search logic into a new `vault::search` module. This includes: database schema, note indexing, incremental reindex by mtime, full-text search, and domain/type-filtered queries. Oracle becomes a thin MCP wrapper around `vault::search`. Cortex gains access to the same search capabilities.

**Cortex Classify:** New action that scans `inbox/`, classifies notes by domain using a 3-tier pipeline (deterministic rules, vault-contextual LLM, hold for review), enriches frontmatter, and promotes to `notes/`.

### Architecture

```
BORG (capture only)
  |
  |  ingest URL/content
  |  detect content type (YouTube, article, etc.)
  |  render markdown with frontmatter (type, origin, tags - NO domain)
  |  write to inbox/
  |
  v
inbox/note.md (no domain field)
  |
  v
CORTEX CLASSIFY
  |
  [Scan inbox/]
  |
  [Validate frontmatter] --- fix missing/deprecated fields
  |
  [Tier 1: Deterministic]
  |--- tag-to-domain map match? --> domain assigned (high confidence)
  |--- source URL pattern match? --> domain assigned (high confidence)
  |
  v (no match)
  [Tier 2: Vault-contextual LLM]
  |--- Query vault::search for similar notes (FTS5)
  |--- Build context: similar notes + their domains + tag clusters
  |--- Fabric cortex_classify pattern with context
  |--- confidence >= threshold? --> domain assigned
  |
  v (low confidence)
  [Tier 3: Hold for review]
  |--- Set cortex-needs-review: true
  |--- Leave in inbox/
  |
  v (classified)
  [Enrich frontmatter]
  |--- Set domain (from classification)
  |--- Set origin: assisted (if missing)
  |--- Set status: unread (always)
  |--- Validate/fix tags
  |--- Add cortex-classified: true
  |--- Add cortex-classified-by: deterministic|llm
  |--- Add cortex-confidence: high|medium
  |
  v
  [Promote]
  |--- Move file from inbox/ to notes/
  |--- Update wikilinks across vault
  |--- Log to report
  |
  v
notes/note.md (with domain)
  |
  v
VAULT::SEARCH (shared library)
  |--- Indexes notes/ on next sweep
  |--- Available to cortex (classify) and oracle (MCP)
```

### Vault Search Module

New module: `vault::search` - extracted from oracle's current implementation.

```rust
// vault/src/search.rs

pub struct SearchIndex {
    db: rusqlite::Connection,
}

pub struct SearchResult {
    pub path: PathBuf,
    pub title: String,
    pub domain: Option<String>,
    pub note_type: Option<String>,
    pub tags: Vec<String>,
    pub snippet: String,           // FTS5 snippet with match highlights
    pub rank: f64,                 // FTS5 rank score
}

pub struct SearchQuery {
    pub text: Option<String>,      // FTS5 full-text query
    pub domain: Option<Domain>,    // Filter by domain
    pub note_type: Option<NoteType>, // Filter by type
    pub limit: usize,              // Max results (default 10)
}

impl SearchIndex {
    /// Open or create index at the given path
    pub fn open(db_path: &Path) -> Result<Self>;

    /// Full reindex: scan vault, index all notes
    pub fn reindex_full(&self, vault_root: &Path, scan_config: &ScanConfig) -> Result<usize>;

    /// Incremental reindex: only notes changed since last index (by mtime)
    pub fn reindex_incremental(&self, vault_root: &Path, scan_config: &ScanConfig) -> Result<usize>;

    /// Full-text search with optional filters
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>>;

    /// Find notes most similar to the given content
    pub fn find_similar(&self, content: &str, limit: usize) -> Result<Vec<SearchResult>>;

    /// Get domain distribution: how many notes per domain
    pub fn domain_stats(&self) -> Result<HashMap<Domain, usize>>;

    /// Get tag-domain correlation: which tags appear most in which domains
    pub fn tag_domain_map(&self) -> Result<HashMap<String, HashMap<Domain, usize>>>;

    /// Get notes by domain (for exemplars)
    pub fn domain_exemplars(&self, domain: Domain, limit: usize) -> Result<Vec<SearchResult>>;
}
```

Key design decisions:
- `SearchIndex` owns a `rusqlite::Connection`, not a path - callers control lifetime
- FTS5 with `porter` tokenizer for English stemming
- Incremental reindex uses file mtime comparison (same as current oracle)
- `find_similar` uses FTS5 term extraction from input content to find related notes
- `tag_domain_map` enables cortex to build dynamic tag-to-domain correlations from actual vault data (supplements the static config map)

### Borg Simplification

Remove from borg:

| Component | File | What to remove |
|-----------|------|----------------|
| `classify_topic()` | `borg/src/fabric.rs` | Entire function |
| `ClassificationResult` | `borg/src/fabric.rs` | Struct |
| `RoutingConfig` | `borg/src/config.rs` | Struct + defaults |
| `routing:` section | `~/.config/borg/borg.yml` | Config block |
| 3-tier routing logic | `borg/src/pipeline.rs:349-389` | All tier 1/2/3 branching |
| `domain` field in `LinkConfig` | `borg/src/config.rs` | Field (keep other link fields) |
| `obsidian_classify` Fabric calls | `borg/src/pipeline.rs` (multiple sites) | All invocations |

Keep in borg:
- Content type detection (YouTube vs article vs social vs image) - needed for rendering
- Fabric usage for content summarization (`obsidian_note` pattern) - unrelated to domain
- `type` field in rendered frontmatter
- `origin: assisted` (hardcoded)
- `tags` from Fabric summarization
- `source`, `method`, `trace` fields

After simplification, borg's rendered frontmatter looks like:

```yaml
---
title: "Example Note"
date: 2026-03-21
type: youtube
origin: assisted
tags:
  - rust
  - cli
source: "https://youtube.com/watch?v=..."
method: telegram
trace: "abc123"
---
```

No `domain` field. Cortex is the sole authority.

### Borg Ledger Impact

The borg ledger (`system/borg-ledger.md`) currently has a `domain` column populated at ingest time. With borg no longer classifying:

- The `domain` column will be empty/omitted for new ingests
- This is acceptable - the ledger is an ingest log, not a live classification index
- The note's frontmatter (set by cortex after classification) is the source of truth for current domain
- No ledger updates by cortex needed

### Data Model

#### New Cortex Frontmatter Fields

```yaml
# Set during classification
cortex-classified: true              # Note has been processed by classify
cortex-classified-by: deterministic  # or "llm"
cortex-confidence: high              # or "medium"
cortex-needs-review: true            # Only set when held in inbox
```

These follow the existing cortex field convention (`cortex-*` prefix, written by cortex actions, not user-edited).

#### Classification Config

New section in `cortex.yml`:

```yaml
actions:
  classify:
    confidence-threshold: 0.7        # Min LLM confidence to promote
    fabric-pattern: cortex_classify  # Fabric pattern for Tier 2
    fabric-timeout-secs: 30          # Per-note timeout
    max-input-tokens: 8000           # Truncation limit (higher - includes vault context)
    similar-notes-limit: 5           # Number of similar notes to include as LLM context
    tag-domain-map:                  # Static tag-to-domain hints (supplemented by vault stats)
      ai: [claude, llm, gpt, anthropic, openai, agents, prompting]
      tech: [rust, python, nix, cli, devops, obsidian, neovim, linux]
      football: [football, offense, defense, coaching, drills, plays]
      work: [tatari, sre, infrastructure, kubernetes, platform]
      writing: [writing, fiction, plot, worldbuilding, publishing]
      music: [music, synth, production, ableton, electronic]
      spanish: [spanish, espanol, vocab, grammar, conjugation]
      knowledge: [health, exercise, learning, vocabulary]
      resources: [book, reference, tools]
    source-domain-map:               # URL patterns imply domain
      ai: [anthropic.com, openai.com, huggingface.co]
      tech: [github.com, docs.rs, crates.io, nixos.org]
      football: [footballoutsiders.com, thetouchdown.co.uk]
```

#### Conflict Resolution

Within Tier 1, if multiple signals point to different domains (e.g., tags suggest "ai" but source URL suggests "tech"), apply this priority order:

1. **Tag-to-domain map** (highest) - tags are curated, most reliable signal
2. **Source URL pattern** - URL host is unambiguous but less specific (github.com hosts ai, tech, and more)

If tag signals conflict with each other (note has tags mapping to two domains), pick the domain with the most matching tags. On a tie, fall through to Tier 2 (LLM) for resolution.

### API Design

#### CLI

```
cortex classify [OPTIONS]

Options:
    --apply          Move notes (default: dry-run showing planned moves)
    --path <GLOB>    Process specific files (default: inbox/**)
    --force          Reclassify notes that already have cortex-classified: true
    --review-only    Only process notes with cortex-needs-review: true
```

#### Library Interface

```rust
// New module: cortex/src/classify.rs

pub struct ClassifyConfig {
    pub confidence_threshold: f64,
    pub fabric_pattern: String,
    pub fabric_timeout_secs: u64,
    pub max_input_tokens: usize,
    pub similar_notes_limit: usize,
    pub tag_domain_map: HashMap<String, Vec<String>>,
    pub source_domain_map: HashMap<String, Vec<String>>,
}

pub struct ClassifyResult {
    pub domain: vault::schema::Domain,   // Uses vault enum, not raw string
    pub confidence: ClassifyConfidence,
    pub method: ClassifyMethod,
    pub reason: String,                  // Human-readable explanation for logging
}

pub enum ClassifyConfidence {
    High,    // Promote to notes/
    Medium,  // Promote to notes/ (above threshold)
    Low,     // Hold in inbox
}

pub enum ClassifyMethod {
    Deterministic,  // Tag-to-domain map or source URL pattern
    Llm,            // Fabric classification with vault context
}

/// Dry-run: returns planned classifications as violations in a Report
pub fn lint_classify(
    notes: &[Note],
    config: &ClassifyConfig,
    schema: &SchemaConfig,
    search_index: &vault::search::SearchIndex,
) -> Report;

/// Apply: classify and move notes
pub fn apply_classify(
    vault_root: &Path,
    notes: &[Note],
    all_notes: &[Note],
    config: &ClassifyConfig,
    schema: &SchemaConfig,
    search_index: &vault::search::SearchIndex,
) -> Result<Report>;
```

Note: both functions now take a `SearchIndex` reference for Tier 2 vault context queries.

#### Tier 2 LLM Context Construction

When Tier 1 (deterministic) doesn't match, cortex builds vault context for the LLM:

```rust
fn build_llm_context(
    note: &Note,
    search_index: &SearchIndex,
    config: &ClassifyConfig,
) -> String {
    // 1. Find similar notes via FTS5
    let similar = search_index.find_similar(&note.body, config.similar_notes_limit);

    // 2. Get domain distribution of similar notes
    // e.g., "3 of 5 similar notes are in 'ai', 1 in 'tech', 1 in 'knowledge'"

    // 3. Get tag-domain correlations for this note's tags
    let tag_stats = search_index.tag_domain_map();

    // 4. Format as context string for Fabric pattern
    format!(
        "Title: {title}\n\n\
         Tags: {tags}\n\n\
         Similar notes in vault:\n{similar_list}\n\n\
         Tag-domain correlations:\n{tag_correlations}\n\n\
         Content:\n{body}"
    )
}
```

This gives the LLM far richer signal than borg ever had - not just the note's content, but how it relates to the existing vault.

#### Fabric Pattern: `cortex_classify`

New custom Fabric pattern, replacing `obsidian_classify`:

```markdown
# IDENTITY and PURPOSE

You are an expert content classifier for an Obsidian vault with deep knowledge
of its existing structure. Given a note's content AND context about similar notes
already in the vault, classify the note into the most appropriate domain.

# DOMAINS

[same domain list as obsidian_classify]

# CONTEXT

You will receive:
- The note's title, tags, and content
- Similar notes already in the vault (with their domains)
- Tag-domain correlations showing which tags associate with which domains

Use the similar notes and tag correlations as strong signals. If 4 of 5 similar
notes are in "ai", this note is very likely "ai" too.

# OUTPUT

Return ONLY a JSON object:

{
  "domain": "single lowercase domain from the list above",
  "confidence": 0.0 to 1.0,
  "reasoning": "Brief explanation referencing similar notes or tag patterns",
  "suggested_tags": ["tag1", "tag2", "tag3"]
}

# RULES

- Pick the MOST SPECIFIC domain that matches
- Weight similar-note domains heavily - the vault's existing classification is a strong signal
- If similar notes disagree, look at tag-domain correlations as tiebreaker
- If still unsure, set confidence below 0.5
- Do not invent domains not in the list above
```

#### Daemon Integration

Add `classify` to the daemon action registry. Classify must be the first action in the sweep - it moves files out of `inbox/`, so subsequent lint and link actions operate on the promoted notes in their final location.

The daemon opens a `SearchIndex` at startup (shared across sweeps):

```rust
// In daemon.rs
let search_index = vault::search::SearchIndex::open(&db_path)?;

// In run_configured_actions()
if daemon_config.is_enabled("classify") {
    // Ensure index is fresh before classifying
    search_index.reindex_incremental(vault_root, &scan_config)?;

    let inbox_notes: Vec<&Note> = all_notes.iter()
        .filter(|n| n.path.starts_with("inbox/"))
        .filter(|n| pending.iter().any(|p| p.ends_with(&n.path)))
        .collect();
    if !inbox_notes.is_empty() {
        apply_classify(vault_root, &inbox_notes, &all_notes,
            &config.actions.classify, &config.schema, &search_index)?;
    }
}
```

#### Oracle Refactor

Oracle becomes a thin wrapper around `vault::search`:

```rust
// oracle/src/lib.rs (simplified)
use vault::search::{SearchIndex, SearchQuery};

struct OracleServer {
    index: SearchIndex,
    vault_root: PathBuf,
}

// MCP tools become thin dispatchers:
// knowledge_search -> index.search(query)
// domain_brief -> index.domain_exemplars(domain, limit)
// vault_overview -> index.domain_stats()
// etc.
```

Oracle retains:
- MCP server scaffolding (rmcp, tool definitions, JSON schema)
- MCP-specific formatting (detail levels: metadata/tldr/summary/full)
- Server lifecycle (startup, config, reindex scheduling)

Oracle loses:
- SQLite schema definition (moves to vault::search)
- Indexing logic (moves to vault::search)
- FTS5 query construction (moves to vault::search)

### Implementation Plan

#### Phase 1: Vault Search Extraction

- Create `vault/src/search.rs` module
- Move SQLite schema, indexing, and FTS5 search from oracle into vault::search
- Add `rusqlite` dependency to vault crate
- Define `SearchIndex`, `SearchResult`, `SearchQuery` types
- Implement `open`, `reindex_full`, `reindex_incremental`, `search`, `find_similar`
- Implement `domain_stats`, `tag_domain_map`, `domain_exemplars`
- Add unit tests for search module
- Refactor oracle to import from vault::search instead of its own implementation
- Verify oracle still passes all existing tests

#### Phase 2: Borg Simplification

- Remove `classify_topic()` and `ClassificationResult` from `borg/src/fabric.rs`
- Remove `RoutingConfig` from `borg/src/config.rs`
- Remove `routing:` section from borg.yml defaults
- Remove 3-tier routing logic from pipeline.rs (all content type handlers)
- Remove `domain` field from `LinkConfig`
- Stop emitting `domain` in rendered frontmatter (`borg/src/markdown.rs`)
- Update borg ledger to omit domain column for new ingests
- Run `cargo test --workspace` to verify nothing breaks
- Update borg README/docs

#### Phase 3: Cortex Classify (Deterministic)

- Add `ClassifyConfig` as a new field on `ActionsConfig` struct in config.rs
- Add `classify` CLI command to cli.rs with `ClassifyOpts` (apply, path, force, review-only)
- Implement `lint_classify()` and `apply_classify()` in new classify.rs module
- Implement Tier 1: tag-to-domain map and source URL pattern matching
- Reuse existing `scope.rs::insert_frontmatter_fields()` for frontmatter updates
- Reuse existing `migrate.rs` move + wikilink update logic for file promotion
- Always set `status: unread` on promotion
- Add `Classify` variant to `Command` enum
- Wire into `lib.rs::run_classify()`

#### Phase 4: Cortex Classify (LLM with Vault Context)

- Create Fabric pattern `cortex_classify` at `~/.config/fabric/patterns/cortex_classify/system.md`
- Implement `build_llm_context()` using `vault::search::SearchIndex`
- Implement Tier 2: query similar notes, build context, call Fabric, parse result
- Integrate with existing `fabric.rs::run_pattern()`
- Add `similar-notes-limit` config field

#### Phase 5: Daemon Integration + Review Workflow

- Add `classify` to `DaemonConfig.actions`
- Wire into `run_configured_actions()` in daemon.rs
- Open `SearchIndex` at daemon startup, incremental reindex before classify sweeps
- Ensure classify runs BEFORE lint and link
- Add cycle detection for classify (skip `cortex-classified: true` notes)
- Add `--review-only` and `--force` flags
- Create Dataview query for `system/views/` to show notes needing review

## Alternatives Considered

### Alternative 1: Keep Classification in Borg

- **Description:** Leave borg's 3-tier routing in place. Cortex only validates and promotes.
- **Pros:** Less refactoring. Borg already works.
- **Cons:** Redundant LLM calls (borg classifies, cortex reclassifies). Borg's context is limited to title + summary. 5-30s extra latency per ingest for classification that may be overridden. Two systems assigning domain creates confusion about source of truth.
- **Why not chosen:** Single responsibility. Borg captures, cortex classifies. Removes redundant work and latency.

### Alternative 2: Cortex Reads Oracle's SQLite Directly

- **Description:** Instead of extracting search into vault crate, cortex opens oracle's DB file read-only.
- **Pros:** Less refactoring. No vault crate changes.
- **Cons:** Tight coupling to oracle's internal schema. Two binaries sharing a DB with no shared library - schema drift risk. Oracle's schema becomes a public API without being designed as one.
- **Why not chosen:** Option 2 (extract to vault) is the right long-term architecture. Oracle's search is a workspace capability, not an oracle-specific feature.

### Alternative 3: Cortex Calls Oracle via MCP

- **Description:** Cortex becomes an MCP client and calls oracle's tools for vault context.
- **Pros:** Most decoupled. Cortex doesn't know oracle's internals.
- **Cons:** MCP client dependency is heavy for what's a local function call. Requires oracle to be running when cortex classifies. Network overhead for same-machine IPC.
- **Why not chosen:** Overkill. Same workspace, same machine. A shared library is simpler and faster.

### Alternative 4: Skip Fabric, Use Direct Anthropic API

- **Description:** Call Claude API directly from cortex instead of shelling out to Fabric CLI.
- **Pros:** No Fabric dependency. Full control over prompt and structured output. Potentially lower latency.
- **Cons:** New HTTP client + API dependency in cortex. Diverges from Fabric-based approach used elsewhere. Harder to iterate on prompts (code change + recompile vs editing a pattern file).
- **Why not chosen:** Fabric works well enough and keeps prompts editable. Revisit if Fabric becomes a bottleneck.

## Technical Considerations

### Dependencies

**New vault crate dependencies:**
- `rusqlite` with `bundled` + `fts5` features (SQLite with FTS5 support)

**Removed borg dependencies (if solely for classification):**
- Fabric classify pattern invocation can be removed from borg's pipeline

**Cortex gains:**
- `vault::search` (via workspace dependency on vault crate)

### Performance

- Inbox typically has 5-50 notes at any given time - classification is not a hot path
- Deterministic Tier 1 is O(n * rules) - negligible
- Vault search queries: FTS5 is fast (<10ms for typical queries on a vault of thousands of notes)
- LLM Tier 2 is bounded by fabric timeout (30s per note) - serial for now, could parallelize later
- Borg becomes faster: no LLM call per ingest saves 5-30s per note
- Incremental reindex before classify: O(changed files) not O(vault size)
- File moves are atomic (rename within same filesystem)

### Security

- No new external inputs - vault::search reads local files only
- Fabric CLI is already trusted (used by cortex intel and autotag actions)
- SQLite database is local, no network access

### Edge Cases

**Filename collision on promote:** If `notes/my-note.md` already exists when promoting `inbox/my-note.md`, append a numeric suffix: `notes/my-note-2.md`. Same strategy borg uses for duplicate filenames.

**Notes without frontmatter:** Borg always writes frontmatter, but manually added inbox notes might lack it. The "Validate frontmatter" step should add minimal frontmatter (title from filename, date from file mtime, origin: authored) before classification proceeds. If frontmatter can't be created, hold for review.

**Path exemptions after promotion:** Cortex config exempts `Inbox/**` from requiring domain, origin, and tags. Once a note moves to `notes/`, these exemptions no longer apply. The enrichment step must ensure all required fields are set BEFORE the file move: domain (from classification), origin (default: assisted), tags (preserve existing or set empty list), status (unread).

**Idempotency:** Classify skips notes where `cortex-classified: true` unless `--force` is passed. This prevents re-processing on daemon retriggering.

**Empty search index:** On first run (or after DB deletion), `find_similar` returns no results. Tier 2 still works - the LLM just has less context. Deterministic Tier 1 is unaffected.

**Oracle and cortex sharing the DB:** Both use vault::search::SearchIndex. Only one writer at a time (SQLite WAL mode allows concurrent reads). Cortex reads during classify, oracle writes during reindex. Schedule reindex to not overlap with classify sweeps, or accept brief lock contention.

**Borg ledger domain column:** New ingests will have no domain in the ledger. This is acceptable - the ledger is an ingest log. The note's frontmatter (set by cortex) is the source of truth.

### Testing Strategy

- **vault::search unit tests:** indexing, FTS5 queries, find_similar, domain_stats, tag_domain_map, incremental reindex
- **oracle integration tests:** verify oracle still works after refactor to use vault::search
- **borg tests:** verify notes render without domain field, pipeline works without routing
- **cortex classify unit tests:** deterministic classification (tag-to-domain, source-to-domain), confidence thresholds, hold-back logic
- **cortex classify integration tests:** create inbox note, run classify, verify moved to notes/ with correct frontmatter. Create ambiguous note, verify stays in inbox with cortex-needs-review
- **Edge cases:** note with no frontmatter, note already classified, conflicting tag signals, filename collision, empty search index, `--force` reclassification

### Rollout Plan

1. Phase 1: Extract vault::search, refactor oracle - verify oracle works unchanged
2. Phase 2: Strip borg classification - verify ingestion still works, notes land in inbox without domain
3. Phase 3: Cortex classify (deterministic) - test with `cortex classify` CLI in dry-run mode on existing inbox
4. Phase 4: Cortex classify (LLM) - create cortex_classify pattern, dry-run validation
5. Phase 5: Daemon integration - keep disabled initially, enable after manual validation
6. Retire `obsidian_classify` Fabric pattern (borg no longer uses it, cortex uses `cortex_classify`)

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Oracle refactor breaks MCP tools | Medium | High | Oracle's MCP interface doesn't change - only the internal implementation. Full test coverage before and after. |
| Borg simplification removes something still needed | Low | Medium | Borg keeps type detection, Fabric summarization, and all non-domain fields. Only domain routing is removed. |
| Misclassification moves note to wrong domain | Medium | Low | Domain is a frontmatter field, easily corrected. Dataview views are the real org layer, not folders. |
| Daemon classify loop (classify moves file, watcher triggers again) | Medium | Medium | Cycle detection exists in daemon. Classify skips `cortex-classified: true` notes. Moved files leave inbox/ so inbox filter excludes them. |
| Fabric unavailable or slow | Low | Low | Tier 1 (deterministic) works without Fabric. Tier 2 has timeout. Fallback to hold-in-inbox. |
| SQLite contention between oracle and cortex | Low | Low | WAL mode handles concurrent readers. Reindex and classify are both infrequent operations. |
| Wikilink breakage during move | Low | Medium | Reuse battle-tested migrate.rs wikilink update logic. |
| Frontmatter exemptions no longer apply after move | Medium | Medium | Enrichment step sets all required fields BEFORE move. Lint runs after classify to catch gaps. |

## Open Questions

- [ ] Fabric pattern for classification: create new `cortex_classify` pattern (vault-contextual, described above) - needs authoring and testing
- [x] Should promote set `status: unread` unconditionally? YES - always.
- [x] Action ordering in daemon: classify runs FIRST (before lint and link).
- [x] Max-age for stale inbox notes? Not now - out of scope.
- [x] Keep classification in borg? NO - strip it. Cortex is the sole classifier.
- [x] How cortex gets vault context: extract oracle's search into vault crate (Option 2).

## References

- [Workspace Consolidation Design Doc](2026-03-20-workspace-consolidation.md)
- [Oracle MCP Design Doc](2026-03-21-oracle-mcp.md)
- Vault Frontmatter Schema: `~/repos/scottidler/obsidian/system/schemas/frontmatter.md`
- Domain Values: `~/repos/scottidler/obsidian/system/schemas/domain-values.md`
- Borg pipeline routing: `borg/src/pipeline.rs:349-389`
- Borg Fabric classify: `borg/src/fabric.rs:133-142`
- Oracle search implementation: `oracle/src/` (to be extracted to vault::search)
- Cortex daemon event loop: `cortex/src/daemon.rs`
- Existing scope-based field assignment: `cortex/src/scope.rs`
- Migrate file move + wikilink update: `cortex/src/migrate.rs`
- Current Fabric pattern: `~/.config/fabric/patterns/obsidian_classify/system.md`
