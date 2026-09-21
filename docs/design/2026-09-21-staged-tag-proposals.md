# Design Document: Staged Tag Proposals (restore open-vocabulary tag growth)

**Author:** Scott Idler (via agent)
**Date:** 2026-09-21
**Status:** Implemented (2026-09-21, phases 1-8; see `2026-09-21-staged-tag-proposals-implementation-notes.md`). Originally: Ready to build (panel rounds 1, 2 and 3 folded in full, round cap reached; the one pushback was accepted and withdrawn; Open Questions empty; every acceptance criterion's literal command run against `main` bff518d with output recorded)
**Review Passes Completed:** 5/5. Pass 2 (correctness) found four defects in the draft: the two candidate arms double-counted a note that appeared in both, "recompute" was contradicted by the non-empty write gates at both call sites (`cortex/src/sweep.rs:76`, `cortex/src/daemon.rs:820`), a wall-clock assertion had been written into a test, and AC3/AC5 were prose rather than commands. Pass 3 (clarity) turned `source` into an enum, dropped `Proposal::action` and `suggested_canonical` (constants with no reader), and recorded why Phase 1 ships alone. Pass 4 (edge cases) found the one that would have shipped broken: `otto deploy` runs `sb bootstrap --force`, which `write_always`-es `tag-proposals.yml` and `glossary.yml` from the repo, so the first populated queue would be wiped by the next deploy and every promotion silently reverted. That added Phase 3, the `--canonical` refusal on Phase 7, and two risk rows; it also renamed `Proposal::notes` to `sources`. Pass 5 (excellence) was the voice lint plus reconciling every phase cross-reference after the renumber. Every acceptance criterion's literal command was run against `main` bff518d and its output recorded. **Panel rounds 1, 2 and 3 found six, four and four must-fix respectively, all folded; see Addendums A, B and C.**

## Summary

`cortex sweep --proposals` cannot propose anything. It scans published note frontmatter, and since the tags-only migration nothing non-canonical can reach published frontmatter: measured on `main` bff518d, 12,737 tag assignments across 3,801 notes, zero outside the 117-tag vocabulary. Meanwhile the open-vocabulary proposer never went away. Twelve of borg's 26 fabric distill patterns still instruct the model to propose freely, and those raw candidates are already durably on disk in every staged trace's `distilled.yml`: 844 distinct non-resolving candidates, of which 112 clear the threshold of 3 once human rejects are suppressed and note identity is reconciled, sitting unread since 2026-08-05. This design feeds the existing proposal machinery from staging, fixes the five defects in that machinery that only bite once it carries data, and adds `sb cortex tag-promote` so a candidate can reach `canonical-tags.yml` through a reviewable diff.

## Problem Statement

### Background

All numbers measured on `main` bff518d, 2026-09-21, against the live vault (`~/repos/scottidler/obsidian`), the live staging root (`~/.local/share/sb/borg/stages`), the live receipts DB, and the deployed config (`~/.config/sb/`). **The staging corpus is live and grows under the daemon:** it was 658 distilled traces / 46,295 trace dirs at 07:18 and 659 / 46,296 by 07:51, one trace having landed during this doc's own review round. Every figure below is as of 07:18 unless noted. This is why the counted assertions in Phases 5 and 7 run against a frozen fixture and the live corpus appears only as a `>=` rollout check.

- The proposal loop was designed in `docs/design/2026-03-23-tag-sweeper.md`: an unmapped tag seen on `proposal-threshold` (default 3) or more notes lands in `tag-proposals.yml`, a human promotes the winners into `canonical-tags.yml` (`:37`, `:88`, `:123-141`). Alternative 4 in that doc, a strict allowlist with no proposal queue, was rejected: "the proposal queue adds minimal complexity while keeping the system adaptive" (`:318`).
- `scan_proposals` (`cortex/src/sweep.rs:195`) reads `note.frontmatter.tags`, keeps any tag where `match_to_canonical` returns empty, and thresholds by distinct notes. Two callers: `sweep::run` (`cortex/src/sweep.rs:75`, reached by a bare `sb cortex sweep`) and the daemon's on-change sweep arm (`cortex/src/daemon.rs:819`).
- `~/.config/sb/tag-proposals.yml` is `proposals: []`. It has never been written.
- Tags-only (`docs/design/2026-09-19-tags-only-classification.md`) made `tags` the sole classification facet and routed ingest through a closed-vocabulary `TagClassifier`. It listed the proposal path as an explicit Non-Goal (`:60`, "Changing how tag proposals are reviewed (`tag-proposals.yml` stays a human queue)"), so the starvation is a consequence of a scoped-out area, not an oversight.
- `config/canonical-tags.yml`: 117 tags in 12 editorial groups, `max-per-note: 8`, `max-canonical: 300`, `no-segment-match: [tech, work, life, homelab, diy]`, `no-classifier-tags: [work, life, homelab, diy, writing]`, zero comments. `config/tag-mapping.yml`: 4,555 entries, 1,411 of them `null` (explicit human rejects).
- `vault/src/distilled.rs:57-58` documents `Distilled.tags` as "Canonical tags applied by the extractor, post-filtered against `canonical-tags.yml`. Max 7." Both halves are false. `distillers/AGENTS.md:20` repeats it.
- Staging: 46,295 trace directories, 439 MB allocated (~82 MB of content; 46k mostly-tiny directories at a 4K block each). 658 carry a `distilled.yml` (10.6 MB total), 0 unparseable, 620 carry at least one tag, `meta.produced-at` spans 2026-08-05 .. 2026-09-21. `staging.retention-days: 14` exists but the sweep is manual: the deployed `~/.config/sb/borg.yml:133` says "Staging is NOT swept automatically: run `sb borg retention sweep`". Nobody has run it, which is why the window is 47 days and not 14.
- Cortex already reads that path read-only: `read_staged_transcript` (`cortex/src/embed.rs:1143`) joins `<staging-root>/<trace>/distilled.yml` via the note's `trace:` frontmatter, with the root from `EmbedConfig.staging_root` (`cortex/src/config.rs:294`) defaulting to `vault::paths::borg_stages_dir()`.
- `cortex::entities` is the complete build of this same pattern for glossary concepts: `discover` aggregates with a `MAX_SAMPLE_NOTES: usize = 5` provenance cap (`cortex/src/entities.rs:106`), `promote_concept` (`:221`) promotes one slug through a printed diff, dry-run by default. Its module doc says it was modeled on `tag-proposals.yml`.

### Problem

The system throws away the only open-vocabulary signal it produces, and the machinery built to catch that signal has been running against an input that is empty by construction.

- **The scanner is structurally starved.** `filter_and_cap` (`vault/src/canonical.rs:170`) drops every unresolvable candidate, and `finalize_tags` (`borg/src/pipeline/tags.rs:129`) routes ingest through a closed-vocabulary classifier that cannot mint a 118th tag. So no note can carry a novel tag, so `scan_proposals` can never see one. Measured, not inferred: 12,737 assignments, 0 non-canonical. `sb cortex sweep --proposals --dry-run` prints "No new tag proposals." over 3,803 notes.
- **The signal exists and is already paid for.** `borg/patterns/distill-article.md:89-93` tells the model to "propose up to 7 lowercase candidate tags ... A downstream canonical-vocabulary filter gates and caps these; propose freely from the content, don't try to guess the canonical vocabulary yourself." 12 of the 26 deployed patterns carry that instruction. `write_distilled_yml` (`borg/src/stages/distill.rs:306`) writes those raw candidates to disk **before** `merge_proposed_tags` and `finalize_tags` (`borg/src/pipeline.rs:881`) filter them. Confirmed on disk: `stages/hv-d6b63698/distilled.yml` holds `tags: [security-review, slack-cli, cargo, versioning]`, none canonical, against a landed note carrying `tags: [cli, security]`.
- **The docs said the opposite, which is why nobody looked.** `vault/src/distilled.rs:57-58` claims the field is post-filtered and capped at 7. It is neither.
- **Three defects in `write_proposals` are latent only because the input is empty.**
  - A corrupt `tag-proposals.yml` is silently replaced: `load_proposals(path).unwrap_or(empty)` (`cortex/src/sweep.rs:240`). A hand-edited queue would be discarded without a word.
  - The write is a bare `std::fs::write` (`:253`) while the note-rewriting sibling twelve lines down uses `vault::note::write_atomic` (`:271`). The daemon writes this file on a tick.
  - It only overwrites and appends, never removes (`:243-250`). The queue would grow monotonically. `~/.config/sb/entity-proposals.yml` is 212 KB, which is what the unbounded version looks like.
- **A human-rejected tag would be re-proposed forever.** `docs/design/2026-03-23-tag-sweeper.md:141` promised the opposite: a `null` mapping "prevents the sweeper from re-proposing them". The code never implemented it. `match_to_canonical` returns `vec![]` for both a `null` mapping and no match at all (`vault/src/canonical.rs:131-136`), and `scan_proposals` treats empty as non-canonical (`:209`). Latent today; live on the first staged scan, because 13 of the 126 eligible candidates are already explicit rejects: `session-management` (16), `documentation` (10), `yaml` (8), `json` (8), `macos`, `hooks`, `search`, `plugins`, `backup`, `harness-engineering`, `nodejs`, `skills`, `customization`.
- **Two config keys would name the same directory.** `EmbedConfig.staging_root` already resolves borg's staging root for cortex. A `sweep.staging-root` beside it is two signals encoding one meaning, and they can drift.
- **`otto deploy` overwrites the queue and would revert every promotion.** The deploy task runs `sb bootstrap --force` (`.otto.yml:246`), whose `extract_canonical_assets` (`sb/src/cli/bootstrap.rs:258-281`) `write_always`-es four shared YAMLs from the binary's `include_str!` copies of `config/`: `canonical-tags.yml`, `tag-mapping.yml`, **`tag-proposals.yml`**, and `glossary.yml`. Two consequences:
  - A populated `~/.config/sb/tag-proposals.yml` is reset to `proposals: []` by every deploy. The queue is generated state living in the shipped-config set.
  - The precedent is already broken the same way. `promote_concept` writes `vault::paths::glossary()`, which is `~/.config/sb/glossary.yml`, and the next deploy restores it from `config/glossary.yml`. Measured: the two files are byte-identical today, which is consistent with no concept ever having survived a promotion. Copying `concept-promote`'s path resolution verbatim would reproduce the bug for tags.

### What is in the corpus

844 distinct non-resolving candidates across the 658 staged traces of the 07:18 snapshot, counting by distinct trace: 240 at frequency >= 2, **126 at >= 3**, 65 at >= 5, 21 at >= 10. 113 of the 126 are absent from `tag-mapping.yml` entirely; 13 are prior human rejects.

That is the naive count. Two of this design's own corrections move it, and the shipped number is the third row:

All three rows are the **659-trace corpus** measured at 08:2x, not the 658-trace 07:18 snapshot the bullets above use; mixing the two is how 844 and 799 came to sit in one table.

| Counting rule (659-trace corpus) | distinct candidates | at >= 3 |
|---|---|---|
| by trace, rejects included | 846 | 126 |
| Phase 1: rejects suppressed (47 of the 846, 13 of the 126) | 799 | 113 |
| Phase 5: plus note-identity reconciliation | 799 | **112** |

The one candidate the reconciliation removes is `system-prompt`, which reached 3 only by counting one note twice. Quote the third row; it is what the queue will hold.

Top 25 eligible, by distinct traces:

```
ci-cd 46   jsonl 33   ci 32   clyde 28   marquee 24   cargo 23   helm 20
okta 18    session-management 16*   token-usage 16   logging 15  refactoring 15
caching 15 supply-chain 15  opentelemetry 13  backstage 12  static-analysis 10
dashboard 10  shell-scripting 10  slack 10  documentation 10*  release-process 9
cost-tracking 9  show-hn 9  argocd 9                    (* = already null-mapped)
```

Raw frequency is a different and misleading measure. The raw top 5 is `security-review` 218, `rust` 215, `claude-code` 205, `developer-tools` 134, `ai-agents` 84, and **all five** resolve, so not one of the raw leaders is a candidate: `rust` is an exact member of the 117-tag vocabulary, `security-review` segment-matches `security`, and `claude-code`, `developer-tools`, `ai-agents` map to `claude`, `cli`, `agents`. Mapping and segment matching, not the classifier, absorb the bulk of the raw output. Quote eligible frequency or raw frequency, never both without the label.

### Goals

- **G1** Feed the proposal scanner from the staged `distilled.yml` candidates, so the vocabulary can grow again. (Scott, 2026-09-21: "the larger issue is that ingestion see 3+ instances of a new [tag] ... what would it take to bring this functionality back with the new jev classifier api?")
- **G2** Give a proposal provenance: which notes a candidate would tag, so it can be judged before promotion. (Scott, 2026-09-21, by invoking this doc on the recommendation that named provenance as item 2.)
- **G3** `sb cortex tag-promote <tag>... --group <g> [--apply]`, dry-run by default, copied from `concept-promote`, so promotion is a reviewable diff and not a hand edit. (Same.)
- **G4** Close the retention-window and recompute-vs-accumulate question explicitly rather than leaving the counts to drift. (Same.)
- **G5** Fix the five machinery defects that only bite once the queue carries data: the reject leak, the silent corrupt-file replace, the non-atomic write, the monotonic growth, and `sb bootstrap --force` wiping the queue on every `otto deploy`. (Author, from the evidence above. Each is a `rules/taste.md` fail-loudly or truth-in-naming violation that ships the moment G1 lands. The last one also fixes the same bug against `glossary.yml` and `concept-promote`, which is a defect that exists today.)
- **G6** Correct `Distilled.tags`' documentation. (Author. The false doc comment is the reason six weeks of signal went unread.)

### Non-Goals

- **Any change to jev / `classifier.dev`.** Closed-vocabulary scoring is the right shape for assignment. The open proposer is fabric, already running, already paid for. This is entirely a read-side feature.
- **Auto-promotion at threshold.** Promotion stays human-gated. `session-management` at 16 and `documentation` at 10 are both already human rejects, which is the argument.
- **Changing how ingest assigns tags.** `finalize_tags`, `filter_and_cap`, and the `TagClassifier` implementations are untouched.
- **Re-adding `resources` and `system` as tags.** Scott asked whether the two dropped domains could come back; he has not said to do it. Revisit condition: he says so. Recorded in the Addendum with what it would cost.
- **Populating or deleting the empty `system: []` group** in `canonical-tags.yml` (`:129`), which sits beside a `system: null` mapping. Adjacent smell, not this doc's scope. `tag-promote --group` will validate against the existing group keys and will therefore accept `system`; that is correct behavior for an empty group.
- **An LLM clustering pass over the proposal queue.** Still open from `2026-03-23-tag-sweeper.md:363`, still deferred: raw frequency with provenance is what a human needs to decide.
- **Incremental or cached staged scanning.** The straight scan is measured in P5; a cache is a capacity feature and waits until the measurement says it is a problem.
- **Automating `sb borg retention sweep`.** Out of scope, but see the Risks table: running it before this ships collapses the corpus from 47 days to 14.

## Proposed Solution

### Overview

Three moves, smallest and most independent first:

1. **Harden the queue before it carries anything** (P1-P3): stop re-proposing human rejects, fail loudly on a corrupt queue, write atomically, stop the queue growing monotonically, and stop `otto deploy` wiping it.
2. **Add the second read-only staging edge** (P4-P6): hoist the staging root to one cortex-wide key, read the staged candidates, union them with the note-derived candidates into one aggregator, and thread the result through the existing `scan_proposals` contract so both the CLI and the daemon pick it up with no new flag.
3. **Add the promote path** (P7): `sb cortex tag-promote`, dry-run by default, targeted textual insert into `canonical-tags.yml`, guarded by `max-canonical`.

Then correct the docs (P8).

### Architecture

```
borg (sole staging WRITER)
  write_distilled_yml  borg/src/stages/distill.rs:306
        |                     writes RAW candidates, pre-filter
        v
  <staging-root>/<trace>/distilled.yml        [658 files, 10.6 MB]
        |
        |  READ-ONLY edge #2 (edge #1 is cortex embed's transcript read)
        v
cortex::proposals                              NEW module
  read_staged_candidates(staging_root)  ->  Vec<StagedCandidates>
  aggregate(note_candidates, staged_candidates, canon, mapping, threshold)
        |                                  ^
        |                                  |  note-derived candidates
        |                          (hand-edited tags in Obsidian, the only
        |                           other live open-vocabulary input)
        v
cortex::sweep::scan_proposals   ->  Vec<Proposal>  -> write_proposals (atomic, overwrite)
        |
        v
  ~/.config/sb/tag-proposals.yml              a rendered view of one scan window
        |
        v
sb cortex tag-promote <tag>... --group <g> --canonical <p> [--apply]   NEW
        |   dry-run prints the diff, writes nothing; --apply refuses the
        |   DEPLOYED copy, because otto deploy regenerates it from the repo
        v
config/canonical-tags.yml  (targeted textual insert into the named group)
        |   git commit, then `otto deploy` -> ~/.config/sb/canonical-tags.yml
        v
next classifier call loads the new vocabulary; `sb cortex classify --retag` backfills
```

Nothing in cortex opens a borg writer path. The new edge is read-only and precedented by `cortex/src/embed.rs:1143`.

### Data Model

**`Proposal`** (`cortex/src/sweep.rs:115`) gains provenance, loses two dead fields, and renames one.

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProposalSource { Note, Staged, Both }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Proposal {
    pub tag: String,
    /// Distinct SOURCES carrying this candidate, where a source key is the
    /// note path when one is known and `trace:<id>` when it is not. A staged
    /// trace that resolves to a note collapses with the note-derived entry
    /// for that same note, so the two arms cannot double-count one note.
    pub frequency: usize,
    /// Sample provenance, capped at 5 to match
    /// `cortex::entities::MAX_SAMPLE_NOTES`. Vault-relative note paths where
    /// they resolve, `trace:<id>` where they do not. RENAMED from `notes`,
    /// which stopped being true the moment a staged-only candidate could
    /// land here.
    pub sources: Vec<String>,
    /// NEW. Which arm(s) produced this candidate.
    pub source: ProposalSource,
}
```

`suggested_canonical` and `action` are **dropped**. Both have been constants since 2026-03-23: `scan_proposals` hardcodes `None` and the literal `"review"` (`cortex/src/sweep.rs:225-226`), and a repo-wide grep for `suggested_canonical` and `.action` finds no reader of either. Shipping `deny_unknown_fields` on a struct carrying two fields that never vary is the worse of the two options. `suggested_canonical` comes back if and when a merge-suggestion feature is built; that is what `docs/design/2026-03-23-tag-sweeper.md:131-138`'s `action: "merge"` example was for, and it was never implemented.

**`ProposalsFile`** (`:125`) gains the house serde attributes it is missing and a scan header.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ProposalsFile {
    /// UTC timestamp of the scan that produced this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanned_at: Option<String>,
    /// Oldest and newest `meta.produced-at` in the staged corpus this scan
    /// read, so a reader can tell what window the frequencies cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_window: Option<String>,
    pub proposals: Vec<Proposal>,
}
```

`proposals: []` parses unchanged under these attributes (both new fields default).

**Config.** `staging_root` moves from `EmbedConfig` to the top-level `Config` (`cortex/src/config.rs:9`):

```rust
pub struct Config {
    ...
    /// Root of borg's per-trace staging directories. cortex reads
    /// `<staging-root>/<trace>/distilled.yml` READ-ONLY from two places: the
    /// embed loop's transcript source and the sweep's open-vocabulary tag
    /// candidates. Defaults to borg's own `vault::paths::borg_stages_dir()`;
    /// an operator who overrode borg's `staging.root` must point this at the
    /// same directory. borg remains the sole staging writer.
    ///
    /// The explicit `rename` is load-bearing: `EmbedConfig` carries
    /// `rename_all = "kebab-case"` but top-level `Config` does not, it
    /// renames per field (`log-level`). Without it the YAML key would
    /// silently be `staging_root` and `staging-root:` would be ignored.
    #[serde(rename = "staging-root", deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]
    pub staging_root: PathBuf,
}
```

`EmbedConfig` gains `deny_unknown_fields` in the same phase, so a leftover `embed.staging-root` errors instead of being silently ignored. Neither the deployed `~/.config/sb/cortex.yml` nor any dotfiles host sets the key today (measured: zero hits), and `config/templates/cortex.yml.example:56` carries it commented out, so this is a template edit and no operator action.

`SweepConfig` (`:559`) gains one key:

```rust
/// Read open-vocabulary tag candidates from borg's staged `distilled.yml`
/// files in addition to note frontmatter. The note-derived arm alone is
/// structurally empty under tags-only: every published tag is canonical by
/// construction, so this is where vocabulary growth comes from.
pub staged_proposals: bool,   // default true
```

### API Design

New module `cortex/src/proposals.rs`, lib-only, no printing:

```rust
/// One staged trace's raw, pre-filter candidate tags.
pub struct StagedCandidates { pub trace: String, pub tags: Vec<String> }

/// Walk `<staging_root>/*/distilled.yml` and return the raw `tags` of each.
/// Failure to enumerate the root is an Err: the caller overwrites the queue
/// unconditionally, so "could not look" must never read as "nothing found".
/// A trace dir with no `distilled.yml` is the ordinary case and is counted,
/// not logged. A file that exists but cannot be read or parsed is a WARN and
/// an omission, mirroring `cortex::embed::read_staged_transcript`.
pub fn read_staged_candidates(staging_root: &Path) -> Result<Vec<StagedCandidates>>;

/// trace id -> vault note path, read from borg's receipts DB READ-ONLY, the
/// way `oracle/src/server.rs:563-580` reads it for `failure_history`. This is
/// the note-identity source: the note `trace:` frontmatter is a one-way
/// note-to-trace map and cannot resolve a superseded trace back to its note.
/// A missing or unreadable DB is an Err, for the same reason the staged
/// reader's root failure is: silently returning an empty map restores exactly
/// the double-counting the reconciliation removes, and the operator sees a
/// clean scan. Every key it returns is VAULT-RELATIVE, matching `Note.path`,
/// so the two arms share one key space.
pub fn resolve_trace_notes(receipts_db: &Path, notes: &[Note]) -> Result<HashMap<String, String>>;

/// Pure. No I/O. Unions the two candidate arms, resolves each raw tag through
/// the canonical tiers, drops anything that resolves AND anything explicitly
/// rejected, counts distinct SOURCE KEYS (note path where known, else
/// `trace:<id>`, so the arms cannot double-count one note), thresholds, and
/// attaches capped provenance. Deterministic ordering: frequency descending,
/// then tag.
pub fn aggregate(
    note_candidates: &[(String, Vec<String>)],   // (note path, tags)
    staged: &[StagedCandidates],
    trace_to_note: &HashMap<String, String>,
    canon: &CanonicalSet,
    mapping: &TagMapping,
    threshold: usize,
) -> Vec<Proposal>;
```

New in `vault/src/canonical.rs`, so both arms and any future caller share one definition:

```rust
/// True when `raw_tag` carries an explicit `null` entry in the mapping file,
/// i.e. a human already rejected it. `match_to_canonical` returns an empty
/// vec for BOTH this and "no match at all"; a proposal scanner must not
/// conflate them or it re-proposes every rejection forever.
pub fn is_rejected(raw_tag: &str, mapping: &TagMapping) -> bool;
```

New CLI, copied from `ConceptPromoteArgs` (`sb/src/cli/cortex.rs:102-109`):

```
sb cortex tag-promote <TAG>... --group <GROUP> [--canonical <PATH>] [--apply]

  <TAG>...          One or more tags to promote (each must be a pending
                    tag-proposals.yml entry).
  --group <G>       The canonical-tags.yml group to insert into (must exist).
  --canonical <P>   The canonical-tags.yml to edit. Defaults to the deployed
                    copy, which --apply refuses: pass the repo's
                    config/canonical-tags.yml, then commit and `otto deploy`.
  --apply           Write the change. Without it, prints the diff and writes
                    nothing.
```

Library: `cortex::proposals::promote_tags(proposals_path, canonical_path, tags, group, apply) -> Result<PromoteReport>`, same three-branch shape as `promote_concept` (already-present / applied / dry-run), same hard bail when a tag does not trace to a pending proposal.

**Why a targeted textual insert and not a serde round-trip.** `CanonicalTagsFile` (`vault/src/canonical.rs:11`) derives only `Deserialize`, and its `tags` field is a `HashMap<String, Vec<String>>`. Reserializing the way `write_glossary` (`cortex/src/entities.rs:279`) does would require adding `Serialize` and would scramble the 12 editorial group keys into hash order on every write. An order-preserving serde path does exist (`serde_yaml::Value`'s `Mapping` is `IndexMap`-backed, and switching the field to `indexmap::IndexMap` works as well), so group order is not the obstacle. Layout is. No serde round-trip preserves sequence indentation, the empty-flow `  system: []` form at `:129`, or comments, and Phase 7's own criterion demands every line outside the touched group be byte-identical. `canonical-tags.yml` has zero comments; `config/glossary.yml` has a five-line header that `write_glossary` already destroys on any `concept-promote --apply`. Copy the command's shape, not its writer. `tag-promote` locates the group's key line, finds its last `    - ` entry, and appends after it. It never re-sorts: neither the 12 group keys nor the tags inside a group are ordered today. Everything else in the file is byte-identical.

### Implementation Plan

Nine phases (P0 is a zero-code spike, already run), one commit each, `otto ci` green at each.

#### Phase 0: Prove the corpus, zero code
**Model:** sonnet
- Aggregate the live staging root under `scan_proposals` semantics and count eligible candidates by distinct trace.
- Measure what fraction of staged traces resolve to a note path.
- Confirm at least one eligible candidate is an explicit `null` mapping.
- **Success criteria:** >= 100 distinct candidates at frequency >= 3; >= 90% of staged traces resolve to a note; >= 1 eligible candidate is a prior reject.
- **Already executed on `main` bff518d, 2026-09-21.** Observed: 126 at >= 3; 631 of 658 (95.9%) resolve via the note `trace:` frontmatter join, and 658 of 658 have a receipts row of which 657 carry a `note_path` (`ht-36c587bd` is `failed` with NULL). 13 eligible candidates are prior rejects. All three pass. Retained here as recorded method. Panel round 1 turned the second measurement from a footnote into the design: see Resolved Decisions.

#### Phase 1: Stop re-proposing human rejects
**Model:** sonnet
- Add `vault::canonical::is_rejected(raw_tag, mapping)`.
- `scan_proposals` (`cortex/src/sweep.rs:209`) skips a tag when `is_rejected` is true, before the `match_to_canonical` emptiness test.
- **Success criteria:** a new unit test with a note carrying a `null`-mapped tag asserts `scan_proposals` returns empty; the existing `test_scan_proposals_mapped_tags_not_proposed` (`cortex/src/sweep/tests.rs:388`) still passes; reverting the guard makes the new test fail (break-the-code evidence recorded in the commit).

#### Phase 2: Harden the proposals file
**Model:** sonnet
- `load_proposals` failure is propagated, not `unwrap_or`-ed into an empty file (`cortex/src/sweep.rs:240`).
- `write_proposals` uses `vault::note::write_atomic` (`:253`).
- `write_proposals` overwrites rather than merges: the file is a rendered view of one scan window, so a candidate that fell below threshold disappears instead of persisting at a stale frequency. Add `scanned-at` and `staged-window`.
- **Remove the non-empty gate at both call sites.** `sweep::run` writes only when `!proposals.is_empty() && !opts.dry_run` (`cortex/src/sweep.rs:76`) and the daemon arm matches on `Ok(proposals) if !proposals.is_empty()` (`cortex/src/daemon.rs:820`). Under overwrite semantics those gates make "recompute" a lie: a scan that drops to zero would leave the last non-empty file in place forever. Both write unconditionally when not `--dry-run`.
- Add `rename_all = "kebab-case"` and `deny_unknown_fields` to `ProposalsFile`; add `deny_unknown_fields` to `Proposal`; drop `Proposal::suggested_canonical` and `Proposal::action`; rename `Proposal::notes` to `sources`.
- **Success criteria:** a corrupt `tag-proposals.yml` makes `sb cortex sweep --proposals` exit non-zero naming the file; a proposal present in the file but absent from the new scan is gone after a write, including when the new scan is empty; the shipped `proposals: []` still parses.

#### Phase 3: Stop `sb bootstrap --force` clobbering generated state
**Model:** sonnet
- `tag-proposals.yml` is a scan artifact, not shipped config. Move it out of the `force`-overwrite branch of `extract_canonical_assets` (`sb/src/cli/bootstrap.rs:271`) into the write-if-missing set, alongside the per-host `borg.yml` / `cortex.yml` / `oracle.yml` templates. A fresh machine still gets its `proposals: []` seed; an existing machine keeps its queue across every `otto deploy`.
- `glossary.yml` has the same defect against `concept-promote`. It is the same two-line change in the same loop and it is fixed here, in the same commit, because leaving one of two identical bugs in place after naming both is not a defensible resting state.
- Update the `--force` help text (`sb/src/cli/bootstrap.rs:46-49`), which currently promises to refresh all three shared YAMLs including tag-proposals.
- Drop `tag-proposals.yml` from `shared_config_findings` (`sb/src/cli/checks.rs:247`). Once the file is machine-generated state it is *always* expected to differ from the embedded constant, so the drift check becomes permanent noise. It is `Finding::info` today, so nothing fails; it is still a line `sb doctor` prints forever about a healthy queue. `glossary.yml` is **not** in that list and never was: the three entries are `canonical-tags.yml`, `tag-mapping.yml`, `tag-proposals.yml` (`:244-247`). Do not add it to keep the count at three.
- `sb/src/cli/checks/tests.rs:127` asserts `errors.len() == 3` against a fresh `XDG_CONFIG_HOME`. Dropping `tag-proposals.yml` from the list makes that 2; update the assertion and its message in the same commit, or this phase turns CI red.
- **Success criteria:** with a populated `~/.config/sb/tag-proposals.yml` and a populated `~/.config/sb/glossary.yml`, `sb bootstrap --force --skip-systemd --skip-prefetch-model` leaves both byte-identical (`md5sum` before and after); with both absent, the same command creates them from the embedded copies; `canonical-tags.yml` and `tag-mapping.yml` are still overwritten under `--force`; `sb doctor` prints no `tag-proposals.yml` drift finding against a populated queue; `cargo test --workspace` green, `checks/tests.rs` now asserting 2. The existing force-overwrite test covers only `canonical-tags.yml` and `glossary.yml` has no fresh-extraction coverage at all, so this phase adds a preservation test for each of the two files it reclassifies.
- **Consequence to state in the commit:** existing installs stop receiving repo updates to `glossary.yml`. That is the point (a promotion must survive a deploy), and the operator path for a genuine upstream glossary change becomes deleting the local file and re-running bootstrap.

#### Phase 4: One staging root for cortex
**Model:** sonnet
- Move `staging_root` from `EmbedConfig` to `Config`; update both `cortex::embed` readers (`cortex/src/embed.rs:254`, `:388`) and the stale reference at `vault/src/paths.rs:303`.
- **The hoisted field needs an explicit `#[serde(rename = "staging-root")]`.** `EmbedConfig` carries `rename_all = "kebab-case"` (`cortex/src/config.rs:253-254`); top-level `Config` does not (`:7-8`, it renames per field, as `log-level` does). Without the explicit rename the hoist produces the worst outcome: `embed.staging-root` starts hard-erroring under the new `deny_unknown_fields` while top-level `staging-root:` is silently ignored and the default is used.
- Add `deny_unknown_fields` to `EmbedConfig` so a leftover `embed.staging-root` errors loudly.
- Move the commented key in `config/templates/cortex.yml.example:56` from under `embed:` to the top level, with the doc comment naming both readers. Add the new `sweep.staged-proposals` key to the same template, annotated, since that file documents every tunable.
- **Success criteria:** a test that parses **literal YAML text** (not a constructed struct) carrying top-level `staging-root: ~/x` resolves `embed`'s reader to that path; a `cortex.yml` carrying `embed: {staging-root: ...}` fails to parse with a message naming the key; `otto ci` green. The template check moves to Phase 6, where `staged-proposals` exists: `SweepConfig` has no `deny_unknown_fields` (`cortex/src/config.rs:559-560`), so "the template parses" cannot fail and proves nothing. Phase 6 asserts the parsed *value* instead.

#### Phase 5: Read and aggregate the staged candidates
**Model:** opus
- New `cortex/src/proposals.rs` with `read_staged_candidates` and the pure `aggregate`, plus `cortex/src/proposals/tests.rs`.
- `read_staged_candidates` deserializes into a minimal local struct (`{ tags, meta }`), not the full `Distilled`, and uses `rayon::par_iter` over the trace directories (`rayon` is already a direct `cortex` dependency).
- **`read_staged_candidates` returns `Result<Vec<StagedCandidates>>`, not a bare `Vec`.** Failure to enumerate the staging root is an `Err` that aborts the scan and leaves the prior queue untouched. Phase 2 makes the write unconditional, so a reader that cannot distinguish "nothing found" from "could not look" is a queue-wipe path. A trace directory with no `distilled.yml` is the ordinary case (45,637 of 46,296 today) and is counted, never logged; only a file that exists and cannot be read or parsed gets a WARN, and the report carries both counts.
- Within a trace, candidates are deduped and tag text is normalized through `vault::hygiene::sanitize_tag`, so one trace contributes at most 1 to any tag's frequency.
- `resolve_trace_notes(receipts_db, vault_root, notes) -> Result<HashMap<String, String>>` runs the four tiers in Resolved Decisions and returns vault-relative note paths. Tier 1 reads only `notes`; tiers 2 and 3 read borg's receipts DB read-only (`OpenFlags::SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_URI`), exactly the way `oracle/src/server.rs:563-580` does for `failure_history`. It needs `vault_root` because receipt paths are absolute and `Note.path` is relative (`vault/src/note.rs:12-13`): without it the function cannot relativize, cannot test existence, and cannot check vault membership, so tier 2 is unimplementable. Both callers already hold the root (`cortex/src/sweep.rs:66-75`; `cortex/src/daemon.rs:568` into `:819`); thread it through `scan_proposals`. An unreadable DB is an `Err`, per the host policy in Resolved Decisions.
- **The identity mechanics live in `vault`, not here and not in borg.** The note index, the `trace:` back-edge, and the `superseded-by` convergence go into a `vault` module beside `vault/src/tombstone.rs`, which already owns `SUPERSEDED_BY_KEY` (`:27`) and documents why independent implementations of this drift. cortex has no borg dependency and must not gain one: that inverts the one-way data flow the root CLAUDE.md pins. `borg::harvest::identity` is not reusable as-is either (it is a publish-replacement policy taking `ResolveIntent`, source, and body hash, with a process-lifetime cache, and its strict trace guard would reject the historical traces this scan must reconcile). Third home, not a dependency edge.
- `aggregate` drops a candidate that resolves through any canonical tier and any candidate where `is_rejected`, counts distinct source keys, sets `source` to note / staged / both, caps `sources` at 5, and sorts by frequency descending then tag.
- Opus rather than sonnet: the union semantics, note identity across superseded traces, and the tier-resolution ordering are judgment, not wiring.
- **Success criteria:** `aggregate` over hand-built inputs reproduces a known frequency and honors the threshold; a trace listing the same tag twice contributes 1; **two distinct traces resolving to the same note contribute 1, asserted against the live counterexample as a frozen fixture (`ht-970437f1` and `ht-ba9e3191`, both `succeeded` against `notes/pi-coding-agent-free-course.md`, both carrying `system-prompt`)**; an unresolvable trace still contributes its own `trace:<id>` key; a staging root that does not exist skips the staged arm and exits 0; a root that exists but cannot be enumerated is an `Err`, and so is an unreadable receipts DB with the staged arm running; a trace whose `trace:` matches six notes resolves to the single non-superseded one, and to `trace:<id>` when two survive or the chain cycles; a tier-3 candidate whose `source` differs from the receipt's is rejected; a malformed `distilled.yml` is one WARN and an omission. Counts are asserted against a frozen fixture tree, never the live staging root, which grew by one trace during this doc's own review. No wall-clock assertion in any test (a timing assert is a flake generator on a shared machine); the phase's commit message records the observed duration of one live read instead.

#### Phase 6: Wire the staged arm into sweep and the daemon
**Model:** sonnet
- `SweepConfig.staged_proposals` (default true), threaded into `scan_proposals`, whose signature gains the staging root. Both call sites change: `sweep::run` (`cortex/src/sweep.rs:75`) and the daemon arm (`cortex/src/daemon.rs:819`).
- Build `trace_to_note` inside `scan_proposals` via `resolve_trace_notes`, passing the vault root, the `notes` slice, and the receipts DB at `vault::receipts::receipts_db_path()`. Neither source alone is sufficient: the note `trace:` frontmatter join is one-way, so a superseded trace has no way back to its note and votes twice, and the receipt's `note_path` is a location snapshot that is stale for 637 of 658 traces (see Resolved Decisions).
- `print_sweep_report` (`sb/src/cli/cortex.rs:1017`) shows frequency, source, and the scan window.
- **Success criteria:** `sb cortex sweep --proposals --dry-run` on the daemon host reports >= 100 proposals and writes zero bytes (assert the file's bytes and mtime are unchanged); the same without `--dry-run` writes a `tag-proposals.yml` that parses back through `ProposalsFile`; `staged-proposals: false` restores the old behavior and reports zero, asserted by parsing a `cortex.yml` that sets it and reading back the loaded value (not merely that the file parses); a scan whose staging root exists but is unreadable exits non-zero and leaves `tag-proposals.yml` byte-identical; the same scan on a host with no staging root at all exits 0, reports note-only coverage, and still writes; `system-prompt` does **not** appear in the live output (it reaches 3 only by double-counting one note, and is the regression canary for M1). The live `>= 100` is a rollout smoke check, not a test: the corpus is mutable and the counted assertions live on the frozen fixture from Phase 5.

#### Phase 7: `sb cortex tag-promote`
**Model:** opus
- `cortex::proposals::promote_tags` plus the CLI variant, args, dispatch arm, and `print_tag_promote_report`, copied from the `concept-promote` shapes at `sb/src/cli/cortex.rs:89`, `:102`, `:693`.
- Targeted textual insert into the named group of `canonical-tags.yml`; every other byte unchanged. **Append to the end of the group, never rearrange.** Neither the 12 group keys nor the tags within a group are sorted today (`config/canonical-tags.yml:16` opens the `ai` group with `ai, agents, claude, anthropic`), so "sorted position" has no defined meaning and re-sorting would produce a whole-file diff. A group in empty-flow form (`  system: []`, `:129`) has no `    - ` line to anchor on: the insert rewrites that one line to block form and adds the entry. Any other layout the editor does not recognize is a hard bail, not a guess. The **candidate buffer** is parsed with `serde_yaml::from_str::<CanonicalTagsFile>` before the bytes are written, and the write is atomic. Not `CanonicalTagsFile::load`: that takes a `&Path` and reads from disk (`vault/src/canonical.rs:57-63`), so it would validate the old bytes and prove nothing.
- **`--canonical <path>` defaults to `vault::paths::canonical_tags()`, and `--apply` refuses that default.** The deployed copy is `write_always`-ed from the repo on every `otto deploy` (see Problem), so applying there writes a promotion that the next deploy deletes. The refusal names `config/canonical-tags.yml` and says to commit and deploy. Dry-run against the deployed copy is allowed and useful: that is the file the daemon reads.
- **Membership first, then traceability.** `promote_concept` bails on "no pending proposal" (`cortex/src/entities.rs:233`) *before* the already-present no-op (`:239`), so a second `--apply` of the same slug exits non-zero rather than reporting a no-op: the first apply removed the proposal it then demands. `promote_tags` checks canonical membership first, returns already-present, and only then requires a pending proposal. The same two-line reorder is applied to `promote_concept`, because Phase 3 makes `concept-promote` survive a deploy for the first time and this is the next thing an operator hits.
- Four hard bails, each naming the offending input: a tag that does not trace to a pending proposal (the `promote_concept:233` message text); a group that does not exist (naming the 12 valid keys); a vocabulary already at `max_canonical` (naming the current count, 117 of 300 today); a tag that is not kebab-case. Use the predicate `vault/src/canonical/tests.rs:342` already defines (`^[a-z0-9]+(-[a-z0-9]+)*$`), promoted to a `vault::canonical` function, not `sanitize_tag(t) != t`: `sanitize_tag("")` is `""`, so the empty tag passes that gate, and `sanitize_slug` keeps non-ASCII alphanumerics the shipped-file test rejects.
- **Replace the fixed-cardinality assertion in `vault/src/canonical/tests.rs:336`** (`assert_eq!(flat.len(), 117)`). It parses the `include_str!` of the exact file this phase mutates, so the first `--apply` turns `cargo test --workspace` red. An exact count is the wrong assertion for a file a shipped command is designed to grow; replace it with growth-compatible invariants: non-empty, `<= max_canonical`, unique, every tag kebab.
- `--apply` drops the promoted entries from `tag-proposals.yml`.
- Opus rather than sonnet: the textual insert has to be correct against two layouts (block sequence and empty flow), and the bail ordering plus idempotence are where this goes wrong.
- **Success criteria:** dry-run leaves both `canonical-tags.yml` and `tag-proposals.yml` byte-identical; `--apply` without `--canonical` exits non-zero naming the repo path; `--apply --canonical <copy>` twice is idempotent, the second run reporting already-present and exiting 0 (this is the M4 reorder, and it fails without it); promoting into `system` (the empty-flow group) produces valid block-form YAML that `CanonicalTagsFile::load` parses; after `--apply`, every line of the file outside the touched group is byte-identical, including `max-per-note`, `max-canonical`, `no-segment-match`, `no-classifier-tags`, and all 12 group keys in their original order; promoting a tag that is not a pending proposal and is not already canonical exits non-zero; promoting `""` exits non-zero; `cargo test --workspace` is green after a promotion (the M3 assertion).

#### Phase 8: Correct the docs
**Model:** sonnet
- `vault/src/distilled.rs:57-58`: state that `tags` are the extractor's raw, **pre-filter** candidates, that the canonical filter runs later at publish, and that this field is the open-vocabulary proposal source.
- `distillers/AGENTS.md:20`: same correction.
- Root `CLAUDE.md`: the second read-only cortex-to-staging edge, the `staging-root` hoist, and `tag-promote`.
- `cortex/AGENTS.md`: the new module and command.
- **Success criteria:** the two AC5 commands each return `0`; `sb cortex tag-promote --help` and `sb cortex sweep --help` both render the new surface.

### Operator steps

Not phase bullets, because no phase agent does them.

1. **Do not run `sb borg retention sweep` before Phase 6 lands.** It would prune the staging tree to the nominal 14 days and collapse the corpus from 47 days and 658 traces to whatever the last fortnight holds.
2. **The first `tag-promote` batch is a human curation pass** over 112 candidates, every one of them absent from `tag-mapping.yml` entirely and so never before ruled on (the 13 prior rejects never reach the queue, Phase 1 suppresses them). No agent does this.
3. After the first promotion batch, `sb cortex classify --retag` over the affected notes so existing notes pick up the new vocabulary.

## Acceptance Criteria

Each was run on `main` bff518d (`protect-cap-fixes` is at the same commit) on 2026-09-21 and the output recorded. AC1 is the baseline this design inverts; AC2-AC5 name the post-ship state and say so.

- [ ] **AC1** The note-derived arm alone produces zero proposals, and after this ships the combined scan produces at least 100.
  - `sb cortex sweep --proposals --dry-run`
  - **Observed on main:** `vault parsed: 3803 notes` then `No new tag proposals.`, exit 0. The post-ship half cannot run until Phase 6.
- [ ] **AC2** `~/.config/sb/tag-proposals.yml` holds at least 100 proposals, each with a non-empty `sources` list of at most 5 entries, and the file parses through `ProposalsFile`.
  - `python3 -c "import yaml;f=yaml.safe_load(open('$HOME/.config/sb/tag-proposals.yml'));p=f['proposals'] or [];assert all(1<=len(x['sources'])<=5 for x in p),'sources out of range';print(len(p))"` expects `>= 100` and exit 0
  - **Observed on main:** `0` (the file is `proposals: []`; the `sources` assertion passes vacuously). Cannot pass until Phase 6; depends on Phase 2's schema, Phase 3's clobber fix, and Phase 5's identity join. The parse-through-`ProposalsFile` half is a Rust test, not this command: `python3` proves shape, not the serde contract.
- [ ] **AC3** No proposal in that file carries a tag with an explicit `null` entry in `tag-mapping.yml`.
  - `python3 -c "import yaml;p=yaml.safe_load(open('$HOME/.config/sb/tag-proposals.yml'))['proposals'] or [];m=yaml.safe_load(open('config/tag-mapping.yml'));bad=[x['tag'] for x in p if x['tag'] in m and m[x['tag']] is None];print(len(bad),bad)"`
  - **Observed on main:** `0 []`, vacuously, because the queue is empty. The guard it tests does not exist yet: 13 of the 126 eligible candidates would violate it today (`session-management`, `documentation`, `yaml`, `json`, `macos`, `hooks`, `search`, `plugins`, `backup`, `harness-engineering`, `nodejs`, `skills`, `customization`). Meaningful only once Phases 1 and 6 have both shipped.
- [ ] **AC4** `sb cortex tag-promote` exists, is dry-run by default, and a dry-run leaves both YAML files byte-identical.
  - `sb cortex tag-promote --help` exits 0; then `md5sum config/canonical-tags.yml ~/.config/sb/tag-proposals.yml`, `sb cortex tag-promote ci-cd --group tech --canonical config/canonical-tags.yml`, `md5sum` the same two files and compare.
  - **Observed on main:** `error: unrecognized subcommand 'tag-promote'`. Cannot pass until Phase 7.
- [ ] **AC5** Nothing outside `docs/design/` claims `distilled.yml` tags are canonical-filtered, and neither of the two files that carried the "max 7" claim still does.
  - `rg -n 'post-filtered against' --glob '!docs/design/**' | wc -l` expects `0`
  - `rg -ni 'max 7' vault/src/distilled.rs distillers/AGENTS.md | wc -l` expects `0`
  - **Observed on main:** `2` and `2`. The first is `vault/src/distilled.rs:57` and `distillers/AGENTS.md:20`; the second is `vault/src/distilled.rs:58` and the same AGENTS.md line. Fails today, which is the point; Phase 8 clears it. The pattern is deliberately split rather than one case-insensitive alternation: `rg -ni 'post-filtered against|max 7' --glob '!docs/design/**'` returns 4 lines, and the fourth (`borg/patterns/obsidian-note.md:44`, "continue as needed, max 7") is an unrelated prompt instruction that must not be edited.

## Resolved Decisions

- **2026-09-21: `cortex classify --retag` needs no candidate-capture path.** The prior session recorded "retag misses have nowhere to land" as a gating question. There are no misses. `apply_retag` (`distillers/src/tags.rs:394`) consumes `fresh: &TagOutput` from a `TagClassifier`, and all three implementations are closed-vocabulary by construction: `ClassifierDev` POSTs only `sorted_vocabulary()` as labels and retains against `canon.all` (`:697`), `FabricClosedVocab` embeds the vocabulary in the prompt and post-filters, and `Deterministic` runs `match_to_canonical` over the note's existing tags, whose misses the note-derived arm of `scan_proposals` already catches. Closed with no work.
- **2026-09-21: recompute, do not accumulate.** `tag-proposals.yml` becomes a rendered view of one scan window, carrying `scanned-at` and `staged-window`, and `write_proposals` overwrites. Merge-on-write cannot express a candidate falling below threshold and produced the 212 KB `entity-proposals.yml`. The frequency a human reads is then always the frequency the current corpus supports, and the window is stated on the file rather than inferred.
- **2026-09-21: the retention window is a stated limitation, not a thing to work around.** Staging retention is 14 days nominal and 47 days actual because the sweep is manual. Under recompute, a candidate that ages out stops being proposed, which is correct: it stopped being evidenced. No shadow accumulator.
- **2026-09-21 (revised three times, after panel rounds 1, 2 and 3): walk the staging root, resolve note identity through the tiered reconciliation below.** The first draft keyed on the note `trace:` frontmatter join. Round 1 broke it: that map is one-way, note to trace, so a superseded trace cannot resolve back and votes again (`ht-970437f1` and `ht-ba9e3191` are both `succeeded` receipts for `notes/pi-coding-agent-free-course.md`, both carry `system-prompt`, and only the second is in that note's frontmatter). The second draft keyed on the receipts `note_path`. Round 2 broke that too, and the measurement is decisive: **of the 658 staged traces carrying a `note_path`, all absolute, only 21 point at a file that still exists.** 637 are stale, because `cortex classify` promotes a note from `inbox/` to `notes/` (`cortex/src/classify.rs:541-543`, filename preserved) and never updates the receipt. A receipt's `note_path` is a location snapshot taken at ingest, not an identity. Two notes are recorded under two directories each (`this-new-postgres-feature-is-crazy-powerful.md`, `pi-setup-after-6-months-of-use.md`) and would still double-count. Separately, `Note.path` is vault-relative (`vault/src/note.rs:13`) while receipts are absolute, so the two arms' key spaces would never have intersected and nothing would have deduped across them.

  The resolution is reconciliation, and the shape is already in-house: `borg/src/harvest/identity.rs:129` treats the receipt as a fast path and falls back to current vault identity, its Step 2 comment naming this exact cause ("covers a stale/absent receipts row (e.g. cortex moved the note between directories)"). `resolve_trace_notes` mirrors it, and every key it emits is a **vault-relative** path, the same space the note-derived arm uses:

  | Tier | Rule | Resolved |
  |---|---|---|
  | 1 | a vault note whose frontmatter `trace:` is this trace, converged through `superseded-by` | 618 |
  | 2 | the recorded `note_path`, relativized, still exists in the vault | 21 |
  | 3 | exactly one vault note whose filename equals the recorded basename, **and** whose `source` matches the receipt's | 19 |
  | 4 | none of the above: the trace keys on itself, `trace:<id>` | 1 |

  **The vault's own back-edge outranks the receipt**, which is a round-3 correction. Receipts-first is wrong in principle: an `inbox/` to `notes/` move vacates a path, a later ingest can land a different note on it (`cortex/src/classify.rs:868` confirms filenames are not reserved), and tier 1 would then name the wrong note. Measured before swapping: across the 21 traces both tiers resolve, 14 agree and **zero disagree**, and 7 more resolve only through the recorded path, so the swap is a no-op on today's corpus and removes the class outright. Free, so take it.

  **Tier 1 is ambiguous for 20 staged traces** (up to six notes each) because reingest leaves the old note carrying the same `trace:`. Converge through `superseded-by` (`vault/src/tombstone.rs:27`, 45 notes carry it): take the one candidate that is not superseded. Measured: **20 of 20 groups have exactly one non-superseded survivor**, so the rule is total on today's data. Three degenerate cases, each defined rather than discovered later: a `superseded-by` pointing at a note that does not exist is ignored, and that candidate stays in the running; more than one survivor, or a cycle, falls through to the next tier rather than picking arbitrarily. Without this rule the count is unaffected but the `sources` list churns between sweep ticks and can name a superseded note as provenance.

  **Tier 3 keeps a source cross-check and is not optional.** "Exactly one basename match" alone is not sufficient: `hv-c8d6b2`'s recorded basename now belongs to a note with a different `source` and `trace`, and only tier 1 currently prevents the wrong fallback. Requiring the receipt's `source` to match closes it. Dropping the tier entirely also yields 112, which makes it look removable, but 12 of its 19 resolutions are one note (`notes/pi-coding-agent-setup-after-2-months.md`, reingested repeatedly) that would otherwise cast 12 separate votes.

  Measured over all 659 staged traces: 632 distinct note keys, 1 unresolved, 10 notes carrying more than one staged trace. The M1 pair both land on `notes/pi-coding-agent-free-course.md`, so `system-prompt` scores **2**, not the 3 the frontmatter join gave it and not the 1 this entry claimed before round 2 corrected it. Tier 3 requires *exactly one* match because 38 basenames collide vault-wide. Status is not a filter: no schema constraint ties `note_path` to `status`, and `borg/src/receipts/tests.rs:1193` has a repaired `failed` receipt carrying one, so the tiers test the path, not the state. The `replay_of` column is the explicit supersession link and is ignored deliberately: it is empty on all 659 staged traces, so it would not have caught the Postgres pair.

  Cost: `rusqlite = { workspace = true }` in `cortex/Cargo.toml`, opened read-only exactly as `oracle/src/server.rs:563-580` opens the same file. It is free in build terms, not merely cheap: `cortex/Cargo.toml:9` already takes `vault` with the `search` feature and `vault/Cargo.toml:10` defines `search = ["dep:rusqlite"]`, so cortex links `libsqlite3-sys` today. The frequency contract is "distinct notes", and with reconciliation it holds.

- **2026-09-21 (panel round 3): the client-only host is discriminated by staging-root existence, not by a flag and not by the receipts DB.** `sb cortex sweep --proposals` works on the laptop today: the vault syncs via Syncthing, the note-derived arm needs nothing else. Both new readers would hard-fail there, and "client-only" appeared nowhere in this doc, so this was an undeclared host-behavior change riding in as hardening. The seats split. The architect wanted both readers to degrade to empty with a warning; the staff seat wanted the `Err` kept and `staged-proposals: false` set per host. Staff is right: degrading silently is exactly how the double-counting round 2 removed comes back, and `staged-proposals` defaults to true, so nobody edits `cortex.yml` before the first sweep and a manual flag is not a policy. The rule:

  | Condition | Behavior |
  |---|---|
  | staging root does not exist | skip the staged arm entirely, note-only coverage, say so in the report, exit 0 |
  | staging root exists but cannot be enumerated | `Err`, queue preserved |
  | staged arm running and the receipts DB cannot be read | `Err`, queue preserved |

  One `Path::exists` check, and it keeps M6's and R1's intent exactly where they matter: a host that was never supposed to have staging is not an error, a host that has it and cannot read it is. **Do not gate on whether `receipts.db` exists.** `sb doctor` calls `borg::receipts::open_default` (`borg/src/receipts.rs:122`), which creates and migrates the file, so any laptop that has ever run `sb doctor` owns an empty receipts DB and would pass an existence check while resolving nothing.

- **2026-09-21: one staging root key, hoisted, not a second one under `sweep`.** Two keys naming one directory is the derived-field pattern `rules/taste.md` says to drop rather than sync. Free to do: nothing deploys the key today.
- **2026-09-21: drop `Proposal::action` and `Proposal::suggested_canonical`.** Both are constants (`cortex/src/sweep.rs:225-226`) with no reader anywhere in the workspace. `action` was meant to distinguish `add` from `merge` and `suggested-canonical` to name the merge target (`docs/design/2026-03-23-tag-sweeper.md:131-138`); neither was implemented. Revisit condition: a merge-suggestion feature. Two always-constant fields behind a new `deny_unknown_fields` is worse than removing them.
- **2026-09-21: Phase 1 ships separately from Phase 6.** The reject guard is a promise `docs/design/2026-03-23-tag-sweeper.md:141` made and the code never kept. It stands on its own, is one function plus one test, and is the difference between the first staged queue having 113 entries worth reading and 126 entries of which 13 are things already rejected.
- **2026-09-21: union both candidate arms, no CLI flag to pick between them.** The note-derived arm is structurally empty today but is not permanently dead: a tag hand-typed in Obsidian is a live open-vocabulary input. A flag choosing between a working source and a dead one is not a choice worth offering.

## Alternatives Considered

### Alternative 1: Open the classifier's vocabulary
- **Description:** Let `classifier.dev` or the fabric closed-vocab prompt mint tags outside `canonical-tags.yml`.
- **Pros:** One mechanism instead of two.
- **Cons:** Destroys the property the whole tags-only migration bought, that the same input yields the same tag list from a governed vocabulary. Returns the junk drawer.
- **Why not chosen:** The proposer and the assigner want opposite properties. Assignment wants closed and stable; proposal wants open and noisy. They are already two mechanisms, correctly.

### Alternative 2: Auto-promote at threshold
- **Description:** A candidate at frequency >= N is added to `canonical-tags.yml` automatically.
- **Pros:** No human in the loop.
- **Cons:** 13 of the current 126 eligible candidates are tags a human already rejected. `session-management` at 16 and `documentation` at 10 would both have been auto-promoted. Several more, `show-hn` and `jsonl` among them, are closer to a note kind or a file format than an interest.
- **Why not chosen:** The queue exists because taste is the scarce input, not frequency.

### Alternative 3: Capture candidates at ingest into a dedicated store
- **Description:** Have borg write the pre-filter candidates to a new table or file at `finalize_tags` time.
- **Pros:** Not bounded by staging retention; captures the union of every candidate source, not just the distiller's.
- **Cons:** A new borg writer, a new store, a new retention policy, and a schema migration, for data that is already on disk.
- **Why not chosen:** The candidates are already durably staged. Building a second store for them is the definition of unrequested scope. Revisit if the retention sweep is ever automated and the window becomes too short to accumulate a threshold.

### Alternative 4: Serde round-trip `canonical-tags.yml` on promote
- **Description:** Copy `write_glossary` (`cortex/src/entities.rs:279`) literally: parse, mutate, reserialize.
- **Pros:** Twenty lines, and it is the in-house precedent.
- **Cons:** it cannot produce byte-exactness outside the touched group, which is Phase 7's own success criterion. Group order is *not* the reason: `serde_yaml::Value`'s `Mapping` is `IndexMap`-backed and preserves insertion order, and switching the `tags` field to `indexmap::IndexMap` works too, so an order-preserving serde path does exist. What no serde round-trip preserves is layout and comments: sequence indentation, the empty-flow `system: []` form, and (in the sibling case) `config/glossary.yml`'s five-line header.
- **Why not chosen:** byte-exactness outside the touched group is the requirement, and no serde path delivers it. The precedent does not even hold for `glossary.yml`: `write_glossary` throws away that file's five-line header. Copy the command's shape, not its writer.

## Technical Considerations

### Dependencies

One new line, no new crate in the workspace. `serde`, `serde_yaml`, `eyre`, `log`, `walkdir`, and `rayon` are already direct dependencies of `cortex` (`cortex/Cargo.toml:22,27,28`). `rusqlite = { workspace = true }` is added to `cortex`, for the read-only receipts join that note identity requires (see Resolved Decisions). It is already a workspace dependency and already a direct dependency of `oracle` (`oracle/Cargo.toml:27`), which opens the same file the same way, so this adds no new third-party code to the build.

### Performance

The staged read is 658 files and 10.6 MB today. A Python reference implementation takes 0.43s to enumerate the 46,295 trace directories and 10.3s to `yaml.safe_load` all 658; the Rust path parses into a two-field struct under `rayon`, so it should land far below that, and Phase 5's criterion records the observed number rather than assuming it. The directory enumeration grows with total traces, not with distilled traces, so it grows even as retention prunes. That is the cost line to watch. The obvious mitigation, driving enumeration from the `trace_to_note` map instead of the directory listing, is **not** semantics-preserving: it drops exactly the unresolvable traces the design deliberately keeps as their own `trace:<id>` key. A mitigation that is needed later has to be an mtime or manifest cache, not that.

The daemon's on-change sweep arm (`cortex/src/daemon.rs:819`) picks the staged scan up automatically, which is why `staged-proposals` is a config key and not a hardcoded true.

### Security

None. Read-only access to a local directory cortex already reads, plus writes to two files under `~/.config/sb/` that cortex already owns. No network, no credentials, no LLM call anywhere in this design.

### Testing Strategy

- `aggregate` is pure and takes its inputs as slices, so its tests are hand-built fixtures with no filesystem.
- `read_staged_candidates` is tested against a `tempfile` tree containing a good file, a malformed file, a missing file, and a file with no `tags` key.
- `promote_tags` is tested against a copy of the shipped `config/canonical-tags.yml`, asserting byte-identity of everything outside the target group and that `CanonicalTagsFile::load` still parses the result.
- Hermetic tests use `crate::testutil::lock_env()` and `crate::testutil::hermetic_config_home()`: `scan_proposals` calls `validate_canonical_assets`, which otherwise reads the developer's own `~/.config/sb/` (`cortex/src/sweep/tests.rs:361-363`).
- Phases 1, 2, 3, and 7 each record break-the-code evidence in the commit: the regression applied, the named test observed failing, the regression reverted.

### Rollout Plan

Phases 1, 3, and 4 are behavior-preserving and ship with no operator action. **Phase 2 is the first phase whose deploy changes what the daemon writes**: it removes the empty-result write gate at `cortex/src/daemon.rs:820`, so an empty scan begins writing a header-only file on the next tick. Phase 6 is the first that changes what the file *contains*; the first sweep tick after it lands populates `tag-proposals.yml` with 112 entries (126 naive, less the 13 Phase 1 suppresses, less `system-prompt`, which Phase 5's identity reconciliation shows never met the threshold). Phase 7 adds a command nothing calls automatically. Standard `otto deploy` per the root CLAUDE.md; no systemd unit changes, no migration, no vault writes anywhere in this design.

**Inter-phase dependencies**, all verified against the phase bodies on bff518d. The draft claimed "Phase 3 before Phase 6 is the only hard constraint"; that was wrong, and the panel found the rest across rounds 1 and 2.

| Constraint | Why |
|---|---|
| P1 -> P5 | `is_rejected` exists nowhere in `vault/src` or `cortex/src` today and P5's `aggregate` calls it. Compile-order. |
| P2 -> P5 | `aggregate` returns the new `Proposal` shape. |
| P2 -> P6 | the scan's output is written through the new `ProposalsFile`. |
| P3 -> P6 | otherwise the first populated queue is wiped by the next deploy. |
| P4 -> P6 | `scan_proposals` resolves the hoisted `staging-root`. |
| P5 -> P6 | P6 wires the reader and aggregator P5 builds. |
| P2, P5 -> P7 | promote reads the queue schema and the module P5 creates. |
| P7 -> P8 | P8's success criterion greps `sb cortex tag-promote --help`. |
| P4 -> P8 | P8 documents the hoisted `staging-root`. |
| P6 -> P8 | P8 documents the staging edge P6 wires. |

The listed 0..8 order satisfies every one of them.

**Schema regeneration, not a shim.** Phase 2 propagates a parse failure instead of `unwrap_or`-ing it, and drops two fields while renaming a third, so a queue file in the old format now fails to load. The deployed queue is `proposals: []`, which parses under both shapes, so nothing breaks today. If a host ever does carry an old-format queue, the procedure is to delete it and let the next sweep regenerate it: the file is a rendered view, so there is nothing in it worth migrating.

**Cross-repo blast radius:** none. Everything is in `second-brain`. The vault is not written; no note changes anywhere in this design. Two files outside the repo change: `~/.config/sb/tag-proposals.yml` (written by the sweep, and after Phase 3 no longer clobbered by deploy) and `~/.config/sb/glossary.yml` (stops being clobbered, same phase). A promotion is a repo edit to `config/canonical-tags.yml`, then a commit, then `otto deploy`, in that order: the deployed copy is generated from the repo and editing it directly is refused.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `sb borg retention sweep` is run before Phase 6, collapsing the corpus from 47 days to 14 | Med | Med | Called out as operator step 1. The sweep is manual and has never been run; nothing in this design or in `otto deploy` triggers it. |
| The first queue is 126 entries and the curation pass stalls | Med | Low | The queue is a rendered view, so a stalled pass costs nothing and the next scan refreshes it. `tag-promote` takes a variadic tag list so a batch is one command. |
| Promotion pushes the vocabulary toward `max-canonical: 300` | Low | Med | 117 in use, 183 free, 112 proposable candidates. Phase 7 bails when at the cap, naming the count. |
| The textual insert into `canonical-tags.yml` corrupts the file | Low | High | Phase 7's criterion asserts byte-identity outside the target group and re-parses through `CanonicalTagsFile::load`. The file is in git; a bad write is one `git checkout` away. |
| The staged scan slows the daemon's sweep tick as the trace count grows | Med | Low | `staged-proposals: false` turns it off. Phase 5 records the observed wall time; the enumeration-from-`trace_to_note` fallback is named in Performance. |
| A candidate is promoted that is a note kind rather than an interest (`show-hn`, `jsonl`, `session-management`) | High | Low | Human gate. This is the reason the gate exists, not a defect. |
| `deny_unknown_fields` on `EmbedConfig` breaks an unmeasured host's `cortex.yml` | Low | Low | Measured zero hits across the deployed config, the dotfiles repo, and the shipped template. Failing loudly is the intended behavior if one exists. |
| Promoting a broad candidate silently absorbs a specific one: add `ci` and `ci-cd` (46 traces) stops being proposable forever, because segment matching resolves it to `ci` | Med | Med | Named here and in the operator steps. The queue shows both with their frequencies side by side, so the choice is visible at curation time. `no-segment-match` is the lever if the broad tag must exist without swallowing the specific one. |
| A promotion is applied to the deployed `canonical-tags.yml` and lost on the next deploy | Low | High | Phase 7's `--apply` refuses the deployed path outright and names the repo path. This is the bug `concept-promote` has today. |
| A staging-root enumeration failure reads as an empty scan and wipes the queue | Low | High | Phase 5's reader returns `Result`; root failure is an `Err` and the prior file is untouched. Phase 2's unconditional overwrite only runs on `Ok`. |
| Two receipts for one note inflate a candidate past the threshold | Med | Med | Was live in the draft (`system-prompt`, 3 keys from 2 notes). Phase 5 joins note identity through receipts; Phase 6 asserts `system-prompt` is absent from the live output as the regression canary. |
| The frozen fixture drifts from the live corpus and stops representing it | Med | Low | The fixture exists to make counts assertable, not current. The live corpus keeps a `>=` smoke check in Phase 6, which is what catches a fixture that has gone unrepresentative. |

## Open Questions

None.

## Addendum A: panel round 1 (2026-09-21), fully folded

Run dir `/tmp/review-panel/AmaAHuzO/`. Architect (Gemini) rc=0, Staff Engineer (Codex) rc=0, snapshot byte-identical to the live file. 6 must-fix, 5 cheap-wins, 4 defers, 7 of 7 questions answered. **Every finding was re-run against `main` bff518d before folding; all 15 reproduced.** Nothing was deferred and nothing was dropped.

Must-fix, and where each landed:

- **M1, the distinct-note threshold did not hold.** The one that changed the design. Both seats converged; I reproduced it and measured the blast radius. Folded into Resolved Decisions (the provenance join is reversed to receipts), Phase 5, Phase 6, Dependencies, and two risk rows. The panel said to carry this as an Open Question. I am not carrying it: it is a design choice, it is mine to make, and I made it on measurement. That is a pushback for round 2, not a silent drop.
- **M2**, the hoist silently breaks the kebab key, because top-level `Config` has no `rename_all`. Staff only; the architect said the hoist was safe, which is wrong. Folded into the Data Model and Phase 4, with a literal-YAML-key test.
- **M3**, the first promotion turns CI red on `vault/src/canonical/tests.rs:336`'s `== 117`. Folded into Phase 7.
- **M4**, Phase 7's idempotence criterion contradicted its own first bail, as does the copied precedent. Folded into Phase 7, membership-first, and the same reorder applied to `promote_concept`.
- **M5**, `system: []` has no insert anchor and neither groups nor tags are sorted. Folded into Phase 7: append, never rearrange, rewrite the empty-flow group to block form, bail on any unrecognized layout.
- **M6**, a failed scan was indistinguishable from an empty one under Phase 2's unconditional overwrite, and 45,637 WARNs per scan. Folded into the API contract and Phase 5.

Cheap wins C1-C5 all folded: `notes` -> `sources` stragglers, AC2 became a parse-and-assert, the kebab predicate replaced `sanitize_tag(t) != t` (which passes the empty tag), `tag-proposals.yml` leaves `shared_config_findings`, and the template gets `staged-proposals`.

Defers D1-D4 all folded rather than deferred: the corpus grew by one trace mid-review (658 -> 659), which is now the stated argument for frozen fixtures; "658/658 via receipts" is 658 rows with 657 paths; 439 MB is allocated blocks against ~82 MB of content; and the performance fallback was struck, because it is not semantics-preserving.

Q6 corrected a claim of mine outright: "Phase 3 before Phase 6 is the only hard constraint" was false. The Rollout Plan now tabulates them (round 2 added two more, so the table is ten rows), and the Rollout's claim about which phase first changes daemon writes was also wrong (it is Phase 2, not Phase 6).

Two seat limitations worth recording, because they bound what the round proves: the architect ran under a policy that blocks `python3` and `sqlite3`, so all of its distribution and dedup claims are reasoning rather than measurement, and it reached the wrong answer on Q4 for that reason. The staff seat measured. The panel agent re-ran every load-bearing claim independently.

## Addendum B: panel round 2 (2026-09-21), fully folded

Run dir `/tmp/review-panel/AmaAHuzO/`, synthesis `synthesis-r2.md`. Both seats rc=0. 4 must-fix, 4 cheap-wins, 2 notes, 7 of 7 questions answered. Every finding re-run against `main` bff518d before folding; all reproduced.

**The pushback was accepted.** The panel withdrew its process objection to M1 (a decision made on measurement, priced and recorded, is a Resolved Decision, not an Open Question) and replaced it with a finding against the decision's content, which is what I asked for. That finding is R1 and it was right.

- **R1, the receipts `note_path` is a location snapshot, not an identity.** Both seats and the panel agent found it independently; I measured it: of 658 staged traces carrying a path, **21 point at a file that still exists**. `cortex classify` promotes `inbox/` to `notes/` (`classify.rs:541-543`) and never updates the receipt. Two notes are recorded under two directories and would still double-count, and `Note.path` is vault-relative while receipts are absolute, so the two arms would never have deduped against each other at all. Folded as the tier table in Resolved Decisions, taking its shape from `borg/src/harvest/identity.rs:129`, whose own Step 2 comment names this cause. Not a mirror: borg confirms identity before accepting a receipt path (`identity.rs:299`) and follows tombstones (`:356`), and round 3 found that skipping both of those is precisely where the tier order and the ambiguity defects came from. Measured result: 632 distinct note keys, 1 unresolved, 10 notes with more than one staged trace.
- **R2**, this document said `system-prompt` scores 1 under receipts. It scores 2. My error, from misreading my own script's below-threshold output as zero. Corrected; the canary holds at 2 < 3.
- **R3**, Phase 3 turned CI red on `sb/src/cli/checks/tests.rs:127` (`errors.len() == 3`), and my claim that "glossary.yml stays in the list" was false: it was never in `shared_config_findings`. Both corrected.
- **R4**, "re-parsed through `CanonicalTagsFile::load`" validates the old bytes, because that function reads from disk. Now parses the candidate buffer.

Cheap wins all folded: the stale "sorted position" half of the round-1 M5 fold, the two missing dependency edges (P4 -> P8, P6 -> P8), the queue-preservation assertion moved to its first caller, and Phase 4's template criterion replaced, since `SweepConfig` has no `deny_unknown_fields` and so "the template parses" could not fail.

From Q1, three things folded that nobody asked about: an unreadable receipts DB is now an `Err` rather than an empty map (round 3 found one stale sentence still saying otherwise, and refined this into the host policy in Resolved Decisions) (an empty map silently restores the inflation the reconciliation removes); status is explicitly not a filter, because no schema constraint ties `note_path` to `status` and `borg/src/receipts/tests.rs:1193` has a repaired `failed` receipt carrying one; and the `replay_of` column is ignored deliberately, empty on all 659 staged traces.

Q6 settled the dependency question three ways: `rusqlite` is free, because `cortex/Cargo.toml:9` already takes `vault` with the `search` feature and `vault/Cargo.toml:10` defines `search = ["dep:rusqlite"]`. cortex links `libsqlite3-sys` today.

Round 2's recommended Open Question, "what evidence merges a stale receipt path, a current receipt path, and a current note into one identity, and what happens when that evidence is ambiguous or unavailable", is answered by the tier table. Round 2's fold answered it only for stale paths; round 3 found the other half, the 20 traces whose `trace:` matches several notes, and the `superseded-by` convergence closes it. The panel withdrew the recommendation, so Open Questions stays empty.

## Addendum C: panel round 3 (2026-09-21), fully folded, cap reached

Run dir `/tmp/review-panel/AmaAHuzO/`, synthesis `synthesis-r3.md`. Both seats rc=0. 4 must-fix, 4 cheap-wins, 6 of 6 questions answered, Open Questions recommendation withdrawn. Every finding re-run against `main` bff518d before folding; all reproduced.

- **R1**, the round-2 missing-DB fold left a stale sentence at the head of Phase 5 saying the opposite of the two corrected lines around it, with the old one-argument signature. Fixed, and superseded by the R4 host policy.
- **R2**, `resolve_trace_notes` could not perform the recorded-path tier (tier 2 after the Q1 swap) as specified: receipt paths are absolute, `Note.path` is relative (`vault/src/note.rs:12-13`), and the signature carried no vault root, so the function could not relativize or test existence. `vault_root` is now threaded through `scan_proposals`; both callers already hold it.
- **R3**, tier 1 is ambiguous for **20 staged traces**, up to six notes each, because reingest leaves the old note carrying the same `trace:`. Closed by converging through `superseded-by` (`vault/src/tombstone.rs:27`): measured, 20 of 20 groups have exactly one non-superseded survivor. The three degenerate cases are now defined rather than left to be discovered.
- **R4**, the client-only host. The seats split, which was the signal. "client-only" appeared nowhere in this doc while both new readers would have hard-failed on the laptop, so this was an undeclared host-behavior change riding in as hardening. Decided with the staff seat and the panel's refinement: discriminate on staging-root existence, never on `receipts.db` existence, because `sb doctor` creates that file (`borg/src/receipts.rs:122`).

Two answers changed the design beyond the must-fix list:

- **Q1: the tier order was backwards.** Receipts-first is wrong in principle, because an `inbox/` to `notes/` move vacates a path a later ingest can occupy (`cortex/src/classify.rs:868`). I measured before swapping: of the 21 traces where both tiers resolve, 14 agree and **zero disagree**, so putting the vault's own `trace:` back-edge first is a no-op today and removes the class permanently.
- **Q3: neither reuse borg's resolver nor write a parallel one.** Both seats converged and I agree. cortex has no borg dependency and gaining one inverts the one-way data flow; but `borg::harvest::identity` is a publish-replacement policy whose strict trace guard would reject the historical traces this scan must reconcile. The mechanics go into `vault`, beside `vault/src/tombstone.rs`, which already documents why independent implementations of this drift. My instinct that a second identity implementation rots was right; the answer is a third home.

Q5 re-derived 112 independently and confirmed it, along with every tier count, the 38-name collision set, and `system-prompt`=2. It also caught the one defect in the counting table: it mixed the 658-trace and 659-trace snapshots. Now single-snapshot at 659: 846/126 -> 799/113 -> 799/112.

Q2 measured the thing that makes tier 3 look optional and is not: dropping it still yields 112, but 12 of its 19 resolutions are one repeatedly-reingested note that would otherwise cast 12 votes. It also found the live near-miss (`hv-c8d6b2`) that motivates the `source` cross-check.

**Three rounds, cap reached: 15 + 8 + 8 findings, every one reproduced against `main` before folding, none deferred and none dropped.** The one pushback I carried (round 1's insistence that M1 be an Open Question) was accepted and withdrawn in round 2, and the content finding it was replaced with turned out to be the most valuable single item of the review.

## Addendum D: the road not taken

**Re-adding `resources` and `system` as tags.** Scott asked on 2026-09-21 whether the domains that did not become tags could be added back. Ten of the twelve did become canonical tags. `resources` and `system` are `null` in `tag-mapping.yml` (`:3401`, `:3993`), an explicit rejection, on the grounds that they were folder buckets and not interests. The cost of that, measured in the prior session: 52 notes carried one of the two, 44 still carry at least one tag from another source, and 4 notes now carry none (`home.md`, `journal/2022/08/2022-08-05.md`, and two `test_folder` fixtures, per `docs/design/2026-09-19-tags-only-classification-implementation-notes.md:768-775`). Adding them back is two lines in `canonical-tags.yml` plus flipping the two `null` mappings, then `sb cortex classify --retag` over the affected notes. Not done, because Scott asked the question and did not give the instruction. Recorded so it is not re-derived.

**LLM clustering of the proposal queue.** Still open from `docs/design/2026-03-23-tag-sweeper.md:363`. Deferred again: with provenance attached, raw frequency is what a human needs, and a clustering pass would put an LLM call on a path that currently has none.

## References

- `docs/design/2026-03-23-tag-sweeper.md`: the origin of the proposal queue, the `null`-mapping reject rule this doc finally implements (`:141`), and Alternative 4's rejection of a queue-free allowlist (`:314-318`).
- `docs/design/2026-09-19-tags-only-classification.md`: the tags-only migration, including the Non-Goal at `:60` that scoped the proposal path out and the Addendum C8 classifier switch that made `classifier-dev` keyless.
- `docs/design/2026-07-07-distillation-output-restore.md`: the precedent for cortex's first read-only staging edge.
- `cortex/src/entities.rs`: `discover` and `promote_concept`, the in-house pattern this design copies.
