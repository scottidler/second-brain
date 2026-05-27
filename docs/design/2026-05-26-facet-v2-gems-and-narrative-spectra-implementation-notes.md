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
