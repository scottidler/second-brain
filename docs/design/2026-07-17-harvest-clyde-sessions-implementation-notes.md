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
