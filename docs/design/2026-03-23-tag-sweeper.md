# Design Document: Tag Sweeper - Vocabulary Consolidation and Constraint

**Author:** Scott Idler
**Date:** 2026-03-23
**Status:** Draft
**Review Passes Completed:** 5/5

## Summary

The vault has 4,760 unique tags across 864 notes - a 5.5:1 tag-to-note ratio caused by borg's unconstrained Fabric tag generation. Tags are semantically redundant (`ai-agents`, `ai-coding-agents`, `ai-strategy-for-operators`), include concatenated junk (`claudecodeai`, `obsidiantutorial`), and overwhelm Tier 1 classification. This design introduces a canonical tag vocabulary (~80-150 tags, ceiling 300), constrains borg at ingestion, adds a cortex sweeper for ongoing governance, and produces a mapping artifact for LLM taste training.

## Problem Statement

### Background

Borg's ingestion pipeline collects tags from three sources: Fabric's `create_tags` pattern (~20+ per note), author/metadata hashtags, and yt-dlp metadata. Tags are sanitized (lowercased, hyphenated) and deduplicated, but there is no vocabulary constraint. Every Fabric call can invent new compound slugs freely.

Cortex's Tier 1 classify uses a `tag_domain_map` with ~50 exact trigger words. The recent segment-matching fix (2026-03-23-classify-pipeline-fix.md) helps, but the underlying tag explosion means the signal-to-noise ratio in tags is poor.

### Problem

Three interrelated failures:

**Tag explosion:** 4,760 unique tags, hundreds used by only 1 note. The `ai-*` prefix alone has 180+ variants. Tags like `ai-strategy`, `ai-strategy-for-teams`, `ai-strategy-for-operators` all mean the same thing.

**No vocabulary constraint at ingestion:** Borg accepts whatever Fabric generates. No cap on tags per note (some have 20+). No canonical list to map against. Concatenated words from author metadata (`claudecodeai`, `obsidiantutorial`) pass through unchallenged.

**No consolidation mechanism:** Cortex can lint tag format and resolve aliases, but has no batch sweeper to consolidate semantically equivalent tags or propose vocabulary changes. The alias map is manually maintained and reactive.

### Goals

- Establish a canonical tag vocabulary (~80-150 tags, hard ceiling 300) shared by borg and cortex
- Constrain borg to emit only canonical tags, capped at 7 per note (configurable)
- Reject concatenated-word tags at ingestion
- Migrate all existing notes to use canonical tags
- Produce a full `old_tag -> canonical_tag` mapping file as an LLM taste-training artifact
- Build a proposal queue so the system surfaces new vocabulary needs rather than silently dropping content signal

### Non-Goals

- Changing Fabric patterns (future project - constrained prompt approach noted for later)
- Adding new vault domains (the sweeper will surface domain gaps; that's a separate decision)
- Building a UI for tag management
- Real-time LLM-in-the-loop tag mapping at ingestion (Approach 2, noted for future)
- Embedding-based tag clustering

## Proposed Solution

### Overview

Three components share a single canonical vocabulary file:

```
canonical-tags.yml (source of truth)
        |
   vault crate (loader + matcher)
      /         \
   borg          cortex
(post-filter)  (sweeper + migration)
```

**Config files** (source of truth in `second-brain/config/`, installed to `~/.config/second-brain/`):
- `canonical-tags.yml` - the curated tag vocabulary (includes `max-per-note` in header)
- `tag-mapping.yml` - full old-to-canonical mapping (training artifact)
- `tag-proposals.yml` - LLM-generated proposals awaiting human review

**New shared config directory:** This introduces `~/.config/second-brain/` as a shared config path alongside the per-binary directories (`~/.config/borg/`, `~/.config/obsidian-cortex/`, `~/.config/oracle/`). This is a deliberate architectural choice - tag vocabulary is truly shared state that neither binary owns. The install step in CLAUDE.md and `otto install` must be updated to create this directory and copy config files from `second-brain/config/`.

### Architecture

#### Shared vocabulary loader (vault crate)

New module in `vault` providing:
- `load_canonical_tags(path) -> Vec<String>` - loads the YAML list
- `match_to_canonical(raw_tag, canonical_set, mapping) -> Vec<String>` - matching logic: check mapping file first (deterministic), then exact match against canonical set, then segment-based fuzzy match. Returns zero (no match), one (deterministic/exact), or multiple (segment match) canonical tags.
- `is_concatenated_word(tag) -> bool` - rejection heuristic for tags with no hyphens. Algorithm: strip hyphens from each canonical tag to build a substring dictionary (e.g., `prompt-engineering` becomes `promptengineering`). If a raw unhyphenated tag contains 2+ of these substrings as non-overlapping matches, it is concatenated (e.g., `claudecodeai` matches `claude` + `code` + `ai`). Does NOT use length thresholds since legitimate long words exist (`infrastructure`, `worldbuilding`). Tags with hyphens skip this check entirely.
- `filter_and_cap(raw_tags, canonical_set, max) -> Vec<String>` - full pipeline both binaries use

Both borg and cortex call the same functions, guaranteeing identical tag behavior.

#### Matching priority

When resolving a raw tag to a canonical tag, the matcher follows this priority:

1. **Mapping file lookup** - if `tag-mapping.yml` has an entry for this exact raw tag, use it (or reject if mapped to `null`). This is the fast path and handles all known tags deterministically.
2. **Exact canonical match** - if the raw tag is already in the canonical set, keep it as-is.
3. **Segment fuzzy match** - split raw tag on `-`, check if any segment exactly matches a canonical tag (single-word canonical tags only; multi-word canonical tags like `prompt-engineering` are only matched via the mapping file or exact match). If exactly one canonical tag matches, use it. If multiple match, return all matches - `filter_and_cap` will include each as a separate canonical tag (e.g., `ai-coding-agents` with segments `ai`, `coding`, `agents` may map to both `ai` and `agents`). This is correct: a single raw tag can contribute multiple canonical tags to the note's final tag set.
4. **No match** - drop the tag. If cortex sees this tag on 3+ notes, it enters the proposal queue.

The mapping file grows over time as the sweeper encounters and resolves new tags. This means the fuzzy matcher (step 3) is primarily a cold-start mechanism - once a tag has been seen and mapped, future encounters use the deterministic path.

#### Borg post-filter (ingestion constraint)

Inserted in `pipeline.rs` after existing tag collection and dedup (lines ~360-374):

1. Fabric generates raw tags (unconstrained, as today)
2. Author/metadata tags collected (as today)
3. Sanitize + dedup (as today)
4. **New - canonical filter:**
   - Load canonical list via vault crate
   - Reject concatenated words (`is_concatenated_word`)
   - For each remaining tag, attempt match via `match_to_canonical`
   - Matched tags map to canonical form
   - Unmatched tags are dropped silently (proposal queue in cortex catches patterns)
5. Dedup again (multiple raw tags may map to same canonical)
6. Cap at `max-per-note` (default 7). When over the cap, keep tags in the order they were matched (mapping hits first, then exact, then segment), which naturally prefers more specific/deterministic matches. Within a tier, alphabetical.

#### Cortex sweeper (governance)

New subcommand: `cortex sweep`

**Migration mode** (`cortex sweep --migrate`):
- Scans all notes in vault
- For each note's tags, applies the `tag-mapping.yml` mapping
- Dedup after mapping (multiple old tags may collapse to same canonical)
- Cap at `max-per-note`
- Rewrites frontmatter in place
- Reports: notes modified, tags consolidated, orphans dropped

**Daemon mode** (periodic task alongside classify/lint):
- Scans for non-canonical tags across all notes
- Unmapped tags accumulate in memory across cycles
- When an unmapped tag appears on `proposal-threshold` or more notes (default 3), writes to `tag-proposals.yml`
- Proposal format:

```yaml
proposals:
  - tag: "3d-printing"
    frequency: 5
    suggested-canonical: "3d-printing"
    action: "add"
    notes:
      - note1.md
      - note2.md
  - tag: "machine-learning-ops"
    frequency: 3
    suggested-canonical: "mlops"
    action: "merge"
```

Human reviews proposals, approves/rejects, and the canonical list + mapping update accordingly. Accept/reject decisions themselves become training signal. Rejected proposals are added to `tag-mapping.yml` mapped to `null`, which prevents the sweeper from re-proposing them. If a rejected tag later accumulates significantly more notes, the human can remove the null mapping to allow re-proposal.

### Data Model

#### canonical-tags.yml

```yaml
# Canonical tag vocabulary for second-brain
max-per-note: 7
max-canonical: 300
tags:
  ai:
    - ai
    - agents
    - claude
    - llm
    - mcp
    - prompt-engineering
    - openai
    - anthropic
  tech:
    - rust
    - python
    - linux
    - obsidian
    - cli
    - devops
    - neovim
    - git
  football:
    - football
    - offense
    - defense
    - spread-offense
    - air-raid
    - coaching
  # ... additional domains
```

Tags are organized by domain for human readability but the loader flattens to a single `HashSet<String>` for matching. The domain grouping is purely cosmetic and editorial - it helps humans browse the list but has no runtime significance. A tag appearing under the "ai" heading does not affect classification; that remains the job of `tag_domain_map` in classify config. Tags that span domains (e.g., `automation`) should be listed under whichever domain feels most natural; the grouping does not constrain usage. Note: this creates two domain-to-tag mappings - one cosmetic here and one functional in classify's `tag_domain_map`. This duplication is accepted for now; a future unification could derive `tag_domain_map` from canonical-tags.yml's grouping, but that is out of scope.

#### tag-mapping.yml (training artifact)

```yaml
# Training data: raw tag -> canonical tag
# null = rejected (concatenated word or unmappable)
ai-agents: agents
ai-coding: ai
ai-coding-agents: agents
ai-strategy: ai
ai-strategy-for-operators: ai
ai-strategy-for-teams: ai
claude-code: claude
claudecode: null
claudecodeai: null
machine-learning: llm
large-language-models: llm
obsidian-tutorial: obsidian
obsidiantutorial: null
prompt-engineering: prompt-engineering
prompting: prompt-engineering
```

#### tag-proposals.yml

```yaml
proposals: []
```

Starts empty, populated by cortex sweeper daemon.

### Relationship to existing tag infrastructure

Cortex already has tag-related code in `tags.rs` (aliases, canonical list, linting) and `autotag.rs` (suggesting tags for under-tagged notes). This design subsumes and replaces parts of that:

- **`tags.aliases`** in cortex config is replaced by `tag-mapping.yml`. The mapping file is a superset of aliases - it maps every known raw tag to its canonical form. Existing alias resolution logic in `lint_tags`/`apply_tags` should be updated to read from the mapping file instead.
- **`tags.canonical`** in cortex config is replaced by `canonical-tags.yml`. Same data, shared location.
- **`autotag.rs`** coexists. Auto-tag suggests canonical tags for notes with too few tags. The sweeper consolidates notes with too many or non-canonical tags. They solve complementary problems. Auto-tag's `canonical_tags` config should point to the shared canonical file.
- **`tags.rs` linting** continues to validate tag format (lowercase-hyphenated). The sweeper adds vocabulary governance on top of format governance.
- **oracle** requires no changes. Its `tag_search` tool queries tags as they exist in note frontmatter. After migration, it naturally searches the consolidated canonical tags. No code changes needed.

### Mixed state during rollout

Between borg deploy (Phase 3) and migration (Phase 5), the vault contains a mix of canonical and non-canonical tags. This is fine:
- Cortex classify's segment matching works on both old and new tag styles
- The sweeper daemon can run before migration to build up the proposal queue
- Migration is the cleanup pass that brings everything into canonical form

### Config Changes

#### cortex.yml additions

```yaml
tags:
  canonical-path: "~/.config/second-brain/canonical-tags.yml"
  mapping-path: "~/.config/second-brain/tag-mapping.yml"
  proposals-path: "~/.config/second-brain/tag-proposals.yml"
  sweep-interval: "1h"
  proposal-threshold: 3
```

Note: `max-per-note` and `max-canonical` live in `canonical-tags.yml` (the shared vocabulary file), not in per-binary configs. Both borg and cortex read these values from the same source.

#### borg.yml additions

```yaml
tags:
  canonical-path: "~/.config/second-brain/canonical-tags.yml"
  mapping-path: "~/.config/second-brain/tag-mapping.yml"
  reject-concatenated: true
```

Note: `max-per-note` is not in borg.yml - it lives in `canonical-tags.yml` where both binaries read it.

### Implementation Plan

**Phase 1: Generate canonical set and mapping (manual + LLM)**
1. Create `second-brain/config/` directory in the repo
2. Extract all 4,760 tags alphabetically to a file
3. LLM proposes canonical set (~100-120 tags) organized by domain
4. Human reviews and approves canonical set
5. LLM generates full mapping (4,760 -> canonical or null)
6. Human reviews mapping
7. Both files committed to `second-brain/config/`

**Phase 2: Vault crate - shared loader and matcher**
1. New module `vault::canonical` (or extend `vault::tags` if it exists)
2. Implement `load_canonical_tags`, `match_to_canonical`, `is_concatenated_word`, `filter_and_cap`
3. Unit tests for matching logic, edge cases, concatenated word detection

**Phase 3: Borg post-filter**
1. Add `TagsConfig` to borg config with `canonical-path`, `mapping-path`, `reject-concatenated`
2. Insert canonical filter step in `pipeline.rs` after tag collection
3. Integration test: verify raw tags are mapped and capped

**Phase 4: Cortex sweeper**
1. Add sweep-related fields to cortex `TagsConfig`
2. Implement `cortex sweep --migrate` subcommand
3. Implement periodic sweep in daemon loop
4. Implement proposal queue logic
5. Tests for migration, sweep, proposal generation

**Phase 5: Migration**
1. Run `cortex sweep --migrate` on vault
2. Verify note tags are canonical and capped
3. Commit migrated vault state
4. Install updated borg and cortex, restart daemons

**Phase 6: Domain gap analysis (post-migration)**
1. Review proposal queue after a week of operation
2. Identify clusters in `resources` domain that warrant new domains
3. Separate design decision for domain additions

## Alternatives Considered

### Alternative 1: LLM-in-the-Loop Runtime (Approach 2)
- **Description:** After Fabric generates tags, a second LLM call maps raw tags to canonical set in real-time
- **Pros:** Better semantic accuracy, handles novel tags gracefully
- **Cons:** Extra LLM call per ingested note (cost, latency, Fabric availability dependency). Same PATH problem that broke Tier 2 could break this. More failure modes.
- **Why not chosen:** Deterministic post-filter is simpler and faster. Noted as future upgrade path if fuzzy matching proves insufficient.

### Alternative 2: Constrained Fabric Prompt (Approach B)
- **Description:** Modify the `create_tags` Fabric pattern to include the canonical list in the system prompt
- **Pros:** Tags are constrained at generation time, fewer wasted tokens
- **Cons:** Fabric patterns are shared tooling, customizing creates maintenance burden. Pattern would need updating whenever canonical list changes.
- **Why not chosen:** Post-filter achieves same result without coupling to Fabric pattern internals. Noted as appealing future project.

### Alternative 3: Embedding-Based Clustering
- **Description:** Generate embeddings for all tags, cluster semantically, use embedding similarity at runtime
- **Pros:** Best semantic accuracy, naturally handles synonyms
- **Cons:** Requires embedding infrastructure (model, vector store). Over-engineered for ~300 tags. Another dependency to break.
- **Why not chosen:** Overkill. The mapping file handles semantic equivalence as a one-time human-reviewed decision.

### Alternative 4: Strict Allowlist (No Proposal Queue)
- **Description:** Canonical list is fixed, only human can add tags, no LLM proposals
- **Pros:** Maximum control, simplest implementation
- **Cons:** List goes stale as interests evolve, no mechanism to surface vocabulary gaps
- **Why not chosen:** The proposal queue adds minimal complexity while keeping the system adaptive. Human remains the taste authority via approve/reject.

## Technical Considerations

### Dependencies
- No new external dependencies
- `vault` crate gains canonical tag loading (YAML parsing already available via serde)
- Both borg and cortex already depend on `vault`

### Performance
- Canonical list loading: cached in memory at startup, ~300 strings
- Fuzzy matching: segment split + hashset lookup, negligible cost per tag
- Migration: single pass over ~864 notes, dominated by frontmatter I/O
- Sweep daemon: periodic scan adds seconds to existing daemon cycle

### Testing Strategy
- Unit tests: `match_to_canonical` with exact matches, segment matches, no matches
- Unit tests: `is_concatenated_word` with known bad tags, valid compound tags
- Unit tests: `filter_and_cap` with various input sizes, verify cap enforcement
- Integration: borg pipeline produces only canonical tags
- Integration: `cortex sweep --migrate` correctly rewrites note frontmatter
- Migration dry-run: `cortex sweep --migrate --dry-run` to preview changes

### Rollout Plan
1. Commit canonical set and mapping files (Phase 1)
2. Deploy vault crate changes (Phase 2)
3. Deploy borg with post-filter (Phase 3) - new notes immediately constrained
4. Deploy cortex with sweeper (Phase 4)
5. Run migration on vault (Phase 5) - existing notes cleaned up
6. Monitor proposal queue for first week (Phase 6)

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Canonical set too small, drops meaningful signal | Med | Med | Start with ~120 tags, expand via proposal queue. Mapping file preserves provenance. |
| Fuzzy matching maps tags to wrong canonical | Low | Med | Segment-based matching is conservative. Full mapping reviewed by human before migration. |
| Concatenated word detection has false positives | Low | Low | Only rejects tags containing 2+ canonical substrings without hyphens. Tested against known good tags before deployment. |
| Proposal queue floods with noise | Low | Low | Threshold of 3+ notes filters one-off tags. Review is batch, not per-note. |
| Borg drops all tags for a note (nothing maps) | Low | Med | If canonical filter returns 0 tags, borg should log a warning. Cortex sweep catches tagless notes. |
| Migration corrupts frontmatter | Low | High | Vault is a git repo - migration is a single commit, trivially revertible. Dry-run mode validates first. |

## Open Questions

- [ ] How should `max-per-note` interact with manually added tags in Obsidian? Should cortex sweep enforce the cap on all notes or only borg-ingested ones?
- [ ] Should the proposal queue use an LLM to cluster proposals, or just surface raw frequency data for human judgment?

## References

- [2026-03-23 Classify Pipeline Fix](2026-03-23-classify-pipeline-fix.md) - segment-based tag matching (prerequisite)
- [2026-03-21 Cortex Classify & Promote](2026-03-21-cortex-classify-promote.md) - original classify pipeline design
- [vault/src/hygiene.rs](../../vault/src/hygiene.rs) - tag sanitization
- [cortex/src/tags.rs](../../cortex/src/tags.rs) - tag linting and normalization
- [cortex/src/classify.rs](../../cortex/src/classify.rs) - Tier 1 tag-domain classification
- [borg/src/pipeline.rs](../../borg/src/pipeline.rs) - tag collection pipeline
- [cortex/src/autotag.rs](../../cortex/src/autotag.rs) - existing auto-tag suggestion logic
