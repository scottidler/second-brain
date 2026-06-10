# Implementation Notes: Code Review Remediation

Running, append-only record of decisions, deviations, tradeoffs, and open
questions encountered while executing
`docs/design/2026-06-09-code-review-remediation.md`.

## Phase 1: Critical correctness fixes

### Design decisions
- `vault/src/watcher.rs` — introduced module-level `INERT_DEBOUNCE`
  (`Duration::from_secs(86400 * 365)`) and replaced BOTH `Duration::MAX`
  usages (initial `sleep` and the post-emit `reset`), not only the one reset
  the doc cited. Both compute `Instant::now() + dur` eagerly and share the
  overflow-panic risk.
- `cortex/src/tags.rs::replace_tags_in_frontmatter` — reimplemented to delegate
  to `scope::insert_frontmatter_fields` (continuation-aware) rather than
  keeping a parallel rewrite. Side effect: the `tags:` line now lands at the
  END of the frontmatter block (insert removes-then-appends) instead of in
  place. Position is not semantically meaningful and all callers/tests are
  position-independent.
- `cortex/src/scope.rs::remove_entry` — promoted from private to `pub(crate)`
  so `migrate.rs` field-drops can reuse it (the doc instructed reuse).
- `borg/src/pipeline.rs` — added a single `with_hard_timeout` helper and wrapped
  the five non-URL dispatch arms at the dispatch site, rather than editing each
  handler's body. `process_url` keeps its own internal timeout (left as-is).
- `borg/src/markdown.rs` — replaced `escape_yaml_string` with `yaml_scalar`
  (serde_yaml round-trip). Routed `title`, `creator`, `source`, `asset`, and
  the `serialize_yaml_value` String arm through it. Simple scalars now render
  BARE (e.g. `creator: Scott`) instead of always quoted; updated the affected
  render-test assertions accordingly. Audit-fix code paths
  (`set_creator_if_empty`, `fix_note_type`) and `language:` (markdown.rs:172)
  still emit explicit quotes — out of scope for this finding, controlled
  values, left unchanged.
- `oracle/src/config.rs` — manual `impl Default` for `Config`. Regression test
  asserts the load-bearing field (`inbound_recompute_interval_secs`) agrees
  between `default()` and `from_str("{}")` and is non-zero, rather than full
  `PartialEq` equality (deriving PartialEq across the whole config tree is
  invasive churn unrelated to the bug).
- `borg/src/telegram.rs` — extracted pure `pub fn chat_allowed`; created a new
  `borg/src/telegram/tests.rs` submodule (2018-style) for its tests.

### Deviations
- None from the specified fixes. The oracle test scope (field-equality vs full
  `==`) is the one interpretive narrowing, noted above.

### Tradeoffs
- `with_hard_timeout` wraps at the dispatch site (one helper, five call sites)
  vs. editing each handler internally (would have touched five functions and
  their error plumbing). Dispatch-site wrapping is smaller and keeps the
  timeout policy in one readable place.
- `yaml_scalar` via serde_yaml vs. hand-rolled escaping: serde_yaml is the
  source of truth for what the cortex parser will read back, so round-trip
  correctness is guaranteed rather than approximated.

### Open questions
- None.

## Phase 2: Runtime hygiene (timeouts, blocking, subprocess I/O)

### Design decisions
- `vault/src/fabric.rs` — added an exported `wait_with_timeout` (drains
  stdout/stderr and writes stdin each on its own thread) returning
  `Ok(None)` on timeout. `run_pattern` now delegates to it. This is the
  deadlock-safe subprocess primitive; introduced a `ProcessOutput` type alias
  to satisfy clippy `type_complexity`. (Also satisfies the Phase 9
  `wait_with_timeout` export item.)
- `borg/src/youtube.rs` — `extract_audio` and `extract_frames` converted to
  `async` (tokio::process + timeout + kill_on_drop), gaining a `timeout_secs`
  param threaded from `pipeline.yt_dlp_timeout_secs`. Their tests became
  `#[tokio::test]`. Moved the `std::process::Command` import into the test
  module (only tests use it now).
- `borg/src/pipeline.rs` — `with_hard_timeout` (Phase 1) is the wrapper; here
  the YouTube slide work dir (`/tmp/borg-youtube-frames/<id>`) is removed
  after `publish_slides` copies the JPEGs out.
- `borg/src/fabric.rs` — `fetch_article` body moved into a sync
  `fetch_article_blocking` run under `spawn_blocking`; the markitdown
  extraction call in `handlers.rs` likewise wrapped in `spawn_blocking`.
- `borg/src/extraction.rs::extract_markdown` — gained a `timeout_secs` param
  (was hardcoded 30s, ignoring `pipeline.markitdown_timeout_secs` default 60).
- `oracle/src/server.rs` — query transform (`transform_queries`) now runs
  BEFORE the SearchIndex lock; lock-holding retrieval runs under a
  `block_in_place_compat` helper that no-ops to inline execution on a
  current-thread runtime (tokio tests) and uses `block_in_place` on the
  multi-thread production runtime. `run_pipeline` takes precomputed `queries`.
- `cortex/src/daemon.rs` — embed-model load failure now degrades to
  `embed_model: None` (embed tick skipped) instead of `?`-crashing the
  daemon; added a `shutdown_signal()` helper handling SIGTERM as well as
  Ctrl-C.
- `cortex/src/embed.rs` — zero-progress guard now breaks on `embedded == 0`
  (any cause), not only the all-skipped case; `daemon_cadence` reads the new
  `embed.cadence-secs` config field.
- `borg` transports — extracted `ExponentialBackoff::reset_if_healthy`
  (shared by telegram/discord/signal): reset only after the connection stayed
  up `HEALTHY_RUN_SECS` (60s), an elapsed-time proxy for "sustained healthy
  operation."
- `borg/src/signal.rs` — `NoteToSelfRateGate` gained a one-shot `alert_sent`
  latch (`take_alert_slot`) so the tripped-gate alert is sent once, not per
  dropped envelope.
- `borg/src/discord.rs` — both pipeline dispatches moved into detached
  `tokio::spawn` tasks (mirroring telegram) so a slow ingest can't stall the
  serenity gateway heartbeat.

### Deviations
- The backoff "reset on first successfully dispatched message" criterion was
  implemented as an elapsed-uptime threshold (`HEALTHY_RUN_SECS`). teloxide's
  per-message dispatch state cannot cleanly signal back to the reconnect loop;
  uptime is a faithful proxy for the doc's stated goal ("sustained healthy
  operation") and is unit-testable.
- The `oracle/src/server.rs` decomposition into `server/pipeline.rs` (a Phase
  8 item) was PULLED FORWARD into Phase 2: the mutex refactor pushed server.rs
  to 1535 lines, over the 1500 gate. server.rs is now 1005 lines; the
  retrieval/pipeline methods live in `server/pipeline.rs`. Phase 8 therefore
  only needs borg config.rs and sb checks.rs (plus the advisory >1200 files).

### Tradeoffs
- `block_in_place_compat` (runtime-flavor switch) vs. forcing every test to
  `#[tokio::test(flavor = "multi_thread")]`: the helper keeps the production
  path optimal and tests unbrittle, at the cost of one small runtime probe.

### Open questions
- None.

## Phase 3: Receipts & durable-capture integrity

### Design decisions
- `borg/src/pipeline.rs` — stage-0 rejection now sets the failure `result` and
  falls through to the single `record_terminal_to_receipts` chokepoint instead
  of early-returning, so the row no longer rots in `received`.
- Non-URL handlers (image/audio/document/text) classify their terminal errors
  as `PublishFailed` (content is in hand → never a fetch), replacing the
  global `FetchFailed` default. Finer per-stage classification within those
  handlers was not pursued (publish dominates; they have no fetch stage).
- Transport download-failure and unsupported-type branches in telegram and
  discord now call `record_failure_at_door` (FetchFailed / IntakeRejected) on
  every early return.
- Telegram disallowed-chat check moved BEFORE intake: a refused chat records a
  receipts row but writes NO sidecar (mirrors the HTTP 401 path; keeps junk
  out of the Syncthing'd vault). This intentionally reorders the previous
  "durable intake before filter" for disallowed chats only.
- `receipts.rs` — `degraded INTEGER NOT NULL DEFAULT 0` column added via an
  idempotent `has_column` probe + `ALTER TABLE` in `run_migrations`
  (SCHEMA_VERSION bumped to 2); fresh DBs get it from the baseline schema.
  `IngestResult.degraded` is derived from
  `distilled.meta.validation.fallback_reason.is_some()` at every publish site
  and threaded to `mark_succeeded`. `sb borg log --degraded` filters on it. No
  change to `stages/distill.rs` was needed — the distillers already populate
  `fallback_reason`; the wiring reads it.
- Dead `promote_stale_to_crashed` deleted; the watchdog path
  (`list_stale` + `promote_single_to_crashed`) is the live mechanism. Tests
  rewritten onto the live path.
- `record_received_expecting_existing` variant demotes the "already present"
  no-op log from WARN to DEBUG on the failure-at-door path.
- `GET /trace/{trace_id}` endpoint (auth-gated) added; `reingest_via_daemon`
  and `crate::reingest` poll it via `poll_trace_terminal` for the terminal
  state (ceiling = `hard_timeout + 90s`), restoring replay accounting and
  one-at-a-time pacing. The shared poll lives in `replay.rs`.

### Deviations
- The doc asked for a stage-0 `process_content` receipts-state integration
  test and a transport "failed download lands FetchFailed" test. Both require
  env-mutation (`XDG_DATA_HOME`) to isolate the per-host receipts DB plus
  canonical-asset/permit setup - heavy and brittle. Instead the receipts
  MECHANISM is covered at unit level (degraded set/filter, v2 migration ALTER,
  mark_failed/IntakeRejected, list_stale, promote_single). The stage-0 fix is a
  structural fall-through verified by compilation + those unit tests.
- `project-halt-on-hard-distill` memory marked SUPERSEDED (degraded flag, not
  halt-to-DLQ). second-brain `CLAUDE.md` had no halt-on-distill text to retire
  (verified by grep).

### Tradeoffs
- Polling `/trace/{id}` over HTTP vs. a `?sync=true` ingest variant
  (Alternative 3): polling keeps the detached-dispatch wire contract and reuses
  the authoritative receipts store, at the cost of a poll loop bounded by the
  pipeline hard timeout.

### Open questions
- None.

## Phase 4: Atomic vault writes everywhere

### Design decisions
- Promoted borg's `pipeline/atomic.rs::write_atomic` (the better of the two
  copies: unique tempfile + fsync + persist + parent fsync) into
  `vault::note::write_atomic` as THE shared primitive. borg's `write_atomic`
  is now a thin re-export; cortex's `summarize::rewrite_note_file` routes
  through it (replacing its fixed-name `.md.tmp` that could collide under
  rayon).
- Added `tempfile` to vault's `[dependencies]` (was dev-only).
- Routed every cortex note-mutation site through `vault::note::write_atomic`
  (scope, classify ×5, naming, frontmatter ×2, tags, autotag ×2, linking,
  quality ×2, duplicates ×2, migrate ×4 incl. the move + wikilink-rewrite
  sites, intel output, sweep tags). Non-note writes (systemd units, state
  manifests, proposal/checkpoint YAML/JSON outside the vault) were left as
  plain `fs::write` - they are not Syncthing'd vault notes.
- Routed the six borg non-URL publish sites (image/audio/document handlers,
  text/vocab/code) through `write_atomic`.
- Added `pipeline::atomic::resolve_publish_path(dest, force)` (mirrors
  classify's `resolve_collision` minus the source-URL/reingest case) and wired
  `force` end-to-end through every non-URL handler's inner fn (previously
  `_force`, ignored), so a same-title note is uniquified (`-2`, `-3`) instead
  of silently clobbered, and `--force` overwrites.

### Deviations
- The doc listed cortex `migrate.rs:350,495`; I also converted the move-write
  (`migrate.rs:118`) and the wikilink-rewrite write (`migrate.rs:223`) since
  both mutate vault notes. `intel.rs:181` (dead `process_new_notes`) was left
  unconverted because Phase 6 deletes that function.

### Tradeoffs
- `resolve_publish_path` is a borg-local helper rather than promoting
  classify's `resolve_collision` to vault, because the non-URL case is
  strictly simpler (no source-URL reingest semantics) and classify's version
  stays where its callers live.

### Open questions
- None.

## Phase 5: Fail-closed defaults & transport security

### Design decisions
- `is_local_host` refactored into a pure `host_matches(host, current)` so the
  fail-closed-on-unreadable-hostname path is unit-testable; an unreadable
  hostname with a pin set now returns false (was `true`, defeating the pin).
- oracle `find_config_file` dropped the `./oracle.yml` CWD fallback (single
  location: `~/.config/sb/oracle.yml`); logs which file loaded at INFO via
  `tracing` (oracle is tracing-based, not `log`).
- `config::resolve_client_auth_token` resolves `server.auth-token` for
  first-party clients; reingest (lib.rs ×1), the hotkey ingest path (lib.rs
  ×1), `reingest_via_daemon`, and the `/trace` poll all set `bearer_auth` when
  a token is configured.
- `DefaultBodyLimit::max(64 MiB)` on `/ingest/file` (was axum's undocumented
  2 MB default).
- Constant-time `constant_time_eq` for the bearer-token compare.
- AMO creds passed to `web-ext sign` via `WEB_EXT_API_KEY` /
  `WEB_EXT_API_SECRET` env vars instead of world-readable argv.
- systemd unit: `ReadWritePaths` now includes the borg data dir
  (`~/.local/share/sb/borg` parent) alongside the vault; the vault path is
  derived from config (`config.vault_root()`) instead of hardcoded. `daemon`
  /`install_service`/`install_systemd` now thread `&Config`.
- ntfy: `force` is never honored (dropped the JsonBody `force` field;
  `ParsedMessage::Url.force` is hardcoded `false`); template documents the
  reserved-topic + token requirement.
- multipart `tags` accepts repeated fields (no comma-split); dropped the
  full-upload `bytes.clone()` + filename clone that were `let _`-discarded.

### Deviations
- `install_systemd` still hardcodes the `secrets` path (manifest age-decrypt
  source) - there is no borg config field for it, so only the vault path was
  derivable from config. The data path is derived from `vault::receipts`.
- `config.rs` inline test module was extracted to `config/tests.rs` (a Phase 8
  item) because the `is_local_host` refactor pushed config.rs to 1504 lines,
  over the gate. Phase 8 now only needs sb checks.rs + the advisory files.

### Tradeoffs
- Hand-rolled `constant_time_eq` over adding the `subtle` crate: the doc
  allowed either; a four-line fold avoids a new dependency.
- 64 MiB body limit is a fixed const (not config) - it bounds the largest
  supported attachment type; making it configurable was out of scope.

### Open questions
- None.

## Phase 6: Schema is law

### Design decisions
- `vault::schema::NoteType` gained `Digest` + `Review` variants (as_str/all/
  from_str/serde all updated); 76 real notes that warn-spammed with empty type
  now index with a real type.
- `cortex::intel` writes `type:` via `NoteType::Digest/Review.as_str()`; dead
  `intel::process_new_notes` (sole writer of invalid `status: processed`,
  zero callers) deleted. Legacy `"processed"` read arms kept.
- `cortex::config::SchemaConfig::default()` rebuilt from `Domain/NoteType/
  Origin/Status/Method::all()` (was the empty derived Default that validated
  nothing and had drifted). Config still overrides.
- `cortex::quality` EXCLUDED_TYPES list → `is_excluded_type()` enum match.
- `cortex::summarize::kind_from_type` → `NoteType::from_str` + enum match.
- `borg::markdown` ContentType → `NoteType::…as_str()`.
- Scattered `"assisted"`/`"unread"` string literals → `Origin::Assisted.as_str()`
  / `Status::Unread.as_str()` at the PRODUCTION sites (summarize, entities,
  classify ×2, autotag ×2). Test fixtures left as literals (rust.md exempts tests).
- `vault::hygiene` DOMAIN_ALIASES: dropped both `Inbox → "inbox"` rows (inbox is
  a location, not a domain; `Domain::from_str` rejects it).
- SQL literals in `vault/search/{cold,stats}.rs` → `format!` with enum `as_str()`.
- **IngestMethod shadow removed:** borg's parallel `IngestMethod` enum is now
  `pub use vault::schema::Method as IngestMethod;` (borg's Cargo.toml enables
  vault's `schemars` feature). `From<IngestMethod> for Method` + its Display +
  `borg::trace::generate`'s remapping deleted; `method` threads straight through.
  Removed the now-reflexive `.into()` at 12 call sites (intake ×2, migrate,
  pipeline, pipeline/text ×3, pipeline/handlers ×3, intake/tests ×2); used
  field-shorthand to avoid `redundant_field_names`. receipts.rs:465's `.into()`
  (String→rusqlite Value) left untouched — unrelated.

### Deviations
- `IngestRequest.method` is now `Option<Method>` (manual lowercase serde variant
  added at the wire). Verified additive: the extension schema tests
  (`extension_body_matches_ingest_request`, `stage_produces_valid_extension_dir`)
  pass unchanged, confirming Rollout note 3 (no extension re-sign needed) holds.

### Tradeoffs
- `cortex/src/config.rs` inline test mod: added the SchemaConfig regression tests
  as a `config/tests.rs` submodule (2018-style) rather than an inline block, per
  rust.md test-placement, even though config.rs itself isn't being decomposed.

### Open questions
- None.

### Migration note
- Per design-doc Rollout note 4: the 76 notes currently indexed with empty
  `note_type` only refresh on mtime change; run the oracle `reindex` tool once
  after deploy so they pick up the real `digest`/`review` types. (Deferred to
  deploy time, not part of this commit.)

## Phase 7: Error-handling integrity

### Design decisions
- `vault::search` gained two shared helpers (in `search.rs`): `optional_row`
  (single-row queries — `QueryReturnedNoRows` → `Ok(None)`, every other
  rusqlite error including `SQLITE_BUSY` propagates) and `warn_row` (query_map
  iteration — yields `Ok` rows, WARNs each dropped `Err`). Replaced every `.ok()`
  conflation (query.rs get_note/note_signals/resolve_wikilink ×3, index.rs,
  vector.rs ×2, stats.rs note_quality, graph.rs graph_state_get/get_entity) and
  every production `filter_map(|r| r.ok())` across query/index/vector/stats/
  cold/schema/graph. Test files left as-is.
- `oracle::eval::cache::JudgmentCache::get` got the same shape inline (oracle
  can't see vault's `pub(crate)` helper): NoRows → cache miss, other errors
  propagate (so a locked DB no longer silently re-buys every LLM judgment).
- `vault::search::stats::classify_stats` — the lone unparameterized
  caller-supplied value. Rewrote the dynamic `AND domain = '{d}'` interpolation
  to `(?1 IS NULL OR domain = ?1)` with `params![domain]` (Option<&str> binds to
  NULL when None). No dynamic SQL string at all. Regression test in group_a.rs
  feeds an injection-shaped domain and asserts 0 rows + no error.
- Typed `vault::fabric::FabricError` (Timeout / Failed) — vault is a library, so
  this uses `thiserror` (added via `cargo add`), per the libs-use-thiserror
  rule. `run_pattern` returns it through `eyre::Report` (downcastable), so the
  `eyre::Result` signature and all ~20 callers are unchanged. `FabricError::
  is_timeout(&report)` replaces `msg.contains("timed out")` at the six distiller
  fallback sites. `FakeFabric::set_timeout(pattern)` injects a real typed
  timeout; the six distiller timeout tests switched from `set_error("...timed
  out...")` to `set_timeout(...)`.
- `cortex::embed::EmbedLockHeld` — a typed, downcastable marker error
  (hand-rolled, NOT thiserror: cortex is otherwise eyre-only and this is its one
  typed error, so adding the dep wasn't warranted). `acquire_lock` returns it on
  `try_lock_exclusive` contention; the daemon-tick caller downcasts instead of
  `e.to_string().contains("embed lock")`.
- Silent-swallow WARNs added: `vault::frontmatter::parse_frontmatter` (malformed
  YAML → empty metadata), `vault::paths::CliConfig::load` (parse failure →
  defaults), `vault::search::graph` graph_note_rows (unparseable tags JSON).
- `borg::fabric::fetch_article_blocking` now logs each tool's exit status +
  500-char stderr preview (and empty-output) for fabric -u AND markitdown,
  instead of swallowing all failure detail behind chained `if let`.
- Timezone parse fallback (septuplicated) → one `FrontmatterConfig::timezone_tz()`
  helper; replaced 7 pipeline sites + backfill.rs. Validated ONCE at config load
  (`Config::validate` WARNs on an unparseable IANA zone) so a bad value surfaces
  at startup rather than silently falling back to LA at every call site.

### Deviations
- `generate_tags` discarded errors: the doc lists "five borg call sites"; there
  are actually six (`pipeline.rs`, `handlers.rs` ×3, `text.rs` ×2). Converted all
  six `if use_fabric && let Ok(..)` to `match` with an `Err(e) => WARN` arm.

### Tradeoffs
- `(?1 IS NULL OR domain = ?1)` over a built `Vec<&dyn ToSql>` param list: the
  NULL-guard idiom keeps the SQL static and sidesteps the `&str`-unsizing /
  lifetime friction of a dynamic positional-param vector.
- Hand-rolled `EmbedLockHeld` vs. pulling `thiserror` into cortex: avoids adding
  a dep (and the thiserror-skew the Phase 9 hoist is consolidating) for a single
  marker error; vault's `FabricError` DOES use thiserror because vault is the
  shared library where the typed-error surface is broad enough to justify it.

### Open questions
- None.
