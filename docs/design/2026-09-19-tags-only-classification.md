# Design Document: Tags-Only Classification (drop `domain`)

**Author:** Scott Idler (via agent)
**Date:** 2026-09-19
**Status:** Implemented (2026-09-20: five passes, **four** panel rounds folded, every acceptance criterion run on `main` a1415bc with output recorded, Open Questions closed. Round 4 was ordered by Scott after an independent due-diligence pass produced new measurements; it corrected four numbers in that pass and closed OQ5 on option A. 2026-09-20, during execution: the `CLASSIFY_API_KEY` secret was re-encrypted and verified live on the Pro tier, closing the two classifier-access risk rows. All 11 phases implemented; Phase 11 closed the design with AC1/AC2/AC4 all passing on the fully-migrated tree, see implementation notes)
**Review Passes Completed:** 5/5 + an independent due-diligence pass on 2026-09-20 (Addendum B: every load-bearing vault number re-derived, the classifier exercised on real notes, three new risks and one schema correction folded in; P0b added as a gate on P1)
**Original pass line:** 5/5 + panel r2 and r3 folded (r1 ran both seats against a 162-line pre-draft and was stopped before synthesis; superseded. r2: 11 must-fix, 9 corrections. r3: 6 must-fix, 3 corrections; the staff seat timed out both rounds and its findings were mined from its trace and re-verified. Every finding verified on `main` a1415bc before folding; run dir `/tmp/review-panel/MLUDM7sw/`. Round cap of 3 reached; a further round needs `PANEL_ROUNDS_ORDERED_BY_SCOTT`). Pass 2: phase numbers re-aligned; every acceptance criterion run on `main` a1415bc and the live vault with output recorded; FK clause dropped; HTTP client pinned to `ureq`. Pass 3: candidate provenance (`Author` vs `Model`); classifier-error behavior split between ingest and cortex; `--retag` never writes empty. Pass 4: block-form writer switch moved into the migration phase; daemon stopped around both `migrate --apply` steps; segment-match guard; sync-conflict check. Pass 5: voice lint clean.

## Summary

Remove the single-valued `domain` frontmatter field from the vault and from every sb surface that reads or writes it. `tags` (multi-valued, governed by `config/canonical-tags.yml`) becomes the only classification axis, gets a schema (required, canonical, capped, one on-disk form), and becomes a first-class query facet. Tag assignment at borg ingest moves from free-text Fabric output plus fuzzy post-filter to a closed-vocabulary classifier shared with cortex, so the same input yields the same tag list, and reingest merges instead of regenerating. Twelve phases (P0-P11; P0 is a zero-code spike already run, and P0b is a zero-code gate on P1), one commit each, `otto ci` green. No **code** that reads or writes `domain` is deleted before P11; P1-P9 only add, P10 clears the vault-side and config artifacts whose only reader is that code, P11 deletes the code and the frontmatter key. Every phase names what its `git revert` leaves behind and in which store.

## Problem Statement

### Background

- `domain` is a 12-variant Rust enum (`vault/src/schema.rs:21-33`), one value per note, written only by cortex classify: Tier 1 tag-trigger map, Tier 1b source match, Tier 2 LLM via Fabric `obsidian-classify`, Tier 3 hold (`cortex/src/classify.rs:653-676`). Borg writes no domain (2026-03-21 classify-promote).
- `tags` is a governed multi-valued field: 110 canonical tags in `config/canonical-tags.yml` (`max-per-note: 7`, `max-canonical: 300`), `tag-mapping.yml` (4,547 raw -> canonical | null entries), `tag-proposals.yml`, `cortex sweep` consolidation, `vault::canonical` matcher shared by borg and cortex (2026-03-23 tag-sweeper).
- `canonical-tags.yml` is keyed by the 12 domain names. `CanonicalTagsFile::all_tags()` flattens `values().flatten()` (`vault/src/canonical.rs:33-35`); nothing reads the keys. The grouping is editorial.
- Vault, measured 2026-09-20 on `~/repos/scottidler/obsidian`: 2,750 lines match `^domain:` (2,748 inside a parseable opening frontmatter block; the other 2 are prose in the vault `CLAUDE.md` and `system/templates/frontmatter.md`), across notes 2,628, system 76, work 42, root 2, journal 1, entities 0. Distribution: ai 1148, tech 840, football 301, work 99, life 84, homelab 47, resources 36, writing 20, music 18, diy 18, spanish 17. In `notes/`: 2,677 with frontmatter; 491 at exactly 7 tags; 376 with no tags (166 `tags: []` inline, 210 bare `tags:` with no items); 88 with no `tags` key; 151 already carrying their domain value as a tag; 1,357 inline lists, 1,126 block lists. `entities/` 915 files all `tags: []`; `work/` 42 all `tags: []`.
- Oracle index (`~/.local/share/sb/oracle/oracle.db`, read-only, 2026-09-20): 3,742 indexed notes; 9,852 `(note, tag)` pairs over 108 distinct tags; 303 rows hold `tags = ''` (empty string, not valid JSON).
- Classification provenance: `cortex-classified: 2246`; `cortex-classified-by: deterministic 1189, llm 1057`; `cortex-confidence: high 1974, medium 272`. `cortex-tagged` / `cortex-suggested-tags`: 0 notes.

### Problem

`domain` is a second classification axis encoding a subset of what `tags` encodes, with a forced-single-value constraint the corpus does not fit, and `tags` is not yet a system of record.

- **The junk drawer recurs.** 2026-03-23 domain-expansion fixed `resources` as a 154-note dumping ground by adding `homelab`, `diy`, `life`. Six months later `resources` held 45 notes, 2 matching its own definition (`hugo-awards-best-novel`, `nebula-awards-best-novel`); the vault index `home.md` sat in it at `cortex-confidence: high`. Single-label needs a residue bucket; the residue bucket refills.
- **Every domain candidate is already a tag.** A 16-candidate classifier sweep (2026-09-19, handoff `handoff-vault-domains.md`) surfaced security, pkm, hardware, gaming, fitness, science, networking as missing domains. All seven exist in `canonical-tags.yml` (`security` at 449 vault uses). `entertainment` and `politics` are missing as tags too.
- **Forced single-label caps agreement.** Back-tests against frontmatter: 80.9% bare, 84.1% with canonical definitions, no gain from exemplars. `work` agrees 18-22% because its definition ("platform engineering, leadership") collides with `tech` ("DevOps, Kubernetes, infrastructure"). Calibration is the usable part: >=0.9 confidence agrees 94.7%.
- **Tier 1 already derives domain from tags.** `default_tag_domain_map` (`cortex/src/classify.rs:131-262`) decides 1,189 of 2,246 classified notes (53%) from tags alone. A stored field derived from another field is the pattern `rules/taste.md` says to drop, not sync.
- **The daemon actively strips the tag `ai`.** `~/.config/sb/cortex.yml:64-66` `actions.tags.aliases` maps `ai`, `ML`, `ml` -> `ai-llm`; `apply_tags` (`cortex/src/tags.rs:103-107`) rewrites any aliased tag; `config/tag-mapping.yml:204` then maps `ai-llm -> llm`; the daemon runs the tags rule with `apply: auto` (`cortex/src/daemon.rs:628`) and `sweep` each tick. Result today: **1** note in the vault carries the tag `ai` against 1,148 with `domain: ai`. Any plan that writes `ai` as a tag without retiring that alias loses it on the next tick.
- **The graph treats domain as a degenerate tag and mostly ignores it.** `shared-tag` is rarity-weighted with blanket suppression; `shared-domain` is fixed weight 0.1 under the same `fanout_cap` 100 (`cortex/src/graph.rs:42-45,300-362`; `cortex/src/config.rs:150-156,208-211`). ai, tech, football exceed the cap, so `shared-domain` edges exist only for the 8 small domains.
- **Retrieval never weights domain.** It is a pass-through filter: SQL `AND n.domain = ?` in `search`/`list_notes`/`recent_notes` (`vault/src/search/query.rs:5-28,92-113,182-198`) and vector search (`vault/src/search/vector.rs:191-226`), path post-filter in the graph retriever (`oracle/src/server/pipeline.rs:49-90`). The eval set has 9 queries, 0 with a domain filter (`config/eval/queries.yml`).
- **Tags are searchable text, not a facet.** `notes.tags TEXT` holds a JSON array fed to FTS5 (`vault/src/search/schema.rs:16,474-522`; `vault/src/search/index.rs:119-125`). No `note_tags` table, no per-tag index, no `tags` filter parameter anywhere. `tag_search` scans every row and filters in Rust (`vault/src/search/stats.rs:276-322`).
- **Tags are regenerated on every reingest; domain is preserved.** `CORTEX_PRESERVE_KEYS` (`vault/src/schema.rs:8-16`) lists `domain` and the `cortex-*` keys, not `tags`. The preserve parser is scalar `line.strip_prefix("{key}:")` (`borg/src/pipeline/publish.rs:124-160`); `apply_cortex_fields` silently `continue`s past any key not in the list (`borg/src/pipeline/atomic.rs:224-227`). On the session-replace path `tags` is in borg-owned `RENDER_NOTE_KEYS` and is overwritten (`borg/src/pipeline/session.rs:104-118`). The two paths also fail differently: a preserve read error yields an empty set on the URL path (`publish.rs:137-140`) and fails closed on the session path (`session.rs:150-165`).
- **Tag generation at ingest is not repeatable.** Raw tags come from distiller JSON (LLM, capped 7 in LLM order, `distillers/src/validate.rs:68-75`), YouTube hashtags, and fabric `create_tags` free text (`borg/src/fabric.rs:341-347`; the pattern is not deployed under `~/.config/sb/patterns/`, so fabric's stock pattern runs). `vault::fabric::run_pattern` passes no temperature or seed (`vault/src/fabric.rs:115-140`). `merge_proposed_tags` (`borg/src/pipeline.rs:138-144`) flattens all sources into one `Vec<String>` before `finalize_tags` (`borg/src/pipeline/tags.rs:47-62`); everything after `sanitize_tag` is a pure function of that set, so drift shows as which 7 survive the cap.
- **Tags have no schema, and the cap is not enforced end to end.** Lint checks format and a legacy 11-entry `actions.tags.canonical` list in `cortex.yml:50-63`, not `canonical-tags.yml` (`cortex/src/tags.rs:49-56`); `sb cortex lint` on 2026-09-20 reports 9,020 `tags.non-canonical`, 300 `frontmatter.required.tags`, 3 `frontmatter.required.domain`, 2 `tags.orphan`. No cap lint. `tags: []` satisfies `frontmatter.required`. Three writers, two forms: borg `render_note` (`borg/src/markdown.rs:221`) and `Frontmatter::to_yaml` (`vault/src/frontmatter.rs:365`) write block lists; cortex `replace_tags_in_frontmatter` writes inline (`cortex/src/tags.rs:199-201`). Session notes get `scope-work` | `scope-personal` and `redacted-source` pushed **after** `finalize_tags` (`borg/src/pipeline/session.rs:442-450`), outside the vocabulary and outside the cap, by design of the harvest doc.
- **Cortex auto-tag is dead code.** `autotag.rs` writes `cortex-suggested-tags` + `cortex-tagged`, never `tags:` (`cortex/src/autotag.rs:72-77,171-182`); nothing consumes the key; the daemon runs it lint-only (`auto-tag: {}` -> `enable` false, `cortex/src/daemon.rs:751-776`); 0 notes carry either key.
- **The ledger's Domain column is dead.** 3,234 of 3,302 rows are `-`; every borg writer passes `domain: None` (`borg/src/pipeline/publish.rs:58`; `borg/src/pipeline.rs:1096`; `borg/src/harvest.rs:336`).

### Goals

Each goal names who asked and when.

- **G1** Drop `domain` everywhere: enum, frontmatter, index column, filters, stats, ledger, graph edge, config keying, schema docs, Obsidian views, AGENTS.md. Use `tags` in every place `domain` was used. (Scott, 2026-09-19: "drop domain, use tags (plural) instead in every place that domain was used and maybe some more")
- **G2** Full scope in this doc: every sb surface (vault, borg, cortex, oracle, `sb` CLI, distillers, `config/`, vault-side artifacts). Nothing deferred to a later doc. (Scott, 2026-09-19: "this design doc needs to handle all of the sb surfaces, do not defer work for further design docs. scope it all here, now.")
- **G3** Step-by-step and undoable: additive phases first, one commit each, `otto ci` green, `domain` live, populated, and read by code until the final deletion phase; each phase states its revert residue per store. (Scott, 2026-09-19: "sequence it so we can do it step by step and so we can undo it if need be")
- **G4** Tags get a schema: required where domain was required, every value canonical, `1 <= n <= max-per-note`, one on-disk list form, preserved across reingest. (Scott, 2026-09-19: "we need to fix the schema for tags")
- **G5** ~~Repeatable~~ **stable** tag assignment at borg ingest through the same classifier cortex uses; where drift remains, a stated reingest merge policy. Restated 2026-09-20 on measured evidence: `classifier-dev` is 23% unstable at the tag-set level between byte-identical calls (P0b), so per-call repeatability is not achievable and the union-on-reingest policy is what delivers stability. `Deterministic` is byte-repeatable **as a function**, not as an ingest: `distilled.tags` (LLM output) merges into `all_tags` before `finalize_tags` (`borg/src/pipeline.rs:862`, `borg/src/pipeline/session.rs:439`), so distiller drift still flows in. P6 therefore carries two criteria, one on the frozen fixture and one that varies the distiller output (panel r4 OQ7). (Scott, 2026-09-19: "make the creation of them during borg ingest in such a way that it is repeatable. perhaps we use this classify method there too ... if not, maybe we have to deal with some non-determinism during reingest")
- **G6** Tags become a query facet: per-tag index, `tags` filter wherever `domain` was one, tag-keyed stats and brief. (From G1: "in every place that domain was used" includes every filter surface listed under API Design.)
- **G7** Retire `resources`: its 36 remaining notes are re-tagged into the vocabulary (gaming, entertainment, politics, science, pkm, and existing tags); `entertainment` and `politics` are added to `canonical-tags.yml`. (Scott, 2026-09-19: "gaming, entertainment, politics, science, pkm and misc and system seem like a good fix for resources", carried into tags-only.)

### Non-Goals

- Adding new domains. Superseded by G1.
- Changing retrieval methodology (vector-first default, RRF, rerank stages, graph expansion). Only the filter and stats surfaces change.
- Changing `NoteType`, `Origin`, `Status`, `Method` enums or their generated schema docs.
- Rewriting hand-written MOC or note prose. Domain-keyed `.base` views and templates are replaced; bodies are not edited.
- Changing how tag proposals are reviewed (`tag-proposals.yml` stays a human queue).
- Labeled-corpus validation of the 0.9 multi-label threshold. 0.9 is the operating value from the single-label calibration back-test and the Phase 0 spike; a precision/recall study over a labeled set is not a gate for this doc.
- URL-host "domain" code (`borg/src/blocklist.rs`, `stages/{alert,raw,artifact}.rs`, `types.rs:250,273`, `description.rs:118`, `config.rs:122,128`, `harvest.rs`, `harvest/select.rs` (both construct the same `RejectionRecord.domain` URL-host field), `sb/src/cli/borg.rs`'s Gate-0 blocklist subcommand, the blocklist string at `borg/src/migrate.rs:472`, `cortex/src/quality.rs:26`). Different meaning of the word; untouched. **Correction (P11 execution, 2026-09-20):** the original line here also named `lib.rs:731-748` / `sb borg reingest --domain` as untouched URL-host code; that was wrong. `borg::reingest`'s `domain` parameter and the `EntryFilter`/`LedgerEntry.domain` fields it fed are the vault-classification meaning (verified: `EntryFilter { domain: domain.clone(), .. }` filters the ledger's now-deleted Domain column), so it was in scope and P11 deleted the `--domain` flag from `sb borg reingest` and the `domain` param from `borg::reingest` entirely, with no `--tags` replacement (the ledger file carries no tags column to filter on).

## Proposed Solution

### Overview

Three moves, sequenced so the second and third are additive while `domain` still works:

1. **Make `tags` carry what `domain` carried, safely.** Retire the aliases that would strip the new tags, add the domain names to the canonical vocabulary pinned to mapping tier 0, make reingest merge tags instead of overwriting them, and only then write each note's domain value onto its tags with a new reversible migration transform. Build the `note_tags` facet and thread a `tags` filter beside every `domain` filter.
2. **Make tag assignment one repeatable mechanism.** A `TagClassifier` trait in `distillers` with three implementations (deterministic canonical mapping, Fabric closed-vocabulary prompt, classifier.dev multi-label), selected by config, called by borg `finalize_tags` and by cortex classify. Promotion from `inbox/` to `notes/` gates on tags, not on a domain.
3. **Delete `domain`, all at once, last.** P8-P9 add the tags siblings; P10 replaces the vault-side views, templates, and config entries; P11 removes every domain code surface and the frontmatter key in one commit, with the obsidian commit taken immediately before as the authoritative frontmatter undo.

### Architecture

```
canonical-tags.yml  (+tech work life homelab diy; +entertainment politics; no-segment-match: [...]; max-per-note)
tag-mapping.yml     (domain names self-mapped -> tier 0; resources/system stay null)
cortex.yml aliases  (ai/ML/ml -> ai-llm RETIRED in P1)
        |
   vault::canonical  CanonicalSet { all, no_segment }  <- shared home for borg's CanonicalState
                     match_to_canonical + filter_and_cap both honor no_segment
        |
   distillers::tags::TagClassifier          <- NEW shared seam
     Deterministic       candidates -> match_to_canonical -> filter_and_cap (no LLM)
     FabricClosedVocab   prompt embeds the vocabulary, JSON {tags:[..]}, post-filtered
     ClassifierDev       multi-label over the sorted vocabulary, sharded <=100 labels/call,
                         threshold (default 0.9), cap max-per-note
     FakeTagClassifier   test double
      /                          \
   borg finalize_tags          cortex classify_note / --retag
   (ingest, 8 call sites)      (inbox promotion, catch-up, retag = replace)
        |                                |
   render_note (block list)      replace_tags_in_frontmatter (block list)
        |
   reingest: read_cortex_fields (list-aware) -> apply_cortex_fields
             tags = union(preserved, fresh)[..max-per-note]; session path same; fail closed
        |
   vault::search index.rs -> notes.tags JSON + note_tags(path, tag)  <- NEW facet
        |
   query.rs / vector.rs / graph post-filter / stats.rs: tags: Option<&[String]>
        |
   oracle tools (tags beside every domain param through P10; domain removed in P11)
   sb CLI, doctor, schema docs (tag-values.md), .base views, templates
```

**Seam placement.** `distillers` is the crate both `borg` and `cortex` already depend on (`*/Cargo.toml`), and it already owns `FabricCaller` + `FakeFabric` (`distillers/src/fabric.rs:26-32`). `vault` stays LLM-free (`oracle/AGENTS.md:42`); `oracle` does not depend on `distillers` and does not need the classifier. Both panel seats confirmed this placement. `CanonicalState` (today borg-private, `borg/src/pipeline.rs:46`) moves to `vault::canonical` as `CanonicalSet` so borg, cortex, and distillers share one loaded vocabulary.

**Why `domain` values become tags rather than being discarded.** They are information Scott curated (2,246 classified notes). Writing them as tags makes the migration reversible, keeps every `.base` view answerable during the transition, and lets the graph treat `tech` (df 840) the way it already treats any blanket tag: routed through a hub or skipped by `fanout_cap`.

**Why `resources` and `system` are not propagated.** `resources` is the junk drawer this doc retires (G7); `resources: null` in `tag-mapping.yml` stays. `system` marks the `system/` folder, a path fact; `system: null` stays and `system/**` joins the tags exemptions.

**Why governance signals leave `tags`.** `scope-work` | `scope-personal` and `redacted-source` are facts about a harvest note's provenance, not interests; they cannot be canonical without polluting the vocabulary and they break the cap on every session note as written. They become frontmatter keys (`scope`, `redacted`), which amends the 2026-07-17 harvest doc's wording "scope-tagged" (Open Question 4). No back-migration is needed: measured 2026-09-20, zero notes in `notes/` or `work/` carry any of the three tags (the daemon's `sweep` strips them as non-canonical on the next tick, which is the same failure this doc fixes); the only file mentioning them is `entities/scope-work.md`, an entity note about the tag.

### Data Model

**Frontmatter (after P11).**

```yaml
---
title: "..."
date: 2026-09-19
type: session
origin: assisted
tags:
  - ai
  - claude
  - rust
scope: work                # session notes only; was the scope-work | scope-personal tag
redacted: true             # session notes only, when any member had redactions; was the redacted-source tag
author-tags:               # ingest only, when the publisher supplied hashtags or yt-dlp tags that map to canonical tags
  - rust
status: unread
cortex-classified: true
cortex-classified-by: classifier-dev      # deterministic | fabric-closed | classifier-dev | llm (legacy value, left as is)
cortex-confidence: high
---
```

Schema for `tags` (G4), enforced by cortex lint and written identically by all three writers:

| rule | value | enforced at |
|---|---|---|
| required | `notes/**`, `work/**`; exempt `entities/**`, `inbox/**`, `system/**`, `notes/ai/**`, `daily` | `frontmatter.required.tags` treats `[]` and bare `tags:` as missing |

`notes/ai/**` is on that list for the same reason `domain` is: `cortex.yml:34` already reads `"notes/ai/**": [domain, origin]`, and the 50 files under it are generated `type: digest` / `type: review` rollups with no `domain` key and no tags. The earlier wording ("1:1 with today's domain exemptions") was wrong in both directions: it omitted `notes/ai/**`, which today's domain rule does exempt, and added `system/**`, which it does not. `system/**` stays on the list on its own merits (Addendum B).
| membership | every value in `CanonicalSet.all` | `tags.non-canonical` reads `canonical-tags.yml`, not `cortex.yml:50-63` |
| count | `1 <= n <= max-per-note` | new `tags.cap` rule |
| form | block list (`- tag` lines), lowercase-hyphenated | `tags.format` extended; readers accept inline and block |
| preservation | survives URL reingest and session replace by union; a preserve read failure fails closed on both paths | `borg/src/pipeline/atomic.rs`, `session.rs` |
| protected | the tags in `no-classifier-tags` (`work`, `life`, `homelab`, `diy`, `writing`) are never removed by `--retag`; the classifier recovers 0 of 17 of them (P0b) | `--retag` in `distillers::tags` |
| ownership | `tags` is machine-managed, with `no-classifier-tags` and `author-tags` as the two curated exceptions. `[]` means "not yet classified" and is repopulated by the next classification. Human curation happens through `canonical-tags.yml`, `tag-mapping.yml`, and `--retag`, not by editing a note's list (Open Question 3) | policy, stated here and in `vault/AGENTS.md` |

Block list is chosen because Obsidian's property editor writes block lists, and two of three writers already do (`borg/src/markdown.rs:221`, `vault/src/frontmatter.rs:365`); only `cortex/src/tags.rs:199-201` changes, in P4, before the migration writes. The P3 preserve parser is list-aware regardless.

**Canonical vocabulary (P1).** `config/canonical-tags.yml` gains `tech work life homelab diy` in a `domains:` group and `entertainment`, `politics` under `life`: 117 tags. A new top-level key `no-segment-match: [tech, work, life, homelab, diy]` lists tags that tier-2 segment splitting may never produce, so `work-life-balance` does not mint `work` and `life`. The list is top-level because `tags: HashMap<String, Vec<String>>` (`vault/src/canonical.rs:13`) has no room for per-group flags and `filter_and_cap` takes a `&HashSet<String>` (`:126`) that cannot carry group metadata; this is a small, honest API change:

```rust
pub struct CanonicalSet { pub all: HashSet<String>, pub no_segment: HashSet<String>, pub max_per_note: usize }
pub fn match_to_canonical(raw: &str, canon: &CanonicalSet, mapping: &TagMapping) -> Vec<String>;
pub fn filter_and_cap(raw: &[String], canon: &CanonicalSet, mapping: &TagMapping) -> Vec<String>;
```

Both functions honor `no_segment`: `match_to_canonical` at its tier-2 loop (`canonical.rs:100-109`) and `filter_and_cap` at its own duplicate of that loop (`:149-158`), which is the copy borg and sweep call. `config/tag-mapping.yml`: remove `tech: null`, `diy: null`, `homelab: null`; add self-maps `ai: ai`, `tech: tech`, `work: work`, `life: life`, `homelab: homelab`, `diy: diy`, `football: football`, `writing: writing`, `music: music`, `spanish: spanish` (tier 0, never the alphabetical casualty of the cap). `resources: null` and `system: null` stay. `~/.config/sb/cortex.yml` `actions.tags.aliases`: delete `ai: ai-llm`, `ML: ai-llm`, `ml: ai-llm` (dotfiles); `ai-llm: llm` in the mapping file stays for raw `ai-llm` input.

**Search index (P5).**

```sql
CREATE TABLE IF NOT EXISTS note_tags (
  path TEXT NOT NULL,
  tag  TEXT NOT NULL,
  PRIMARY KEY (path, tag)
);
CREATE INDEX IF NOT EXISTS idx_note_tags_tag ON note_tags(tag);
```

No foreign key, even though `open` and `open_memory` both set `PRAGMA foreign_keys=ON` (`vault/src/search.rs:416,428`): `index_one` maintains the facet explicitly inside its savepoint, so a declared FK would only duplicate that maintenance. Rows are maintained explicitly in `index.rs` inside one `SAVEPOINT` per note around the `notes` upsert and the `note_tags` delete-then-insert (`index_changed`, `vault/src/search/index.rs:332`, has no enclosing transaction today), and deleted with the note in `remove_stale_notes`. `notes.tags` JSON stays for FTS and for `NoteRow`; the 303 rows holding `tags = ''` are normalized to `[]` at index time. Backfill is `sb oracle index --force` (`sb/src/cli/oracle.rs:26,91`); plain `sb oracle index` skips unchanged files (`index.rs:48`) and would leave the table empty. **`notes.domain` and `idx_notes_domain` stay in the schema after P11, unpopulated**: the previous binary runs `CREATE INDEX IF NOT EXISTS idx_notes_domain ON notes(domain)` at open (`search/schema.rs:49`), so dropping the column would make a code revert fail before reindex, and the same file holds `note_embeddings`, which a delete-and-rebuild would cost.

**Migration config (P4).** `MigrationConfig` gains two transforms beside `field-renames` / `field-drops` / `value-renames` (`cortex/src/config.rs:972-990`), and `sb cortex migrate` gains `--only <name>`; `apply_migrate` today runs every configured migration (`cortex/src/migrate.rs:68-83`) and `--plan` is parsed (`cortex/src/opts.rs:150-151`) but unused, so an inverse in `cortex.yml` would run:

```yaml
# ~/.config/sb/cortex.yml (dotfiles)
migrations:
  - name: v5-domain-as-tag
    field-to-tags:
      domain:
        exclude: [resources, system]
  - name: v6-drop-domain             # P11
    field-drops: [domain]
```

```yaml
# config/migrations/v5-domain-as-tag-undo.yml, run only by hand: sb cortex migrate --plan <file> --apply
- name: v5-domain-as-tag-undo
  tags-remove: [ai, tech, football, work, writing, music, spanish, life, homelab, diy]
```

`field-to-tags` reads the scalar, strips quotes and whitespace (350 files quote the value), normalizes through `hygiene::normalize_domain` (`knowledge` -> `life`), creates the `tags` key when absent (88 notes in `notes/`; all 42 in `work/` have `tags: []`), appends the value if absent, and rewrites the block in canonical block form for every file it visits, idempotent. Its dry-run lists notes that would exceed `max-per-note` and notes that already carried two or more domain-name tags before the run. `tags-remove` removes the named tags wherever present; it cannot tell a migrated `football` from a pre-existing one (125 notes carry `football` today), which is why the obsidian commit, not the inverse, is the authoritative undo.

**Classifier config (P6, P7).** Same key in `borg.yml` and `cortex.yml`, parsed by one struct in `distillers`:

```yaml
tags:
  classifier:                       # the whole block is `TagsClassifierConfig`
    classifier: classifier-dev      # deterministic | fabric-closed | classifier-dev
    threshold: 0.9
    api-key-env: CLASSIFY_API_KEY   # classifier-dev only; value via env-bootstrap
    fallback: deterministic         # when the selected impl errors: borg runs this and marks the receipt degraded;
                                    # cortex runs this over the note's existing tags and author-tags, and holds only if it returns Low
    endpoint: https://classifier.dev/v1/classify
    timeout-secs: 30
```

The nesting is load-bearing: `tags.classifier` is the struct, not a scalar. Borg's `tags:` block also carries `canonical-path` / `mapping-path` / `reject-concatenated`, so the classifier's six keys live one level down. Amended 2026-09-20 during the implementation audit, which ran the flat shape this block originally showed against the built binary:

```
Error: failed to load configuration
Caused by: tags.classifier: invalid type: string "classifier-dev",
           expected struct TagsClassifierConfig at line 254 column 15
```

**The shipped default is `deterministic`, not `classifier-dev`** (`TagsClassifierConfig::default()`). A machine bootstrapped without this block classifies locally and never calls out; `classifier-dev` is opt-in per host because it needs both network and a key. The deployed `borg.yml` / `cortex.yml` set it explicitly, as above.

**Confidence mapping (drives promotion and `cortex-confidence`).** Each method reports `High | Medium | Low` the way Tier 1/2 do today; promotion happens on `High` or `Medium`, hold on `Low` (today's rule):

| method | High | Medium | Low |
|---|---|---|---|
| `deterministic` | any tier-0 or tier-1 hit | only tier-2 (segment) hits | no hit |
| `fabric-closed` | never (unscored) | one or more canonical tags returned | none returned |
| `classifier-dev` | one or more tags at >= `threshold` (these are the selected tags) | never | none at >= `threshold` |

`classifier-dev` has no Medium band on purpose: selection and promotion use the same threshold, so a promoted note always carries the tags that promoted it. The 0.7-0.89 band showed a misfire in the Phase 0 spike (`defense .71` on a CI note) and is not trusted for either purpose.

### API Design

**`distillers::tags`**

```rust
pub enum CandidateSource { Author, Model, Preserved }
// Author: publisher hashtags, yt-dlp tags, and the note's `author-tags` key.
// Model: distiller/LLM output from this ingest.
// Preserved: the note's existing canonical `tags` (cortex call sites only).
pub struct TagCandidate { pub text: String, pub source: CandidateSource }
pub struct TagInput<'a> { pub title: &'a str, pub text: &'a str, pub candidates: &'a [TagCandidate] }
pub enum Confidence { High, Medium, Low }
pub struct TagOutput { pub tags: Vec<String>, pub scores: Option<Vec<(String, f32)>>, pub confidence: Confidence, pub method: TagMethod }
pub enum TagMethod { Deterministic, FabricClosed, ClassifierDev }

pub trait TagClassifier {
    fn classify(&self, input: &TagInput) -> eyre::Result<TagOutput>;
    fn classify_batch(&self, inputs: &[TagInput]) -> Vec<eyre::Result<TagOutput>> { inputs.iter().map(|i| self.classify(i)).collect() }
    fn method(&self) -> TagMethod;
}
pub fn build(cfg: &TagsClassifierConfig, canon: &CanonicalSet, fabric: Option<&dyn FabricCaller>) -> Box<dyn TagClassifier>;
```

`text` is the note summary when one exists, else the first 900 characters of the body; never a whole transcript. The ceiling is `distillers::tags::CLASSIFIER_TEXT_CHARS` and both callers clip through `body_excerpt` (borg: `TagSources::from_body`; cortex: `classify::classifier_text`). It is a data boundary, not a tuning knob: under `classifier-dev` this string is POSTed to a third-party API, so a summary-less audio note sends the head of its transcript, never all of it. The trait is synchronous: borg and cortex call it from their existing blocking tag paths; `FabricClosedVocab` calls a sync `FabricRunner` port (`distillers/src/tags.rs:201`), whose production impl is `ShellFabricRunner` (`:207`) wired in by borg and cortex, with `fabric.timeout` from config and one in-flight call per call site; borg wraps the whole classify call in `spawn_blocking` (`borg/src/pipeline/tags.rs:166`) so the sync trait never blocks the runtime; `ClassifierDev` overrides `classify_batch` to send up to 1,000 texts per call.

Candidate handling is what makes G5 hold:
- `Deterministic` maps every candidate (`Author`, `Model`, `Preserved`) through `match_to_canonical` and caps. Pure function of its input; repeatable only when the candidates are. At cortex call sites the `Preserved` source is the note's existing canonical tags, which is exactly what today's Tier 1 `classify_by_tags` reads (`cortex/src/classify.rs:809-810`, deciding 53% of classified notes locally); mapping already-canonical tags is idempotent, so this carries no LLM drift.
- `ClassifierDev` and `FabricClosedVocab` score `title + text` against the vocabulary and **ignore `Model` and `Preserved` candidates** (yesterday's LLM output would carry its drift back in; preserved tags are handled by the merge policy). `Author` candidates that map to a canonical tag are unioned in at tier 0, since they are deterministic and publisher-provided.
- `Author` candidates survive the ingest: borg writes the canonical-mapped set to an `author-tags` frontmatter key (borg-owned via `SESSION_OWNED_KEYS`, `borg/src/pipeline/session.rs:74-89`, not `RENDER_NOTE_KEYS`; `insert_author_tags` explains why; omitted when empty) so a later `--retag` can supply them again; without it a creator's own hashtag would be lost the first time the classifier failed to re-derive it from the text.
- At borg, candidates are built at the call sites before `merge_proposed_tags` (`borg/src/pipeline.rs:138-144,859-870`), where the four sources are still separate variables; after the merge the provenance is gone. At cortex, candidates are `Preserved` (the note's `tags`) plus `Author` (its `author-tags`).

`ClassifierDev`: labels = `CanonicalSet.all` **sorted**, then sharded into chunks of at most 100 (the API's `labels` maximum, observed in the tool schema 2026-09-20; vocabulary is 117 after P1, so two shards), one call per shard per batch, per-label scores merged (the API documents independent per-label probabilities, so no renormalization), then `>= threshold`, then cap. Any shard failing fails the whole classification for that batch. HTTP client: `ureq`, the blocking client `cortex/src/llm.rs:45` already uses (in `Cargo.lock`); added to `distillers/Cargo.toml`, no new crate in the workspace. `reqwest` stays borg-only.

Invariants every impl upholds, unit-tested: output subset of `CanonicalSet.all`; `len <= max_per_note`; sorted by score descending then name; `Deterministic` is a pure function of its candidates; an impl never returns an empty set when the input had at least one `Author` or `Preserved` candidate that maps; a `High` result always has at least one tag.

**Borg.** `finalize_tags(raw, state)` -> `finalize_tags(input, classifier, canon)`; the 8 call sites (`borg/src/pipeline.rs:880`, `pipeline/text.rs:137,294,673`, `pipeline/session.rs:440`, `pipeline/handlers.rs:768,994,1224`) pass title, the summary or body excerpt, and the candidates tagged by source (distiller `Distilled.tags` -> `Model`; YouTube hashtags and yt-dlp tags -> `Author`). `generate_tags` (fabric `create_tags`) and the `create_tags` config key are deleted. When the selected classifier errors at ingest, the `fallback` runs, the receipt is `degraded=true`, and the note is still published. Session notes write `scope: work|personal` and, when any member has `redaction_count > 0`, `redacted: true` (`session.rs:442-450`) instead of pushing tags after `finalize_tags`; both keys and `author-tags` join `RENDER_NOTE_KEYS` and are recomputed on replace.

**Reingest merge (both paths).** `read_cortex_fields` returns `Vec<(String, FieldValue)>` with `FieldValue = Scalar(String) | List(Vec<String>)`, parsing inline and block lists and YAML-decoding quoted items before dedup. `apply_cortex_fields` merges `tags` as `union(preserved, fresh)`, preserved first, truncated to `max-per-note`, and logs one line when the two sets differ or the union is truncated. `borg_owned_keys()` (`session.rs:104-118`) treats `tags` as merge-not-replace and the policy test `borg_owned_key_policy_matches_the_writer` is updated. A preserve read failure fails closed on both paths (the URL path today returns an empty set, `publish.rs:137-140`): the reingest is aborted with receipt `failed`, stage `publish-failed`, and the old note is left untouched. Consequence, stated: a note already at the cap keeps its tags and the fresh set is dropped with a log line; content changes reach its tags only through `--retag`.

**Cortex.** `classify_note` returns `ClassifyResult { tags: TagOutput, domain: Option<Domain> }`; `domain` is still filled from `tag_domain_map` through P10 and removed in P11. Promotion uses the confidence table above. A classifier error inside cortex runs the configured `fallback` (`deterministic` over the note's `tags` and `author-tags`, which is today's Tier 1); only a `Low` result holds the note in `inbox/` for the next tick. Consequence, stated: under the `classifier-dev` default, promotion of a note that arrives with no tags depends on the network; a key outage does not stall notes that borg already tagged at ingest, because the fallback confirms those locally. `--reclassify-domain <d>` is replaced by `--retag <path|glob>...`, which is **replace, not union**: the fresh classification (with the note's `author-tags` unioned at tier 0) wins outright; the previous tags are kept only when the fresh result is `Low` or empty (never writes empty; logs). `cortex-classified-by` is written as the method name. `autotag.rs` is deleted. `obsidian-classify.md` is replaced by `obsidian-tags.md` (closed-vocabulary prompt, JSON `{tags: [...]}`) used only by `FabricClosedVocab`.

**Vault search.** Every function taking `domain: Option<&str>` takes `tags: Option<&[String]>` beside it through P10 and instead of it from P11: `search`, `list_notes`, `recent_notes`, `search_vector`, `note_matches_filters`, `tag_search`, `notes_by_creator`, `notes_by_source_domain`, `classify_stats`. Semantics: OR across the list by default; `tags_all: bool` for AND. `vault::search::find_similar` is the exception: it has no `tags` param (FTS5 term extraction, not a schema-filtered query), so oracle post-filters the resolved rows (`oracle/src/server.rs:742-752`). New: `tag_brief(tag, limit) -> TagBrief` (count, status split, type split, recent notes) mirroring `domain_brief` (`stats.rs:190`); `VaultStats.by_tag` top-20 by count; the schema-gap field list at `stats.rs:174` (`["domain", "note_type", "origin"]`) gains `tags` in P8 and loses `domain` in P11. `domain_stats`, `tag_domain_map`, `domain_exemplars`, `DomainBrief` are deleted in P11.

**Oracle MCP tools.** Each request struct below (`oracle/src/tools.rs`) gains `tags: Option<Vec<String>>` in P8 and loses `domain` in P11: `knowledge_search`, `list_notes`, `tag_search`, `find_similar`, `recent_activity`, `creator_browse`, `source_browse`, `classify_status`. `ingest_history` loses `domain` in P11 with no `tags` sibling: the ledger never carried one. `tag_brief(tag: String)` is added in P8 beside `domain_brief`, which is deleted in P11. `vault_overview` gains `by_tag` (P8) and loses `by_domain` (P11). `schema_info` gains `"tags"` from the vocabulary (P8) and loses `"domains"` (`oracle/src/server.rs:684,1079`) in P11. `inbox_status.classified` becomes `tags` non-empty. Note formatting drops `"domain"` from every returned note (`server.rs:279`) in P11. The eval query struct (`oracle/src/eval/queries.rs:27 domain: Option<String>`, used at `eval.rs:177-221`) gains `tags` in P8 and loses `domain` in P11; `config/eval/queries.yml` has 0 domain filters, so scores do not move. Server instructions text (`server.rs:1053-1059`) rewritten in P11.

**`sb` CLI.** `sb cortex classify --retag <path|glob>...` (P7) replaces `--reclassify-domain` (deleted P11); `sb cortex summarize --tag <t>` added P9 beside `--domain` (deleted P11); `sb cortex migrate --only <name>` and a working `--plan <file>` (P4); `sb cortex schema --render` writes `tag-values.md` (P9) and stops writing `domain-values.md` and deletes the existing file (P11; `render_all_at` only visits current specs and would otherwise leave it); `sb oracle stats` prints "By tag (top 20)" (P8) and drops "By domain" (P11); `sb doctor` strings (`sb/src/cli/checks.rs:779,904-936`) updated P11.

### Implementation Plan

Phases are P0 to P11; prose elsewhere in this doc refers to them by that id. **P1-P9 add code; P10 replaces vault-side and config artifacts whose only reader is domain code; P11 deletes that code and the frontmatter key.** Stores: **code** = second-brain (`git revert` undoes fully). **cfg** = `~/.config/sb` (`cortex.yml`, `borg.yml` live in `scottidler/dotfiles`; `canonical-tags.yml`, `tag-mapping.yml`, patterns come from second-brain `config/` and `borg/patterns/` via `otto deploy`; a cfg revert is a dotfiles commit plus `otto deploy`, paired with the code revert). **notes** = obsidian repo, Syncthing'd; a second-brain revert does nothing here; undo is an obsidian commit or the inverse plan file. **db** = oracle SQLite index, per host, rebuildable by `sb oracle index --force` (which also holds `note_embeddings`, so it is never deleted).

#### Phase 0: Prove the classifier's shape and quota (done 2026-09-20, zero code)
**Model:** sonnet
- Called `classify_multi_label` with 100 canonical labels, 2 fixture texts, `max_labels: 7`, free pool.
- **Success criteria:** 200 OK, per-label scores 0-1, cap honored, 100 labels accepted.
- Observed: `tier: fast`, `model: jev-1.13.0`, `ms: 192`, `usage.classifications: 2`. Fixture 1 (Ollama/Open WebUI): `ai .98 llm .97 privacy .97 hardware .89 infrastructure .84 open-source .79 programming .76`. Fixture 2 (CI security review): `security .98 github .97 programming .97 software-engineering .97 devops .96 rust .95 git .91`; sub-cap `automation .89 infrastructure .89 auth .78 productivity .75 defense .71`. `defense .71` is a football tag on a CI note, which is why the default threshold is 0.9. Tool schema: `labels` max 100, `inputs` max 1,000.
- Revert residue: none.

#### Phase 0b: Shard invariance and repeatability (done 2026-09-20, zero code)
**Model:** sonnet
- The vocabulary is 117 tags against a 100-label API ceiling, so every production call is sharded and `ClassifierDev` merges per-shard scores into one ranking before applying `threshold`. That merge is only valid if a label's score does not depend on which co-labels shared its call. Measured on 53 real notes (10 ai, 10 tech, 6 football, 6 work, 6 resources, 4 life, 3 homelab, 2 each writing/diy/music/spanish), six calls, 318 classifications: sorted shards A/B, an identical repeat of each, and the same 117 labels re-partitioned by interleaving. Harness `~/Claude/tags-only-trial/trial.py`.
- **Shard invariance: PASS, a null result.** Score delta across partitions p50 0.010, p95 0.040, max 0.180, against p50 0.010, p95 0.040, max 0.170 for two byte-identical merged passes. Final tag set changes on 10 of 53 notes across partitions, against 12 of 53 across identical repeats: re-partitioning is *less* disruptive than simply calling twice. Scores are per-label independent rather than a softmax. (Corrected by panel r4: the first write-up compared cross-partition *merged* deltas against a shard-A-only noise floor of p95 0.030 / max 0.130. Like-for-like is 0.040 / 0.170, which strengthens the result.) `shards_are_sorted_and_merge_to_recorded_scores` stays in P2 as a regression test on the merge code.
- **Scope of that PASS (panel r4).** The trial used an even 59/58 split (`trial.py:38`); the doc otherwise says only "at most 100 labels" and never pins the split rule. P2 pins even-split sharding, because a greedy 100/17 split is an untested co-label context. The result is not a licence to grow the vocabulary without limit: re-run this trial before the vocabulary passes ~200 labels, where a third shard starts.
- **Repeatability: FAIL.** Two byte-identical requests produce a different final tag set on **12 of 53 notes (23%)**, with 14 labels crossing the 0.9 threshold in one run and not the other. classifier.dev states this in `llms.txt`: "Neither tier guarantees identical answers across calls." See Risks and Addendum B; G5 is restated below.
- **Migrated domain values are not re-derivable: 0 of 17 recovered, 14 of 17 actually lost.** `work` (0/6), `life` (0/4), `homelab` (0/3), `writing` (0/2), `diy` (0/2) never reach 0.9 on notes carrying them as their `domain`, against `tech` 10/10, `ai` 8/10, `football` 6/6, `music` 2/2, `spanish` 2/2. Three of the 17 produce no label at all, and `--retag` never writes empty (API Design, above), so the loss is **14 of 17** under the sorted split and 13 of 17 re-partitioned.
- **The five are two different failures, not one (corrected by panel r4).** Measured scores on their own notes: `work` [0.48, 0.50, 0.52, 0.52, 0.58, 0.80], `life` [0.33, 0.45, 0.79, 0.87], `writing` [0.67, 0.77], `homelab` [0.09, 0.33, 0.68], `diy` [0.10, 0.85]. `work`, `life` and `writing` are **context collisions**: the signal is present but competes with a topical tag that wins. `homelab` and `diy` are **flat absences** on part of their corpus (0.09, 0.10), which no threshold change reaches. The earlier "consistent near-misses, 0.33-0.87" claim was wrong: it quoted only the top of each range.
- Revert residue: none.

#### Phase 1: Vocabulary, mapping, aliases, segment guard
**Model:** sonnet. **Stores:** code (`config/`, `vault/src/canonical.rs`), cfg (dotfiles `cortex.yml` aliases; `otto deploy` for the two config files).
- `config/canonical-tags.yml`: `domains:` group `tech work life homelab diy`; `entertainment`, `politics` under `life`; top-level `no-segment-match: [tech, work, life, homelab, diy]`; top-level `no-classifier-tags: [work, life, homelab, diy, writing]` (panel r4 OQ5), which `--retag` never removes; `max-per-note: 8` (Scott, 2026-09-20; 10 acceptable if 8 ever binds). `config/tag-mapping.yml`: delete `tech: null`, `diy: null`, `homelab: null`; add the ten self-maps. Regenerate `config/all-tags-alphabetical.txt`.
- `vault::canonical`: `CanonicalSet` (absorbs borg's `CanonicalState`, `borg/src/pipeline.rs:46`), `no_segment` honored in both `match_to_canonical` (`:100-109`) and `filter_and_cap` (`:149-158`); borg and cortex call sites take the new signature. `vault/AGENTS.md` updated.
- dotfiles `~/.config/sb/cortex.yml`: delete the three `ai-llm` aliases (`:64-66`).
- **Success criteria:** `for d in ai tech football work writing life homelab diy music spanish; do grep -qE "^\s+- $d$" ~/.config/sb/canonical-tags.yml && echo ok; done | wc -l` == 10 (today 5); `grep -c '^max-per-note: 8$' ~/.config/sb/canonical-tags.yml` == 1 (today 0, the file says 7); `grep -cE "^(tech|diy|homelab): null" ~/.config/sb/tag-mapping.yml` == 0 (today 3); `grep -cE "^(resources|system): null"` == 2 (unchanged); `grep -cE '^\s+(ai|ML|ml): ai-llm' ~/.config/sb/cortex.yml` == 0 (today 3); `grep -c '^no-classifier-tags:' ~/.config/sb/canonical-tags.yml` == 1 with all five of `work life homelab diy writing` under it; unit tests `segment_match_skips_no_segment_tags_in_both_matchers` (raw `work-life-balance` -> `[]` through `match_to_canonical` and through `filter_and_cap`; raw `rust-cli-tooling` -> `[rust, cli]` as today) and `alias_free_ai_survives_apply_tags`; `sb cortex sweep --migrate --dry-run` reports 0 notes losing a domain-name tag.
- **Vocabulary-removal rule (panel r4 OQ9):** `match_to_canonical` checks the mapping file *before* canonical membership (`vault/src/canonical.rs:86-93`), so any tag removed from `canonical-tags.yml` must have its `tag-mapping.yml` self-map deleted in the same commit, or the mapping keeps minting a tag the vocabulary no longer contains. Applies to the ten self-maps this phase adds and to `writing: writing`, which already exists at `config/tag-mapping.yml:4479`.
- Revert residue: code revert plus a dotfiles revert and `otto deploy`; no notes touched.

#### Phase 2: Shared tag classifier, unwired
**Model:** opus. **Stores:** code.
- `distillers/src/tags.rs` (+ `tags/tests.rs`, `tags/fixtures/`): trait, three impls, `FakeTagClassifier`, `TagsClassifierConfig`, `build()`, confidence mapping, batch method, sorted sharding **pinned to an even split** (panel r4 OQ8: the trial validated 59/58; a greedy 100/17 is an untested co-label context), `ureq` client, whole-batch failure on any shard error. `--retag` honors `no-classifier-tags`. `distillers/AGENTS.md` module map updated (`bin/agents-map` gates it).
- **Success criteria:** `deterministic_is_a_pure_function` (byte-identical over 10 runs on a 20-note fixture set); `outputs_are_canonical_and_capped` against `FakeTagClassifier` and the recorded Phase 0 response checked in as `distillers/src/tags/fixtures/classifier-dev-2026-09-20.json`; `model_candidates_are_ignored_by_repeatable_impls`; `shards_are_sorted_and_merge_to_recorded_scores` (117 labels -> two calls); `confidence_mapping_per_method`; `retag_never_removes_no_classifier_tags` (a note carrying `work` and `rust` is retagged against a recorded response that returns neither; `work` survives, `rust` does not) and `retag_replaces_tags_outside_the_protect_list`; `cargo test -p distillers` green.
- Revert residue: none.

#### Phase 3: Reingest preserves tags
**Model:** opus. **Stores:** code.
- List-aware `read_cortex_fields` / `apply_cortex_fields`; `tags` added to `CORTEX_PRESERVE_KEYS` with union semantics; `borg_owned_keys` tags exception on the session path; preserve read failure fails closed on both paths; `borg/src/pipeline/AGENTS.md` updated. Lands **before** the migration so a URL reingest between the two cannot drop a migrated tag.
- **Success criteria:** `reingest_keeps_cortex_added_tag` (URL path) and `session_replace_merges_tags` green; `borg_owned_key_policy_matches_the_writer` green; block, inline, and quoted fixtures round-trip; `union_at_cap_keeps_preserved_and_logs`; `preserve_read_failure_aborts_reingest` on both paths.
- Revert residue: notes reingested in the window carry merged tags (harmless).

#### Phase 4: Domain-as-tag migration
**Model:** sonnet. **Stores:** code, cfg, notes.
- `cortex/src/tags.rs:199-201` `replace_tags_in_frontmatter` switches to block form. `field-to-tags` and `tags-remove` transforms in `cortex/src/migrate.rs`; `--only <name>`; `--plan <file>` made functional; `config/migrations/v5-domain-as-tag-undo.yml`; `v5-domain-as-tag` entry in `cortex.yml` (dotfiles). Operator sequence: `systemctl --user stop borg cortex` (cortex's per-tick `sweep` and `lint` write the same files; borg snapshots preserved fields at `borg/src/pipeline.rs:636` and applies them minutes later at `:1008`, so an in-flight reingest straddling the migration would write back a pre-migration snapshot), `sb cortex migrate --only v5-domain-as-tag --apply`, `find . -name '*.sync-conflict*' | wc -l` == 0, `systemctl --user start borg cortex`, commit the obsidian repo.
- **Success criteria:** `sb cortex migrate --only v5-domain-as-tag` (dry-run) reports 0 files after `--apply`; observed on main by an equivalent frontmatter scan: 2,748 parseable files carry `domain:`, 52 skipped as `resources`/`system`, **2,545** lack their value as a tag; second `--apply` writes 0 files; `grep -lE '^tags: \[[^]]' $(grep -rl '^domain:' notes work) | wc -l` == 0 (today 1,357 inline in `notes/`). **Amended 2026-09-20 during execution**, twice, both times because the original was unsatisfiable by this phase rather than because the code fell short. (a) `system/**` was dropped from the path list: it is excluded from the vault scan (`~/.config/sb/cortex.yml:14`, with only `system/2026-*.md` and `system/design-*.md` re-included), so `migrate` never visits it, and P10 explicitly owns the 10 templates that hold the remaining inline lists. Measured after the apply: 17 such files, every one under `system/`. (b) The pattern gained `[^]]` so it matches only NON-EMPTY inline lists: `tags: []` is already "no tags" by this doc's own Data Model, and rewriting it to a bare `tags:` swaps one spelling of empty for another while churning the 915 `entities/` files. Measured after the apply: 4 such files in `notes/`, all `domain: resources`. Observed with the amended pattern: **0**; `sb oracle stats` "By domain" unchanged; next daemon tick strips none of the ten domain tags (`grep -c '^  - ai$' notes/*.md` unchanged across the tick; today the daemon would reduce it to 1).
- Revert residue: one extra canonical tag per note and block-form lists, both harmless after P1; the tag is removable with the undo plan file, but only the obsidian commit restores a note that had a pre-existing `football`, `writing`, `music`, or `spanish` tag or, at cap 7, a displaced tag.

#### Phase 5: Tags facet in the index
**Model:** opus. **Stores:** code, db.
- `note_tags` table + index; `SAVEPOINT` per note in `index_changed`; `tags` param threaded beside `domain` through `query.rs`, `vector.rs`, `oracle/src/server/pipeline.rs::note_matches_filters`, `find_similar`, `stats.rs`; `tag_brief`, `VaultStats.by_tag`; `tags = ''` normalized to `[]` at index time. `vault/src/search/AGENTS.md` updated. Domain params untouched. Operator: `sb oracle index --force` per host.
- **Success criteria:** after `--force`, `sqlite3 <index> "SELECT count(*) FROM note_tags"` equals a frontmatter scan of the indexed population at the same moment (today 9,852 + 2,545 after P4); for each of the ten migrated values, `SELECT count(*) FROM notes WHERE domain=x` is a **subset** of `note_tags` membership (test `domain_members_are_tag_members`; strict equality is wrong because tags already exist outside their domain: measured on the index, 5 notes carry `writing` without `domain: writing`, 2 carry `music` without `domain: music`, and the 1 note tagged `ai` has a different domain, so `ai` will show 1,148 domain rows against 1,149 tag rows; all 125 `football` tags sit inside the 301 `football` domain notes); a fixture proving a failed `note_tags` insert rolls back the `notes` row (`index_is_atomic_per_note`); vault baseline 406 tests plus new ones green.
- Revert residue: an unused table in a per-host index file; `sb oracle index --force` rebuilds.

#### Phase 6: Borg ingest through the classifier; governance keys
**Model:** sonnet. **Stores:** code, cfg.
- `finalize_tags` takes the classifier; candidates built before `merge_proposed_tags`; 8 call sites; `generate_tags` and the `create_tags` config key deleted; `author-tags` written when non-empty; `scope` and `redacted` frontmatter keys replace the three governance tags (`session.rs:442-450`, tests `:212,247`, `harvest.rs:109` doc comment); `scope`, `redacted` and `author-tags` are owned through `SESSION_OWNED_KEYS` (`session.rs:74-89`), not `RENDER_NOTE_KEYS`, because that constant must equal exactly what `render_note` emits from its own fields (`borg/src/markdown/tests.rs:752-797`) and these three are caller-computed; `tags.classifier` block in `borg.yml` (dotfiles) with `classifier-dev` default and `deterministic` fallback. `CLASSIFY_API_KEY` reaches `borg.env` through the existing `env-bootstrap`. `keep/manifest.yml:65` is committed (`e40789a`) and the re-encrypted secret is verified working (2026-09-20, HTTP 200 on the Pro tier); the remaining operator step is `manifest secrets env` plus a daemon restart.
- **Success criteria:** `ingest_tags_are_repeatable_under_deterministic` (two ingests of one fixture URL, byte-identical `tags:` blocks) **and** `ingest_tags_are_stable_under_distiller_drift` (two ingests of the same URL with *different* distiller `tags` output, asserting the P3 union absorbs the drift and removes nothing) -- the first alone cannot falsify G5, because a fixture freezes the distiller output that is the actual drift source (panel r4 OQ7); with the recorded classifier.dev response the produced note has no non-canonical tag and at most `max-per-note`; `classifier_failure_degrades_visibly` (forced error -> fallback tags, receipt `degraded=true`); a YouTube fixture with a creator hashtag that maps writes `author-tags` (`author_tags_are_recorded`); a session fixture carries `scope: work` and no `scope-work` tag (`session_governance_is_keys_not_tags`).
- Revert residue: canonical tags and `scope`/`redacted`/`author-tags` keys on notes ingested meanwhile; old code ignores the new config key and the new frontmatter keys.

#### Phase 7: Cortex classify through the classifier
**Model:** opus. **Stores:** code, cfg.
- `classify_note` calls the classifier with `Preserved` (the note's `tags`) and `Author` (its `author-tags`) candidates; promotion by the confidence table; `fallback: deterministic` on classifier error; enrichment writes tags via the P3 union and `cortex-classified-by: <method>`; `domain` still written from `tag_domain_map`; `--retag <path|glob>...` (replace semantics) replaces `--reclassify-domain`'s role while the old flag stays until P11; `autotag.rs` deleted; `borg/patterns/obsidian-tags.md` added beside `obsidian-classify.md` (deleted P11); `tags.classifier` block in `cortex.yml`; `cortex/AGENTS.md` updated. Retag the 36 ex-`resources` notes with `sb cortex classify --retag $(grep -lE '^domain: *"?resources"?' notes/*.md) --apply` and compare against Addendum A.
- **Success criteria:** `promotion_gates_on_confidence` (inbox fixture promotes at High and Medium, holds at Low); `deterministic_fallback_preserves_tier_one` (an inbox fixture already carrying canonical tags promotes with the classifier forced to error, `cortex-classified-by: deterministic`); `sb cortex classify` without `--apply` on the live vault reports 0 "would classify" for already-classified notes; the 36 ex-`resources` notes each carry at least one tag, from Addendum A's expected set **where the classifier reaches 0.9 and by hand otherwise** -- measured 2026-09-20, `homeowner-finds-massive-cave-beneath-his-house.md` peaks at `entertainment 0.87` and clears nothing, so a blanket "all 36 are auto-tagged" criterion is already known false (Addendum B2), and this phase records which were hand-assigned; `retag_replaces_and_never_writes_empty`; `cargo test -p cortex` (baseline 531) green.
- Revert residue: tags written in the window (harmless). `otto deploy` (`sb bootstrap --force`, `sb/src/cli/bootstrap.rs:63-130`) writes embedded patterns and never removes one, so after a revert the operator deletes `~/.config/sb/patterns/obsidian-tags.md` by hand.

#### Phase 8: Oracle MCP and `sb` CLI gain tags siblings
**Model:** sonnet. **Stores:** code.
- `tags` param on every request struct listed under API Design; `tag_brief` tool beside `domain_brief`; `vault_overview.by_tag` beside `by_domain`; `schema_info` `"tags"` beside `"domains"`; eval query struct `tags` field; `stats.rs:174` gap fields gain `tags`; `inbox_status` classified heuristic reads tags; `sb oracle stats` "By tag (top 20)" beside "By domain". Nothing removed. `oracle/AGENTS.md`, `sb/AGENTS.md` updated.
- **Success criteria:** every request struct that has `domain` also has `tags` (test over the schemars output, `every_domain_param_has_a_tags_sibling`); `sb oracle eval` scores unchanged to two decimals (0 domain queries); `sb oracle call knowledge_search '{"query":"ollama","tags":["privacy"]}'` returns the Ollama fixture; oracle baseline 96 tests plus new ones green.
- Revert residue: none.

#### Phase 9: Cortex lint, schema docs, summarize
**Model:** sonnet. **Stores:** code, notes (`system/schemas/`).
- `tags.non-canonical` reads `canonical-tags.yml`; `tags.cap`; `frontmatter.required.tags` treats `[]` and bare `tags:` as missing; `tags.format` accepts both list forms on read and flags inline as a fixable violation; `schema_docs` gains a `tag-values.md` renderer from `canonical-tags.yml` (keeps rendering `domain-values.md` until P11); `summarize --tag` beside `--domain`; cold report keeps its grouping until P11 and switches to a flat list then (a "first tag" is an ordering artifact, not a category). Run `sb cortex schema --render`; commit obsidian.
- **Success criteria:** `sb cortex schema --check` exit 0 and `system/schemas/tag-values.md` exists; on a three-note fixture vault `sb cortex lint` reports exactly one `tags.non-canonical`, one `tags.cap`, one `tags.format` (`tags_schema_rules_fire_once_each`); on the live vault the three counts plus `frontmatter.required.tags` are recorded; `system/views/cold-notes.md` renders.
- Revert residue: regenerated docs under `system/schemas/` (regenerable).

#### Phase 10: Vault and config artifacts
**Model:** sonnet. **Stores:** notes, cfg (operator commits in obsidian and dotfiles). This phase replaces artifacts, not code: every file it touches is read only by domain code or by a human.
- `.base` views: `all-notes`, `borg-ledger`, `unread`, `work` each drop the `domain` **column** and gain a `tags` column, filters unchanged (`work.base` filters on `origin == "authored"` and `type`, never on domain: 605 notes today, 0 of which carry the tag `work`, so a filter change would empty it); `domains.base` -> `tags.base` (group by tag); `untriaged.base` filter `domain == null` -> tags empty. 10 templates lose `domain:` (including `system/templates/book.md`, which carries `domain: resources`); `bin/wn:30`; `system/schemas/frontmatter.md`; vault `README.md`, `CLAUDE.md`. `cortex.yml`: `required` -> `[title, date, type, origin, tags]`, exemptions carried 1:1 to `tags`, add `system/**`, drop `v3-domain-expansion`, drop the `folder: domain` rename from `v2-field-renames` (`cortex.yml:227`; with `cortex/src/frontmatter.rs:22` it would re-mint `domain:` from any `folder:` key after P11), delete legacy `actions.tags.canonical` and its aliases block if empty. `borg.yml`: delete dead `fallback-domain: inbox` (`:114`, no reader in `borg/src`).
- **Success criteria:** `grep -rlw domain system/views system/templates bin | wc -l` == 0 in the vault; `sb cortex lint` reports 0 `frontmatter.required.domain`; `grep -c fallback-domain ~/.config/sb/borg.yml` == 0; `grep -c 'folder: domain' ~/.config/sb/cortex.yml` == 0 (today 1).
- Revert residue: per-repo git revert; Syncthing propagates either way.

#### Phase 11: Delete `domain`
**Model:** sonnet. **Stores:** code, cfg (deployed pattern), notes, db, ledger.
- Obsidian commit first (authoritative undo). Operator: `systemctl --user stop borg cortex` for the apply step; `rm ~/.config/sb/patterns/obsidian-classify.md` after deploy (`otto deploy` never removes a pattern file). Remove `Domain` enum and alias, `"domain"` from `CORTEX_PRESERVE_KEYS`, `Frontmatter.domain`, `hygiene::{DOMAIN_ALIASES, normalize_domain}`, `SchemaConfig.domains`, `cortex/src/frontmatter.rs:22` folder mapping, ledger `Domain` column with a one-time row rewrite of `~/.local/share/sb/borg/borg-ledger.md` (backup copy beside it; header repair at `vault/src/ledger.rs:75` touches only the header), both `LedgerIdx` layouts, `NoteRow.domain`, 25 SELECT lists, `--reclassify-domain`, `classify_stats(domain)`, `IngestHistoryRequest.domain`, `domain_brief`, `domain_stats`, `tag_domain_map`, `domain_exemplars`, `DomainBrief`, `by_domain`, `"domains"` in `schema_info` and `vault_overview`, eval query `domain`, `stats.rs:174` `"domain"`, `shared-domain` edge kind + `domain_weight`, `summarize --domain`, `obsidian-classify.md`, deprecated tool params, `schema_docs` domain renderer plus deletion of `system/schemas/domain-values.md`, cold report to flat list, `v6-drop-domain` (`field-drops: [domain]`) run vault-wide with the daemon stopped, `vault/AGENTS.md` and root `CLAUDE.md` "Schema is law" line. `notes.domain` column and `idx_notes_domain` stay in the schema, unpopulated. Commit obsidian.
- **Success criteria:** AC1 == 0; AC2 == 0; `otto ci` green; `sb doctor` output contains no `domain`; `sb oracle index --force` then `sb oracle call vault_overview '{}'` has `by_tag` and no `by_domain`.
- Revert residue: **the only phase with a data undo.** Code revert restores the enum, lint, and the old binary opens the index because the column still exists. Frontmatter undo is the obsidian commit taken first; the inverse plan file is not exact (it also strips pre-existing same-name tags and cannot reconstruct `resources`/`system`). Ledger undo is the backup copy. The deployed `obsidian-classify.md` returns on the next `otto deploy` of the reverted binary.

## Acceptance Criteria

Run against current `main` before ready-to-build; observed output recorded under each.

- [ ] **AC1** One case-insensitive substring grep from the second-brain root, == 0. Substring (not `-w`) so compound identifiers count: `DomainBrief`, `tag_domain_map`, `source_domain_map`, `domain_scores`, `DomainBriefRequest` are all matched. The exclusions are content-based and each names a thing that stays on purpose:
  `grep -rniE 'domain' --include=*.rs vault borg cortex oracle sb distillers | grep -viE 'blocklist|stages/(alert|raw|artifact)|types\.rs|description\.rs|quality\.rs|cli/borg\.rs|borg/src/lib\.rs|borg/src/config\.rs|borg/src/github\.rs|harvest\.rs|harvest/select\.rs' | grep -viE 'anonymous access to domain|domain TEXT,|idx_notes_domain|source_domain_stats|notes_by_source_domain|source URL domain|subdomain|URL domain|v5-domain-as-tag-undo' | wc -l`
  Exclusion classes: URL-host files (Non-Goals) and the one blocklist string; the two retained schema lines (`domain TEXT,` and `CREATE INDEX ... idx_notes_domain`, `vault/src/search/schema.rs:11,49`, kept so the previous binary can open the index); the source-host statistics that keep their name (`source_domain_stats`, `notes_by_source_domain`, and their doc comments in `stats.rs`, `oracle/src/server.rs`, `oracle/src/tools.rs`). `source_domain_map` (`cortex/src/classify.rs:110,126,863`, Tier 1b vault-domain code) is **not** excluded and is deleted in P11.
  - Observed on main (a1415bc, 2026-09-20): **738** lines across **76** files (tests included). Listed and read: every excluded line is one of the three classes above.
  - **P11 execution correction (2026-09-20):** the command above (both exclusion greps case-sensitive, and the file-path exclusion list missing two files) undercounted three real classes, all proven on the fully-migrated tree: (1) `-vE` (case-sensitive) let two ALL-CAPS/Title-Case block-page fixtures through (`borg/src/stages/classify/tests.rs:50` `"ANONYMOUS ACCESS TO DOMAIN"`, `cortex/src/quality/tests.rs:216` `"Anonymous access to domain"`) even though the lowercase form of the identical URL-host-blocking phrase is excluded - fixed by changing both exclusion `grep -vE` calls to `grep -viE`; (2) `borg/src/harvest.rs:337` and `borg/src/harvest/select.rs:1,76` construct the URL-host `RejectionRecord.domain` field (the same Non-Goals meaning already excluded via `stages/(alert|raw|artifact)`, `types.rs`, `lib.rs`) but the file-path exclusion list never named these two files - added `harvest\.rs|harvest/select\.rs`; (3) `cortex/src/migrate/tests.rs:538,541` names `config/migrations/v5-domain-as-tag-undo.yml`, the real shipped historical-undo artifact for the already-executed P4 migration (fixed content, fixed filename, not renameable) - added a literal exclusion for it. Also, the Non-Goals line's `lib.rs:731-748 behind sb borg reingest --domain` entry was **wrong**: that code path is vault-classification `domain` (an `EntryFilter`/`LedgerEntry` field, not a URL host), so it was in scope and P11 deleted the `--domain` flag from `sb borg reingest` entirely (no tags replacement: the ledger has no tags column to filter on). With the corrected command: **0**, workspace-wide, on the completed P11 tree.
- [ ] **AC2** Frontmatter-scoped: from the vault root, `for f in $(grep -rl '^domain:' --include=*.md . | grep -v '/.obsidian/'); do awk 'NR==1&&$0!="---"{exit} /^---$/{c++; if(c==2)exit} c==1&&/^domain:/{print FILENAME; exit}' "$f"; done | wc -l` == 0.
  - Observed on main (2026-09-20): **2748** (the unscoped `grep -rl` gives 2,750; the two extra are prose lines in `CLAUDE.md` and `system/templates/frontmatter.md`).
- [ ] **AC3** `cargo test --workspace --features vec -- ingest_tags_are_repeatable_under_deterministic ingest_tags_are_stable_under_distiller_drift retag_never_removes_no_classifier_tags reingest_keeps_cortex_added_tag session_replace_merges_tags` passes (libtest accepts multiple positional filters), and each of the five fails when its production code path is reverted (break-the-code evidence in the implementation notes). The drift and protect-list tests were added by panel r4 (OQ7, OQ5); the original three could all pass while both of those goals failed.
  - Observed on main: none of the five tests exist yet (P2, P3 and P6 deliverables); `cargo test -p borg --lib` alone fails on main with `E0433 cannot find search in vault` (feature unification), which is why the command runs at workspace level with `--features vec` as `otto ci` does.
- [ ] **AC4** `sqlite3 <index> "SELECT count(*) FROM note_tags"` equals `SELECT count(*) FROM notes, json_each(notes.tags) WHERE json_valid(notes.tags)` on the same index at the same moment, and `SELECT count(DISTINCT tag) FROM note_tags` >= 108 (both clauses are standing invariants and carry past P11); for each of the ten migrated values `x`, `SELECT count(*) FROM notes WHERE domain=x AND path NOT IN (SELECT path FROM note_tags WHERE tag=x)` == 0, measured once between P5 and P11 to prove the migration landed, and retired at P11 by design when the column stops being populated.
  - Observed on main (2026-09-20, `~/.local/share/sb/oracle/oracle.db` read-only): table does not exist (P5 deliverable). Today's index: 3,742 notes; `json_each` over valid rows = **9,852** pairs, **108** distinct; 303 rows hold `tags = ''` and are excluded by `json_valid`; `domain='ai'` 1,148 vs tag `ai` **1**; `domain='football'` 301 vs tag `football` 125.
- [ ] **AC5** `sb cortex lint` on the live vault reports 0 `frontmatter.required.domain` and 0 `tags.cap`; on a three-note fixture vault it reports exactly one each of `tags.non-canonical`, `tags.cap`, `tags.format`; `sb cortex schema --check` exit 0 with `system/schemas/tag-values.md` present and `domain-values.md` absent.
  - Observed on main (2026-09-20, live vault): `tags.non-canonical` **9,020** (against the legacy 11-entry list), `frontmatter.required.tags` **300**, `frontmatter.required.domain` **3**, `tags.orphan` **2**; `tags.cap` and the fixture do not exist yet; `sb cortex schema --check` exit **0** with `domain-values.md` present and no `tag-values.md`.

## Resolved Decisions

- **2026-09-19, Scott:** tags-only, not "domain plus a tag facet" and not "`entertainment` as a domain". Both were recommended by the agent and overridden. Alternatives 1 and 2; not re-litigated.
- **2026-09-19, Scott:** `security` is not a domain and not a candidate. 254 of 273 security-titled `tech` notes are `type: session`, 239 of those `/security-review` agent boilerplate; 10 notes carry security content. It stays a tag.
- **2026-09-19, Scott:** 9 `resources` stubs deleted via `rkvr rmrf` (archive `/var/tmp/rmrf/2026-09-19-202730-000/`): `example-domain-2`, `youtube`, `starting-note`, `the-iliad-homer-william-lucas-collins`, `website-that-feels-illegal-to-know-part-2`, `central-america-explained-in-2-minutes`, `even-this-kind-of-woman-is-rare`, `how-i-make-100000-per-week`, `upper-middle-lower-class-in-charts-percentages-income-by-state-2`.
- **2026-09-19, Scott:** full scope, no deferrals; additive/revertible phases; repeatable ingest tagging with a stated reingest policy (G2, G3, G5).
- **2026-09-20, author:** classifier seam lives in `distillers` (both callers depend on it; `FabricCaller` is there; `vault` stays LLM-free). Panel r2: both seats confirmed.
- **2026-09-20, author:** on-disk form is the block list (Obsidian-native; two of three writers already).
- **2026-09-20, author:** ledger `Domain` column deleted with a row rewrite and backup, not made multi-valued (3,234 of 3,302 rows are `-`).
- **2026-09-20, author:** `resources` and `system` are not propagated as tags; `system/**` joins the tags exemptions.
- **2026-09-20, author:** the shipped default classifier is `deterministic` (`TagsClassifierConfig::default`, `distillers/src/tags.rs`); the deployed hosts opt in to `classifier-dev` per host in `borg.yml` / `cortex.yml`, with `deterministic` fallback at ingest and a visible `degraded` receipt; threshold 0.9 from the calibration back-test and the Phase 0 spike.
- **2026-09-20, panel r2 M1 -> author:** the `ai`/`ML`/`ml` -> `ai-llm` aliases are retired in P1, before any phase writes `ai` as a tag. Verified live: 1 note carries `ai` today.
- **2026-09-20, panel r2 M2 -> author:** reingest preservation (P3) lands before the migration (P4). Both seats converged; `apply_cortex_fields` skips non-preserve keys silently.
- **2026-09-20, panel r2 M4 -> author:** segment guard is a top-level `no-segment-match` list threaded into both `match_to_canonical` and `filter_and_cap`, with `CanonicalSet` replacing the bare `HashSet` (API change acknowledged). The per-group flag was rejected: `tags: HashMap<String, Vec<String>>` cannot carry it.
- **2026-09-20, panel r2 M6 -> author:** reingest = union preserved-first; `--retag` = replace. Two operations, two policies.
- **2026-09-20, panel r2 M7 -> author:** session governance signals become `scope` and `redacted` frontmatter keys (Open Question 4 records that this amends the harvest doc's wording).
- **2026-09-20, panel r2 M8 -> author:** `sb oracle index --force` for the backfill; per-note `SAVEPOINT`; subset criterion, not equality.
- **2026-09-20, panel r2 M10 -> author:** `notes.domain` column stays in the schema unpopulated; the obsidian commit is the frontmatter undo; the inverse plan file is documented as inexact.
- **2026-09-20, panel r2 M11 -> author:** per-method confidence mapping (High/Medium/Low) drives promotion and `cortex-confidence`; `scores` is `Option`.
- **2026-09-20, panel r2 Q5 -> author, refined by r3 F5:** "domain live until P11" means no code that reads or writes `domain` is deleted before P11. P1-P9 add; P10 replaces vault-side views, templates, and config entries whose only reader is that code; P11 deletes the code, the deployed pattern, and the frontmatter key. `domain_brief`, `shared-domain`, `summarize --domain`, `domain-values.md` are P11.
- **2026-09-20, panel r3 F3/F4 -> author:** cortex passes the note's existing tags as `Preserved` candidates that only `Deterministic` consumes (today's Tier 1, kept local); cortex gets the same `fallback: deterministic` as borg, so a classifier outage holds only notes with no tags at all; `classifier-dev` has no Medium band, so a promoted note always carries the tags that promoted it.
- **2026-09-20, panel r3 Q4 -> author:** publisher-provided tags are recorded in `author-tags` at ingest so `--retag` can union them again; without it a creator's hashtag would be lost on the first retag that failed to re-derive it.
- **2026-09-20, panel r3 F6 -> author:** `.base` views change columns only; no view's filter changes except `untriaged` (`domain == null` -> tags empty) and the `domains` -> `tags` grouping.
- **2026-09-20, panel r3 F9 -> author:** both daemons stop for the P4 and P11 apply windows.
- **2026-09-20, Scott (Open Question 1):** `max-per-note` goes to **8** in P1, so the domain tag never displaces an existing tag on the 491 capped notes. Scott accepts 10 as headroom if 8 ever binds; the doc sets 8. Architect (r2: 7, r3: 8), staff 8, synthesis 8.
- **2026-09-20, Scott (Open Question 2):** neither leftover `resources` note is deleted. `illium.md` (a 43-byte template stub for a book Scott loves) stays and is retagged in P7 into the entertainment bucket (`books`, `reading`, `fiction`); `when-karma-hits-back-at-you-instantly...` stays unless Scott later says otherwise. Both seats had recommended deletion on the note-is-empty ground; the owner decides.
- **2026-09-20, Scott (Open Question 3):** confirmed: `tags` is machine-managed. `[]` means unclassified and is repopulated by the next classification; a tag removed by hand in Obsidian returns on the next reingest or `--retag`; no tombstone key. Curation is through `canonical-tags.yml`, `tag-mapping.yml`, and `--retag`. Publisher hashtags survive through `author-tags`.
- **2026-09-20, Scott (Open Question 4):** confirmed: session governance signals become `scope` and `redacted` frontmatter keys. This amends the 2026-07-17 harvest doc's "scope-tagged" wording. Zero notes carry the old tags today, so nothing migrates; two borg tests and one doc comment change.
- **2026-09-20, panel r2 Q2 -> author:** preserve read failures fail closed on both reingest paths.
- **2026-09-20, panel r2 Q3 -> author:** existing frontmatter tags are preserved state under the merge policy, not `Author` candidates.
- **2026-09-20, panel r4 OQ5 -> author (OPTION A, all five):** `work`, `life`, `homelab`, `diy`, `writing` go into `canonical-tags.yml` as planned and are added to a new top-level `no-classifier-tags` list that `--retag` never removes. Option B (demote `work` to provenance) is **disqualified by code, not taste**: `cortex/src/classify.rs:174-179` derives `domain: work` from the topical tags `tatari, sre, infrastructure, kubernetes, platform`, so it is an inference from subject matter, not a provenance fact. Verified counterexample: `notes/these-git-repositories-changed-how-i-manage-my-home-lab.md` carries `domain: work` and `cortex-classified-by: deterministic` while its summary addresses home-lab hobbyists. The path is not a proxy either: 145 notes carry `domain: work` and only 42 sit under `work/` (29%). B and C both strand 61 untagged work-domain notes (including all 42 under `work/`, e.g. `work/start-here.md:8` with `tags: []`) with no migration seed and no exemption, since `work/**` is **not** path-exempt (`~/.config/sb/cortex.yml:33-36`). The author had recommended B for `work` and A for the rest; that recommendation is withdrawn. The objection that a protect list breaks the machine-managed-tags invariant does not stand: `no-segment-match` and `author-tags` are already curated exceptions.
- **2026-09-20, panel r4 OQ6 -> author:** 23% non-repeatability is acceptable at ingest and not at `--retag`. Union is monotone (replaying one pass onto the other changes 9 of 53 notes, adds 10 tags, removes none). **Hysteresis is rejected on measurement**, not on cost: simulating add >= 0.90 / keep >= K gives 12/53 disagreement at no hysteresis, 14/53 at K=0.87, 10/53 at K=0.80, because churn is dominated by labels crossing *upward* through 0.90 (14 note-label crossings over 12 labels) and hysteresis only guards the downward direction. Best-of-N is not evaluable from two passes and is not adopted. `--retag` is fixed by the `no-classifier-tags` protect list, not by tuning the classifier.
- **2026-09-20, panel r4 OQ7 -> author:** the G5 restatement from "repeatable" to "stable" is honest, but G5 still overclaims one layer down and is corrected in place: `Deterministic` is byte-repeatable **as a function**, not **as an ingest**, because `distilled.tags` (LLM output) merges into `all_tags` before `finalize_tags` (`borg/src/pipeline.rs:862`, `borg/src/pipeline/session.rs:439`). The P6 criterion `ingest_tags_are_repeatable_under_deterministic` ingests a fixture URL, so the distiller output is frozen and the test passes whether or not the goal holds: it is structurally incapable of falsifying its own claim. P6 adds a second criterion that varies the distiller output and asserts the union policy absorbs it.
- **2026-09-20, panel r4 OQ8 -> author:** the shard PASS stands and is scoped. P2 pins even-split sharding; the unscoped claim "the vocabulary does not need capping at 100" is deleted and replaced with a re-test trigger at ~200 labels. Two sample caveats recorded: the trial classified title + 1,500 body chars (`sample.py:6-13`) where production sends title + summary or 900 chars (API Design), and the 53-note sample contains no `work/` note and no short stub.
- **2026-09-20, panel r4 OQ9 -> author:** independent of OQ5, `vault/src/canonical.rs:86-93` returns mapping hits **before** canonical membership, and `config/tag-mapping.yml:4479` already reads `writing: writing`. Rule stated in P1: removing a tag from the vocabulary must delete its self-map in the same commit, or the mapping keeps minting a tag the vocabulary no longer contains. `system/views/work.base` filters on `origin`/`type` and only displays `domain` as a column; `system/views/domains.base:19` groups by it. `vault/src/search/schema.rs:8-16` has no `scope` column, so under option B no facet could have answered a work query at all.

## Alternatives Considered

### Alternative 1: Keep `domain`, add a tags facet beside it
- **Description:** Build `note_tags` and a `tags` filter; leave `domain` as the shelf.
- **Pros:** No enum or migration work; `.base` views unchanged.
- **Cons:** Two axes encoding overlapping meaning; the residue bucket stays; Tier 1's tag-derived domain and the stored domain can diverge.
- **Why not chosen:** Scott overrode it 2026-09-19. Two signals never encode the same meaning (`rules/taste.md`).

### Alternative 2: Add `entertainment` as a domain, everything else tags
- **Description:** One enum variant and one migration for the bucket with nowhere to go.
- **Pros:** Smallest change that clears `resources`.
- **Cons:** Keeps single-label and its recurrence.
- **Why not chosen:** Overridden 2026-09-19 with Alternative 1.

### Alternative 3: Add five domains (gaming, entertainment, politics, science, pkm)
- **Description:** Scott's first approval for the `resources` split, as domains.
- **Pros:** Direct fix for the hand-counted buckets.
- **Cons:** Four of five exist as tags. Back-test tax: retention 82.6% -> 79.8% with four labels added; `writing` 84.2% -> 68.4% (n=19).
- **Why not chosen:** Superseded the same day by tags-only.

### Alternative 4: Config-driven domain enum
- **Description:** String validated against a config list.
- **Pros:** Adding a domain is a config edit.
- **Cons:** Still single-valued; loses `schemars` derives for MCP schemas.
- **Why not chosen:** Rejected 2026-03-23 (domain-expansion, Alternative 1) as unnecessary because "this is the first expansion". This is the second. Remove the axis rather than make it cheaper to grow.

### Alternative 5: Keep `domain` as a field derived from tags
- **Description:** Compute `domain = f(tags)` from `default_tag_domain_map` at write time.
- **Pros:** Zero change for readers.
- **Cons:** A stored derived field diverges the first time the map changes; every consumer keeps single-value semantics.
- **Why not chosen:** "A field derived from another never diverges: drop it rather than sync it" (`rules/taste.md`).

### Alternative 6: Query tags with `json_each(notes.tags)` instead of a `note_tags` table
- **Description:** No schema change; filter via SQLite JSON1 over the existing JSON column.
- **Pros:** Zero migration; one column of truth.
- **Cons:** No index on tag; every tag filter is a full scan plus JSON parse; 303 rows hold `''` and make `json_each` error today; `tag_stats` already does this in Rust and is the slowest stats call.
- **Why not chosen:** The facet replaces an indexed column (`idx_notes_domain`); it should not be slower than what it replaces.

### Alternative 7: Constrain Fabric `create_tags` to the vocabulary instead of a classifier
- **Description:** Tag-sweeper's Alternative 2 (2026-03-23): put the vocabulary into the prompt.
- **Pros:** No new vendor; no new crate module.
- **Cons:** Still sampled free text with no seed; repeatability unmeasured; `run_pattern` has no temperature flag. Kept as the `FabricClosedVocab` impl for hosts without a classifier key.
- **Why not chosen as default:** The classifier is the only path with measured run-to-run drift (~0.1%, handoff) and calibrated per-label scores (94.7% at >=0.9).

### Alternative 8: Normalize all tags to inline `[a, b]` so the scalar preserve parser works unchanged
- **Description:** Brief's recommendation: inline form, add `tags` to preserve keys, no parser change.
- **Pros:** Smallest borg change.
- **Cons:** Obsidian's property editor writes block lists, so the vault drifts back to two forms within a day; two of three writers already write block.
- **Why not chosen:** The parser must accept both regardless; making it list-aware is the one change that ends the drift.

### Alternative 9: A per-group `segment-match: false` flag in `canonical-tags.yml`
- **Description:** Pass 4's original guard: flag the `domains:` group.
- **Pros:** Keeps the flag next to the tags it governs.
- **Cons:** `tags: HashMap<String, Vec<String>>` (`vault/src/canonical.rs:13`) has no slot for group metadata; the plan patched only `match_to_canonical` while `filter_and_cap` (the production path) carries its own segment loop.
- **Why not chosen:** Panel r2 M4. Replaced by the top-level `no-segment-match` list threaded into both matchers.

### Alternative 10: Keep governance tags in `tags` under a cap-exempt group
- **Description:** Add `scope-work`, `scope-personal`, `redacted-source` to the vocabulary and exempt them from the cap.
- **Pros:** No change to the session writer's shape.
- **Cons:** Three more exceptions in the matcher and the lint (cap-exempt, non-interest, must-appear-on-sessions); the vocabulary stops meaning "interests".
- **Why not chosen:** Two frontmatter keys say the same thing with no exceptions. Open Question 4 records the harvest-doc wording change.

## Technical Considerations

### Dependencies
- Crates: no new ones in the workspace. `ureq` (already a `cortex` dependency, `cortex/src/llm.rs:45`) is added to `distillers/Cargo.toml` for `ClassifierDev`; `reqwest 0.13.2` stays borg-only (`borg/Cargo.toml:19`); `rusqlite 0.34` bundles SQLite 3.49.1.
- Cross-repo blast radius and ship order: **second-brain** (P1-P11 code; `config/` in P1 and P4), **dotfiles** (`cortex.yml` P1 aliases, P4 migration entry, P7 classifier block, P10 schema and rules; `borg.yml` P6 classifier block, P10 `fallback-domain`), **obsidian** (notes P4, P7, P11; `system/` P9, P10), **keep** (`manifest.yml:65` `classify-api-key`, committed `e40789a`; the secret is re-encrypted and verified live 2026-09-20, so nothing here gates P6 beyond `manifest secrets env`). Order within a phase: code merged and `otto deploy`ed, then cfg commit, then the migration or render step, then the obsidian commit.
- Daemons: borg and cortex restart on `otto deploy`; both units are stopped by hand for the P4 and P11 apply steps; P5 and P11 db steps run per host where `sb oracle serve` runs (`sb oracle index --force`). `otto deploy` adds pattern files and never removes one; P7's revert and P11 each carry a manual `rm` of a deployed pattern.

### Performance
- Ingest: one or two classifier HTTP calls per note (sharded labels), 192 ms measured for 2 texts; replaces one Fabric subprocess call (`create_tags`, 5-30 s). Net faster.
- Migration: 2,748 files, rayon, `write_atomic`; same shape as `v3-domain-expansion`.
- Index: `note_tags` about 12,400 rows after P4; `idx_note_tags_tag` makes the tag filter an index lookup, matching what `idx_notes_domain` gave. `--force` reindex of 3,742 notes is the existing full-index cost.
- Daemon: `--retag` and catch-up batch up to 1,000 texts per call; classifier.dev limits observed in the handoff (`3000;w=60, 20000;w=86400` free, 10x Pro) bound the daemon's tick. The client reads no rate-limit header: every non-200 response is an error, there is no retry, and borg runs the `deterministic` fallback and marks the receipt `degraded`.

### Security
- `CLASSIFY_API_KEY` rides the established channel: age-encrypted in `keep/.secrets`, exported by `manifest secrets env`, delivered to the daemons by `env-bootstrap` -> `EnvironmentFile`. Never logged; presence checked with `env -r`.
- Data leaving the host: title + summary (never transcript) to classifier.dev, the same class already sent to Anthropic via Fabric. One more recipient of the same data class; recorded.
- Fail closed on the write path: a classifier error never writes a partial or non-canonical tag set; at ingest the fallback runs and the receipt says `degraded=true`; in cortex the note holds; a preserve read failure aborts the reingest.

### Testing Strategy
- Unit: `distillers::tags` invariants (subset, cap, purity, sorted sharding merge, confidence mapping, Model-candidate exclusion); migration transforms (apply, lint, idempotence, `--only`, plan file); preserve parser on inline, block, quoted, and empty lists; union-and-cap ordering; fail-closed preserve reads; `tags.cap`, `tags.non-canonical`, `tags.format` rules; `note_tags` maintenance and per-note atomicity; `no_segment` in both matchers.
- Negative paths, per phase: classifier timeout, one failed shard, malformed JSON, HTTP 429, quoted-tag decoding, `tags = ''` index rows.
- Integration: end-to-end ingest fixture twice under `deterministic` (byte-identical); URL reingest and session replace keep a cortex-added tag; inbox promotion by confidence; `domain_members_are_tag_members` for the ten migrated values; a daemon tick after P4 strips none of them.
- Break-the-code: for AC3 each of the five named tests is shown failing with its production path reverted, recorded in the implementation notes.
- Baselines from `main`: vault 406, cortex 531, oracle 96 lib tests; borg via `otto ci`.

### Rollout Plan
1. P0 recorded. P1 lands with its dotfiles alias removal; `otto deploy`; confirm `sb cortex sweep --migrate --dry-run` drops nothing and the alias grep is 0.
2. P2 lands (unwired). P3 lands and deploys (borg now preserves tags).
3. P4 lands; dotfiles `cortex.yml` migration entry; `systemctl --user stop borg cortex`; `sb cortex migrate --only v5-domain-as-tag --apply`; sync-conflict check; `systemctl --user start borg cortex`; one daemon tick; verify the ten tags survived; obsidian commit.
4. P5 lands; `sb oracle index --force` per host.
5. Operator: `keep/manifest.yml` is already committed (`e40789a`) and the secret verified; re-run `manifest secrets env` and restart borg and cortex so `borg.env`/`cortex.env` carry `CLASSIFY_API_KEY`.
6. P6, P7 land with their dotfiles blocks; `--retag` the ex-`resources` notes; obsidian commit.
7. P8, P9 land; `sb cortex schema --render`; obsidian commit.
8. P10: dotfiles and obsidian commits.
9. P11: obsidian commit first; land; `systemctl --user stop borg cortex`; `sb cortex migrate --only v6-drop-domain --apply`; sync-conflict check; `rm ~/.config/sb/patterns/obsidian-classify.md`; `systemctl --user start borg cortex`; `sb oracle index --force` per host; obsidian commit; `sb doctor`.
10. Stop point after any phase: the system works with `domain` still present, populated, and read by code through P10.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Daemon alias chain strips `ai` on the tick after the migration | Certain without P1 | High | P1 retires the three aliases first; P4 success criterion counts `- ai` across a tick |
| Daemon `sweep` strips migrated domain tags before P1 is deployed | Med | Med | P1 ships and deploys before P4; P1 dry-run shows 0 drops |
| URL reingest between migration and preservation drops a migrated tag silently | Certain in the window | Med | P3 (preservation) lands before P4 (migration); the window does not exist |
| Daemon `sweep`/`lint` writes the same file as `migrate --apply` | Med | Med | Cortex unit stopped for both apply steps; `write_atomic` on both sides means a lost update, never a torn file |
| `work`, `life`, `tech` minted from raw compounds by tier-2 segment matching | High without mitigation | Med | `no-segment-match` list honored by both matchers; test proves both |
| 491 notes at the cap lose a specific tag to the added domain tag | Closed | Low | `max-per-note: 8` in P1 (Scott, 2026-09-20); the P4 dry-run still lists any note that would exceed 8 |
| Union policy starves fresh tags on notes already at the cap | Med | Low | Deliberate: a refetch never rewrites classification silently. Logged; `--retag` (replace) is the explicit refresh |
| `[]` or a manual de-tag is repopulated by the next classification | Certain | Low | Stated policy: tags are machine-managed (Open Question 3) |
| classifier.dev outage or quota at ingest | Med | Low | `fallback: deterministic`, `degraded=true` receipt, `sb doctor` warns on `degraded_24h` |
| Classifier drift rewrites tags on reingest | Low (~0.1%) | Low | Union policy: preserved tags are never removed by a refetch |
| `field-to-tags` corrupts frontmatter | Low | High | Same `write_atomic` + continuation-aware path as `value-renames`; dry-run first; obsidian is git |
| An inverse migration in `cortex.yml` runs by accident | Certain as originally drafted | High | Inverse lives in a plan file under `config/migrations/`, run only by `--plan`; `--only` selects forward migrations |
| Oracle clients cache old tool schemas with `domain` | Low | Low | Domain params stay until P11; one schema refresh after |
| P11 frontmatter drop is not `git revert`-able from second-brain | Certain | Med | Obsidian commit immediately before; the column stays so the old binary opens the index |
| `folder: domain` rename rule re-mints `domain:` after P11 | Low | Low | P10 removes the rule from `v2-field-renames` and P11 removes the code mapping |
| Bulk migration writes while another Syncthing peer edits the same file | Low | Low | Run on desk; success criterion counts `*.sync-conflict*` files (== 0) before the obsidian commit |
| Lint noise jump: empty-tag notes join the missing-key rows as `required.tags` errors once `[]` counts as missing | Certain | Low | Measured 2026-09-20: 507 files in `notes/` + `work/` have no tags, against the 300 the lint reports today. P4 seeds 438 of them from their own `domain` value, the `notes/ai/**` exemption covers 50, and the remaining 19 are the `resources` residue already itemized in Addendum A. Backlog needing a classifier: 0 (Addendum B) |
| `bin/agents-map` fails CI on a module map miss | Med | Low | Each phase that adds or deletes a `.rs` file updates its crate's `AGENTS.md` in the same commit |
| Sharded scores are not comparable across calls | Closed | High if true | P0b measured it: shard effect is inside the run-to-run noise floor (p95 0.040 vs 0.030). No change needed |
| `classifier-dev` is not repeatable: 23% of notes get a different tag set between byte-identical calls | Certain, measured | Med | Union-on-reingest (P3) absorbs it at ingest. `--retag` (replace) does not; see the row below. G5 restated |
| `--retag` deletes the migrated `work`/`life`/`homelab`/`diy`/`writing` tags: the classifier recovers 0 of 17 and 14 of 17 are actually lost (3 are saved only by "never writes empty") | Closed by panel r4 OQ5 | High if unfixed | `no-classifier-tags` protect list in P1, honored by `--retag` in P2. Option B (demote `work` to provenance) disqualified: `cortex/src/classify.rs:174-179` derives it from topical tags, and only 42 of 145 work-domain notes are under `work/` |
| Hysteresis or a lower threshold is assumed to fix classifier churn | Closed by panel r4 OQ6 | Low | Simulated and rejected: add >= 0.90 / keep >= K gives 12/53 at no hysteresis, 14/53 at K=0.87, 10/53 at K=0.80. Churn is dominated by *upward* crossings, which hysteresis does not guard |
| P7 assumes every ex-`resources` note receives an Addendum A tag automatically | Certain | Low | Falsified in trial: the cave note scores `entertainment 0.87` and clears nothing. P7 states that some of the 36 need hand assignment, consistent with Addendum B3 |
| Free-tier quota (20,000 fast classifications per IP per day) blocks a tuning cycle | Closed 2026-09-20 | Med | Moot on Pro. The live probe below returns `ratelimit-policy: 30000;w=60, 200000;w=86400`, the Pro policy, metered per account rather than per IP. One full retag is 5,440 calls (2.7% of a Pro day) against 27% of a free day |
| `CLASSIFY_API_KEY` is not usable: `keep/.secrets/classify-api-key.age` was 279 KB and decrypted to a PNG (every other API-key secret is ~450-510 bytes), so the env var it produced was the 8-byte PNG magic header, truncated at the first NUL | **Closed 2026-09-20** | High | Fixed and verified. `classify-api-key.age` is now 605 bytes; a fresh shell exports a 144-character `CLASSIFY_API_KEY`; `POST https://classifier.dev/v1/classify` with `Authorization: Bearer` returns **HTTP 200**, `tier: fast`, `model: jev-1.13.0`, correct scores (`ai .97`, `privacy .95`, `football .01`, `cooking .01`) and the Pro rate-limit policy. `keep/manifest.yml` is committed (`e40789a`, "added jev classify api key"), so the P6 operator step is now only `manifest secrets env` plus a daemon restart |
| `CLASSIFY_API_KEY` stopped working the same day it was verified: classifier.dev answers `HTTP 401 {"error": "Invalid API key. Create a workspace key at /app/keys.", "code": "invalid_api_key"}` | **REOPENED 2026-09-20, during P7** | High | Not a code fault and not a secret-plumbing fault. The decrypted value is unchanged and well-formed (144 characters, no whitespace, no quotes, the length this table recorded at 200 OK), and **raw `curl` with `Authorization: Bearer` gets the same 401**, so `ClassifierDev` is exonerated: the account or workspace key is what changed. The keyless public path is also unavailable on this host right now - `HTTP 429 rate_limit_day`, "20000 fast classifications per IP per day", spent by the P0b trial. Consequence measured in P7: every `classify` call falls through to the configured `deterministic` fallback with a visible WARN, which is the designed behavior, but it means the 19 untagged ex-`resources` notes recovered nothing automatically and were hand-assigned. Needs Scott at classifier.dev `/app/keys`; nothing in second-brain or `keep` can resolve it |
| Generated `notes/ai/**` digests become `required.tags` failures with nothing sensible to tag them with | Certain as originally drafted | Med | `notes/ai/**` added to the tags exemptions, matching the domain rule at `cortex.yml:34` |

## Open Questions

None. OQ5 was opened by the P0b measurement on 2026-09-20 and closed the same day by panel round 4; see Resolved Decisions.

## References

- `docs/design/2026-03-21-cortex-classify-promote.md`: classify pipeline, Tier 1/2/3, borg stripped of domain
- `docs/design/2026-03-23-tag-sweeper.md`: canonical vocabulary, `vault::canonical`, `cortex sweep`, proposals; Alternative 2 (constrained Fabric prompt)
- `docs/design/2026-03-23-domain-expansion.md`: first `resources` remediation; `value-renames`; rejected config-driven domains
- `docs/design/2026-03-30-reingest-domain-preservation.md`: `CORTEX_PRESERVE_KEYS`, scalar preserve parser, catch-up classify
- `docs/design/2026-07-17-harvest-clyde-sessions.md`: session notes, scope and redaction tags
- `docs/design/2026-09-05-discovery-remediation.md`: schema docs rendered from `vault::schema`, `bin/agents-map`, lint counts
- Research brief (session scratchpad `tags-only-brief.md`, 303 lines, `main` @ a1415bc): every `path:line` above
- Panel r2 synthesis: `/tmp/review-panel/MLUDM7sw/synthesis-r2.md`; r3: `/tmp/review-panel/MLUDM7sw/synthesis-r3.md`
- Handoff `handoff-vault-domains.md` (session 6e6c44f5): classifier back-tests, 16-candidate sweep, calibration, rate limits
- `~/repos/.claude/rules/taste.md`: derived fields, two-signals rule, phasing, evidence standards

## Addendum A: expected re-tags for the 36 ex-`resources` notes

38 files carry `domain: resources` on 2026-09-20: the 36 notes below, `system/templates/book.md` (a template, handled in P10), and one quarantined copy under `system/quarantine/` (excluded from indexing). Hand-classified from titles and summaries; P7's `--retag` output is compared against this. A note is a pass if it carries at least one of its expected tags.

| bucket | expected tags | notes |
|---|---|---|
| gaming (7) | `gaming`, `noita` | five Noita videos, Off The Grid, Rise of the Dad Game |
| entertainment (8) | `entertainment`, `reading`, `books`, `fiction` | two sci-fi movie lists, Heretic trailer, Severance, Netflix secret menu, Hugo Awards, Nebula Awards, `illium` (kept, Scott 2026-09-20) |
| politics and money (7) | `politics`, `finance` | Boeing (LWT), UFOs (LWT), Epstein files, Galloway, Reeves, income-by-state, `fastcompanycom` (Red Lobster and private equity) |
| science (4) | `science` | James Webb cosmology, natural selection sim, game theory, the cave |
| pkm (2) | `pkm`, `obsidian`, `note-taking` | Obsidian 10 tips, PhD note-taking |
| food (2) | `cooking` | waffle hack, Dairy Queen |
| ai (1) | `ai`, `llm`, `privacy` | Ollama + Open WebUI |
| misc (4) | any canonical | 3D relief maps, PortlandMaps, Sweden vs Canada hockey, `when-karma-hits-back` (kept unless Scott says otherwise) |
| index (1) | `pkm` or exempt | `home.md` (vault index; today `domain: resources` at high confidence) |

## Addendum B: independent due-diligence pass (2026-09-20)

Run before ready-to-build at Scott's instruction, against `main` a1415bc and the live vault, with an independently written frontmatter parser rather than this doc's own commands. Artifacts: `~/Claude/tags-only-trial/` (`scan.py`, `sample.py`, `trial.py`, `notes.json`, `sample.json`, `micro.json`, `vocab.json`).

### B1. The doc's numbers hold

| claim | doc | re-derived |
|---|---|---|
| parseable notes carrying `domain:` | 2,748 | 2,744, plus 5 with an empty value |
| files the P4 migration would write | 2,545 | 2,541 |
| frontmatter tag `ai` | 1 | 1 |
| `resources` notes in `notes/` | 36 | 36 |
| notes at exactly 7 tags | 491 | 515 vault-wide; 489 of them do not already carry their domain as a tag |
| eval queries using a domain filter | 0 of 9 | 0 of 9; `config/eval/queries.yml:5` states it outright |

Two corrections, neither of which changes a decision:

- The "Vault, measured 2026-09-20 on `~/repos/scottidler/obsidian`" distribution quotes **index** figures for `ai` (1148) and `work` (99). The vault itself is `ai` 1192, `tech` 847, `football` 302, `work` 145, `life` 86, `homelab` 47, `resources` 38, `writing` 20, `diy` 18, `music` 18, `spanish` 17, `system` 14.
- 108 of the 110 canonical tags are in use and **zero** non-canonical values appear anywhere in vault frontmatter (`nix` and `synth` are the two unused). That corroborates this doc's reading that the 9,020 `tags.non-canonical` hits are the legacy 11-entry `cortex.yml:50-63` list, not dirty data: `tags` is already a governed field, and this doc promotes a mechanism that works rather than inventing one.

### B2. The classifier, exercised on real notes

53-note stratified sample, real bodies (title + first 1,500 chars), the 117-label proposed vocabulary, `max_labels: 8`. One full shard-A pass landed before the free-tier daily quota ran out:

- Live: `jev-1.13.0`, `tier: fast`, 124 ms for 3 notes and 366 ms for 53 texts against 59 labels. Throughput is not a constraint.
- Scores are per-label independent, not a softmax: an AI note returned `ai .99, future-of-work .97, enterprise-ai .96, agents .92, autonomous-agents .89, human-ai-collaboration .87, chatgpt .85, automation .81`, summing far above 1.0.
- `super-easy-rpos-to-run-in-the-spread-offense.md` returned `football .99, coaching .96, air-raid .95`, with the junk (`defense .75`, `drills .75`, `books .70`) all below 0.9. The 0.9 threshold does the work this doc claims for it.
- `homeowner-finds-massive-cave-beneath-his-house.md` (`domain: resources`, no tags) returned `entertainment .92` in a 3-note micro probe, which read as G7's intended outcome. **In the full 53-note trial the same note scores `entertainment 0.87` and clears no label at all** (next: `travel 0.58`, `life 0.55`). Retained here as a correction, not as evidence: the micro-probe figure should never have been quoted alongside trial results. It also falsifies P7's acceptance criterion as written, which assumes all 36 ex-`resources` notes receive an Addendum A tag; some will need hand assignment.

Full trial, 53 notes, 117 labels, six calls, 318 classifications, on the Pro tier (`30000;w=60, 200000;w=86400`), 581-821 ms per call:

| measurement | identical repeat (merged) | different shard split |
|---|---|---|
| score delta p50 | 0.010 | 0.010 |
| score delta p95 | 0.040 | 0.040 |
| score delta max | 0.170 | 0.180 |
| notes whose final tag set changes | 12 of 53 | 10 of 53 |

(Corrected by panel r4. The first version of this table put 0.030 / 0.130 in the left column, which was the shard-A-only noise floor compared against a merged cross-partition figure. Like-for-like the two columns are indistinguishable, which is a stronger PASS than the mismatched comparison suggested.)

**Sharding is safe.** Re-partitioning the 117 labels moves nothing beyond what two byte-identical requests already move. The worry that motivated P0b is closed, and the vocabulary does not need to be capped at 100.

**The service is not repeatable, and that is the finding that matters.** 23% of notes get a different tag set from one pass to the next, 14 labels crossing 0.9 in one run and not the other. `llms.txt` says so plainly: "Neither tier guarantees identical answers across calls." G5 is restated accordingly; union-on-reingest (P3) is what makes ingest stable, not the classifier.

**Quality against the existing human tags:** precision 0.340, recall 0.447, 3.77 predicted tags per note against 2.87 existing, 49 of 53 notes get at least one tag at 0.9. Precision reads low mainly because the classifier is more generous than the current pipeline and the existing tags are not ground truth: on `review-kms-key-rotation-security-policy.md` it returned `security, software-engineering, tech, devops, infrastructure` against a human `infrastructure, security`, which is richer, not wrong.

**Domain-value recovery, the second finding:** `tech` 10/10, `ai` 8/10, `football` 6/6, `music` 2/2, `spanish` 2/2, and `work` 0/6, `life` 0/4, `homelab` 0/3, `writing` 0/2, `diy` 0/2. Full measured scores on their own notes: `work` [0.48, 0.50, 0.52, 0.52, 0.58, 0.80], `life` [0.33, 0.45, 0.79, 0.87], `writing` [0.67, 0.77], `homelab` [0.09, 0.33, 0.68], `diy` [0.10, 0.85]. These are **two failures, not one**: `work`, `life` and `writing` are context collisions where the signal is present but a topical tag wins (nothing in the text of a Tatari platform review distinguishes it from any other Kubernetes note), while `homelab` and `diy` are flat absences on part of their corpus that no threshold reaches. The first write-up called all five "consistent near-misses, 0.33-0.87", quoting only the top of each range; panel r4 corrected it. 14 of the 17 are actually lost under `--retag`, the other 3 being saved only because the fresh result is empty and `--retag` never writes empty. Closed on option A; see Resolved Decisions.

### B3. Decision: the fallback story for the untagged backlog

The concern was that `fallback: deterministic` cannot help the notes that most need help, because `Deterministic` consumes only existing tags and `author-tags` as candidates and those notes have neither. Measured, the backlog is 507 files in `notes/` + `work/` (465 + 42), and it is three populations, not one:

| population | count | who tags it | classifier calls |
|---|---|---|---|
| has a propagated `domain` value | 438 | the P4 migration writes the domain value as the tag | 0 |
| generated rollups under `notes/ai/**` (`type: digest`, `type: review`) | 50 | nobody: added to the tags exemptions, matching `cortex.yml:34` for `domain` | 0 |
| `resources` residue | 19 | P7 runs `--retag` over all 36 ex-`resources` notes; whatever clears 0.9 is automatic and the rest are hand-assigned against Addendum A | up to 36, some yielding nothing (the cave note peaks at 0.87) |

**Decision: no fallback gap exists, and no new mechanism is needed.** What the finding changes is ordering and one schema line:

1. `notes/ai/**` joins the tags exemptions (folded into the Data Model table above). Without it these 50 become permanent `required.tags` failures that no classifier can sensibly clear, since a daily digest spans every topic in the vault.
2. `frontmatter.required.tags` must not be promoted to an error until **after** P4, because P4 is what clears 438 of the 507. The lint change belongs in P9, which is already where this doc puts it.
3. Once P4 lands, every note in scope carries at least one tag, so `Deterministic` can reproduce a compliant tag list for every existing note with no network at all. The classifier is load-bearing only for **new** ingests that arrive with no distiller tags and no author hashtags, and that path already fails visibly through the `degraded=true` receipt and `sb doctor`'s `degraded_24h` warning.

The residual exposure is therefore bounded to new ingest, which is what the existing mitigation was written for. No change to `TagClassifier`, no change to the confidence table, no new holding state.
