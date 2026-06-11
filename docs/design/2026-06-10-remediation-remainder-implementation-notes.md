# Implementation Notes: Remediation Remainder

Running record of decisions, deviations, tradeoffs, and open questions while
executing `docs/design/2026-06-10-remediation-remainder.md`. Append-only.

## Phase 1: Schema literal stragglers

### Design decisions
- Used the inline fully-qualified form `note.frontmatter.origin.as_deref() == Some(vault::schema::Origin::Assisted.as_str())` rather than adding a `use vault::schema::Origin;` import — `cortex/src/memgraph.rs:79` and `borg/src/backfill.rs:165` — because that is the exact idiom already established at `cortex/src/autotag.rs:50`, `cortex/src/summarize.rs:398`, and `cortex/src/entities.rs:89`. Matching the peer sites keeps the substitution a one-liner with no import churn.
- The memgraph site was a `matches!(..., Some("assisted"))`; rewrote it as an `==` comparison since `as_str()` returns a `&'static str` and `matches!` against a non-literal pattern is not possible. The truth value is identical.

### Deviations
- None.

### Tradeoffs
- Inline path vs. a module-level `use` import — chose inline to mirror the existing peer sites exactly; a single import would have been marginally shorter but would diverge from the established pattern in these crates.

### Open questions
- None.

## Phase 2: borg config per-transport submodules

### Design decisions
- Module declarations and `pub use` re-exports placed immediately after `APP_NAME` (`borg/src/config.rs:9`), ordered alphabetically (`desktop, discord, ntfy, signal, telegram`) to satisfy rustfmt/clippy import ordering without a manual fmt pass.
- `desktop.rs` imports `APP_NAME` via `use super::APP_NAME;` (an explicit import) rather than referencing `super::APP_NAME` inline at the one use site — cleaner and matches how the const is consumed elsewhere. The const itself stays in `config.rs` per the doc (shared by `lib.rs` and the config-path fallback).
- Each transport submodule carries `use serde::{Deserialize, Serialize};` since the structs derive both. The two private default fns (`default_signal_rate_threshold`, `default_ntfy_server`) moved verbatim into their owning submodules; their `#[serde(default = "...")]` string paths resolve module-locally, so no path qualification was needed.

### Deviations
- None. Per-transport files exactly as the doc specified (no `transports.rs` consolidation).

### Tradeoffs
- Alphabetical `mod`/`pub use` ordering vs. grouping by "primary transports first" — chose alphabetical because it is what rustfmt enforces and avoids a bikeshed.

### Open questions
- During the first full recompile after the split, one borg lib test failed once ("730 passed; 1 failed"); its name was filtered out of that run's output. It did not recur across 15 subsequent runs (all 731 passed) and the full-suite count matches the 734 baseline exactly. The config relocation is behavior-neutral (struct definitions moved behind `pub use` re-exports; `cargo check` and the `config/tests.rs` wire-format tests pass), so this reads as a pre-existing flake that fired under heavy first-compile load, not a regression. Flagging in case the user wants to hunt the flake separately.

## Phase 3: Inline test-mod extraction - vault (11 files)

### Design decisions
- The extraction is mechanized by a Python script (`/tmp/extract_tests.py`) that reads the ORIGINAL block from `git show HEAD:<path>`, writes the block body VERBATIM (no manual dedent) into `<stem>/tests.rs`, and leaves `#[cfg(test)] mod tests;` behind. `cargo fmt` then normalizes the over-indented code to column 0. This is reused for phases 4 and 5.
- Baseline `vault` count: 190 passed, 1 ignored. Post-move: identical.

### Deviations
- None. Pure relocation; the 11 files are exactly those the doc named.

### Tradeoffs
- Verbatim-body + `cargo fmt` vs. a manual 4-space dedent in the script. The first attempt dedented every body line by 4 spaces, which corrupted an embedded YAML raw-string literal in `canonical/tests.rs` (the `serde_yaml::from_str` test then failed with "did not find expected key"). `rustfmt` reformats from the AST and never touches raw-string contents, so writing the body verbatim and letting fmt dedent the code is the only safe mechanization. The script was rewritten to do this; it is the version carried into phases 4-5.

### Open questions
- None.

## Phase 4: Inline test-mod extraction - borg (19 files)

### Design decisions
- The extraction script was extended to walk back over the full contiguous attribute block preceding `mod tests {`, not just a single `#[cfg(test)]` line. `borg/src/pipeline/atomic.rs` carries `#[cfg(test)]` + `#[allow(clippy::unwrap_used)]` on its test module; both attributes stay on the `mod tests;` declaration in the source (they apply to the whole module, now `atomic/tests.rs`).
- Nested modules extract to nested dirs: `extension/schema.rs` -> `extension/schema/tests.rs`, `pipeline/atomic.rs` -> `pipeline/atomic/tests.rs`, `pipeline/inflight.rs` -> `pipeline/inflight/tests.rs`.
- Baseline `borg` count: 734 passed. Post-move: identical.

### Deviations
- None. The 19 files are exactly those the doc named.

### Tradeoffs
- None beyond the verbatim-body + fmt approach already chosen in Phase 3.

### Open questions
- `cargo fmt` is non-idempotent in one pass on these verbatim-extracted files: feeding rustfmt a uniformly over-indented module body makes pass 1 dedent the code but emit a spurious leading blank line, which pass 2 removes. Confirmed for both vault (Phase 3) and borg (Phase 4); two passes converge and `cargo fmt -- --check` is then clean. The procedure is "run `cargo fmt` to convergence" before the CI gate. Content is otherwise byte-verbatim from the original block. Not a blocker, but noting in case the user prefers a single deterministic dedent in the script (would require lexing to avoid corrupting raw-string interiors, the Phase 3 failure mode).

## Phase 5: Inline test-mod extraction - cortex (17 files)

### Design decisions
- The crate root is special-cased: `lib.rs`'s `mod tests;` resolves to `cortex/src/tests.rs`, not `cortex/src/lib/tests.rs`. The script handles `lib`/`main` stems by writing the tests file directly under `src/`.
- `sweep.rs`'s single `#[serial_test::serial(xdg_data_home)]` attribute and its three `// SAFETY`/serialization comments moved verbatim into `sweep/tests.rs` (the attribute is inside the test fn, so the verbatim body copy carries it). The fixture path `"src/sweep/fixtures/cold-notes-expected.md"` is crate-root-relative at runtime (not a source-relative macro), so it is unaffected by the test code moving into `sweep/tests.rs` alongside the existing `sweep/fixtures/` dir.
- Baseline `cortex` count: 259 passed, 1 ignored. Post-move: identical.

### Deviations
- None. The 17 files are exactly those the doc named.

### Tradeoffs
- None beyond the verbatim-body + fmt-to-convergence approach from Phases 3-4.

### Open questions
- None. Final workspace grep gate (`grep -rln 'mod tests {' */src --include='*.rs' | grep -v '/tests.rs'`) returns nothing; all 47 inline test mods are extracted.
