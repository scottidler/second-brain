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

## Phase 1: Contract null-tolerance + per-record resilience + created guard

### Design decisions
- Relaxed `cwd`/`created`/`title`/`first_prompt` to present-null
  `Option<String>` (`#[serde(default)]`) in `SessionRecord`
  (`borg/src/harvest/contract.rs`). `host`/`scope` left NON-null per the
  code-verified Resolved Decision. `BodyMessage.role`/`text` got a DEFENSIVE
  `Option<String>`, labeled in-code as future-malformed tolerance (clyde
  constructs them non-null today).
- `parse_export` (`contract.rs`) rewritten from one whole-array
  `serde_json::from_slice` to an envelope parse (`RawExport`, `sessions:
  Vec<serde_json::Value>`) + per-element `serde_json::from_value`. A malformed
  element is SKIPPED, logged WARN, and carried out as a `ParseRejection`
  (`session_id` recovered from the element's `session-id` Value BEFORE the
  record is consumed; `index` fallback when the id is unreadable). New return
  type `ParsedExport { export, rejections }`. The schema-version check stays
  FAIL-CLOSED (wrong MAJOR still bails the whole run before any per-record work).
- Durable receipt discipline wired at the run seam
  (`harvest.rs::run_with`): `reader.export_bulk` now returns `ParsedExport`; on
  the LIVE path the parse rejections are converted to `RejectionOutcome`
  (`parse_rejection_to_outcome`, `GateId::Parse`, `StageKind::Raw`, `source:
  clyde://<id>`) and flow through the EXISTING `write_rejections`
  (`received->rejected` receipt + `rejection.yml`) BEFORE `state.save` advances
  the cursor. Dry-run persists nothing and only WARNs (surfaced in
  `HarvestReport.parse_rejections` and the CLI report for the operator's soak
  review).
- Selection-stage `created` guard added in
  `select.rs::evaluate_selection`: a null OR non-RFC-3339 `created` is rejected
  (`GateId::Selection`) so it never reaches `cluster.rs::parse_ts` (which errors
  the whole plan). `modified` stays non-null, so only `created` is guarded.
- Call sites fixed behavior-preserving: `select.rs:149` exclusion match ->
  `.as_deref().unwrap_or("")` on `title`/`first_prompt` (a `None` matches no
  pattern); `select.rs` non-repo message -> `cwd.as_deref().unwrap_or("<null>")`;
  `cluster.rs::cluster_key` -> `cwd.clone().unwrap_or_default()`;
  `cluster.rs::parse_ts` signature -> `Option<&str>` with a fail-loud `None`
  backstop; `pipeline/session.rs:160` title fallback -> `match
  primary.title.as_deref()` (null OR empty -> `Session <id>`);
  `pipeline/session.rs::earliest_created` -> `Option`-guards `created` (warn +
  skip); `render_member_details` -> `title.as_deref().unwrap_or("")`;
  `watermark.rs::canonical_body_text` -> `role`/`text` `as_deref().unwrap_or("")`;
  `stages/alert.rs::format_gate_alert` -> new `GateId::Parse` arm.
- New `GateId::Parse` variant (`types.rs`) so a parse-skip's `rejection.yml`
  tells the truth about which gate declined (not `Selection`).

### Deviations
- `ParseRejection` carries the element `index`, not a raw byte offset, as the
  fallback identifier when `session-id` is unreadable. The doc says "byte-offset
  fallback"; the per-element `serde_json::from_value` seam exposes no byte
  offset (byte offsets only exist for the old whole-slice parse). The element
  index is the equivalent durable positional key - same effect, correct seam.
- Added a `parse_rejections: Vec<ParseRejection>` field to `HarvestReport` and
  two CLI report lines (`sb/src/cli/borg.rs`) beyond the strict Phase 1 bullet
  list, so a parse skip is visible in dry-run output too (the doc calls dry-run
  WARN-only; this is additive visibility, not a persisted receipt, consistent
  with "never limp along silently").
- Consolidated the two per-file XDG_DATA_HOME test locks into ONE shared
  `crate::harvest::TEST_XDG_LOCK` (`harvest.rs`, used by both `harvest::tests`
  and `harvest::publish::tests`). The new live-run test redirects XDG the same
  way `publish::tests` does; a per-file lock let them race the redirected
  receipts DB (caught by `otto ci`: `publish_plan_publishes_and_rerun_is_idempotent`
  flaked once under the full parallel suite). Not called out in the spec, but
  required to keep the suite deterministic.

### Tradeoffs
- The live-path "receipt before cursor advance" test
  (`harvest::tests::live_run_writes_parse_skip_receipt_before_cursor_advances`)
  asserts BOTH the durable `received->rejected` receipt exists AND the state
  file cursor advanced, after a live `run_with`. The strict temporal ordering is
  structural in `run_with` (`write_rejections` precedes `state.save`); the test
  proves both effects landed in the one live run. Chosen over refactoring
  `run_with` to inject a connection purely for a finer ordering probe - the
  existing `publish::tests` XDG pattern is the precedent, and `write_rejections`
  itself is separately unit-tested for the durable-receipt half.
- Per-element `serde_json::from_value` re-parses each already-parsed `Value`
  into a `SessionRecord` (a second pass over the element). At tens-to-low-
  thousands of sessions per run this is negligible, and it buys per-record
  resilience the whole-slice parse cannot give.

### Open questions
- **`sb borg harvest --dry-run --since 7d` success criterion is structurally
  unverifiable under clyde's defaults.** clyde `session export` defaults
  `--dormant-after 7d` and computes `dormant = now - modified > 7d`
  (`clyde/sessions/src/db/query.rs:316`, `clyde/clyde/src/cli.rs:225`).
  Harvest's `--since 7d` returns only sessions with `now - modified <= 7d`, which
  are by definition NOT dormant - the dormancy gate is the FIRST selection
  check, so a 7d window can never select a session. Confirmed live:
  `clyde session export --since 7d` reports 0 dormant of 174. The `--since 60d`
  criterion PASSES (exit 0, 209 publishable, previously aborted by the null-string
  bug). The 7d publishable>0 criterion is reported UNVERIFIED (mis-specified);
  it needs the parent/Scott to either drop it or restate it (e.g. the steady-
  state cursor window, or a `--since` longer than `--dormant-after`).
