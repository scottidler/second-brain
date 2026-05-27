# Implementation Notes — facet v2 gems and narrative spectra

Append-only record of design decisions, deviations, tradeoffs, and open questions
that arose during execution of `docs/design/2026-05-26-facet-v2-gems-and-narrative-spectra.md`.

Per `~/.claude/skills/how-to-execute-a-plan` Step 2.5: new entries are appended;
never edit prior entries. If a later decision supersedes an earlier one, write a
new entry that says so explicitly.

---

## Phase 1: Prototype the extract v2 pattern on a real session

Phase 1 was executed during doc Pass 2 (before this skill was invoked), but is
formalised here for the record. Artifacts: `facet/patterns/facet-extract-v2.md`,
`facet/tests/fixtures/v2/slice-rename-input.yaml`,
`facet/tests/fixtures/v2/gems-prototype-rename-slice.json`, `bin/build-session-slice.py`.

### Design decisions

- **Per-turn cap of 1500 chars for `ai_says` / `user_says` text** — `facet/patterns/facet-extract-v2.md` line 111 — chosen to keep verbatim turns intact while bounding any single gem's contribution to a fabric call's output budget; revisit if real sessions show truncations clipping the apprenticeship recipe.
- **Tool-result truncation rule (>800 chars → placeholder)** — `facet/patterns/facet-extract-v2.md` lines 114-122 — added in this skill invocation after Architect Round 2 flagged the verbatim-tool-result blowout risk; not present in the original Pass-2 prototype pattern.
- **`bin/build-session-slice.py` as line-range chunker** — Python reproducer; the heuristic is range-based, not semantic. Phase 3 promotes this to library code with user-turn-boundary + max-50-turn + 4-turn-overlap requirements.

### Deviations

- **None from the design doc as written.** The pattern updates landed in this session match the doc's tool-result truncation spec.

### Tradeoffs

- **Manually curated 200-turn slice vs. random session sample** — chose curated to validate the schema is achievable before investing in chunker library code. Trades generalisation evidence (N=1) for a clean prototype run. Risk table now reflects this honestly (malformed-JSON risk stays at Medium until multi-session soak).
- **JSON output vs. YAML output** — chose JSON for v2 (stricter schema, less LLM flexibility to flub). v1 was YAML; the dirty tree's `facet-extract.md` v1 pattern was also converted to JSON during Pass-2 scaffolding so v1 and v2 share a parser.

### Open questions

- None for Phase 1. The schema, prototype, and fixtures are complete; chunker generalisation is a Phase 3 question.

---

## Phase 2: Ledger schema + bash migration + Rust models

Artifacts:
- `bin/migrate-facet-v2.sh` (bash script; idempotent; v2 tables alongside v1)
- `facet/src/gems.rs` + `gems/tests.rs` (`Gem`, `InteractionTurn`, `Review` + `content_hash` impl)
- `facet/src/narrative.rs` + `narrative/tests.rs` (`Narrative`, `NarrativeAxes`, `Archetype`, `SpectrumStatus`)
- `facet/src/dream.rs` + `dream/tests.rs` (`Dream` enum; kebab-case tagged JSON)
- `facet/src/lib.rs` (new module declarations)
- `facet/Cargo.toml` (`sha2`, `hex` deps added via `cargo add`)

### Design decisions

- **SQLite schema split into four tables, not two with JSON columns** — `bin/migrate-facet-v2.sh` — design doc named `gems`, `interaction_turns`, `narratives`, `narrative_axes` as separate tables; honored that split. `interaction_turns` is FK to `gems.id` with a `seq` column for ordering; `narrative_axes` is one row per narrative (FK PK) so queries that don't need axes can ignore the join.
- **`Archetype` and `SpectrumStatus` Rust enums introduced beyond doc Data Model** — `facet/src/narrative.rs` — the doc names these as frontmatter values (`facet-spectrum-archetype: session | cross-session | evergreen`, `facet-spectrum-status: active | rejected`) but does not require a Rust type. Adding typed enums now so Phase 5's narrate-pass code can be discriminated-union safe; serde renames to kebab-case so the frontmatter values are the wire format.
- **`Gem::content_hash` hashes both AI and user turn UUIDs, sorted** — `facet/src/gems.rs:73` — design doc says "sha256(sorted turn UUIDs in span)" without specifying which turns. Chose to hash every UUID in the gem's interaction (both ai_turn_uuid and user_turn_uuid for every turn), sorted ascending, joined with `|`, hex-encoded. This makes the hash maximally stable against chunker shifts: if a chunker re-decide doesn't add or remove any turn, the hash is unchanged.
- **`boundary_user_turn_uuids()` returns `Option<(&str, &str)>`** — `facet/src/gems.rs:96` — empty interaction is invalid (gem must have >= 2 turns per the v2 pattern) but the type returns `None` rather than panicking so callers can decide. The doc's idempotency-key spec stores `first_user_turn_uuid` and `last_user_turn_uuid` for inspection only.

### Deviations

- **None from the design doc spec.** The schema and structs match the Data Model section verbatim except for the additive `Archetype`/`SpectrumStatus` enums noted above.

### Tradeoffs

- **JSON columns for `Vec<String>` fields (`context_loaded`, `context_missing`, `tags`, `gem_ids`, `mode_mix`, `repos`, `workitem_ids`) vs. side tables** — chose JSON columns. Reasons: (a) these are always read together with the parent row, never queried independently; (b) one less JOIN per gem-read; (c) write transaction stays small. Tradeoff: cannot index into them efficiently, so any future "find all gems with tag X" query needs a json_each() scan or a denormalised tag table. Acceptable for the current corpus size.
- **`expect(...)` over `unwrap()` in tests** — the crate denies `clippy::unwrap_used` at the root, which extends into test modules. Matched the existing convention from `facet/src/ledger/tests.rs` (uses `.expect("reason")`). Adds one expected message per call, costs no runtime, gives a better panic message on test failure.

### Open questions

- **Should the v2 schema also be registered with `facet/src/ledger/schema.rs`'s `MIGRATIONS` slice as a v2 entry?** Currently no - bash-only per the doc. Phase 7 cleanup (drop v1 tables) is the natural moment to fold v2 into Rust schema management. Until then, fresh installs need to run `bin/migrate-facet-v2.sh` after the v1 schema is created by the Rust `migrate()` function. The bash script errors with a clear message if the DB doesn't exist yet.
- **Should `Gem::content_hash` include the `task` text or other gem-level fields?** Currently it's pure-UUID. If an extract re-run produces a different `task` summary for the same turn span, we'd lose that revision. Argued the other way: the UUIDs ARE the canonical content; gem-level fields are LLM-derived and naturally re-generated. Defer until Phase 3 surfaces an actual case.

---

## Phase 3: Gem extraction library

Artifacts:
- `facet/src/extract.rs` (reshape into dispatcher; `pub mod v1; pub mod v2;` plus a back-compat `pub use v1::mine;` re-export so existing internal callers still resolve)
- `facet/src/extract/v1.rs` + `facet/src/extract/v1/mine.rs` (verbatim move of v1 logic; only the `super::{ExtractOutput, ExtractedMoment}` import path widened to `crate::extract::`)
- `facet/src/extract/v1/mine/tests.rs` (moved alongside)
- `facet/src/extract/v2.rs` + `extract/v2/chunker.rs` + `extract/v2/chunker/tests.rs` (user-turn-boundary chunker with 50-turn cap and 4-turn overlap per Phase 3 sub-spec)
- `facet/src/extract/v2/gems.rs` + `extract/v2/gems/tests.rs` (`ExtractedGem` LLM-shape, `mine_gems` async pipeline, JSON parsing, idempotent persistence)
- `facet/src/ledger/gems.rs` + `ledger/gems/tests.rs` (`NewGem`, `upsert_gem`, `gem_by_id`, `gem_by_content_hash`, `gems_for_workitem`, `apply_facet_v2_schema`)
- `facet/src/daemon/harvest.rs` (one-line import path change from `extract::mine` to `extract::v1::mine`)
- 22 new unit tests covering chunker shapes, ledger gem upsert idempotency, content_hash stability, and the v2 mine pipeline with `FakeFabric`

### Design decisions

- **`extract.rs` becomes a thin dispatcher namespace; v1 and v2 each own their full extract pipeline.** No common return type, no shared trait. The doc said "dispatcher" — the cleanest dispatch in Rust is namespace-level, not trait-object-level, because v1 returns `Vec<ExtractedMoment>` and v2 returns `Vec<Gem>` and downstream rendering pipelines are different. The CLI `--v1` flag (Phase 4) routes to the right path.
- **`ledger/gems.rs` carries the V2 DDL as a Rust constant in addition to `bin/migrate-facet-v2.sh`.** Necessary so `Ledger::open_in_memory()` in tests can produce a working v2 schema. The Rust copy is documented as "must match the bash script"; both are idempotent CREATE TABLE IF NOT EXISTS so divergence is mechanically detected at runtime. This is a minor evolution of the Phase 2 stance ("v2 schema is bash-only").
- **`Ledger::upsert_gem` deletes-then-reinserts interaction_turns on every upsert.** A re-extract that changes per-turn tags must propagate; appending would double rows, leaving stale rows would lose the revision. The delete is bounded (per-gem) so the write transaction stays well under the 200 ms rule.
- **`gems_for_workitem` collects gem rows in an inner scope, then re-borrows the connection for interaction-turn loading.** Rusqlite's `prepare_cached` holds an immutable borrow until the `CachedStatement` drops; loading interaction turns needs a mutable borrow. Restructured to avoid double-borrow without dropping the prepared-statement optimisation. See `facet/src/ledger/gems.rs:gems_for_workitem`.
- **`ExtractedGem` is a separate type from `Gem` in `extract/v2/gems.rs`.** The LLM emits only the gem-shape fields, not server-side ones (`workitem_id`, `session_uuid`, `extractor_model`, `extracted_at`). Keeping these as two types means the `Gem` struct's required fields stay genuinely required (no `#[serde(default)]` salt-pile) and the conversion site is explicit.

### Deviations

- **CLI `--v1` flag plumbing deferred to Phase 4.** The doc puts dispatcher wiring in Phase 3 but a `--v1` flag with no v2 renderer in place produces gems that nothing renders. Phase 4 (prism renderer v2) ships the flag and the dispatch logic together so the user-visible behaviour matches the flag's name from the first commit. The v2 extractor is library-callable today; the daemon still calls `extract::v1::mine::mine_moments` exclusively.
- **`mine_gems` defensively drops `ExtractedGem`s with empty `interaction` arrays instead of erroring the whole chunk.** Not in the spec; resulted from writing tests that exposed how a partially-malformed LLM output (one bad gem plus several good ones) would otherwise lose all the good gems. Defensive drop is logged at WARN.

### Tradeoffs

- **`chunker` is a sync function operating on owned `Vec<Turn>` chunks vs streaming.** Owned chunks are simpler and let each chunk be passed to a separate fabric task by value. The cost: cloning the turn data once per chunk. The 50-turn cap with 4-turn overlap means chunks share at most 4 turns; the cloning overhead is small vs the network/LLM call cost.
- **Chunker fallback "hard split at window_end when no user boundary is found" + "force +1 progress when next_start <= start" — both log at WARN.** A purely heuristic chunker can in theory pathologically split a stream of all-assistant turns; the fallbacks make the function provably terminate. Tested.
- **`extract.rs` retains the `pub use v1::mine;` back-compat re-export** so any caller that types `crate::extract::mine` still resolves (the doc rewrite removed `pub mod mine;` but I kept the symbol reachable for one cycle). Phase 7 removes the re-export and v1 entirely.

### Open questions

- **Does v2 need a separate `extract.gem_extract_model` config knob, or is reusing `extract.extract_model` (sonnet by default) enough?** Currently reusing. If gem extraction wants opus selectively, add a knob. Defer until shadow runs surface a quality gap.
- **Should the chunker's max-50 / overlap-4 be configurable?** Currently hardcoded as module-level consts. The doc names "50 turns (configurable)" but does not name where. Probably belongs under `ExtractConfig`; defer until Phase 4 wires the v2 path into config.
- **Phase 3 sub-spec called for "per-turn AND per-gem mode tags" — implemented. But the doc's risk table also calls out tag-vocabulary drift between per-turn and per-gem.** The pattern enforces the same closed list in both places; no separation of vocabularies. Note for Phase 5 narrate-pass: if it relies on tag mix, it should look at gem-level tags first, fall back to aggregating turn tags.

---
