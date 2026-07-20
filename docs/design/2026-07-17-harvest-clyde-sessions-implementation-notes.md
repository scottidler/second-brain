# Implementation Notes: Harvest - Claude Sessions into the Vault

Design doc: `docs/design/2026-07-17-harvest-clyde-sessions.md`

## Phase 0: Contract spike (zero code)

### Design decisions

- Ran the shipped `clyde session export` binary (`clyde v0.10.1`, on PATH,
  `~/.cargo/bin/clyde`) against the live catalog on `desk`
  (1450 sessions, `schema-version: 1`) - no read of `sessions.db` or any
  `.jsonl` at any point, per the design doc's hard constraint.
- Confirmed `clyde v0.10.1` predates the `files-touched-export` branch
  (`ebf9aff` is HEAD; the branch tip carrying `files-touched`/`repos-touched`
  is unmerged in `~/repos/tatari-tv/clyde`), so the captured envelopes'
  omission of `repos-touched` is the expected contract-v1 shape, not a
  capture mistake.
- Field-by-field mapping, every Selection signal (design doc "Selection
  (what earns a note)" section) against the real captured contract:

  | Selection signal | Contract field(s) | Verified how |
  |---|---|---|
  | `dormant: true` | `dormant` (bool) | present in both fixtures, both `true`/`false` observed live |
  | `enrich-status: ok` | `enrich-status` (`ok\|skipped-personal\|skipped-empty\|failed\|null`) | `ok`, `skipped-personal`, `failed`, `null` all observed live (596/579/45/230 of 1450 rows respectively); `skipped-empty` has zero live rows in the current catalog - confirmed instead as a frozen legal value in `clyde/docs/design/2026-07-17-session-export-contract.md:87`, which cites the exact write sites (`db.rs:358/388/400`) |
  | `n-msgs >= threshold` | `n-msgs` (integer) | present, values 3-7120 observed across the catalog |
  | cwd is a real repo | `cwd` (string) + `repo` (string, present-null) | both present; `repo` populated (`tatari-tv/marquee`, `tatari-tv/pagerduty-cli`, `NateBJones-Projects/ringer`) and present-as-`null` for non-repo cwds, matching the doc's "present-null, not omitted" claim |
  | title/first-prompt exclusion patterns | `title`, `first-prompt` (string) | both present on every record |
  | thread-cluster key `(cwd, git-branch)` | `cwd`, `git-branch` (string) | both present; observed values include `"HEAD"` (no branch/detached), `"main"`, and a real feature branch (`release-promote-prod`) |
  | body need (`--with-body`, `body-truncated`) | `clyde session export --id <id> --with-body` -> `body` (array of `{role, text, subagent}`) + `body-truncated` (bool) + `body-error` (string\|null) | `body` array captured in `with-body-envelope.json`; `body-truncated: false` there, separately confirmed `true` via `--id <id> --with-body --max-body-bytes 500` against a 1151-message session (body array empties to `[]`, `body-error: null`) |
  | `repo` present-null shape | `repo` (string\|null, no `skip_serializing_if`) | confirmed: appears as literal `null` (not an omitted key) on non-repo-cwd sessions in `bulk-envelope.json` |
  | `repos-touched` (may be omitted, contract v1) | `repos-touched` (`Option<Vec<String>>`, omitted when the underlying `files_touched` column is NULL) | confirmed OMITTED (key absent entirely) on every session in both captured envelopes - matches the doc's expectation that clyde's files-touched branch is unmerged as of this capture |
  | `cursor` | top-level envelope `cursor` (integer) | present on every export call; watermark-store target for Phase 3 |
  | `modified` | `modified` (ISO8601 string) | present; feeds `--since <span>` first-run backfill filtering |
  | `scope` | `scope` (`work\|personal`) | present, both values observed live; re-derived at export time via `classify(cwd)` per clyde's own contract doc (never the nullable stored column) |
  | `redaction-count` | `redaction-count` (integer, `COALESCE(..., 0)`) | present; 0 in most captured rows, one fixture row (`88547451-...`) deliberately picked with `redaction-count: 1` to prove nonzero values occur (57 of 1450 live rows have `redaction-count > 0`, up to 6 observed) |

  **No Selection signal failed to map to a real contract field. No blocker.**

- Fixture layout: `config/eval/distill-fixtures/session/` (new subdirectory,
  following the existing `distill-fixtures/<kind>/` convention but holding
  raw contract payloads instead of `source.md`/`distilled.yml` pairs, since
  no distillation exists yet at Phase 0):
  - `bulk-envelope.json` - a curated 8-session slice of a real bulk
    `clyde session export` call, hand-picked to exercise every live
    `enrich-status` value, both `dormant` states, `repo` present (three
    different orgs) and present-null, and a nonzero `redaction-count`.
  - `with-body-envelope.json` - one real `--id --with-body` payload: a
    benign work session (marquee PR #23 CodeRabbit-comment housekeeping).
  - `README.md` - provenance and field-coverage notes for the fixture pair,
    matching the existing `distill-fixtures/README.md` style.

### Deviations

- The design doc's Phase 0 bullet says "captured envelope" (singular); this
  spike checked in a curated 8-session slice rather than either a single
  session or the full 1450-session catalog dump. A single-session envelope
  would not exercise the `enrich-status` value variety or the `repo`
  present/present-null contrast the mapping table above needed; the full
  catalog dump would check ~1450 sessions' worth of personal titles/prompts
  into the repo, which is disproportionate to a contract-shape spike and
  unnecessarily widens what's committed. The curated slice is still a real,
  unmodified capture (every field verbatim from the live export) - same
  intent, correct scope.
- `config/eval/distill-fixtures/session/` holds raw contract JSON, not the
  `source.md`/`distilled.yml` pair every other `distill-fixtures/<kind>/`
  subdirectory uses - there is no distillation yet at Phase 0. Phase 7's
  golden session fixtures (source.md/distilled.yml pairs derived from these
  raw captures) are a separate, later artifact per the design doc's own
  phrasing ("reuse clyde's Phase 0 fixtures where they fit").

### Tradeoffs

- Picked one benign, low-message-count, no-redaction work session
  (marquee PR #23 review) for the `--with-body` fixture over a longer/more
  substantive session, to minimize the amount of real transcript content
  checked into a shared repo while still exercising the full `body` shape
  (`role`/`text`/`subagent`, multi-turn, includes an error-message turn to
  show non-narrative content survives the parse).
- Did not attempt to synthesize a live `skipped-empty` example (e.g. by
  forcing clyde to enrich an empty session) - the design doc's own contract
  reference (clyde's `session-export-contract.md`) already pins that value
  as frozen and cites its write sites; reproducing it live would have
  required mutating clyde's catalog for a fixture that a documentation
  citation already settles.

### Open questions

None.

## Phase 1: Schema seams

### Design decisions

- New enum arms landed across every enumerated site; each carries an `as_str`
  arm, an `all()` arm, a `FromStr` arm, and a round-trip test:
  - `NoteType::Session` and `Method::Harvest` - `vault/src/schema.rs`
    (`:135`/`:389`) + `vault/src/schema/tests.rs`.
  - `IngestKind::Session` and `ContentKind::Session { .. }` -
    `borg/src/types.rs` (`:62`/`:40`) - both live in borg, not vault, as the
    doc's round-2 correction noted.
  - `GateId::Selection` - `borg/src/types.rs:119` (the selection gate's id),
    wired through `borg/src/stages/alert.rs:73` so a selection rejection
    formats a real alert.
- Match sites wired to compile: `vault/src/trace.rs`, `borg/src/markdown.rs`
  (frontmatter renderer), `vault/src/distilled.rs`, `distillers/src/render.rs`,
  `distillers/src/dispatcher.rs`, `borg/src/triage.rs`, `borg/src/pipeline.rs`,
  `borg/src/stages/{raw,distill}.rs`, `borg/src/signal.rs`,
  `sb/src/cli/checks.rs`. The compiler drove the list; the earlier
  rust-analyzer E0308 diagnostics were a mid-edit snapshot and are fully
  resolved (`cargo check --workspace` clean).
- Receipts migration (`borg/src/receipts.rs`, `borg/src/receipts/schema.sql`):
  `SCHEMA_VERSION` bumped to `3`; both CHECK constraints widened -
  `kind IN ('url','text','binary','session')` and
  `status IN ('received','succeeded','failed','rejected')`. SQLite cannot
  `ALTER` a CHECK, so the migration rebuilds the live `receipts` table only
  when the existing SQL lacks `'rejected'` (idempotent guard at
  `receipts.rs:215`), then records the version row.
- New receipts vocabulary in `vault/src/receipts.rs`: `ReceiptKind::Session`
  (`:111`, an honest kind - NOT reusing `text`, per the doc's "lying
  identifier" note) and `ReceiptStatus::Rejected` (`:155`, distinct from
  `Failed`: a below-bar candidate is a clean decline, not a broken ingest).
- `borg::receipts::mark_rejected` (`:431`) promotes a `received` row to
  `rejected` with a reason; `count_by_status` (`:665`) now returns the
  `rejected` bucket so `sb doctor`/`GET /health/audit` aggregate it without
  hard-erroring once the first rejected row lands (CLI surface itself is
  Phase 6).

### Deviations

- None from the doc's spec. The enumerated site list in the doc used stale
  line numbers (round-2 acknowledged this); I grepped each enum rather than
  trusting the exact lines, which is what the doc's own correction instructs.

### Tradeoffs

- Table-rebuild migration vs a fresh `CREATE TABLE` on version mismatch:
  chose the in-place rebuild guarded by a substring check on the live schema
  so existing daemon-host receipts rows survive the widening (the receipts DB
  is authoritative for ingest state and must not be dropped). The guard makes
  it a genuine no-op on an already-migrated DB.

### Open questions

None.

### Orchestration note

Phase 1 was implemented by the `phase1` delegated agent, which completed all
edits but stalled before running `otto ci`/committing (no build process, no
commit). The orchestrator took over inline: verified every required arm and
the migration by grep, ran `cargo check` (clean), `cargo test --workspace`
(all green), and `otto ci` (`✅ All CI checks passed!`), then authored these
notes and committed. No code was re-written - the stalled agent's edits were
verified and adopted as-is.

## Phase 2: Config

### Design decisions

- New `HarvestConfig` + `HarvestMode` in a dedicated submodule
  (`borg/src/config/harvest.rs`), mirroring the existing per-section submodule
  pattern (`config/{desktop,discord,ntfy,signal,telegram}.rs`) rather than
  inlining the struct into `config.rs`. Re-exported via `pub use
  harvest::{HarvestConfig, HarvestMode};` and wired as a new
  `#[serde(default)] pub harvest: HarvestConfig` field on `Config`
  (`borg/src/config.rs::Config`), following the exact idiom already used for
  `distill`/`daemon` (a non-`Option` section with sensible defaults, not an
  opt-in transport like `telegram`/`signal`).
- `HarvestMode` (`dry-run | live`) mirrors `StagingLayout`'s enum shape
  (`Copy, Default, PartialEq, Eq`, `#[serde(rename_all = "kebab-case")]`,
  `#[default]` on the safe variant). Default is `DryRun`: the design doc's
  Rollout Plan states the first week runs dry-run via the timer before
  flipping to live, so the config default must match that, not `Live`
  (fail-closed default per `rules/taste.md` Security instincts).
- Every field relies on the container-level `#[serde(default)]` +
  hand-written `impl Default for HarvestConfig` to fill missing keys - the
  same mechanism `DistillConfig`'s comment documents in detail (container
  `#[serde(default)]` on a struct fills each MISSING field from that struct's
  own `Default::default()`, not from the field type's own `Default`). This
  means no field needs a separate `#[serde(default = "fn")]`; one
  `impl Default` block is the single source of truth for every default,
  including the non-zero/non-empty ones (`initial_since: "7d"`,
  `thread_window: "2h"`, `min_msgs: 4`, `token_cap: 12_000`,
  `clyde_binary: ~/.cargo/bin/clyde` expanded).
- `clyde_binary: PathBuf` carries
  `#[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]`
  per the CLAUDE.md path-handling invariant. Its default is built by calling
  `vault::paths::expand_tilde("~/.cargo/bin/clyde")` directly inside
  `impl Default` (not through serde, since `Default::default()` never runs
  the deserializer) - the same pattern `StagingConfig::default` uses for
  `vault::paths::borg_stages_dir()`.
- `exclude_patterns: Vec<String>` is a plain YAML list (never comma-split),
  per `rules/cli.md`; default is an empty vec (no built-in exclusions
  shipped, unlike `canonicalization`'s built-in rules - the design doc gives
  illustrative examples of what an operator might exclude, not a fixed
  built-in list, so defaulting to none is the honest "config drives behavior"
  reading).
- `initial-since`/`thread-window` are kept as raw `String` spans (`"7d"`,
  `"2h"`) rather than a typed duration, matching the existing `--since`
  convention: `borg::receipts::parse_since` already parses this exact shape
  (relative duration / RFC-3339 / bare date) for the CLI `--since` flag.
  Phase 2 is schema + defaults only; Phase 3's export reader and thread
  clustering own actually parsing/validating these spans.
- Mirrored the full section into `config/templates/borg.yml.example` with one
  annotated comment per key, appended just before the `log-level` tail,
  matching the existing section style (`distill`/`youtube` blocks).

### Deviations

- None from the doc's spec. The doc names the keys generically ("selection
  thresholds", "exclusion patterns", "thread window", "token cap") without
  pinning exact field names/defaults; I chose concrete kebab-case names
  (`min-msgs`, `exclude-patterns`, `thread-window`, `token-cap`) and starter
  default values (`min-msgs: 4`, `token-cap: 12000`) since none were
  specified numerically in the doc. These are documented in the example
  template as tunable and are not load-bearing for Phase 2's success
  criteria (config parses with/without the section; every key documented);
  Phase 3 is free to retune the defaults against the golden fixture without
  touching the schema.

### Tradeoffs

- Non-`Option<HarvestConfig>` (always-present section with defaults) over
  `Option<HarvestConfig>` (opt-in, like `telegram`/`signal`/`ntfy`): harvest
  is a batch job that ships with sane defaults and should work out of the
  box once `sb bootstrap` drops the template, not a transport that must be
  explicitly wired to a credential/host before it does anything - matches
  `distill`/`pipeline`'s precedent, not the bot-transport precedent.
- Kept `initial-since`/`thread-window` as un-parsed strings in this phase
  rather than introducing a typed `Duration`/span wrapper now: no consumer
  exists yet (Phase 3 is the first reader), and `borg::receipts::parse_since`
  already proves the exact parsing shape needed, so adding a wrapper type
  ahead of its first use would be speculative machinery this phase doesn't
  need.

### Open questions

- The concrete default values for `min-msgs` (4) and `token-cap` (12000) are
  starter values, not derived from the design doc or a golden fixture (none
  existed to derive them from at Phase 2). Phase 3's golden-fixture work
  (the checked-in 2026-07-02 catalog slice) is the natural place to tune
  these against real selection behavior; flagging so Phase 3 doesn't assume
  they are load-bearing constants.

## Phase 3: Export reader + selection gate + watermark

### Design decisions

- New `borg::harvest` module, side-by-side per the house source-addition
  pattern (nearest precedent: `signal.rs`), split into five submodules so no
  file approaches the size limit and each seam is independently testable:
  - `harvest/contract.rs` - the clyde `session export` schema-version-1 types +
    `parse_export` (the ONE loud boundary: schema-version mismatch and
    unparseable JSON both `bail!`, never an empty result).
  - `harvest/reader.rs` - `ExportReader` trait (port) + `ClydeExportReader`
    (shells out to the configured binary, mirroring `youtube.rs` subprocess
    hygiene: `kill_on_drop(true)` + wall-clock timeout + `wait_with_output`
    concurrent drain). Never reads `sessions.db`/`.jsonl`.
  - `harvest/select.rs` - `evaluate_selection` (the real gate; Gate-0 is a
    no-op for sessions), signals dormant+enrich-ok+real-repo+min-msgs+exclude.
  - `harvest/cluster.rs` - deterministic `(cwd, git-branch) + gap` clustering.
  - `harvest/watermark.rs` - state file, exclusive lock, body hashing,
    re-appearance classification.
  - `harvest.rs` - orchestration (`plan_harvest`, `write_rejections`,
    `apply_plan_to_state`, `record_published`).
- `plan_harvest` is disk-side-effect-free: it computes a `HarvestPlan`
  (thread decisions + rejections + new cursor). Writing reject artifacts
  (`write_rejections`) and advancing state (`apply_plan_to_state`) are separate
  explicit steps - `harvest::plan_harvest` returns data, the thin steps do I/O
  (return-data-not-side-effects). Body fetch happens ONLY on the deep-check
  path (published id whose `n-msgs` changed), via the injected reader.
- Watermark state at `vault::paths::borg_harvest_state()`
  (`~/.local/share/sb/borg/harvest-state.json`), a new path fn beside the
  existing borg data-dir fns. Exclusive advisory lock via `fs2` on a dedicated
  sibling `.lock` file (survives the atomic temp+rename of the JSON), mirroring
  cortex's `embed.lock`; contention is a typed `HarvestLockHeld` (not a message
  substring), so the timer-vs-hand-run collision fails loudly.
- Durable identity anchors on the INPUT body hash (SHA-256 of the canonical
  role-labeled thread body), never a distillation output. `canonical_body_text`
  / `thread_body_text` are defined here as the single source of truth Phase 4/5
  reuse, so the hash a re-appearance compares equals the bytes the note was
  built from. Sub-agent turns and member boundaries are encoded so a resume
  that only re-runs a sub-agent, or a different member split, still changes the
  hash.
- Three-state `repos-touched` modeled as `Option<Vec<String>>` with
  `#[serde(default)]` (None=omitted/unknowable, Some(vec![])=parsed-no-repo,
  Some(xs)=set) and proven distinct by test. `repo`/`git-branch` are
  present-null `Option<String>`. `enrich-status` is a `#[serde]` enum over the
  frozen vocabulary; `null`/omitted -> `None`. The record type is deliberately
  NOT `deny_unknown_fields` (forward-compatible-envelope carve-out): the
  contract gains fields additively within schema-version 1 (clyde's
  files-touched branch), so the version assertion is the real gate.
- Reject path (`GateId::Selection`): each rejected candidate gets a trace at
  SELECTION TIME (`trace::generate(IngestMethod::Harvest)`, before any body
  fetch), a `received`->`rejected` receipts row (`ReceiptKind::Session`,
  `ReceiptStatus::Rejected` via the Phase 1 `record_received`/`mark_rejected`),
  and a `rejection.yml` written through the existing `FsArtifactStore`.

### Golden-fixture provenance + the "4 sessions" correction

- The 07-02 golden fixture (`config/eval/harvest/golden-2026-07-02.json`) was
  captured from the LIVE clyde catalog via `clyde session export --id <id>` on
  `desk` (2026-07-19, `clyde v0.10.1`). Every SELECTION-RELEVANT field is real
  and verbatim (session-id, cwd, repo, git-branch, created, modified, n-msgs,
  dormant, enrich-status, scope); the free-text title/first-prompt/summary are
  REDACTED to benign placeholders (the doc's "pick benign sessions or redact" -
  the real work prompts carry internals and are not load-bearing for
  selection).
- The doc's original premise "07-02 token-broker arc: 4 sessions = 1 note" does
  NOT survive contact with the real data. `9521f589` (29 msgs) and `4e55a52c`
  (389) have cwd `/home/saidler` and `enrich-status: skipped-personal` - both
  correctly REJECTED (personal enrichment + non-repo cwd, two counts).
  `871f6428` (486) and `4ae69e3a` (320) are cwd `tatari-tv/slack-cli/main`,
  branch main, `ok`, ~15s apart - both SELECTED, clustering to ONE note.
  Deterministic truth = **2 selected -> 1 note, 2 rejected**. Surfaced to the
  parent immediately; the parent accepted the deterministic outcome and
  corrected the design doc's Acceptance Criterion #1 + added a 2026-07-20
  Resolved Decision. The hand-note's "4" was a human cross-cwd SUBJECT grouping
  that crossed the selection bar - exactly the repo-hub layer's job
  (Phases 9-13), never the selection gate's.
- Edge fixtures (`config/eval/harvest/`): `same-cwd-unrelated.json` (two
  sessions ~4h apart in one repo -> 2 notes, proving the window),
  `reject-cases.json` (one session per rejection reason), `single-repo-session.json`
  (real marquee PR#23 structural fields, benign prose - the re-appearance
  base). The constructed fixtures are labeled as such in the README.

### Retuned config defaults

- `min-msgs` 4 -> **6**. The real fixtures show a clean gap: one-shots cluster
  at <=3 messages (the canonical `"what"` reject is 3), every substantive
  engineering thread is >=29. 6 sits inside that gap with margin against 4-5
  message near-one-shots. (4 also separated the fixtures; 6 adds headroom.)
- `thread-window` stays **2h**: validated by the golden (the two survivors are
  15s apart -> merge) and `same-cwd-unrelated` (4h apart -> split). Documented
  as the tunable knob if real noise later disproves it.
- `token-cap` stays **12000**: NOT tunable from selection fixtures - it governs
  the distiller's head+tail windowing (Phase 4), which has no signal at Phase 3.
  Deferred to Phase 4 honestly rather than tuned against data that can't
  inform it.

### Deviations

- **Divergence from a WRITTEN acceptance criterion (#1), fully traceable.** The
  doc originally required the golden fixture to reproduce the hand-written
  summary's "4 sessions = 1 note" for the 07-02 token-broker arc. The golden
  fixture instead asserts the DETERMINISTIC outcome **2 selected -> 1 note, 2
  rejected**, because the real live-catalog metadata makes "4 -> 1 note"
  impossible under the doc's own Selection rules. Exact evidence (captured via
  `clyde session export --id <id>`, `desk`, 2026-07-19):

  | session id | n-msgs | cwd | git-branch | enrich-status | scope | outcome |
  |---|---|---|---|---|---|---|
  | `9521f589-1243-4264-8302-ce28d9e524ff` | 29 | `/home/saidler` | HEAD | `skipped-personal` | personal | REJECTED |
  | `4e55a52c-f0be-40eb-88a7-3184c7640738` | 389 | `/home/saidler` | HEAD | `skipped-personal` | personal | REJECTED |
  | `871f6428-92d8-4035-a66c-87f6d1edee83` | 486 | `.../tatari-tv/slack-cli/main` | main | `ok` | work | selected |
  | `4ae69e3a-6bde-47d3-946d-c9757f810610` | 320 | `.../tatari-tv/slack-cli/main` | main | `ok` | work | selected |

  Two-count rejection of the personal pair: (1) `enrich-status: skipped-personal`
  fails the `enrich-status == ok` signal, and (2) cwd `/home/saidler` is not a
  `~/repos/<org>/<repo>` anchor (`repo: null`) so it fails the real-repo signal.
  The two work survivors share `(cwd, git-branch)` and are ~15s apart
  (`871f6428.modified` 06:08:39 -> `4ae69e3a.created` 06:08:54) so they cluster
  to one note (primary `871f6428`, 486 > 320 msgs). Preserving the literal "4"
  would require harvesting personal/non-repo sessions - loosening the selection
  gate in direct contradiction of the Selection section and the reaffirmed
  "harvest must SELECT" principle. The hand-note's "4" was a human cross-cwd
  SUBJECT grouping that crossed the selection bar - the repo-hub layer's job
  (Phases 9-13), never the selection gate's. Surfaced to the parent before
  building; approved. **Acceptance criterion #1 corrected 2026-07-20 (see the
  design doc's Resolved Decisions); this fixture asserts that corrected ground
  truth.**
- `evaluate_selection` returns `Result<(), Box<RejectionRecord>>`, not the
  doc's literal `Result<(), RejectionRecord>` - clippy's `result_large_err`
  (`-D warnings`) denies the 176-byte unboxed Err. Same effect, correct seam
  (clippy's own suggested fix); the raw.rs gates sidestep this by returning
  `Result<()>` + writing the record as a side effect, but returning the record
  keeps the trace generated at the orchestrator (selection time) as the doc
  requires.
- "Trace per candidate" is realized as trace-per-reject (each rejected session)
  + trace-per-thread (the note's trace = the primary member's selection-time
  trace). A selected non-primary member session does not carry an independent
  staging trace in Phase 3 because it has no independent note; Phase 5 owns the
  received-row/sidecar for the note's trace. This matches "one note = one
  trace" (the rest of borg staging) while still giving every REJECTED candidate
  its own trace + receipts key, which is the concrete Phase 3 requirement.
- Selection rejects write `rejection.yml` + the `rejected` receipts row, but no
  raw-input sidecar. The sidecar is the accepted-input durability record
  (Phase 5's door); for a reject the `rejection.yml` IS the forensic artifact.
- Added `#[derive(PartialEq)]` to `types::RejectionRecord` (was Debug/Clone/
  serde only) so `RejectionOutcome`/`HarvestPlan` can derive it for test
  assertions. Harmless (all fields already PartialEq).

### Tradeoffs

- Async `ExportReader` trait (the only async surface) vs a fully-sync reader:
  chose async to inherit borg's subprocess-hygiene pattern (timeout +
  kill_on_drop via tokio) exactly. Selection/clustering/watermark stay pure and
  sync-testable; only the reader and `plan_harvest` are async, and tests inject
  a `FakeReader` so no test needs the clyde binary.
- Body hash keyed under the thread's PRIMARY session id (with a hash over ALL
  members' bodies) rather than a per-member scheme: threads never span runs and
  Day-2 same-cwd is a new note, so the single-session re-appearance is the
  common case and this generalizes cleanly without a composite key.
- Reused the existing `FsArtifactStore` for `rejection.yml` rather than a
  harvest-specific writer: the per-trace layout + atomic write are already
  correct, and `read_rejection` gives the test a clean round-trip.

### Success criteria (Phase 3) - all verified by passing tests

- golden fixture selects EXACT ids / cluster / note count: PASS
  (`golden_fixture_selects_expected_ids_and_one_note` - selected
  `{871f6428, 4ae69e3a}`, 1 thread, 1 note, primary 871f6428, 2 rejects) -
  asserting the corrected deterministic outcome (2->1), not the retracted "4".
- same-cwd-unrelated does NOT merge: PASS (`same_cwd_unrelated_does_not_merge`
  -> 2 threads).
- rerun with unchanged catalog is a no-op: PASS
  (`rerun_with_unchanged_catalog_is_a_no_op` - cheap-filter Skip, zero body
  fetches, nothing publishable).
- resumed-session (body hash changed) -> follow-up: PASS
  (`resumed_session_body_hash_changed_is_follow_up`).
- unchanged-body skips WITHOUT re-distilling: PASS
  (`unchanged_body_skips_and_advances_without_redistilling` - Skip + snapshot
  advance, then cheap-filter with no re-fetch).
- rejects leave `rejection.yml` + a `rejected` receipts row keyed by a
  selection-time trace: PASS (`write_rejections_leaves_yaml_and_a_rejected_receipts_row`
  - kind `session`, gate `selection`, trace-keyed).

### Open questions

- None blocking Phase 3. For the parent to note: Phase 5 must record the
  `NewNote`/`FollowUp` published snapshot AFTER publish (it needs the landed
  note path) via `harvest::record_published`; `apply_plan_to_state` intentionally
  only advances the cursor + `Skip` in-place snapshot updates. The Phase-2 open
  question on `min-msgs`/`token-cap` starter values is resolved above.

## Phase 4: Session distiller

### Design decisions

- `SessionDistiller` in `distillers/src/session.rs` (+ `session/tests.rs`),
  following the `ThreadDistiller` add-a-kind pattern. `DistillKind::Session`
  wired into the dispatcher (`distillers/src/dispatcher.rs:41` enum arm,
  `:55` as_str, `:74`/`:127` the `session:` field on the dispatcher struct,
  `:113` `SessionConfig` construction) - dispatcher registration is the
  commonly-skipped seam and is done + covered by dispatcher tests.
- `KindPayload::Session(SessionPayload)` (`vault/src/distilled.rs:252`/`:306`):
  `SessionPayload` carries repo, session ids, msg counts, date range.
- `render()` extended for the session kind and `distill_for_publish_session`
  added (`borg/src/stages/distill.rs:876`), mirroring `distill_for_publish_thread`.
- Fabric patterns in `borg/patterns/`: `distill-session.md` (+ `-chunk` /
  `-reduce` map-reduce variants). Prompt contract per the Distillation
  section: decisions / rejected approaches+why / gotchas / reusable patterns;
  explicit no-narration, no-activity-ledger instruction (the conductor
  anti-pattern). Deployed via the existing pattern path; added to the
  `PATTERNS` array.
- Truncation is never silent: `TRUNCATION_MARKER = "[TRANSCRIPT TRUNCATED]"`
  (`session.rs:59`). Two sources feed it - (a) the export's `body-truncated`
  flag threaded via `DistillInputs` (`lib.rs:82`), and (b) local head+tail
  windowing when the body exceeds the token budget (the excised middle is
  replaced by the marker). Both routes put the marker in the assembled prompt.
- Model/limits inherit from the article config in the dispatcher
  (`dispatcher.rs:110-117`), so `HarvestConfig.model` -> `llm.model` resolution
  rides the established per-feature override chain rather than a bespoke path.

### Deviations

- Session distiller inherits its model/max_chars/timeout from the article
  distiller's config in the dispatcher rather than introducing a standalone
  config-resolution path. Same effective source (`llm.model` unless overridden),
  fewer moving parts; documented here as the deliberate choice.
- Gate-2 (paraphrase check) is NOT re-implemented in the session distiller: it
  is a downstream pipeline gate applied uniformly to every `Distilled` at
  publish time. Session distillation flows through that same gate with no
  kind-specific bypass - which is exactly "Gate-2 applies". No session-only
  Gate-2 code was added or needed.
- Adding the `Session` arm to `DistillKind` forced match-arm updates across
  every sibling distiller's tests (article/idea/image/passthrough/repo/thread/
  video/voicenote) - mechanical exhaustiveness wiring, no behavior change.

### Tradeoffs

- Token cap 12000 CONFIRMED reasonable, not changed: the 806-msg golden thread
  windows to a single fabric call under the default cap
  (`large_thread_windows_to_single_call_under_default_cap`), and a raised cap
  correctly routes to map-reduce (`raised_token_cap_routes_to_map_reduce`).
  So 12000 keeps the common case single-pass while the chunk/reduce path is
  proven to engage when a thread genuinely overflows. This resolves the
  Phase-3 deferral of token-cap tuning.
- Head+tail windowing over naive head-truncation: preserves both the framing
  (early decisions) and the resolution (late gotchas) of a long session, which
  is where the reusable knowledge concentrates.

### Open questions

None blocking. Phase 5 handoff (unchanged from Phase 3's note): call
`harvest::record_published` AFTER publish; wire `distill_for_publish_session`
into the pipeline handler.

### Orchestration note

Phase 4 was implemented by the `phase4` delegated agent, which completed all
edits (session.rs, three patterns, dispatcher/render wiring, SessionPayload,
distill_for_publish_session, 18 session tests, sibling-test match arms) but
went quiet before running `otto ci`/committing - the same pre-commit stall as
Phase 1. The orchestrator took over inline: verified every required arm/marker
by grep, ran `cargo test --workspace` (0 failures) and `otto ci`
(`✅ All CI checks passed!`), authored these notes, and committed. No code was
re-written; the stalled agent's edits were verified and adopted as-is.

## Phase 5: Pipeline handler + publish

### Design decisions

- **Two new modules split along the existing seam** (harvest side vs pipeline
  side, per `borg/AGENTS.md`'s "add an ingest source" pattern):
  - `borg/src/harvest/publish.rs` (`publish_plan`/`publish_thread`/
    `publish_thread_inner`) - for every publishable `ThreadDecision`: fetch
    every member's `--with-body` transcript (Phase 3 only fetched a body for
    the deep re-appearance check, so a `NewNote`/cheap-filter `Skip` has none
    yet and a `FollowUp`'s fetch result was never propagated onto
    `ThreadDecision` - this always re-fetches, deterministically, from the
    same reader/ids Phase 3 validated), concatenate via the Phase 3
    `watermark::thread_body_text`/`body_hash` (the SAME identity-anchor
    functions, so the hash stored on publish is byte-identical to what a
    future re-appearance check would recompute), door-capture via
    `intake::record_received_with_sidecar` (`IntakeKind::Session` - new
    variant, see below), dispatch through `pipeline::process_content`, and on
    `Completed` call `harvest::record_published` with the landed note path +
    `thread.total_msgs` + the body hash. A single thread's failure converts to
    a `Failed` outcome + `intake::record_failure_at_door` rather than aborting
    the run (mirrors `write_rejections`'s per-item best-effort policy) - one
    bad session fetch must not silently drop the rest of the night's harvest.
  - `borg/src/pipeline/session.rs` (`process_session`/`process_session_inner`)
    - the `ContentKind::Session` handler wired into `process_content`'s match
    (mirrors `process_text`'s timing/error-wrapper shape). Builds
    `SessionMetadata` from the carried `members` (repo from primary, session
    ids primary-first, summed `msg_count`, min `created`/max `modified` via
    parsed-timestamp comparison - not string comparison, since RFC3339 strings
    with differing UTC offsets don't sort lexically), resolves
    `harvest.model`/`llm.model`/`harvest.token-cap` into a `SessionConfig`,
    calls `distill_for_publish_session`, renders via
    `distillers::render(..., RenderOptions { include_transcript: false })`
    (Article/Repo/Video's transcript-free policy - the design doc's embedding
    policy: only the distilled note is embedded, the staged transcript stays
    trace-recallable), appends a richer per-member `## Session Details`
    footer (id/title/repo/duration) for thread notes (`members.len() > 1`,
    reusing `pipeline::append_distilled_below_slides` for the splice - the
    same generic "append a section below a body" helper the slide path
    already uses), writes full frontmatter (`type: session`, `method:
    harvest`, `origin: generated`, `status: unread`, `source:
    clyde://<primary-id>`, `repo:` verbatim/present-null, `trace:`,
    `ingested:`, `trace-expires:`, tags), and publishes via the shared
    `atomic::resolve_publish_path` + `vault::note::write_atomic` +
    `publish_note` path every non-URL kind already uses (no hand-rolled
    `fs::write`).
- **`ContentKind::Session` extended from `{ body }` to `{ body, members,
  primary_id, body_truncated }`** (`borg/src/types.rs`) - the Phase 1 seam
  carried only the concatenated transcript, but the pipeline handler needs
  repo/scope/title/duration/redaction-count/dates to build `SessionMetadata`
  and the note's frontmatter+footer, none of which round-trips through a bare
  `String`. `members` are the bulk-metadata `SessionRecord`s (no `body` field
  populated - the concatenated transcript already carries that as `body`, so
  nothing is duplicated); `primary_id` and `body_truncated` (true if ANY
  member's `--with-body` fetch flagged clyde-side truncation) ride alongside.
  Every match site (`stages/raw.rs::write_capture`, `signal.rs`, existing
  tests) updated to the new shape.
- **`ThreadDecision` gained a `members: Vec<SessionRecord>` field**
  (`borg/src/harvest.rs`) - Phase 3's `plan_harvest` already clusters full
  `SessionRecord`s (via `cluster::Thread`) but only projected `member_ids`/
  `total_msgs` onto `ThreadDecision`; Phase 5 needs the full records (repo,
  scope, redaction-count, title, duration) without re-deriving them, so this
  is additive cross-phase wiring on an already-committed Phase 3 type (the
  "most-skipped" class the taste rules call out) rather than a re-fetch or a
  parallel lookup table.
- **`dispatcher_for_session` + `distill_for_publish_session` signature
  change** (`borg/src/stages/distill.rs`) - Phase 4's `dispatcher.rs` doc
  comment explicitly invited this ("borg's harvest handler (Phase 5) rebuilds
  it via with_configs when harvest.token-cap differs"): `dispatcher_for_session`
  builds every other kind's config identically to `dispatcher_from_fabric_config`
  and only substitutes a caller-supplied `SessionConfig` (carrying
  `harvest.model`-or-`llm.model` + `harvest.token-cap`) for the session
  distiller, via `Dispatcher::with_configs`. `distill_for_publish_session`
  gained a `session_config: SessionConfig` parameter and now builds its stage
  via `DistillStage::with_dispatcher(dispatcher_for_session(...))` instead of
  the article-default `DistillStage::from_fabric_config(...)`.
- **`IntakeKind::Session` new variant** (`vault/src/intake.rs` +
  `borg/src/intake.rs`'s `receipt_kind` mapping) - the harvest door needed to
  call the SAME `intake::record_received_with_sidecar` every other transport
  uses (per `borg/AGENTS.md`'s add-a-source pattern), but the existing
  `IntakeKind` vocabulary only maps to `ReceiptKind::{Url,Text,Binary}`, and
  `Text` would be the exact "lying identifier" the design doc calls out for
  sessions. Added `IntakeKind::Session -> ReceiptKind::Session` so the door
  capture is both reused AND honest.
- **`NoteContent` gained `origin: Option<Origin>` / `status: Option<Status>`**
  (`borg/src/markdown.rs`) - every existing kind hardcoded `origin: assisted`
  and never wrote `status:` at all; harvest notes need `origin: generated` /
  `status: unread` per the design doc's Data Model. `None` (every existing
  literal's default) renders byte-identically to the pre-Phase-5 behavior
  (`origin: assisted`, no `status:` line); only the Session handler sets
  `Some(...)`.
- **`repo:` frontmatter emitted now, validated in Phase 9** - per the design
  doc's explicit Phase 9 note ("write it verbatim; the full validation/hub
  wiring is Phase 9, but the renderer should emit the field now"), the session
  handler writes `repo:` straight from `SessionMetadata.repo` (itself the
  primary member's `SessionRecord.repo`, present-null preserved via
  `serde_yaml::Value::Null`) into `frontmatter_additions` with NO shape
  validation. `repos-touched:` is not emitted - Phase 9's addition once
  clyde ships files-touched.
- **Scope/redaction tags bypass canonical filtering** - `scope-work`/
  `scope-personal` (from the primary member's `scope` field) and
  `redacted-source` (any member's `redaction-count > 0`) are pushed onto
  `all_tags` AFTER `finalize_tags` runs, not before. None of the three is in
  the 110-tag canonical interest vocabulary `canonical::filter_and_cap`
  filters against (confirmed: `scope-work`/`scope-personal`/`redacted-source`
  absent from `config/canonical-tags.yml`, and no segment of them matches
  either), so pre-filter placement would silently drop them - the same fate
  the pre-existing `code-snippet`/`*-vocab` tags already have (pushed before
  `finalize_tags` in `pipeline/text.rs`, not touched here). Since the design
  doc states these tags as a REQUIRED frontmatter guarantee on every harvest
  note (not an optional interest tag), they ride after the filter instead of
  risking silent loss.
- **`force` semantics kept intentionally narrow** - the harvest publish loop
  always calls `pipeline::process_content(..., force=false, ...)` regardless
  of the CLI's `--force`. Design-level `--force` ("re-distill this in-scope
  published id") is already expressed upstream as Phase 3's `FollowUp`
  decision, which lands as a brand-new note (notes are immutable once
  published - Resolved Decisions). The pipeline's own `force` parameter means
  "overwrite a same-filename collision in place" - a distinct, narrower
  concept the harvest loop never wants; documented inline at the call site so
  Phase 6 doesn't wire `--force` through this parameter by mistake.

### Deviations

- **`ContentKind::Session`'s shape is NOT what Phase 1 shipped** (`{ body }`
  only). Extending it to `{ body, members, primary_id, body_truncated }` was
  necessary for the handler to reach repo/scope/title/duration/redaction
  without a second lookup; same effect (one `ContentKind` variant carrying
  everything the Session handler needs), correct seam - the exact "spec-gap"
  pattern the phase-implementer brief calls out (exact signatures in design
  docs are chronically wrong at the field level; Phase 1's own doc comment on
  the field literally said "Phase 5's pipeline handler dispatches on it"
  without committing to a shape).
- **The design doc's `## Sessions` footer (Phase 4, `SessionPayload`-driven)
  is supplemented, not replaced, by a richer `## Session Details` block for
  thread notes** - `distillers::render`'s `push_session_footer` doc comment
  explicitly defers the id/title/repo/duration footer to "borg's publish
  layer, which holds the full clustered `SessionRecord`s" (Phase 5). Rather
  than modifying the already-shipped, tested `render()` function, Phase 5
  appends a second section below it (`render_member_details` +
  `append_distilled_below_slides`) only when the thread has more than one
  member - a single-session note gets no extra section, matching "the design
  collapses to trivial at N=1".
- **No fabric/network dependency in Phase 5's tests** - `distill_for_publish_session`
  (like every other `distill_for_publish_*`) is not generic over `FabricCaller`;
  it always builds a real `FabricShell`. Every Phase 5 test points
  `config.fabric.binary` at a guaranteed-absent binary name, so the subprocess
  spawn fails and the existing `fallback_distilled` path produces a degraded
  (but valid) `Distilled` - consistent with how every other kind's
  `distill_for_publish_*` handler already behaves under a missing fabric
  binary; no new test double was needed or invented.
- **`config.tags.canonical_path`/`mapping_path` pointed at guaranteed-absent
  paths in every Phase 5 test, not the real default `~/.config/sb/...` or a
  fixture file.** `pipeline::tags::get_or_init_canonical` caches the loaded
  canonical vocabulary in a process-wide slot on first SUCCESS, keyed on
  nothing (not per-config) - discovered live: an earlier draft of these tests
  used `Config::default()`'s real default path, which exists on this dev
  machine, and non-deterministically poisoned the shared cache with the real
  ~110-tag catalogue for every OTHER test in the same binary run depending on
  execution order, breaking `pipeline::tags::tests::distiller_proposed_tags_survive_canonical_filter`
  (which expects its own tempdir fixture to win the race). Pointing at
  absent paths makes `CanonicalTagsFile::load` fail loudly and return `None`
  WITHOUT caching (confirmed by reading `get_or_init_canonical`), so
  `finalize_tags` no-ops for these calls and the shared cache is never
  touched either way - deterministic regardless of test execution order. Not
  a fix to the underlying cache design (out of Phase 5's scope); flagged as
  an open question below.

### Tradeoffs

- **Fetch bodies twice (Phase 3's deep-check path + Phase 5's publish path)
  rather than threading a fetched body through `ThreadDecision`** - Phase 3
  only fetches on the `FollowUp`-candidate path, and even then the result
  isn't retained on the `Reappearance`/`ThreadDecision` value (by design:
  `plan_harvest` is meant to be side-effect/fetch-cheap for the common `Skip`
  case). Re-fetching in Phase 5 for every publishable thread is simpler and
  keeps `plan_harvest`'s "pure w.r.t. disk except the identity-check fetch"
  contract from Phase 3 unchanged, at the cost of one extra `clyde session
  export --id --with-body` call per already-fetched `FollowUp` member. At
  harvest's scale (few threads/night) this is not a real cost.
- **Session's transcript-free render policy (`include_transcript: false`)
  chosen explicitly rather than following the doc's silence** - the design
  doc's Embedding policy line ("only the distilled note is embedded... the
  staged transcript is trace-recallable... never embedded") maps directly to
  the SAME policy Article/Repo/Video already use (`RenderOptions::
  for_url_publish` degenerates to `false` for a `None`/non-Thread payload);
  Session is not a verbatim-preservation kind like Thread/Idea/Vocabulary, so
  it takes the transcript-free branch rather than introducing a new policy
  value.
- **`earliest_created`/`latest_modified` log-and-skip on an unparseable
  timestamp rather than propagating an error** - every member here already
  passed through Phase 3's `cluster_threads`, which itself `bail!`s loudly on
  an unparseable `created`/`modified` before a `Thread` can even exist. A
  parse failure at this seam would mean a caller bypassed that gate (a
  programmer error, not an operator-facing data problem), so logging + falling
  back to the next-best candidate is a defensive backstop, not the primary
  correctness mechanism - it never blocks a publish that would otherwise
  succeed.

### Open questions

- `pipeline::tags::get_or_init_canonical`'s process-wide, config-blind cache
  (first successful load wins for the rest of the test binary's life,
  regardless of which config subsequent callers pass) is a pre-existing
  test-isolation hazard, not introduced by Phase 5 but newly TRIGGERED by it
  (Phase 5 is the first harvest-side caller of `finalize_tags` with a
  `Config::default()`-shaped config in the test suite). Worth a follow-up
  design note (scope: cortex/borg tag-pipeline test infrastructure) either
  keying the cache by resolved path or giving tests an explicit reset hook;
  not fixed here since it is shared, unrelated infrastructure outside this
  phase's assigned files.
- The design doc's "envelope.yml (the export metadata for the thread)" staged
  artifact is, in practice, the SAME generic `Envelope` (`trace`/`kind`/
  `method`/`received_at`) every content kind already gets from `stages::raw::
  stage_0_init` - it is not enriched with thread-level export metadata
  (repo/scope/dates/redaction). The concatenated `body.txt` (already written
  generically for `ContentKind::Session`) IS the parsed-body artifact the doc
  names, and `distilled.yml` is written by `distill_for_publish_session`
  unchanged from Phase 4 - so 2 of the 3 named staged artifacts match exactly
  and the third (envelope) carries strictly less thread-specific detail than
  the doc's phrasing implies. Enriching `stage_0_init`'s envelope write would
  require changing its shared signature (touching every content kind, not
  just Session) or a second `write_envelope` call from the Session handler
  that races/duplicates the generic one - flagged for the parent to decide
  whether this is worth a follow-up rather than done speculatively here.

## Phase 6: CLI + observability

### Design decisions

- `sb borg harvest [--since <span|date>] [--dry-run] [--limit <n>] [--force]`
  added as `Command::Harvest(HarvestCliArgs)` (`sb/src/cli/borg.rs`). The arm
  resolves `dry_run = args.dry_run || config.harvest.mode == DryRun` (CLI flag
  forces dry-run; otherwise the config default decides, DryRun out of the box),
  calls `borg::harvest::run`, and prints via `print_harvest_report`.
- New core `borg::harvest::run` (+ injectable `run_with<R: ExportReader>`) is
  the ONE shared entry the CLI calls today and the Phase 8 timer will call
  ("on-demand and scheduled share one core"). It acquires the exclusive state
  lock (`watermark::acquire_lock`, loud `HarvestLockHeld` on contention), loads
  the watermark, resolves the window (explicit `--since` > stored cursor >
  first-run `harvest.initial-since`), fetches the bulk export, plans, and -
  unless dry-run - writes reject artifacts, publishes, and advances the
  watermark. Returns a typed `HarvestReport { dry_run, plan, outcomes }`;
  `sb` does all printing (borg house rule).
- `--limit` is threaded into `ExportReader::export_bulk` as a `limit` arg (a
  4th param; trait + `ClydeExportReader` impl + 2 test fakes updated) so it
  caps the clyde export PAGE, not the post-plan thread set. This is lossless:
  clyde's paging is gap-free, so the cursor advances only over the returned
  rows and the next run resumes cleanly. Capping after planning while the
  cursor still advanced would have silently dropped candidates - rejected.
- `sb borg log` already filtered on `--method`/`--status`/`--stage` as
  free-string args parsed to the typed enums by `triage::receipts_log`
  (`m.parse::<Method>()`, etc.), so `--method harvest` / `--status rejected` /
  `--stage selection` work via Phase 1's enum arms with no new plumbing - only
  the help text was updated to advertise the harvest values.

### Deviations

- Split `run` into a thin `run` (builds the production `ClydeExportReader` +
  `vault::paths::borg_harvest_state()`) and an injectable
  `run_with<R>(reader, config, state_path, ...)`. Not a spec deviation - it is
  the house DI pattern (generic reader port + explicit path) that makes the
  dry-run path unit-testable with a fake reader and a temp state path.

### Tradeoffs

- Chose to extend the `export_bulk` trait signature (4 sites) over a
  post-plan thread cap for `--limit`, trading a slightly wider change for
  lossless correctness (no silently-dropped candidates). The trait is
  internal to borg, so the blast radius is contained.

### Open questions

None.

### Validation

- `otto ci`: `✅ All CI checks passed!`.
- Unit tests: `run_with_dry_run_writes_nothing_and_reports_selection`
  (golden fixture -> dry-run report asserts 1 publishable thread + 2 rejects,
  no state file, zero body fetches) and `query_filters_by_harvest_method`
  (a `Method::Harvest`/`ReceiptKind::Session` row is the only match for a
  harvest-method filter).
- Real shakedown: `sb borg harvest --dry-run --since 3d --limit 15` against the
  LIVE clyde catalog loaded config, printed the DRY RUN summary (0 selectable /
  15 "would reject", all "not dormant (still in flight)" - correct for a 3-day
  window), exited 0, and wrote NO `harvest-state.json`.

## Phase 7: Eval + replay

### Design decisions

- **Stage-2 replay, session-only.** `replay_one` (`borg/src/replay.rs`) now
  reads the envelope first and, when `from_stage == 2 && envelope.kind ==
  IngestKind::Session`, dispatches to `replay_session_stage2` instead of the
  daemon re-POST path (a `clyde://` source cannot be re-POSTed like a URL).
  Every other kind's `--from-stage > 0` still bails loudly. The helper reads
  the staged transcript (`body.txt`) + `members.yml`, reconstructs
  `process_session`'s inputs, and re-publishes with `force = true` (overwrite
  the same note path in place). Structurally equivalent, not byte-identical
  (LLM pass).
- **`members.yml` staged artifact** (`borg/src/harvest/publish.rs`,
  `harvest::SessionReplayMeta`): the thread's full `SessionRecord`s +
  `primary_id` + `body_truncated`, staged via `write_attachment` on a
  successful publish. This is the concrete "thread export metadata" the Data
  Model called for and RESOLVES Phase 5's OQ#2: `distilled.yml`'s
  `SessionPayload` lacks scope/redaction/title, which the publish path needs to
  reproduce the note, so the full records are staged. Best-effort: a stage
  failure warns but never fails an already-landed publish.
- **`read_attachment`** added to the `ArtifactStore` trait + both impls
  (`FsArtifactStore`, `MemArtifactStore`) - the explicit read counterpart to
  `write_attachment`, returning `Ok(None)` when absent (a pre-Phase-7 note has
  no `members.yml` and replay says so loudly).
- **Eval session kind.** `render_options_for_kind("session")` returns
  `include_transcript: false` (session notes publish transcript-free, so the
  eval note-size must exclude the transcript too). The loader is kind-agnostic,
  so a `session/<slug>/{source.md,distilled.yml}` golden fixture
  (`slack-cli-release-promote`) is scored automatically by `sb borg eval`.
- `pipeline::session` promoted from `mod` to `pub(crate) mod` so replay can call
  `process_session`.

### Deviations

- None from spec. The design assumed the staged artifacts sufficed for stage-2
  replay; in practice they did not (Phase 5 OQ#2), so Phase 7 adds the
  `members.yml` artifact. This is the design's own "envelope.yml = the export
  metadata for the thread" intent, realized as a thread-specific file rather
  than overloading the shared generic envelope.

### Tradeoffs

- Staged the FULL `SessionRecord`s (not just the `SessionPayload`) so replay
  reproduces scope/redaction tags and the footer exactly. Slightly larger
  staged artifact, bought faithful re-derivation.

### Open questions

None.

### Validation

- `otto ci`: `✅ All CI checks passed!` (with `--features vec`, the workspace
  feature set otto wires).
- Tests: `publish_plan_publishes_and_rerun_is_idempotent` extended to assert
  `members.yml` is staged and `replay --from-stage 2` re-derives the note
  (`report.succeeded == 1`); `session_kind_loads_and_renders_transcript_free`
  and `real_repo_fixtures_load_and_include_session` (the latter guards that the
  checked-in session `distilled.yml` parses as a valid `Distilled`).

### Process note (orchestrator)

Burned cycles this phase running bare `cargo test -p borg` (missing the
`vault/vec` feature -> spurious `vault::search` errors) and misreading piped
exit codes (a `| grep | head` pipeline's exit masked cargo's). Corrected to:
`otto ci` is the single authoritative gate (right features, honest exit), and
`cargo fmt` runs before every validation. No code impact - all four Phase 7
tests pass.

## Phase 8: Timer

### Design decisions

- `borg/src/harvest/timer.rs`: `render_units(home, binary, config) -> (service,
  timer)` (pure/testable) + `install`/`uninstall`. `sb borg harvest --install`
  writes `sb-harvest.service` (Type=oneshot, `ExecStart=<abs binary> borg
  harvest`) and `sb-harvest.timer` (`OnCalendar=<harvest.schedule>`,
  `Persistent=true`) to `~/.config/systemd/user/`. `--uninstall` removes them
  (idempotent).
- The timer bakes in ONLY `OnCalendar`; every behavioral knob stays in
  `borg.yml`, read by the service's `sb borg harvest` at fire time (which goes
  through the same `harvest::run` core as an on-demand run - "on-demand and
  scheduled share one core"). New config key `harvest.schedule` (default
  `*-*-* 03:00:00`, nightly off-peak) + example template entry.
- Stripped-PATH safety: the service sets an explicit `Environment="PATH=..."`
  AND uses the absolute `current_exe()` binary in ExecStart; `harvest.clyde_binary`
  is already an absolute tilde-expanded default. So the unit resolves under a
  systemd timer's empty inherited environment.
- Light hardening only (`NoNewPrivileges`, `PrivateTmp`) - harvest writes the
  vault + `~/.local/share/sb`, so a `ProtectHome`/`ProtectSystem` lockdown
  (which cortex's daemon uses with an enumerated `ReadWritePaths`) would block
  the writes; omitted deliberately rather than enumerate every write path.

### Deviations

- Install lives on `sb borg harvest --install/--uninstall` flags (mirroring the
  daemon's install pattern) rather than a separate subcommand - the design said
  "bootstrap-installed like existing units" without pinning the exact verb.

### Tradeoffs

- "Two consecutive timer runs double-ingest nothing" is satisfied by the
  watermark (already tested: `publish_plan_publishes_and_rerun_is_idempotent`),
  not re-tested here - the timer is a thin scheduled wrapper over the same
  `harvest::run` core, so there is no new dedup logic to test at the timer
  layer. The timer tests instead assert the unit-structure criteria.
- "Runs with an empty inherited PATH" is asserted structurally (absolute
  ExecStart binary + explicit PATH env in the rendered unit) rather than by
  actually exec'ing the unit with `PATH=""` (which would run a real harvest with
  side effects) - the render properties are the right unit-test proxy.

### Open questions

None.

### Validation

- `otto ci`: `✅ All CI checks passed!`.
- Tests: `service_uses_absolute_binary_and_explicit_path`,
  `timer_bakes_only_oncalendar_from_config` (asserts no behavioral tunable
  leaks into the .timer), `schedule_change_is_the_only_timer_difference`.

## Phase 9: `repo:` anchor wiring chain

### Design decisions

- `vault::Frontmatter` gains typed `repo: Option<String>` (present-null/absent
  -> None, stored verbatim, never re-derived) and `repos_touched:
  Option<Vec<String>>` (THREE-STATE: None=omitted/unknowable, Some(vec![])=
  present-empty/definitively-no-bridge, Some(xs)=the set). Parsed in the manual
  extraction loop, emitted in `to_yaml` (as join keys, so a backfill rewrite
  never strips them), and added to `is_empty`.
- `vault::schema::validate_repo_slug` - the ONLY check (exactly one `/`, two
  non-empty components). A caller that fails it skips the repo hub edge (Phase
  10) and logs, but the note still publishes.
- Index: `ensure_repo_columns` adds `notes.repo` via the idempotent
  `PRAGMA table_info` + `ALTER ADD COLUMN` pattern (mirrors
  `ensure_trace_columns`); `repo` bound as param `?35` in BOTH the INSERT and
  the UPDATE upsert branches; `GraphNoteRow` carries `repo` end to end (struct
  field + SELECT + query_map tuple + construction).
- Phase 5's session renderer already emits `repo:` (present-null), so this
  phase is the read/index/validate half; the renderer was not re-touched.

### Deviations

- `clone_frontmatter` (cortex `summarize --backfill`) and the
  `cortex::testutil` note builder both construct `Frontmatter` field-by-field
  (no `..Default`), so both gained the two new fields - `clone_frontmatter`
  MUST carry them (join-key strip trap), the testutil is `None`/`None`.
- `repos-touched` is frontmatter-only in Phase 9 (no DB column): the single-repo
  hub slice (Phases 9-10) keys on `repo`; bridging (which consumes
  `repos-touched`) is deferred on clyde's files-touched release, so no index
  column is warranted yet.

### Tradeoffs

- `repo` stored as `TEXT DEFAULT ''` (empty = no repo), matching the trace
  columns' empty-string convention rather than nullable - keeps the graph
  SELECT's `unwrap_or_default()` uniform.

### Open questions

None.

### Validation

- `otto ci`: `✅ All CI checks passed!`.
- Tests: `validate_repo_slug_accepts_org_repo_and_rejects_malformed`,
  `parse_repo_and_three_state_repos_touched` (asserts omitted None is distinct
  from Some(vec![])), `repo_round_trips_through_index_to_graph_note_row`
  (frontmatter -> upsert -> notes.repo -> GraphNoteRow, verbatim).

## Phase 10: Repo hub kind + deterministic edge

### Design decisions

- `HubKind::Repo` (`cortex/src/hub.rs`) with `as_str`/`ontotype` "repo".
- `repo_hub_slug("<org>/<repo>") = repo-<slugify(org)>--<slugify(repo)>`,
  splitting on the single `/`. INJECTIVE on the org/repo split - the `--`
  boundary can't be forged (slugify collapses runs to a single `-` and trims
  boundary hyphens), so `a/b-c` -> `repo-a--b-c` and `a-b/c` -> `repo-a-b--c`
  are distinct. NOT per-component injective (`.`/`_` fold, so `org/.github` ==
  `org/github`) - accepted, membership-only, `repo:` frontmatter stays truthful,
  same lossiness Creator/Source already carry. Case-fold inherited + correct.
- `collect_stubs` mints a repo hub for every note with a well-formed `repo:`
  (gated by `vault::schema::validate_repo_slug`; malformed -> WARN + skip, note
  still indexed). The `repo-` prefix makes the namespace disjoint from the
  bare-token Concept/Creator/Source/Tag slugs, so a concept named "loopr" and
  repo `scottidler/loopr` never collide.
- The `repo-member` graph edge (`cortex/src/graph.rs`, `build_edges_for`):
  UNCONDITIONAL note -> `entities/repo-<org>--<repo>.md` at full weight -
  genuinely new routing, distinct from the fan-out-capped note<->note shared-*
  buckets. Rides the resolve-endpoint-or-skip rule (lands once the hub pass
  stubbed the hub; re-added each sweep - monotonic).

### Deviations / Tradeoffs

- Repo membership rides at `REPO_MEMBER_WEIGHT = 1.0` (a strong deterministic
  signal), not a rarity weight - repo co-membership is definitional, not
  incidental like a shared blanket tag.

### Open questions

None.

### Validation

- `otto ci`: `✅ All CI checks passed!`.
- Tests: `repo_hub_slug_is_injective_on_the_org_repo_split` (the mandatory
  /-bearing fixture: adversarial pair distinct, case-fold merge, `.`-fold merge
  documented), `collect_stubs_mints_repo_hub_deterministically_and_disjoint_from_concepts`
  (sweep-twice byte-identical, one hub for two same-repo notes, concept
  coexists), `collect_stubs_skips_malformed_repo`.
