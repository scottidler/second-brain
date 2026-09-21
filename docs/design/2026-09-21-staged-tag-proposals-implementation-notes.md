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
