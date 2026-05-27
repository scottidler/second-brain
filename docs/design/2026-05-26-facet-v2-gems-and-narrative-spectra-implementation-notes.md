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
