# Implementation Notes: Staged Tag Proposals

## Phase 1: Stop re-proposing human rejects
### Design decisions
- `vault::canonical::is_rejected(raw_tag, mapping)`, `vault/src/canonical.rs:166`: a thin `matches!(mapping.get(raw_tag), Some(None))` predicate, placed next to `match_to_canonical` since it exists precisely to disambiguate the two `vec![]` cases that function conflates.
- `scan_proposals` now calls `is_rejected` and `continue`s before calling `match_to_canonical`, `cortex/src/sweep.rs:207-215`: matches the doc's literal instruction ("skips a tag when `is_rejected` is true, BEFORE the `match_to_canonical` emptiness test"), so a rejected tag never even reaches the ambiguous empty-vec path.

### Deviations
- None. Implemented exactly as specified: new `vault::canonical::is_rejected`, guard in `scan_proposals` ahead of the `match_to_canonical` emptiness test.

### Tradeoffs
- Added unit tests for `is_rejected` itself in `vault/src/canonical/tests.rs` (null mapping, no-match, and mapped-tag cases) in addition to the doc-mandated `scan_proposals` test, per the "every public function gets a test" bar, rather than relying solely on the integration-level `scan_proposals` coverage the doc's success criteria names.

### Open questions
- None.

## Phase 2: Harden the proposals file

### Design decisions
- `Proposal::source` (the `ProposalSource` enum in the doc's Data Model) is **not** added here; it lands in Phase 5, `cortex/src/sweep.rs:115`. Phase 2's bullet list enumerates its schema changes exhaustively (drop `suggested_canonical` and `action`, rename `notes` to `sources`, add the two serde attributes) and does not name `source`, while Phase 5 is the phase that can compute note/staged/both. Adding a field here whose only possible value is `Note` would be gold-plating a phase that ships alone.
- `write_proposals` still *parses* an existing queue before replacing it, `cortex/src/sweep.rs:245`, even though the parse result is discarded under overwrite semantics. The doc asks for both "propagate a corrupt file" and "overwrite"; a validating read is the only way to satisfy both, and it is what makes a broken hand edit loud instead of silently reverted.
- A missing `tag-proposals.yml` is not an error: the `path.exists()` guard skips the validating read. Only a file that exists and cannot be parsed fails the scan.
- `load_proposals` and `write_proposals` errors now interpolate `path.display()`, `cortex/src/sweep.rs:254`/`:266`, because the doc's success criterion is that the failure *names the file*; the previous `wrap_err` strings were static.
- `scanned_at` is `chrono::Utc::now().to_rfc3339()`; `chrono` is already a direct `cortex` dependency (`cortex/Cargo.toml:10`), so no new dep.

### Deviations
- None.

### Tradeoffs
- `write_proposals` keeps its `(config, proposals)` signature rather than gaining a `staged_window` parameter now. Phase 5/6 own the staged arm and will thread the window through then; a parameter that can only be `None` for two phases is a worse intermediate state than one edit later.
- Tested overwrite semantics through the public `write_proposals` against a `tempfile` queue rather than unit-testing a extracted pure renderer. The behavior under test is the file replacement itself, which a pure function would not exercise.

### Open questions
- None.
