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

## Phase 3: Stop `sb bootstrap --force` clobbering generated state

### Design decisions
- `extract_canonical_assets` now has three sets, not two (`sb/src/cli/bootstrap.rs:262`): per-host templates, **generated state** (`tag-proposals.yml`, `glossary.yml`), and shipped shared YAMLs (`canonical-tags.yml`, `tag-mapping.yml`). The generated set is its own loop with its own comment rather than being folded into the template loop, because the *reason* they are write-if-missing differs (machine-produced vs per-host) and a future reader deciding where a new file belongs needs that distinction.
- `shared_config_findings` (`sb/src/cli/checks.rs:241`) carries a comment saying why `tag-proposals.yml` is absent and that `glossary.yml` was never there, so nobody "restores" either one later.

### Deviations
- None.

### Tradeoffs
- Added a third test (`extract_seeds_generated_state_when_absent`) beyond the two preservation tests the doc names. Write-if-missing has two halves and the phase reclassifies two files; a test that only proves the preserve half would pass if the files stopped being written at all.
- The live success criteria were verified by driving the built `sb` binary against two throwaway `XDG_CONFIG_HOME` trees rather than only through the unit tests, since the criterion is about `sb bootstrap --force` end to end. Observed: generated files byte-identical across `--force`, `canonical-tags.yml` still refreshed from the embedded copy, `sb doctor` printing no `tag-proposals`/`glossary` drift line, and a fresh tree receiving both seeds.

### Open questions
- None.

### Consequence (stated per the design doc)
- Existing installs stop receiving repo updates to `glossary.yml`. That is the point: a `concept-promote` must survive a deploy. The operator path for a genuine upstream glossary change is to delete the local file and re-run `sb bootstrap`.

## Phase 4: One staging root for cortex

### Design decisions
- The hoisted field carries `#[serde(rename = "staging-root")]` exactly as the doc requires (`cortex/src/config.rs:24`), and the doc comment states WHY, so nobody "cleans it up" later: top-level `Config` renames per field rather than via `rename_all`, so dropping the rename would silently make the key `staging_root` and ignore `staging-root:`.
- `cortex/src/testutil.rs:442` builds `Config` as a struct literal rather than from `Default`, so the hoist required adding the field there too. It uses `::vault::paths::borg_stages_dir()` with a leading `::` because `crate::vault` shadows the `vault` crate in that file (`cortex/src/testutil.rs:135`).
- The `staging-root` test parses literal YAML text and asserts three things separately: the key is honored (not silently defaulted), the value is tilde-expanded, and the path is the one given. The doc asks for the first; the second is the invariant the root CLAUDE.md calls out as a repeat bug source.

### Deviations
- The doc puts the `sweep.staged-proposals` template key in this phase, but `config/templates/cortex.yml.example` had no `sweep:` section at all, so the doc's premise ("that file documents every tunable") was not accurate. Added the section with the key commented out, as the doc directs; the field itself and the assertion on its parsed value stay in Phase 6.

### Tradeoffs
- Fixed a **pre-existing test race** rather than routing around it. `cortex/src/tests.rs`'s three `lint_apply_*` tests resolve `canonical-tags.yml` through `vault::paths` off `XDG_CONFIG_HOME` without taking `crate::testutil::lock_env()`, so they race every test holding a `hermetic_config_home()` tempdir, which is deleted on drop. Two of them failed in this phase's first CI run with "No such file or directory" on a `.tmpXXXX/sb/canonical-tags.yml` path. Proven a race, not a Phase 4 regression: the same two pass when run in isolation (`cargo test -p cortex --lib lint_apply`, 3 passed) and fail only under the full parallel suite. Nothing in Phase 4 touches lint, canonical loading, or env. The fix is `lock_env()` on all three (the third has the same defect and merely got lucky), which is the pattern `cortex/src/sweep/tests.rs` already uses. Chose the lock over `hermetic_config_home()` because the latter substitutes a small fixture vocabulary and would change which tags these tests see as non-canonical.

### Open questions
- None.

## Phase 5: Read and aggregate the staged candidates

### Design decisions
- `Proposal::source` / `ProposalSource` land here rather than in Phase 2, per the Phase 2 note above: this is the phase that can compute note/staged/both. `scan_proposals` sets `ProposalSource::Note` until Phase 6 wires the union.
- **Tier 3's "source match" is `Note.frontmatter.source` vs the receipt's `raw_input`** (`cortex/src/proposals.rs:283`). The design doc says "whose `source` matches the receipt's", but the receipts table has no `source` column (`borg/src/receipts.rs:224`): the columns are `trace_id, received_at, method, kind, raw_input, status, terminal_at, note_path, failure_stage, failure_reason, replay_of, degraded`. `raw_input` is the verbatim input, which for a URL ingest is the URL the note records as `source:`. That is the cross-check the doc describes.
- `NoteIndex` borrows the notes rather than cloning (`vault/src/identity.rs:33`): it is rebuilt on every sweep tick over a 3,800-note vault.
- `resolve_trace` returns `None` for three distinct situations (no claimant, several survivors, a `superseded-by` cycle) because every caller treats them identically: fall through to the next tier. Documented on the function so the collapsing is deliberate rather than accidental.
- `read_staged_candidates` deserializes into a local `StagedDistilled { tags, meta }` (`cortex/src/proposals.rs:74`), not `vault::distilled::Distilled`: the full type pulls every per-kind payload and the transcript, and this parses 10.6 MB of YAML per tick. Unknown keys are ignored by design - a narrow read of someone else's artifact is not a schema this scanner owns, which is the opposite of the `deny_unknown_fields` call in Phase 2 (that one IS cortex's own file).

### Deviations
- **`read_staged_candidates` returns `StagedScan`, not `Vec<StagedCandidates>`.** The doc's API block gives the bare `Vec`, but the doc's own text requires two things a `Vec` cannot carry: "the report carries both counts" (missing vs unreadable) and `ProposalsFile.staged_window`. `StagedScan` also carries `scanned: bool`, which is how the Resolved-Decisions host rule ("staging root does not exist -> skip the arm, exit 0" vs "exists but unreadable -> Err") is expressed in one return value instead of making every caller re-run the `Path::exists` check.
- **`vault::identity` and `cortex::proposals` are documented in `vault/AGENTS.md` and `cortex/AGENTS.md` in THIS phase**, not Phase 8. `otto ci`'s `agents-map` task fails any undocumented module the moment it exists (`FAIL: cortex/AGENTS.md: proposals.rs undocumented`), so deferring would mean shipping three red-CI phases. Phase 8 still owns the rest of its doc corrections.

### Tradeoffs
- The frozen fixture's first draft recorded `ht-970437f1`'s `note_path` under a basename (`course.md`) that matched no note, so no tier resolved it and the test failed with `system-prompt` at 3. The fixture was wrong, not the code: live, both receipts record the pre-classify `inbox/` path whose basename IS the surviving note's stem, so tier 3 resolves them. Corrected the fixture to that shape and left a comment saying so, because a fixture that cannot reproduce the bug it guards is worse than no fixture.
- Clippy's workspace-wide `unwrap_used` ban applies to test code too; the new tests use `expect()` with a message.

### Observed, one live read (not asserted anywhere)
- `read_staged_candidates` over the live staging root: 659 traces with `distilled.yml`, 45,637 without, 0 unreadable, window `2026-08-05T07:54:16Z .. 2026-09-21T14:51:16Z`, **83.6 ms**. Matches the corpus the design doc measured.

### Open questions
- None.
