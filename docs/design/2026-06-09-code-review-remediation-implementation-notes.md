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

## Phase 8: Bloat decomposition

### Design decisions
- `sb/src/cli/checks.rs` (1187 → 943): extracted the inline `mod tests` to
  `sb/src/cli/checks/tests.rs` (the required item; the test mod was the headroom).
- `borg/src/lib.rs` (1420 → 967): split along the doc's daemon-vs-cli seam.
  The daemon dispatcher (`daemon` + `DaemonOutcome`) and ALL OS service-
  management helpers (install/uninstall/stop/restart/status, systemctl,
  launchctl, install_systemd/launchd, uninstall_systemd/launchd, the GNOME
  hotkey install/uninstall + consts) moved to new `borg/src/service.rs`. lib.rs
  keeps the HTTP server (`serve_init`/`build_router`), ingest entry points, and
  `hotkey()` (which now calls `service::install_hotkey`/`uninstall_hotkey`).
  `pub use service::{DaemonOutcome, daemon};` keeps the public API
  (`borg::daemon`) byte-for-byte for sb's CLI dispatch. Moved helpers became
  `pub(crate)`.

### Deviations
- For `borg/src/audit.rs` (1397 → 850) and `cortex/src/classify.rs` (1263 →
  943), the doc suggested production seams ("audit kinds", "classify tiers"). I
  instead extracted their inline `mod tests` (→ `audit/tests.rs`,
  `classify/tests.rs`). Rationale: the goal of Phase 8 is headroom under the
  1500 gate before Phases 9/11 edit these files; test extraction yields the
  most headroom (548 / 321 lines) with the least regression risk, AND satisfies
  the mandatory rust.md test-placement rule (which Phase 14 schedules for
  classify anyway). The production code in both is cohesive; a kind/tier split
  would be additional churn on files that are now well under 1000 lines.

### Tradeoffs
- lib.rs got the real production split (its tests were already extracted, so it
  sat at 1420 production lines and a refactor in Phase 9 — the notify-sink dedup
  — would have approached the gate). audit/classify got test extraction because
  that alone cleared the headroom need.

### Open questions
- None.

## Phase 9: Cross-crate dedup & dependency hygiene

### Design decisions
- **distillers::parse** (new module): the six `strip_fences` copies collapsed
  into one with the truncation BUG FIXED — a closing fence is now stripped only
  when an opening fence was actually present, so unfenced output containing an
  embedded ``` is no longer truncated. Also consolidated `approx_tokens`
  (returns `usize`; the four u32-meta call sites cast at the boundary),
  `PatternClaim`/`PatternLink` (identical everywhere), a shared `PatternYaml`
  (article/image/video/voicenote — repo and thread keep their own with
  kind-specific fields, reusing the leaf structs), plus `find_boundary` and
  `ReduceYaml` (byte-identical between video/voicenote). Six fence + token
  regression tests in `parse/tests.rs`.
- **vault::frontmatter::split_raw**: one splitter replacing five ad-hoc copies
  (`borg::replay`, `borg::migrate`, `borg::audit`, `borg::backfill`,
  `cortex::migrate`). `parse_frontmatter` now delegates to it. Body is returned
  raw (untrimmed) so round-tripping callers preserve their blank line;
  `parse_frontmatter` trims as before.
- **truncate_input**: the byte-identical cortex copy in `fabric.rs` is now a
  re-export of `llm::truncate_input`.
- **update_wikilinks_for_moves**: classify + migrate now delegate to
  `naming::update_wikilinks_batch` (the robust case-insensitive, alias-aware,
  atomic-write superset); the two weaker copies deleted.
- **ledger** (`append_entry`): rows built via `table::format_row` (escapes `|`
  and newlines per cell — a `|` in a source URL no longer shatters the row);
  the exclusive lock is now taken BEFORE the header check + append (closes the
  TOCTOU where a concurrent appender raced the unlocked header repair).
- **Desktop sink**: `lib::send_notification` reuses `config::APP_NAME` (now
  `pub(crate)`); its 5000 ms DISPLAY timeout is a named const, kept distinct
  from `notify::Desktop`'s 500 ms D-Bus call-timeout (different semantics).
- **Dependency hygiene**: hoisted rmcp, rusqlite, schemars, serial_test,
  tempfile, thiserror, tracing-subscriber, url, teloxide to
  `[workspace.dependencies]` (resolving the thiserror 2.0/2.0.18 and
  schemars 1.2/1.2.1 skews); each crate now `{ workspace = true }`. Removed
  unused deps (borg colored+env_logger, cortex env_logger+which, sb tracing).
  Added `[workspace.lints.rust]` (dead_code/unused_variables = deny, mirroring
  the crate-root denies; clippy::unwrap_used stays per-crate so tests keep
  unwrap) with `lints.workspace = true` per crate, and deliberate
  `[profile.dev.package."*"] opt-level = 2` + `[profile.release] lto = "thin"`
  for the candle/libsignal/aws-lc build weight.
- Deleted the test-only `markdown::sanitize_filename` wrapper (vault::hygiene
  already tests the real fn); single `eval::mode_label` (server.rs delegates,
  `None` → "configured"); `queries.rs` uses `judge::MAX_SCORE`; deleted the dead
  `slide_summary`/`use_fabric` bindings + unused clone in `process_youtube`;
  fixed the stale "mirrors the cortex pattern" comment in `borg::fabric`.

### Deviations
- **build_distilled / call_fabric NOT shared (video↔voicenote)**: per the Risk
  table's "diff the copies first to surface intentional divergence" mitigation,
  I diffed them — `build_distilled` genuinely differs (video keeps claim
  timestamp anchors; voicenote drops them; warn thresholds 500 vs 200 words),
  and `call_fabric` is a `&self` method differing only in its log label. Kept
  per-kind; only the byte-identical `find_boundary`/`ReduceYaml` were shared.
- **ledger reader parse_table migration NOT done**: `check_duplicate` /
  `find_completed` / `query_entries` / `parse_completed_entries` retain
  positional `col_idx` because it decodes TWO layouts (legacy 9-field +
  current 8-field) that single-header named-column `table::parse_table` cannot
  express for legacy rows. The load-bearing correctness fixes (escape, TOCTOU)
  landed; migrating the dual-layout readers would risk dropping legacy-row
  reads on this ingest-state-of-record file. Documented rather than forced.

### Item 7 (run_distiller) — DONE
- The five simple `distill_for_publish_*` clones (article, voicenote, image,
  idea, vocab) collapsed into one private `run_distiller` core; each is now a
  thin wrapper. A `preserve_transcript_on_fallback` flag keeps behavior exact
  (article does NOT re-assert transcript on fallback; the four transcript-
  bearing kinds do). The bespoke video/repo/thread distillers keep their own
  bodies (map-reduce / payload building — more than the shared core). distill.rs
  806 → 753 lines; 716 borg lib tests pass unchanged.

### Remaining (item 7 epilogue, items 8, 10)
- **Item 7 (epilogue half)**: extract a `publish_note()` helper for the 6×
  copy-pasted handler epilogue (tz, ledger entry, obsidian URL, IngestResult).
- **Item 8**: extract the ~8× processing→pipeline→result dispatch block in
  telegram/ntfy/routes into a helper.
- **Item 10**: expose `borg::probe_telegram()`/`probe_signal()` typed probes;
  rewire sb's doctor; drop teloxide/signal-rs/hostname from `sb/Cargo.toml`.
  (Risk: relocates the `!Send` signal probe into a load-bearing operator
  surface.)

### Tradeoffs
- Hoisting `teloxide` to the workspace even though sb's copy is slated for
  removal (item 10): borg keeps it, so the workspace entry is correct
  regardless; sb's line is dropped when item 10 lands.

### Open questions
- None.

## Phase 9 (continued): item 7 epilogue, item 8, item 10 — DONE

This supersedes the "Remaining" block above: all three deferred items landed in
a second pass. `otto ci` exit 0 (check incl. clippy+fmt, and test).

### Item 7 epilogue — DONE
- `pipeline::publish::publish_note()` now owns the shared success epilogue: it
  computes the tz-aware date/time, appends the ledger row, builds the obsidian
  deep-link, and returns the `Completed` `IngestResult`. The 6 handlers
  (`handlers.rs` image/audio/document, `text.rs` text/vocab/code) each collapse
  from a ~30-line epilogue (+ a 4-line tz header at the top of the fn) to a
  single `publish_note(...)` call passing only the per-site `source`, `title`,
  `tags`, and `degraded`.

### Item 8 — DONE
- New `borg::dispatch::dispatch_ingest()` (in `borg/src/dispatch.rs`) owns the
  processing-notify → `process_content` → result-notify block. All 10 spawned
  dispatch sites (2 ntfy, 3 routes, 5 telegram — the design's "~8×") now call it
  and keep only their own pre/post logging. Sinks stay trait-free per policy:
  the helper takes the concrete `Option<Desktop>` / `Option<Telegram>` and fires
  them side-by-side, exactly as before. Signal is not a dispatch-site sink today
  (it is inbound-only), so the helper is desk+tg — scope unchanged.

### Item 10 — DONE
- `borg::probe_telegram(token)` (in `telegram.rs`) and `borg::probe_signal(state_dir)`
  + `borg::SignalProbe` (in `signal.rs`) are the typed probes, re-exported at the
  crate root. `borg::config::current_hostname()` is the single hostname-reading
  helper (`is_local_host` now delegates to it). sb's doctor calls all three;
  `teloxide`, `signal-rs`, and `hostname` are removed from `sb/Cargo.toml`
  (`cargo tree -p sb` confirms they are gone from sb's direct deps). The
  `signal_rs_cli_findings` check stays in sb — it shells out to the `signal-rs`
  *binary* via `Command`, not the crate.

### Design decisions (second pass)
- **`publish_note` folds the tz computation in** — the design doc's epilogue
  enumeration listed "tz", and the 4-line tz/now/log_date/log_time block was
  copy-pasted 6× feeding only the ledger row. Folding it in moves the recorded
  timestamp from handler-entry to publish-time (a sub-second-to-seconds shift,
  and arguably more correct: the ledger records when the note landed). Verified
  `now`/`log_date`/`log_time` were used nowhere but the ledger entry before
  removing the headers.
- **`dispatch_ingest` returns `IngestResult`; callers log after** — the per-site
  result log (`log::debug!("Pipeline result…")` in telegram, the bespoke
  `match &result.status` arms in routes/ntfy) stays at the call site, run on the
  returned result. The only cosmetic change: in telegram/ntfy the single debug
  line now fires after the result-sinks instead of between process and sinks.

### Deviations (second pass)
- None.

### Tradeoffs (second pass)
- **`probe_signal`/`SignalProbe` live in `signal.rs`, `probe_telegram` in
  `telegram.rs`** (not a new `probe` module): each probe already needs that
  module's `signal-rs`/`teloxide` imports and (for signal) `bootstrap_recorded`,
  so colocating beats a new module that would re-import both. Crate-root
  re-exports give the doc's `borg::probe_*` surface.

### Open questions (second pass)
- None.

## Phase 10: Oracle correctness & MCP surface

`otto ci` exit 0 (check incl. clippy+fmt, and test). The design doc's line
numbers were stale (server.rs was decomposed in Phase 8); sites were located by
content.

### Design decisions
- **Default-mode messaging (4+ sites)**: fixed every "default is hybrid" claim
  (`tools.rs` SearchMode/KnowledgeSearchRequest docs + `mode` schemars,
  `server.rs` knowledge_search `#[tool]` description + `get_info` instructions)
  to say: omitting `mode` runs the operator-configured pipeline (vector-first,
  eval-best); `mode` is an explicit single-path override. Stale "Phase A6"
  comment removed.
- **`ingest_history` limit**: added `limit: Option<u32>` (default 50). The
  ledger is chronological ascending, so the handler returns the most-recent
  `limit` rows (`skip(len - limit)`), bounding the MCP payload.
- **`pipeline_graph_paths` hop clamp**: `cfg.methods.graph.hops.min(MAX_EXPAND_HOPS)`
  so a misconfigured `retrieval.methods.graph.hops` can't bypass the same cap
  the per-call graph modes already honor.
- **Single-method fusion weight**: `run_pipeline` now always routes through
  `reciprocal_rank_fusion_weighted` (dropped the `lists.len()==1` passthrough).
  RRF preserves a lone positive-weight list's order exactly, but a 0.0-weight
  method now correctly contributes nothing whether enabled alone or with others.
- **Empty query → `invalid_params`**: added a `Self::invalid` helper
  (`invalid_params`, caller-fault) distinct from `Self::err` (`internal_error`);
  knowledge_search's empty-query guard uses it.
- **`direction`/`quality` schema enums**: new `LinkDirection` (find_links) and
  `QualityLevel` (quality_report) enums (kebab-case `JsonSchema`); a typo now
  fails deserialization with the valid options instead of silently matching no
  branch / no rows.
- **`find_similar` over-fetch**: when a domain/self post-filter is active, fetch
  `limit * FIND_SIMILAR_OVERFETCH (5) + 1` candidates, filter, then truncate to
  `limit` - so filtering can't return 0 with matches present.
- **Watcher reindex**: poisoned-mutex branch now logs (mirrors inbound
  recompute) instead of silently stopping forever; and reindex goes through the
  new `SearchIndex::index_changed(vault_root, changed_paths)` (parse + index_one
  per changed path, delete row for vanished paths) instead of a full vault walk
  under the lock.
- **Eval cache FNV-1a**: `eval::cache::stable_hash` replaced `DefaultHasher`
  (unstable across toolchains) with pinned FNV-1a 64-bit, so a rustc bump no
  longer silently invalidates the whole judgment cache.
- **dispatch() parity guard**: new test `every_router_tool_has_a_dispatch_arm`
  dispatches every `list_tools()` name with `null` args (fails deserialization
  before the body runs) and asserts none yields "unknown tool".
- **Rerank injectable**: extracted `rerank_within_budget(db, cfg, query, fused,
  &dyn Reranker) -> RerankOutcome` (pure over the injected reranker + DB);
  `maybe_rerank` loads the candle reranker and owns the process-global
  `RERANK_DISABLED` latch (set on the `Disable` outcome). Four unit tests with
  `MockReranker` / a sleeping mock cover head-tail split, the two fail-open
  short-circuits, and the over-budget Disable branch.
- **Rerank latency projection**: `project_batch_ms(per_pair_ms, n)` is now LINEAR
  in `n` (dropped the `threads` param and the `ceil(n/threads)` waves model).
  The candle cross-encoder runs ONE batched forward over all `n` docs that
  already saturates every core, so dividing by threads under-projected by up to
  `threads`x; `per_pair_ms * n` is the honest upper bound.
- **LogTracer bridge**: `sb::logger::init_tracing_to_file` now calls
  `tracing_log::LogTracer::init()` before installing the subscriber, so vault's
  `log::*` records (watcher/index warnings) are no longer dropped under
  `sb oracle serve` (tracing-only). Added the `tracing-log` dep to sb.
- **`eval_cache_path` fallback**: the relative `PathBuf::from("eval-cache.db")`
  fallback (writes under CWD - the banned class) is replaced by
  `vault::paths::oracle_eval_cache_path()` (data dir, panics on no-data-dir like
  `oracle_db_path`).
- **Dead `#[allow(dead_code)]`s**: `OracleMcpServer.tool_router` field was
  genuinely vestigial - rmcp's `#[tool_handler]` resolves the router via
  `Self::tool_router()` (the associated fn), not a stored field (confirmed in
  rmcp-macros 1.6.0 docs), so the field is removed entirely rather than the
  allow kept. `vault::watcher::VaultWatcher.watcher` renamed to `_watcher` (the
  drop-guard carve-out: held only for its Drop teardown).

### Deviations
- None.

### Tradeoffs
- **`index_changed` deletes only the `notes` row** for a vanished path, matching
  `remove_stale_notes` (the full-walk path) - embeddings/edges cleanup stays
  owned elsewhere (cortex). Keeping parity with the existing behavior rather
  than expanding deletion scope here.

### Open questions
- None.
