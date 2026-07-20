# Implementation Notes: Harvest End-to-End Completion (clyde + second-brain)

Design doc: `docs/design/2026-07-20-harvest-completion.md`

## Phase 0: Real null-bearing fixtures + failing regression (spike)

### Design decisions
- Captured live 60-day `clyde session export --since 60d --limit 2000` (1483
  sessions on `desk`, `clyde v0.10.1`) and mined it for the FULL null class
  the Problem Statement names, rather than hand-authoring any record —
  `borg/src/harvest/contract/tests.rs::parse_tolerates_null_string_fields`.
- One real record (`9b17cdba-7995-4be9-a1a4-65af5e7a3250`) carries `cwd`,
  `created`, `title`, `first-prompt`, `git-branch`, `repo`, `model`, and
  `summary` ALL as JSON `null` simultaneously (`n-msgs: 0`, a genuinely
  empty/never-touched session) — the richest single null-bearing row in the
  60d catalog. It anchors `config/eval/distill-fixtures/session/null-string-fields.json`
  alongside four more real sessions covering the partial-null combinations
  (`title`-only-null with `first-prompt` present, `first-prompt`-only-null
  with `title` present, and a real WORK session with `repo` present but
  `model`/`summary` null).
- `config/eval/distill-fixtures/session/empty-body.json` is a REAL
  `clyde session export --id 9b17cdba --with-body` capture: clyde itself
  reports `body: null, body-error: "parsed empty"` for this session — the
  "empty body" class from the doc is a real clyde output, not a
  hypothetical, and it happens to be the same session as the null-bomb
  record above.
- `config/eval/distill-fixtures/session/malformed-record.json` is the one
  deliberately SYNTHETIC fixture: a real session
  (`28b526fb-7061-477d-8399-bf310671d6b5`) plus a hand-edited twin whose
  `session-id` is renamed to `malformed-00000000-0000-0000-0000-000000000000`
  and whose `n-msgs` field (contract type `i64`) is corrupted to the string
  `"not-a-number"`, matching the doc's Phase 1 note that `session_id` stays
  parseable via `serde_json::Value` even on a malformed record — every
  other field is left as the real record's, so the fixture proves "one
  wrong-typed field breaks the batch," not a wholesale fabrication.
- The RED test (`borg/src/harvest/contract/tests.rs`) matches on
  `parse_export`'s `Result` rather than asserting `None` directly on
  `cwd`/`created`/`title`/`first_prompt` — those four fields are still
  plain, non-Option `String` in today's `contract.rs`, so a direct `None`
  comparison would be a COMPILE error, not a runtime RED. The `match` arm's
  `Err` branch panics with the exact underlying serde message, which today
  reads `clyde session export: failed to parse the schema-version-1
  payload: invalid type: null, expected a string at line 11 column 17` —
  the same failure class as the doc's reproduction
  (`sb borg harvest --dry-run --since 60d` -> "invalid type: null, expected
  a string at line 796 column 19"). Assertions that DO run today (and must
  keep passing after Phase 1) are scoped to the fields that are already
  `Option<...>` (`repo`/`git_branch`/`model`/`summary`/`body`/`body_error`).
- README.md in `config/eval/distill-fixtures/session/` gets an appended
  section (append-only, existing Phase-0-of-a-different-doc prose left
  untouched) documenting the three new fixtures and which are real vs
  synthetic-malformed.

### Deviations
- None. Zero production code touched; `contract.rs` is byte-identical to
  the base commit.

### Tradeoffs
- Reused ONE real record (`9b17cdba`) to cover 8 of the 9 required null
  classes (cwd/created/title/first-prompt/repo/git-branch/model/summary) in
  a single fixture row, rather than hunting for 8 separate records each
  isolating one class — the doc's "capture REAL exports covering the FULL
  null class" is satisfied either way, and a single dense real record is
  more honest evidence than eight artificially-isolated ones would be. Four
  more real records supply the partial-null combinations for variety.
- The RED test asserts on the `Result` shape (match + panic-with-message)
  instead of pre-writing Phase 1's eventual `Option`-typed field assertions.
  This means Phase 1 will need to EXTEND this test with direct
  `assert_eq!(..., None)` calls on `cwd`/`created`/`title`/`first_prompt`
  once those fields become `Option<String>` — chosen over blocking Phase 0
  on code that cannot compile against today's contract.
- `malformed-record.json`'s "good" companion record
  (`28b526fb-7061-477d-8399-bf310671d6b5`) also has a null `first-prompt`,
  so today it fails the whole-batch parse for the SAME reason as the
  null-string-fields fixture, not specifically because of the malformed
  `n-msgs`. This is disclosed here rather than swapped for an artificially
  "clean" companion record, because the fixture's job (per the doc) is
  only to prove "current whole-batch parser rejects a malformed element" —
  true regardless of which field trips it — and Phase 1's per-record
  resilience test (Phase 6) is the one that will need a companion record
  with NO other null-string fields, so the malformed-ness is isolated.

### Open questions
- None.
