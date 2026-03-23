# Design Document: Domain Expansion - Add homelab, diy, life; Remove knowledge

**Author:** Scott Idler
**Date:** 2026-03-23
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

The `resources` domain has become a dumping ground for 154 notes spanning distinct interest areas (self-hosting, building/making, personal development) that deserve their own domains. Add three new domains (`homelab`, `diy`, `life`), remove `knowledge` (folded into `life`), add `value-renames` to the migration system, reclassify affected notes via LLM, and update all downstream artifacts.

## Problem Statement

### Background

The vault has 10 domains defined in `vault/src/schema.rs` as a Rust enum. The classify pipeline (Tier 1 deterministic + Tier 2 LLM via Fabric) assigns domains to incoming notes. The `resources` domain is defined as "books, general reference material, articles not fitting other categories" - effectively a catch-all.

### Problem

After fixing the classify pipeline (2026-03-23-classify-pipeline-fix.md), all 83 inbox notes were classified and promoted. But the domain distribution reveals `resources` is overloaded:

- **154 notes** in `resources` across distinct clusters: self-hosting/networking (~15-20), building/making (~10-15), personal development/culture (~15-20), and genuine reference material (~100)
- **`knowledge`** (28 notes) overlaps heavily with the personal development cluster in `resources` - health, motivation, habits, learning. The boundary between "knowledge" and "resources" is unclear.
- The LLM classifier has no good option for a Unifi doorbell install (not `tech` - that's software/dev), a walnut table build (not `tech`, not `knowledge`), or a midlife crisis article (not `knowledge` in the current narrow sense).

### Goals

- Add `homelab`, `diy`, `life` domains to the enum
- Remove `knowledge` (merge into `life`)
- Migrate all `domain: knowledge` notes to `domain: life`
- Reclassify `resources` notes that now fit the new domains via LLM
- Update all downstream artifacts: cortex.yml, tag_domain_map, Fabric pattern, Obsidian views, domain-values.md
- Add `value-renames` support to the migration system for reusable domain/field value migrations

### Non-Goals

- Making the Domain enum config-driven (it stays a Rust enum)
- Reclassifying notes in other domains (ai, tech, football, etc. are fine)
- Adding more than 3 new domains in this pass

### Coordination with Tag Sweeper

The tag sweeper (v0.5.14) landed while this design was being written. It introduced:
- `config/canonical-tags.yml` with domain-grouped tags (currently groups under `knowledge` and `resources`)
- `vault/src/canonical.rs` - shared tag loader/matcher
- `cortex/src/sweep.rs` - sweep command
- `cortex/src/config.rs` - `SweepConfig` struct
- `cortex/src/cli.rs` - `Sweep(SweepOpts)` command variant
- `cortex/src/testutil.rs` - SweepConfig in test builder

This design must update `canonical-tags.yml` domain groupings (rename `knowledge` -> `life`, add `homelab`/`diy`, redistribute tags like `fitness`/`home-automation` to new domains). The sweep/canonical code itself needs no changes - domain groupings in canonical-tags.yml are cosmetic (flattened to a single HashSet at load time).

## Proposed Solution

### Overview

Seven coordinated changes across the workspace:

1. **Enum change** - add `Homelab`, `Diy`, `Life` to `Domain`, remove `Knowledge`
2. **Migration system** - add `value-renames` support to `MigrationConfig`
3. **Cortex config** - new migration entry, updated schema.domains, updated tag_domain_map
4. **Classify triggers** - new trigger words for homelab, diy, life in `default_tag_domain_map()`
5. **Fabric patterns** - move `cortex_classify` into repo at `cortex/patterns/`, update both patterns with new domain descriptions
6. **Obsidian artifacts** - new Dataview views, update domain-values.md, remove domain-knowledge view
7. **Reclassify** - run migration for knowledge->life, then reclassify resources notes via LLM

### Architecture

#### Value-renames in MigrationConfig

Add a new migration primitive: `value-renames` maps field values within a specific field. This is distinct from `field-renames` (which renames the key).

```yaml
migrations:
  - name: v3-domain-expansion
    value-renames:
      domain:
        knowledge: life
```

This tells `apply_migrate` to find all notes where `domain: knowledge` and rewrite to `domain: life`. The implementation operates on raw frontmatter text (same as `field-renames`) to preserve formatting.

The `MigrationConfig` struct gains:

```rust
#[serde(rename = "value-renames", default)]
pub value_renames: HashMap<String, HashMap<String, String>>,
```

Where the outer key is the field name (`domain`) and inner map is `old_value -> new_value`.

#### Domain enum changes

`vault/src/schema.rs`:

```rust
pub enum Domain {
    Ai,
    Tech,
    Football,
    Work,
    Writing,
    Music,
    Spanish,
    Life,       // replaces Knowledge
    Homelab,    // new
    Diy,        // new
    Resources,
    System,
}
```

Total: 12 variants (was 10, +3 new, -1 removed). `Knowledge` is removed entirely from the enum since the migration handles the data side. The `FromStr` implementation keeps `"knowledge"` as an alias that maps to `Life` for backwards compatibility during the migration window.

#### Classify trigger updates

`cortex/src/classify.rs` `default_tag_domain_map()`:

| Domain | Triggers |
|--------|----------|
| life | `health`, `exercise`, `learning`, `vocabulary`, `productivity`, `motivation`, `fitness`, `psychology`, `mindset`, `habits` |
| homelab | `homelab`, `selfhosted`, `plex`, `unifi`, `pfsense`, `proxmox`, `nas`, `pihole`, `home-automation` |
| diy | `diy`, `woodworking`, `building`, `knots`, `construction`, `makeover`, `furniture`, `timber` |

The existing `knowledge` entry is renamed to `life` and expanded. Two new entries are added.

#### Fabric pattern update

`cortex_classify/system.md` gains three new domain descriptions and loses `knowledge`:

```
- "life" - Health, fitness, motivation, psychology, habits, personal development, culture, relationships, learning
- "homelab" - Self-hosting, home networking, Plex, NAS, Unifi, pfSense, Proxmox, Pi-hole, home automation hardware. NOT professional infra (Docker/k8s/networking for work goes in tech)
- "diy" - Building, woodworking, construction, knots, furniture, physical making, tools, crafts
```

The `resources` description narrows to: "Books, general reference material, articles that genuinely don't fit other categories. NOT a catch-all."

#### Obsidian artifacts

1. Create `system/views/domain-homelab.md`, `domain-diy.md`, `domain-life.md` (same template as existing views)
2. Remove `system/views/domain-knowledge.md`
3. Update `system/schemas/domain-values.md` with new domain table

#### Reclassification strategy

After the enum change and migration deploy:

1. **Deterministic migration**: `cortex migrate --apply` handles `knowledge -> life` via value-renames
2. **LLM reclassification**: Run `cortex classify --reclassify-domain resources --apply` - a new flag that re-runs the classify pipeline on all notes with a specific domain, using the updated Fabric pattern that now knows about homelab/diy/life. Notes that still classify as `resources` stay there.

The `--reclassify-domain` flag requires a new filter path. Currently, `filter_inbox_notes` (classify.rs:553) hard-filters to `inbox/` paths. The new flag adds a parallel filter function `filter_domain_notes` that selects notes in `notes/` matching a given domain value in frontmatter. It also sets `force: true` internally to bypass the `cortex-classified` check. The `ClassifyOpts` struct gains a `reclassify_domain: Option<String>` field, and `apply_classify` branches on whether to use `filter_inbox_notes` or `filter_domain_notes`.

Additionally, `vault/src/hygiene.rs` has legacy folder-to-domain mappings that reference "Knowledge" (lines 25-27). These must be updated to map to "life" instead.

### Implementation Plan

**Phase 1: Migration system - value-renames**
1. Add `value_renames: HashMap<String, HashMap<String, String>>` to `MigrationConfig`
2. Implement `lint_value_transforms` and `apply_value_transforms` in `migrate.rs`
3. Add tests for value-rename migration
4. `otto ci`

**Phase 2: Domain enum and classify triggers**
1. Update `Domain` enum in `vault/src/schema.rs` - add `Homelab`, `Diy`, `Life`, remove `Knowledge`
2. Update `FromStr`, `as_str`, `all()`, `Display` implementations
3. Add `"knowledge"` as backwards-compat alias in `FromStr` -> `Life`
4. Update `vault/src/hygiene.rs` legacy folder mappings: Knowledge -> life
5. Update `default_tag_domain_map()` in `cortex/src/classify.rs` - rename knowledge to life, add homelab and diy
6. Update cortex.yml: schema.domains list and migration entry `v3-domain-expansion`
7. Update `cortex/src/frontmatter.rs` if it references knowledge domain in validation
8. Add `--reclassify-domain` flag to classify CLI and implement `filter_domain_notes`
9. `otto ci`

**Phase 3: Patterns, config, and Obsidian artifacts**
1. Move `cortex_classify` pattern into the repo at `cortex/patterns/cortex_classify.md` (matching borg's convention where `borg/patterns/` is the source of truth, installed to `~/.config/borg/patterns/`). Install step copies to `~/.config/fabric/patterns/cortex_classify/system.md`. Update the pattern with new domain descriptions.
2. Update `borg/patterns/obsidian-classify.md` with the same domain list
3. Update `config/canonical-tags.yml` domain groupings:
   - Rename `knowledge:` group to `life:` (keep obsidian/note-taking/pkm tags - they're cosmetic groupings)
   - Add `homelab:` group with `home-automation` (moved from resources). Docker/kubernetes/networking stay in `tech` - those are professional SRE/platform tools, not homelab topics.
   - Add `diy:` group (currently no canonical tags fit, but the group exists for future additions)
   - Move `fitness`, `exercise`, `nutrition`, `health` from `resources:` to `life:`
   - `gaming`, `noita` stay in `resources:` (no gaming domain)
4. Create Dataview views: `domain-homelab.md`, `domain-diy.md`, `domain-life.md`
5. Remove `domain-knowledge.md`
6. Update `domain-values.md`

**Phase 4: Deploy and reclassify**
1. Build and install cortex: `cargo install --path cortex`
2. Restart daemon: `systemctl --user restart cortex`
3. Run migration: `cortex migrate --apply` (knowledge -> life)
4. Run reclassification: `cortex classify --reclassify-domain resources --apply`
5. Verify domain distribution: check resources count has decreased
6. Commit vault changes

## Alternatives Considered

### Alternative 1: Config-driven domains (no enum)
- **Description:** Replace `Domain` enum with string validation against cortex.yml list
- **Pros:** Adding domains becomes a config change, no recompilation
- **Cons:** Loses compile-time type safety. Oracle MCP tool schemas use `schemars::JsonSchema` derive on the enum - would need manual schema generation. Every match arm in the codebase becomes a string comparison.
- **Why not chosen:** The enum works fine. Domain changes are infrequent (this is the first expansion). The ceremony of changing the enum is ~30 minutes of mechanical work, not a real bottleneck.

### Alternative 2: Keep knowledge, add homelab/diy only
- **Description:** Leave `knowledge` as-is, don't add `life`
- **Pros:** Smaller change, no migration needed for knowledge notes
- **Cons:** `knowledge` is poorly defined ("health, learning, English vocabulary") and overlaps with what `life` would cover. Having both `knowledge` and `life` would create the same ambiguity we're trying to fix.
- **Why not chosen:** Merging knowledge into life is cleaner. The boundary between knowledge and the personal-development cluster in resources is artificial.

### Alternative 3: Manual reclassification only
- **Description:** Add the new domains but don't reclassify existing resources notes. Let the daemon handle them as they're modified.
- **Pros:** No reclassification risk, simpler rollout
- **Cons:** 154 notes stay miscategorized indefinitely. The daemon only runs classify on inbox notes, not notes already in `notes/`.
- **Why not chosen:** The `--reclassify-domain` flag is straightforward to implement and the vault is version-controlled, so reclassification is safe and revertible.

## Technical Considerations

### Dependencies
- No new external dependencies
- `vault` crate enum change propagates to all three binaries (borg, cortex, oracle)

### Performance
- Value-rename migration: single pass over ~28 knowledge notes, negligible
- LLM reclassification of ~154 resources notes: ~3-5s per note via Fabric, total ~8-12 minutes. One-time cost.

### Testing Strategy
- Unit tests: value-rename migration (apply + lint)
- Unit tests: Domain enum FromStr with backwards-compat "knowledge" alias
- Unit tests: new tag_domain_map entries for homelab, diy, life
- Integration: migrate knowledge->life on test vault, verify frontmatter rewritten
- Integration: reclassify resources notes, verify some move to new domains

### Rollout Plan
1. Deploy code changes (Phases 1-2) - new enum, migration system, classify flags
2. Deploy Fabric pattern and Obsidian artifacts (Phase 3)
3. Run migration and reclassification (Phase 4)
4. Monitor domain distribution for a week

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| LLM reclassifies resources notes incorrectly | Med | Low | Vault is git-tracked. Review diff before committing. `cortex-classified-by` field enables audit. |
| Removing Knowledge variant breaks oracle MCP schema | Low | Med | Oracle regenerates schema from enum at startup. Clients that cache old schema will get an error on "knowledge" - acceptable for a personal vault. |
| Backwards-compat alias "knowledge" causes confusion | Low | Low | Alias is only in FromStr for migration window. Remove after vault is fully migrated. |
| Tag sweeper design doc references current domain list | Low | Low | Tag sweeper's canonical-tags.yml domain groupings are cosmetic. Update when implementing tag sweeper. |
| Borg pattern not updated, new notes still classify with old domains | Med | Med | Both patterns (cortex_classify AND obsidian-classify.md) must be updated in Phase 3. Install step copies borg pattern. |

## Open Questions
- [ ] Should the `--reclassify-domain` flag be general-purpose (any domain) or specific to this migration?
- [ ] Should reclassified notes retain their old `cortex-classified` metadata or get fresh timestamps?

## References
- [2026-03-23 Classify Pipeline Fix](2026-03-23-classify-pipeline-fix.md) - prerequisite, fixed the classify pipeline
- [2026-03-23 Tag Sweeper](2026-03-23-tag-sweeper.md) - upcoming tag consolidation, references domains
- [vault/src/schema.rs](../../vault/src/schema.rs) - Domain enum definition
- [cortex/src/classify.rs](../../cortex/src/classify.rs) - classify pipeline
- [cortex/src/migrate.rs](../../cortex/src/migrate.rs) - migration system
- [cortex_classify/system.md](~/.config/fabric/patterns/cortex_classify/system.md) - Fabric classify pattern
- [domain-values.md](~/repos/scottidler/obsidian/system/schemas/domain-values.md) - Obsidian domain reference
