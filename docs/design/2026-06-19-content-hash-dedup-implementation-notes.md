# Implementation Notes: Content-Hash Duplicate Detection

Append-only record of how the implementation interprets or diverges from
`docs/design/2026-06-19-content-hash-dedup.md`. One section per phase.

## Phase 1 + Phase 2: detection swap + fix split + second proof

### Design decisions
- `MIN_DUP_BODY_LEN = 32` chars (counted on the normalized body) — `audit.rs`
  `content_fingerprint`. The doc mandated an empty/short-body guard but left the
  threshold open; 32 chars excludes stubs/redirects/one-line placeholders while
  admitting any real note paragraph.
- `normalize_body` strips TRAILING whitespace per line and leading/trailing blank
  LINES, but PRESERVES leading whitespace within a line — `audit.rs::normalize_body`.
  Markdown indentation (code blocks, nested lists) is semantic; stripping it could
  merge two genuinely different notes, against the doc's core invariant.
- D1 shape (`audit.rs::FindingKind`): added `FindingKind::DuplicateQuarantine` as
  the opt-in destructive selector. `--fix duplicate` and blanket `--fix` are
  report-only; only `--fix duplicate-quarantine` moves files. `DuplicateQuarantine`
  is never produced as a finding `kind()`; it exists solely as a `--fix` selector
  matched in `apply_fixes`.
- D2 identity proof (`audit.rs::quarantine_eligible`): a group is quarantine-
  eligible only if every note's normalized body is byte-identical AND all share a
  non-empty `source:`. Unreadable note → fail-closed (not eligible).
- D3 (gap-fill, also see Phase 3): report/tag mode writes NO frontmatter. The
  `DuplicateReported` / `DuplicateNotEligible` events print to stdout only; borg
  stays out of cortex's `cortex-duplicate*` field ownership. No on-disk schema
  change.
- Keep-selection is now the lexicographically-first path
  (`apply_fix_duplicate`), replacing the mtime-newest tiebreak. Bodies are
  byte-identical post-eligibility, so the choice is arbitrary-but-stable; the old
  mtime rule was what preserved an empty stub over real notes in the incident.

### Deviations
- The doc split this into Phase 1 (detection, report-only) and Phase 2
  (fix-action split + second proof). They were implemented in ONE commit because
  a report-only Phase-1 intermediate would orphan `apply_fix_duplicate` and
  `quarantine_key` as dead code under the crate's `#![deny(dead_code)]`, failing
  CI between phases. Combining keeps every commit compiling and dead-code-free.
- Detection now builds a SECOND index (`build_dup_index`, keyed by content
  fingerprint) alongside the retained source-keyed `build_note_index`. The doc
  implied repointing one index; in practice both are needed — `build_note_index`
  still feeds Mistype / Blocked / RawTitle / GithubCreatorMissing. This is two
  passes over the `.md` set (extra read I/O), accepted for an on-demand audit.

### Tradeoffs
- SHA-256 via the existing `sha2` dep vs. a faster non-crypto stable hash (fnv):
  chose `sha2` — already a dependency, collision-irrelevant here, and the second
  proof re-byte-compares anyway, so hash speed is not on the hot path.
- Two index passes vs. one combined pass producing both keys: chose two simple
  passes over one fused builder, per "no gold-plating" — audit is not latency-
  critical.

### Open questions
- None blocking. The doc's open questions (flag naming beyond
  `duplicate-quarantine`, mixed-type groups, re-sourcing the 19) remain product
  decisions, unchanged by this implementation.
