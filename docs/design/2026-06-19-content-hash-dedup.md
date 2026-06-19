# Design Document: Content-Hash Duplicate Detection for `borg audit`

**Author:** Scott A. Idler
**Date:** 2026-06-19
**Status:** Implemented
**Review Passes Completed:** 5/5 (self) + Staff Engineer (Codex) incorporated

## Summary

`sb borg audit --fix duplicate` currently declares two notes duplicates when they
share a `source:` frontmatter string, then quarantines all but the mtime-newest.
This is a proxy for "the same input was ingested twice," and the proxy is wrong:
it false-positived 19 distinct authored notes that shared one batch-import label
(`source: "pais-migration"`) and quarantined them, keeping an empty stub as the
"winner." This doc replaces the source-string trigger with an **exact normalized
body-hash** trigger. Critically, body-identity is treated as authority to
*report* a candidate group — **not** as authority to *remove* a note. Any
destructive quarantine requires a **second, independent proof** of identity, and
is **opt-in** (the default `--fix duplicate` only reports/tags). This keeps the
real benefit (catching genuine re-ingests) while making it structurally
impossible to silently destroy a distinct note.

## Problem Statement

### Background

Borg ingests URLs and writes one note per ingest, recording the origin URL in the
note's `source:` frontmatter. A genuine duplicate arises when the *same URL is
ingested twice*, producing two notes with the same `source:` and (essentially)
the same content. `sb borg audit --fix duplicate` exists to detect and clean
those up.

The detection (`borg/src/audit.rs`):

- `build_note_index` (line 726) reads every note's `source:` field and groups
  notes by that string into a `HashMap<String, Vec<PathBuf>>`.
- The duplicate-finding loop (line 341) flags **any `source` shared by more than
  one note** as a `Duplicate` finding.
- `apply_fix_duplicate` (line 607) keeps the note with the newest mtime and
  `fs::rename`s the rest into `system/quarantine/<source-key>/<original-rel-path>`.

### Problem

The heuristic assumes `source == content identity`. That holds for borg-ingested
notes (where `source` is the origin URL) but is false in general, because
`source` is just a string and nothing guarantees it is unique-per-content.

Concrete failure, observed in the live vault (commit `017363e`):

- 20 hand-authored research notes were imported from a prior system with the
  identical batch label `source: "pais-migration"` (not a per-note URL).
- The duplicate loop saw "20 notes sharing one source" and declared 19 of them
  duplicates of the mtime-newest.
- `apply_fix_duplicate` kept the note with the newest mtime. The kept note,
  `notes/rule-of-five.md`, is today an **empty 244-byte stub** (its body hashes to
  the empty-string SHA-256), while the 19 quarantined notes each have **distinct,
  non-empty bodies** (verified: three sampled bodies hash to three different
  values). Caveat on certainty: the *current* filesystem mtimes and git history
  do not preserve the runtime mtimes that drove the original selection, so "mtime
  kept the stub" is **consistent with** the code path and the observed end state,
  not independently proven. What *is* proven: 19 distinct-body notes were
  quarantined under one shared `source`, and the surviving note is an empty stub.
- Result: 19 real notes (Ralph Wiggum Loop, Four Disciplines of Prompting, RAG
  with LLMs, ...) sit in `system/quarantine/pais-migration/notes/`, excluded from
  every cortex/oracle scan.

Two aggravating facts:

1. The `origin: authored` governance carve-out (cortex commit `fa3f9a8`,
   `cortex/src/duplicates.rs:39` → `scope.rs:18`) is **cortex-only**;
   `borg/src/audit.rs` has no `origin` awareness (confirmed), so authored notes
   were never protected from this path.
2. The mtime tiebreak preserved a worthless note while quarantining the valuable
   ones — even a "successful" dedup run destroyed information.

### Goals

- Make body-identity the trigger for *reporting* a duplicate candidate group, so
  two notes are never flagged on a shared frontmatter string alone.
- Require a **second, independent proof of identity before any destructive move**,
  so a distinct note can never be silently quarantined.
- Make the destructive quarantine **opt-in**; the default fix reports/tags only.
- Recover the 19 wrongly-quarantined notes and assess the empty `rule-of-five.md`.
- Keep catching the real case: the same input ingested twice.

### Non-Goals

- Near-duplicate / semantic-similarity detection (reworded content, embeddings).
  Explicitly rejected — any threshold below exact identity can re-merge distinct
  notes, the exact failure we are eliminating. See Alternatives.
- Changing cortex's separate `duplicates` lint (`cortex/src/duplicates.rs`); that
  path already exempts `origin: authored` and is out of scope. This doc must,
  however, *reconcile* with cortex's on-disk `cortex-duplicate*` convention (see
  D3).
- Fixing the shared `source: "pais-migration"` label itself. Restoring the notes
  no longer depends on the label being unique (the new heuristic ignores `source`
  for grouping), so re-sourcing is optional cleanup, tracked as an open question.

## Proposed Solution

### Overview

Two-layer design:

1. **Detection (reporting authority):** group notes by a **normalized body
   fingerprint** (SHA-256). A group of size > 1 is a *candidate duplicate set* and
   is reported. `source:` is not read for grouping. Empty/near-empty bodies are
   never eligible.
2. **Destructive action (removal authority):** quarantine requires **opt-in** AND
   a **second proof** beyond the shared body hash — both (a) a post-grouping
   byte-for-byte comparison of the normalized bodies (guarding against
   normalization drift / implementation mistakes, which are the realistic risk —
   not a SHA-256 collision), and (b) an identity match (same `source:` value, or
   same ledger entry). A candidate set that fails the second proof is reported,
   never moved.

### Architecture

Components touched, all in `borg/src/audit.rs` unless noted:

1. **`build_dup_index`** (new, replaces the source-keyed grouping *for the
   duplicate check only*). Keys on `content_fingerprint(body)`. Reuses
   **`vault::frontmatter::split_raw`** (`vault/src/frontmatter.rs:247`, already
   called from `audit.rs:768`) to separate frontmatter from body — **no new
   splitter is introduced** (avoids the exact frontmatter-parsing drift this repo
   has already consolidated). Skips files whose normalized body is empty or
   shorter than `MIN_DUP_BODY_LEN`, counting and logging the skips.

2. **The existing `source:`-keyed `build_note_index` stays**, because other audit
   checks legitimately key on `source`: **Mistype** (`audit.rs:265`), the
   **Blocked / RawTitle** note lookup (`audit.rs:282`), and **GithubCreatorMissing**
   (`audit.rs:350`). Note: **`OrphanReplace` does NOT use this index** — it parses
   the ledger directly (`audit.rs:306`); the earlier draft wrongly listed it.

3. **`content_fingerprint`** (new). `body -> Option<String>`. Returns `None` for
   empty/too-short normalized bodies; otherwise the hex SHA-256 of the normalized
   body. Uses `sha2` (already in `borg/Cargo.toml:23`; cryptographic + stable,
   never `DefaultHasher`).

4. **`normalize_body`** (new). Line/`char`-oriented only (never byte-slices — UTF-8
   footgun): CRLF→LF, strip trailing whitespace per line, strip leading/trailing
   blank lines. Internal blank runs preserved.

5. **`AuditFinding::Duplicate`** keeps `note_paths` and carries the `fingerprint`
   for the report; its `source` field becomes display-only (the first note's
   source), no longer part of the decision.

6. **Fix action split (see D1).** The current single `duplicate` fix conflates
   "detect" and "quarantine." It is split: the default reports/tags; a distinct
   opt-in verb performs the gated, second-proof-checked quarantine, keeping the
   lexicographically-first path (deterministic; bodies are byte-identical so the
   choice is arbitrary-but-stable, never mtime).

### Data Model

No new persisted schema *owned by this change*. In-memory index type is unchanged
in shape, only in keying:

```rust
// before: keyed by `source:` string
HashMap<String, Vec<PathBuf>>
// after (duplicate check only): keyed by hex SHA-256 of normalized body
HashMap<String, Vec<PathBuf>>
```

On-disk frontmatter: **decided (D3) — report mode writes NO frontmatter.** The
`DuplicateReported` / `DuplicateNotEligible` events print to stdout only; borg
writes no duplicate markers and stays entirely out of cortex's `cortex-duplicate*`
field ownership. There is no on-disk schema change.

### API Design

```rust
/// Minimum normalized-body length (chars) for a note to be dedup-eligible.
/// Empty and trivially-short bodies (stubs, redirects, one-line placeholders)
/// are never grouped — otherwise every empty note hashes alike and "duplicates"
/// every other empty note (this is "same input -> same hash", NOT a SHA collision).
const MIN_DUP_BODY_LEN: usize = 32;

/// Normalize a note body for content comparison. Line-oriented; never indexes
/// bytes (UTF-8 safety). CRLF->LF, trailing-whitespace-per-line stripped,
/// leading/trailing blank lines removed. Internal blank runs preserved.
fn normalize_body(body: &str) -> String;

/// Content fingerprint: hex SHA-256 of the normalized body, or `None` if the
/// normalized body is empty or shorter than MIN_DUP_BODY_LEN.
fn content_fingerprint(body: &str) -> Option<String>;

/// Build the duplicate index keyed by content fingerprint. Body is obtained via
/// vault::frontmatter::split_raw — NOT a new splitter.
fn build_dup_index(vault_root: &Path, skip_folders: &[String])
    -> Result<HashMap<String, Vec<PathBuf>>>;

/// Second proof before any destructive move: returns true only if every note in
/// the group is byte-identical after normalization AND shares an identity key
/// (same `source:` or same ledger entry). A group failing this is reported only.
fn quarantine_eligible(group: &[PathBuf]) -> bool;
```

`--fix` currently accepts only `FindingKind` unit variants (`sb/src/cli/borg.rs:64`),
and `duplicate` *means quarantine* in both the event model (`audit.rs:171`) and CLI
output (`borg.rs:677`). D1 specifies how the new opt-in verb slots in without
breaking that contract.

### Implementation Plan

#### Phase 1: Detection swap (fingerprint index, report-only)
**Model:** opus
- Implement `normalize_body`, `content_fingerprint`, `build_dup_index`, reusing
  `vault::frontmatter::split_raw`. Apply `MIN_DUP_BODY_LEN` + empty-body guard.
- Repoint the duplicate-finding loop (`audit.rs:341`) at `build_dup_index`; leave
  the source-keyed index feeding Mistype / Blocked / RawTitle / GithubCreatorMissing.
- Carry `fingerprint` on the `Duplicate` finding.
- Add observability logging: files scanned, short/empty bodies skipped (count),
  candidate groups found, group sizes.
- Unit tests: distinct bodies sharing a `source` produce **zero** quarantine and
  (per D1) only a report (the pais-migration regression); empty/short bodies never
  group; a multibyte-char body (em-dash, non-ASCII) does not panic normalization.

#### Phase 2: Fix-action split + second proof (D1, D2)
**Model:** opus
- Implement the D1 CLI/event surface: default `--fix duplicate` reports/tags only;
  add the opt-in destructive verb and its event variants, fix counts, and help
  text; preserve backward-compat semantics for any scripted callers.
- Implement `quarantine_eligible` (D2): byte-compare normalized bodies + identity
  match; deterministic lexicographically-first keep.
- Tests: a candidate group with identical bodies but differing `source:`/ledger
  identity is reported, never moved; an eligible group moves deterministically;
  report-only path emits the right `AuditEvent`s and moves nothing.

#### Phase 3: On-disk convention reconciliation (D3)
**Model:** opus
- Decide and implement what report/tag mode writes (if anything) to frontmatter,
  reconciled with cortex's `cortex-duplicate*` fields (`cortex/src/duplicates.rs:87`,
  cleared at `:222`). Either reuse cortex's fields, add borg-namespaced fields, or
  write nothing and report to stdout/receipts only.
- Update the "Data Model / no schema change" wording to match the decision.

#### Phase 4: Recovery of the 19 quarantined notes
**Model:** sonnet
- Recovery is a one-off data migration → **bash**, not Rust. Correct location:
  `bin/` at the second-brain repo root or a vault-side script — **there is no
  `borg/bin/` directory** (confirmed); do not invent one.
- **Precondition: the vault working tree must be clean.** The vault is currently
  dirty; recovery cannot proceed (and must refuse) until the operator commits or
  stashes. State this in the runbook.
- Enumerate `system/quarantine/pais-migration/notes/*.md`, `git mv` each back to
  `notes/<name>.md`, refuse to clobber an existing target.
- Verify rows-moved == 19 (verify by count, not exit code).
- **Rollback:** on partial failure, the inverse is `git checkout -- .` /
  `git reset --hard` against the pre-move commit (clean-tree precondition is what
  makes this safe); document the exact command.
- Assess `notes/rule-of-five.md` (empty stub): report its state for a human
  decision; do not auto-delete.

#### Phase 5: Tests, CI, docs
**Model:** sonnet
- Fixture mini-vault in `borg/src/audit/tests.rs` reproducing the pais-migration
  shape (N distinct-body notes, one shared `source`) → assert zero quarantine.
- `otto ci` green (verify by exit code).
- Update `borg/AGENTS.md` and the `FindingKind::Duplicate` doc-comment
  (`audit.rs:30`) to describe the content-hash + second-proof rule.

## Design Decisions

### D0: Body-identity is reporting authority, not removal authority
Identical normalized bodies prove the *content* matches; they do **not** prove the
notes are the same note. Title, date, type, `source`, `origin`, path, and inbound
link targets can all differ. Therefore body-hash gates **reporting** a candidate
group; **removal** requires the second proof in D2. This is the load-bearing
correction from review.

### D1: Fix action — report/tag by default, quarantine by explicit opt-in
The current `--fix duplicate` *is* the quarantine action. We split it:
- Default `--fix duplicate` → detect + report (+ optional tag per D3). Destroys
  nothing.
- A distinct opt-in performs the gated quarantine. Candidate shapes (decision
  required, tracked as open question): a new `FindingKind::DuplicateQuarantine`
  value, a separate flag, or a separate subcommand. Whichever is chosen must
  define event variants, fix counts, help text, and backward-compat behavior for
  existing scripted callers that pass `--fix duplicate` expecting a move.
- Per `cli.md`: a destructive op gated behind an explicit opt-in gets **no
  `--dry-run`**; recovery is via git + `rkvr`, not a preview.

### D2: Second proof before any destructive move
Quarantine proceeds only if `quarantine_eligible(group)` is true: every note's
normalized body is byte-identical (re-checked after hash grouping, because the
realistic failure is normalization drift / a coding mistake, not a SHA-256
collision) **and** the notes share an identity key (same `source:`, or same ledger
entry). Otherwise the group is reported, never moved.

### D3: Reconcile report/tag with cortex's on-disk convention
`cortex-duplicate` / `cortex-duplicate-group` frontmatter is owned by cortex
(`cortex/src/duplicates.rs:87`) and cortex *clears* stale instances (`:222`). If
borg writes duplicate markers, it must either reuse those exact fields (and accept
cortex may clear them) or use borg-namespaced fields — and the "no schema change"
claim must be amended accordingly. Default recommendation: **report to
stdout/receipts only, write no frontmatter**, keeping borg out of cortex's
field-ownership entirely; revisit if a persistent tag is needed.

## Alternatives Considered

### Alternative 1: Origin gate (skip `origin: authored`)
- **Description:** Keep source-keyed dedup, exclude `origin: authored` notes.
- **Pros:** Smallest change; mirrors the cortex carve-out.
- **Cons:** Still groups by raw `source:` for `assisted` notes, so any future
  shared non-URL `source` among ingested notes recreates the failure. Patches the
  symptom, not the string-proxy cause.
- **Why not chosen:** Can land us right back here.

### Alternative 2: Source-shape gate (only URL-shaped sources count)
- **Description:** A shared `source` is a dedup key only when URL-shaped.
- **Pros:** Cheap; fixes this specific incident.
- **Cons:** Two distinct notes can share one URL (a GitHub repo-root URL ingested
  as a `repo` note and as a `thread` — the `GithubCreatorMissing` loop already
  assumes repo-root URLs map to multiple notes). Still collapses distinct content.
- **Why not chosen:** Still a proxy; can recur.

### Alternative 3: Near-duplicate (similarity threshold / embeddings)
- **Description:** Flag notes whose body similarity exceeds a threshold.
- **Pros:** Catches reworded re-ingests.
- **Cons:** Any threshold below exact identity can merge two genuinely different
  notes; couples audit to the embedding index; threshold tuning; wrong tool for
  the old desk CPU.
- **Why not chosen:** Directly reintroduces the false-positive class.

### Alternative 4: Whole-file identity (frontmatter included, volatile keys stripped)
- **Description:** Hash the entire file minus volatile frontmatter, flag exact
  matches.
- **Pros:** Also false-positive-proof for *reporting*.
- **Cons:** Strictly lower recall than body-hash — any tag/title divergence on a
  true re-ingest spares the duplicate, and the volatile-key strip list is ongoing
  drift.
- **Why not chosen:** Body-hash catches the same true duplicates with less
  fragility; frontmatter is not where ingest-duplication lives. (Note: D2 still
  brings identity back in as the *second* proof, where it belongs — gating
  removal, not grouping.)

## Technical Considerations

### Dependencies
- `sha2 = "0.10"` — already in `borg/Cargo.toml:23`. No new dependency. SHA-256 is
  stable across toolchains (satisfies "never persist `DefaultHasher`").
- `vault::frontmatter::split_raw` — already used by `audit.rs`; reused, not
  reimplemented.

### Performance
- One file read + `split_raw` + normalize + hash per note, parallelized with the
  existing `rayon` `par_iter`. SHA-256 over note-sized bodies is negligible next to
  the file read already done. No measurable regression vs. the current
  source-extraction pass.

### Security
- None. Local filesystem governance tool; no new external input or surface.

### Observability
- Log (per the function-logging rule): files scanned, short/empty bodies skipped
  (count), candidate group count and sizes, and — in quarantine mode — per-group
  the second-proof verdict (eligible / reported-only with reason). This makes a
  "why was/wasn't this moved" question answerable from the DEBUG log without a
  rerun.

### Testing Strategy
- `tempfile::TempDir` mini-vaults (per rust testing rules). Cases: pais-migration
  regression (distinct bodies, shared source → 0 moves, report only); true
  duplicate with matching identity → 1 move, deterministic keep; identical bodies
  but differing `source:`/identity → reported, not moved; empty/short body never
  groups; multibyte body no panic; report-only path moves nothing. Test bodies in
  `borg/src/audit/tests.rs` (not inline).

### Rollout Plan
- This is a **CLI-only heuristic**; it activates on the next `sb borg audit`
  invocation. `systemctl --user restart borg` is **not** the activation step
  (the earlier draft was wrong) **unless** a daemon path also invokes audit — if
  so, restart only for that. Ship via `bump` patch + `otto install`; no extension
  change → not `otto deploy` (per `feedback-skip-extension-resign`).
- Recovery (Phase 4) runs **once, on the daemon host**; the vault is Syncthing'd,
  so the un-quarantine propagates to other hosts automatically — do not re-run per
  host (receipts-migration precedent).

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| A distinct note silently quarantined | — | High | D0/D2: body-hash reports only; removal needs byte-compare + identity match + opt-in |
| Empty-body notes group as duplicates (same input → same hash) | High if unguarded | High | `MIN_DUP_BODY_LEN` + `None` fingerprint for empty bodies; explicit test |
| Normalization drift makes a true dup look distinct (or vice-versa) | Med | Med | Post-hash byte-compare of normalized bodies before any move (D2); conservative normalization |
| UTF-8 panic in `normalize_body` on multibyte chars | Low | Med | Line/char-oriented only; never byte-slice; multibyte test |
| New `split_frontmatter` drifts from `vault::frontmatter` semantics | — | Med | Reuse `vault::frontmatter::split_raw`; add none |
| Recovery clobbers an existing `notes/<name>.md` | Low | High | Refuse-on-collision; `git mv`; clean-tree precondition; verify count==19; documented rollback |
| Recovery blocked by dirty vault | High (vault is dirty now) | Low | Runbook states the clean-tree precondition; operator commits/stashes first |
| Backward-compat break for scripts passing `--fix duplicate` expecting a move | Med | Med | D1 defines the compat behavior of the new default/opt-in split explicitly |
| True re-ingest with drifted content no longer flagged | Med | Low | Accepted: a drifted re-ingest is a different note; conservative-by-design |

## Open Questions
- [ ] D1 opt-in shape: new `FindingKind::DuplicateQuarantine`, a separate flag, or
      a separate subcommand? And the exact backward-compat behavior for existing
      `--fix duplicate` callers.
- [ ] D3: does report/tag mode write frontmatter at all, and if so reuse cortex's
      `cortex-duplicate*` fields or borg-namespaced ones?
- [ ] Behavior when a candidate group mixes note types, origins, or includes paths
      under excluded folders — report, skip, or special-case?
- [ ] `notes/rule-of-five.md` empty stub — delete (via `rkvr`), restore from
      history, or keep as an intentional redirect? Human decision.
- [ ] After recovery, re-source the 19 to unique per-note `source:` values, or
      leave `source: "pais-migration"` (now harmless to grouping)? Optional cleanup.
- [ ] Is the `origin: authored` exemption still worth adding to `borg audit` as an
      independent belt? (D0/D2 make it unnecessary for safety; it would only reduce
      scanned scope.)

## Review History
- **Self (Rule of Five), 5 passes** — edge-case pass caught the empty-body hash
  collision; excellence pass promoted report-by-default to a decision.
- **Staff Engineer (Codex), Design Review** — incorporated: D0 (reporting vs.
  removal authority) and D2 (second proof) added; D1 CLI/event surface and D3
  cortex-field reconciliation made explicit; reuse `vault::frontmatter::split_raw`;
  recovery section corrected (no `borg/bin/`, dirty-vault precondition, rollback);
  four factual fixes (OrphanReplace does not use the source index; mtime-kept-stub
  hedged; `systemctl restart borg` rollout removed; "SHA collision" → "same input
  → same hash"); observability logging spec added.
- **Architect (Antigravity/agy, Implementation Audit)** — completed 2026-06-19.
  Verdict: load-bearing safety watertight (empty-body guard, `quarantine_eligible`
  byte-compare + `source:` identity, `split_raw` reuse, UTF-8-safe normalization,
  blanket-`--fix` cannot quarantine). Four completeness/acknowledgment findings, no
  correctness bugs: D2 narrowed to `source:` only (ledger-entry dropped), D1
  `--fix duplicate` silently report-only (no shim), group sizes not logged, Phase 4
  not hard-gated on 19. All four addressed or accepted-with-rationale in the
  implementation notes; group-sizes logging fixed in code.

## References
- `borg/src/audit.rs` — duplicate detection (`build_note_index` :726, duplicate
  loop :341, `apply_fix_duplicate` :607; source-index consumers: Mistype :265,
  Blocked/RawTitle :282, GithubCreatorMissing :350; OrphanReplace ledger parse :306)
- `vault/src/frontmatter.rs:247` — `split_raw` (the canonical frontmatter splitter)
- `sb/src/cli/borg.rs:64` — `--fix` FindingKind surface; `:677` quarantine output
- Cortex carve-out + field ownership: `cortex/src/duplicates.rs:39/87/222`,
  `scope.rs:18` (commit `fa3f9a8`)
- Quarantine incident: commit `017363e` (19 notes → `system/quarantine/pais-migration/`)
- Rule of Five: `~/repos/scottidler/obsidian/notes/jeffrey-emanuel-rule-of-five-agentic-llm.md`
- CLI destructive-flag rule: `~/repos/.claude/rules/cli.md`
