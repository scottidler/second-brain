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

## Phase 5 (partial: timer env-bootstrap + PATH hygiene)

This is the CODE portion of Phase 5 only (design doc bullets "Secret bootstrap
on the timer's `.service`" and "PATH hygiene in all generated units"). The
remaining Phase 5 work - wiring `--install` into `sb bootstrap`, running
`--install` on the daemon host, the dry-run soak, and the live flip - is
unimplemented; see Open Questions.

### Design decisions
- Added `env_bootstrap: Option<EnvBootstrapConfig>` to `HarvestConfig`
  (`borg/src/config/harvest.rs`), reusing the EXISTING `EnvBootstrapConfig`
  type (`borg/src/config.rs`, already used by `DaemonConfig.env_bootstrap` and
  cortex's identical type) rather than inventing a parallel harvest-specific
  struct - same shape, same serde behavior, one type to reason about.
- `harvest::timer::render_units` (`borg/src/harvest/timer.rs`) now emits the
  `ExecStartPre=/bin/sh -c '<command> > <env_file>'` + `EnvironmentFile=-<env_file>`
  pair when `config.harvest.env_bootstrap` is `Some`, byte-for-byte the same
  directive shape `borg::service::install_systemd` and
  `cortex::daemon::render_systemd_unit` already emit for the daemons. `None`
  omits both directives - a host with nothing to bootstrap still gets a
  valid, complete unit (verified by
  `harvest::timer::tests::service_omits_env_bootstrap_when_unconfigured`).
- PATH hygiene applied identically in all three generators
  (`borg/src/harvest/timer.rs`, `borg/src/service.rs`,
  `cortex/src/daemon.rs`): dropped the stale `{home}/go/bin` segment (the
  hand-built fabric binaries there were retired 2026-07-20), and inserted
  `{home}/.local/share/mise/shims` FIRST (ahead of `.local/bin`) so the
  mise-managed fabric shim wins over any stale duplicate elsewhere on PATH.
- Added `PartialEq` to `EnvBootstrapConfig` (`borg/src/config.rs`) so
  `HarvestConfig`'s existing `#[derive(PartialEq)]` (used by
  `assert_eq!(config.harvest, HarvestConfig::default())` in
  `config/tests.rs`) keeps compiling with the new `Option<EnvBootstrapConfig>`
  field.
- Extracted a pure `render_systemd_unit` function out of
  `borg::service::install_systemd` (previously the string-building was
  inlined in the async, filesystem-touching function, unlike
  `cortex::daemon::render_systemd_unit` which was already split this way).
  This mirrors the existing cortex pattern and is what makes
  `borg/src/service/tests.rs` possible at all - `install_systemd` itself
  touches the filesystem and shells out to `systemctl`, which is not a unit-test
  seam.
- Documented the new `harvest.env-bootstrap` config block in
  `config/templates/borg.yml.example` (commented-out example, matching the
  existing `cortex.yml.example` `daemon.env-bootstrap` documentation pattern)
  and corrected the stale `~/go/bin` guidance in `docs/onboarding.md`'s "Known
  traps" section to describe the mise-shims mechanism instead.

### Deviations
- **Shared helper vs replicated block:** did NOT extract a single shared
  `render_env_bootstrap(bootstrap: &EnvBootstrapConfig) -> String` helper used
  by all three generators, even though the `ExecStartPre` +
  `EnvironmentFile` block is now byte-identical in three places
  (`timer.rs`, `service.rs`, `daemon.rs`). Reason: `service.rs` and
  `timer.rs` live in the `borg` crate while `daemon.rs` lives in the
  `cortex` crate, so a shared helper would need a new home in a crate both
  depend on (`vault`), which is a larger surface change than this phase's
  scope ("timer env-bootstrap + PATH hygiene") calls for, and `vault` is
  explicitly config/schema-first, not systemd-unit-rendering code. Per the
  task's own guardrail ("if a shared helper is awkward across the borg
  module boundary, replicating the small block is acceptable"), the block is
  replicated 3x (2 in borg, already 1 pre-existing in cortex before this
  phase). Each site is independently tested, and the tests would each catch a
  divergence.
- **"Runtime-dir resolution the daemon uses" does not exist in code.** The
  task description asked to "mirror how the daemon derives its env-file
  path/uid... use the same runtime-dir resolution the daemon uses." There is
  no such resolution: `EnvBootstrapConfig.env_file` is a plain operator-supplied
  config value (e.g. `/run/user/1000/borg.env` in `borg.yml`/`cortex.yml`),
  never computed from `getuid()` or `$XDG_RUNTIME_DIR` in Rust. The "distinct
  env-file" requirement is therefore satisfied entirely at the config layer:
  `harvest.env-bootstrap.env-file` is a separate YAML key from
  `daemon.env-bootstrap.env-file`, and the shipped example
  (`config/templates/borg.yml.example`) names it `sb-harvest.env` next to the
  daemon's `borg.env`. No code changes needed or made to uid/runtime-dir
  handling.

### Tradeoffs
- `render_systemd_unit` (borg) takes `exe_path: &str` (matching
  `install_systemd`'s existing `&str` parameter) rather than harmonizing it to
  `&Path` like `cortex::daemon::render_systemd_unit`'s `binary: &Path`. Chosen
  to minimize the diff at the one call site inside `install_systemd`, which
  already holds `exe_path: &str` from `std::env::current_exe()... .display().to_string()`
  further up the call chain (`install_service`); re-typing that chain end-to-end
  is out of scope for a PATH/env-bootstrap phase.
- Left `borg::harvest::config::HarvestConfig`'s doc comment claiming the
  timer unit "bakes in nothing but `OnCalendar`" unchanged, since that
  sentence is specifically about the `.timer` file (which is still true -
  `env_bootstrap` and PATH only affect the `.service` file). Did not also
  add a parallel sentence documenting `env_bootstrap`'s effect on the
  `.service` file at that exact spot, to avoid conflating the two unit
  files' contracts; the effect is documented at `render_units`'s own doc
  comment instead.

### Open questions
- **Phase 5's non-code bullets are NOT done and are out of scope for this
  phase per the phase-implementer contract:** wiring `sb borg harvest
  --install` into `sb bootstrap`, running `--install` once on the daemon
  host, updating `CLAUDE.md` to document the new units, the `mode: dry-run`
  soak for one cycle, and the flip to `mode: live` all remain. The design
  doc's Phase 5 success criteria that require an INSTALLED, RUNNING unit
  ("`sb-harvest.timer` is installed + enabled"; "two consecutive runs
  double-ingest nothing"; "a live timer run publishes a note with non-empty
  claims") are UNVERIFIED by this phase and need the parent orchestrator (or
  Scott) to run them against the real daemon host.
- **The two `otto ci` test failures seen mid-session
  (`harvest::watermark::tests::lock_releases_on_drop`,
  `harvest::publish::tests::publish_plan_publishes_and_rerun_is_idempotent`)
  were NOT caused by this phase's changes** - neither `watermark.rs` nor
  `publish.rs` was touched here, both tests pass cleanly in isolation
  (`cargo test -p borg -p cortex --lib harvest -- --test-threads=1`), and the
  Phase 1 notes above already document
  `publish_plan_publishes_and_rerun_is_idempotent` flaking once under the
  full parallel suite. `otto ci` re-run clean immediately after. Flagged here
  rather than silently ignored, per "tests must bite" - if this recurs it is
  a pre-existing test-isolation gap (shared temp-dir file locks across
  parallel test binaries), not a Phase 5 regression, and is a candidate for a
  future hardening pass, not this phase's scope.
