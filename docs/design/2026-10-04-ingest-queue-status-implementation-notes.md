# Implementation Notes: Ingest Queue Status

Design doc: `docs/design/2026-10-04-ingest-queue-status.md`

## Phase 0: Prove lappy reaches the daemon

### Design decisions
- Ran over ssh from desk (`ssh ltl-7007.lan "curl -s -o /dev/null -w '%{http_code}\n' http://desk.lan:8181/health/audit"`), so the curl executed on lappy itself. Observed 2026-10-04: `200`.

### Deviations
None.

### Tradeoffs
None.

### Open questions
None.

## Phase 1: `started_at` column (receipts v5)

### Design decisions
- `renew_lease` takes an explicit `started_at: &str` and stamps it via `started_at=COALESCE(started_at, ?)` — `borg/src/receipts.rs:renew_lease` — keeps the receipts layer clock-free (callers inject time, like `list_stale`); `TraceLeaseGuard::renew` derives `lease_until` and `started_at` from one `Utc::now()` so they cannot disagree.
- v5 `ALTER TABLE ... ADD COLUMN started_at` is `has_column`-guarded and placed after the v4 lease block, i.e. after the v3 rebuild — `borg/src/receipts.rs:run_migrations`.
- `TIMESTAMP_FMT` is `pub`; `pipeline/permits.rs:lease_until` and `dedupe.rs:effective_timestamp` use it.

### Deviations
- The design says "fold into `renew_lease`" without a signature; the extra `started_at` parameter is the correct seam (same effect). The two existing test call sites were updated to pass it.

### Tradeoffs
- Explicit `started_at` parameter vs. calling `Utc::now()` inside `renew_lease` — injected time is testable and matches the repo's `now`-injection convention.

### Open questions
None.

## Phase 2: Shared config + snapshot contract and logic

### Design decisions
- `load_config` + `Normalize` moved verbatim into `vault/src/config.rs`; the CWD fallback name is the `BORG_FALLBACK_CONFIG = "borg.yml"` const (borg's `APP_NAME` stays in borg for the desktop/syslog appname). borg re-exports both from `borg::config`, so every `borg::config::load_config` call site is untouched.
- `HotkeyConfig` and `client_auth_token` live in the new `vault/src/daemon.rs`, re-exported from `borg::config`.
- Wire types in `vault/src/queue.rs`. `ItemState` has no `Succeeded` variant (succeeded items are counted, never listed); borg's private `Partition` enum carries the five-way partition. Failed items carry `failure-stage`/`failure-reason` only and in-flight items carry `received`/`started`/`age-secs` only, per the API Design example. `failure-stage` is typed `vault::receipts::FailureStage` (already kebab-serialized).
- `borg::queue::snapshot` returns `Option<QueueSnapshot>`: `None` only when `?batch=<id>` names no batch (Phase 3 maps it to 404). Plain calls always return `Some`.
- Batch merge: rows sorted by `(received_at, trace_id)`, joined while `received_at - running_group_end <= batch-gap`. Batch id matches only the earliest member's trace id. Idle `elapsed-secs` and draining `elapsed-secs` are one expression (`max interval end - started`), because an in-flight row's interval ends at `now`.
- Wedged is strictly `age > wedged-after` (equal is still queued/processing), per the partition table; pinned by `snapshot_wedged_boundary_is_strictly_greater`.
- `load` binds `Method::Harvest.as_str()`, `ReceiptStatus::Rejected.as_str()` and `ReceiptStatus::Received.as_str()`; no status/method literals. An unparseable timestamp, status or failure stage is an error from `load` (fail closed), never a dropped row that could read as idle.
- `source` and `failure-reason` truncated with `vault::text::truncate` (200 chars exactly, no ellipsis), char-safe.
- `QueueConfig` (with `DEFAULT_QUEUE_WEDGED_AFTER` 15m, `DEFAULT_QUEUE_BATCH_GAP` 2m) is in `borg/src/config.rs` as `Config.queue`, since it needs `humantime`, which vault does not depend on. `serialize_humantime` added because `Config` derives `Serialize`.
- Module-map entries for `vault/src/daemon.rs`, `vault/src/queue.rs` and `borg/src/queue.rs` added to `vault/AGENTS.md` / `borg/AGENTS.md`: `otto ci`'s `agents-map` lint fails on any undocumented module. Fuller docs remain Phase 6.

### Deviations
- `borg::config::resolve_client_auth_token(&ServerConfig)` is removed rather than re-exported: vault has no `ServerConfig`, so the shared resolver is `vault::daemon::client_auth_token(auth_token: Option<&str>)` (the shape oracle's view struct needs). Its four call sites (two in `borg/src/lib.rs`, two in `borg/src/replay.rs`) now pass `config.server.auth_token.as_deref()`. Same effect, correct seam.
- `deserialize_humantime` takes the key name: `deserialize_humantime(deserializer, key)`, wrapped by per-field `deserialize_wedged_after` / `deserialize_batch_gap`. Observed: with a plain `deserialize_with` helper, serde_yaml's error path stops at the enclosing map (`queue: invalid duration "soonish" ...`), which fails the "naming `wedged-after`" criterion. The message now carries `wedged-after: invalid duration "soonish" (humantime, e.g. 15m): ...`; `queue_config_rejects_bad_duration` asserts both the key and the bad value.
- `rejected` rows are excluded in the `load` query (`status != 'rejected'`), a filter the doc does not list. `rejected` is the harvest selection gate's status (`vault::receipts::ReceiptStatus::Rejected` doc), harvest is already excluded, and the partition table has no `rejected` state; without the filter a stray non-harvest `rejected` row would have no partition. `parse_row` errors if one ever reaches it.
- `snapshot_batch_is_stable_and_ignores_harvest` drives `load` against an in-memory receipts DB rather than calling `snapshot` directly: the harvest filter and the 24h/in-flight window live in the SQL, so only a `load`-level test can prove "100 harvest rows change nothing" and "a 30h-old received row is visible".

### Tradeoffs
- Harvest/rejected/window filtering in SQL only (snapshot trusts its input) vs. re-filtering in `snapshot`: one place for each rule; the `load`-level test covers the SQL.
- `Option<QueueSnapshot>` for "unknown batch" vs. a typed error enum: one absent case, no other failure mode in a pure function.
- Per-field humantime wrappers vs. a `try_from` shadow struct: keeps `Duration` fields with the doc's `deserialize_with` mechanism; two three-line wrappers.

### Open questions
None.

## Phase 3: `GET /queue`

### Design decisions
- Route `/queue` sits in the `protected` group of `build_router` (`borg/src/lib.rs`), so `require_auth` gates it exactly like `/trace`.
- Handler `routes::queue` (`borg/src/routes.rs`) takes `Query<QueueParams { batch }>`, opens the receipts DB and runs `queue::load` inside one `spawn_blocking` (watchdog precedent). `Config.queue` is `Copy`, so it is copied into the closure.
- Three outcomes, no fourth: `Ok(Some)` -> 200 snapshot; `Ok(None)` -> 404 `{"error":"unknown batch <id>"}`; `Err` (DB open, query, parse, or a `spawn_blocking` join failure) -> 500 `{"error":"..."}`. No `unwrap_or_default()` anywhere, so a DB failure can never read as `idle`.
- Entry and exit DEBUG logs on the handler (batch param, resulting state / 404 / error at ERROR).
- Route tests live in `borg/src/tests.rs` beside the other tower `oneshot` tests. They redirect the receipts DB by setting `XDG_DATA_HOME` to a tempdir under `harvest::TEST_XDG_LOCK` (the repo's existing env-serialization lock); the 500 test points `XDG_DATA_HOME` at a regular file so the receipts directory cannot be created.
- `borg/AGENTS.md` endpoint list updated (the Phase 2 module map already lists `queue.rs`).

### Deviations
- Added `queue_route_idle_is_exactly_idle` (empty DB -> body is exactly `{"state":"idle"}` over HTTP), beyond the three named criteria tests; it pins the wire form the Phase 4 `sb borg queue` acceptance relies on.
- The 404 for an unknown batch carries a JSON `{"error": ...}` body rather than an empty body; the doc only specifies the status. Same shape as the 500, so clients parse one error form.

### Tradeoffs
- Opening the DB via `receipts::open_default()` inside the handler (as `trace_state` does) vs. holding a pooled connection in `AppState`: matches the sibling route and keeps `AppState` unchanged; a per-request open is negligible at 2s polling.
- Env-var (`XDG_DATA_HOME`) test seam vs. adding a DB-path field to `AppState`: the env seam is the repo's established pattern for receipts-backed tests and avoids production-struct churn for test purposes.

### Open questions
None.

## Phase 4: `sb borg queue` + output helper

### Design decisions
- `borg::queue::fetch(config, batch, timeout) -> Result<QueueSnapshot, FetchError>` — `borg/src/queue.rs`. `FetchError` is a `thiserror` enum (`PredatesQueue`, `UnknownBatch`, `Unauthorized`, `Unreachable`, `Http`, `Parse`, `BadAddress`) so Phase 5's `wait` can branch on cause: plain-`/queue` 404 is "daemon at <host:port> predates /queue; run otto deploy there", `?batch=` 404 is `UnknownBatch`. Every message names `host:port`.
- `timeout` is reqwest's per-request timeout (covers connect, headers and body); the caller owns the overall deadline. Mutation check: removing `.timeout()` fails `fetch_honours_the_explicit_request_timeout`.
- Connect errors map as in `replay.rs` (the address is named), but as a typed variant rather than a context string.
- The query string is built with `reqwest::Url::query_pairs_mut` (reqwest's `query` feature is not enabled in borg; adding a feature for one pair was not worth it), so a batch id is always percent-encoded.
- `sb/src/cli/output.rs`: `Format` (`ValueEnum`, kebab-case, `ignore_case = true` on the arg), pure `resolve_format(explicit, tty)`, pure `render(&T, Format)`, thin `emit(&T, Option<Format>)`. yaml is `serde_yaml` (idle is exactly `state: idle\n`); json is compact one line + newline.
- `sb borg queue [--format yaml|json]`: one `fetch` (10s request timeout), `emit`; any error propagates through the eyre hook, exit 1, message on stderr.
- `sb/AGENTS.md` and `borg/AGENTS.md` module/command maps updated (`agents-map` lint).

### Deviations
- Criteria say "after `otto deploy` on desk". Not run (restarts live daemons; gated on finalization). Proven instead without deploying: `fetch` tests against a stub axum daemon (200 idle, 200 draining + batch param, plain 404, batch 404, 401 then 200 with bearer, 500, connect refused, request timeout, garbage body), `emit`/format tests, the freshly built `sb borg queue` against the real deployed daemon (observed: `daemon at desk.lan:8181 predates /queue; run otto deploy there`, exit 1), and the same binary against a local python stub (`--format yaml` prints exactly `state: idle\n`, piped output is `{"state":"idle"}` and `jq -e .state` exits 0).
- The real-daemon run needed the sandbox off: the sandbox blocks connections to the host's own addresses (desk.lan resolves to this machine), which surfaced as an HTTP 403 from the sandbox proxy.
- `cargo fmt` also reflowed four statements in `borg/src/tests.rs` (Phase 3's `queue_route_*` tests) that the format check flagged; folded into this commit because `otto ci` cannot be green otherwise. No behaviour change.
- `fetch` returns a typed error, not `eyre::Result`; the doc gives no return type and Phase 5 needs the distinction. Callers `?` it into eyre.

### Tradeoffs
- Typed `FetchError` vs `eyre` strings: tiny enum, lets `wait` tell unknown-batch / old-daemon / unreachable apart without string matching.
- Tests use a real axum stub on an ephemeral port vs mocking reqwest: exercises the actual wire, status handling and bearer header.

### Open questions
- Live post-deploy criteria are PENDING DEPLOY (not passed): `sb borg queue | jq -e .state` exits 0 and `sb borg queue --format yaml` while idle prints exactly `state: idle`, both against the redeployed desk daemon. Needs the user's finalization approval for `otto deploy`.

## Phase 5: `sb borg wait` + typed exit codes

### Design decisions
- Poll loop and exit table live in `sb/src/cli/borg/wait.rs` (`wait::run`, `wait::decide`). `decide(&QueueSnapshot, deadline_reached) -> Verdict` is pure: any `wedged` item -> 4; `state: idle` -> 0 if `batch.failed` is 0 (or no batch) else 3; draining past the deadline -> 5; else keep waiting. Wedged and idle outrank the deadline, so an answer that lands exactly at the deadline is reported for what it says. Unit-tested per table row in `wait/tests.rs`.
- `wait::run` pins `batch.id` from the first plain `/queue`, then polls `?batch=<pinned>` every 2s (`POLL_INTERVAL`). One `std::time::Instant` deadline; each request gets `min(REQUEST_TIMEOUT = 10s, time left)` and the inter-poll sleep is `min(2s, time left)`. A fetch error with `now >= deadline` is exit 5 (the request could only time out at the deadline because of the `min`); a fetch error before it propagates (exit 1). Mutation check: dropping the `min` makes the never-responds stub test take 11.5s and fail.
- `decide` runs on the first plain response too, so a batch already wedged at start exits 4 on the first read rather than one poll later (same batch, same outcome, sooner).
- A `draining` answer with no `batch` to pin is a loud error (exit 1), never a silent plain-`/queue` poll that could follow a different batch.
- `ExitWith(u8)` in `sb/src/error.rs` plus `error::exit_code(&Report) -> Option<u8>` (`ExitWith(n)` -> n, `SilentFailure` -> 1, else `None`); `main.rs`'s one match calls it. Clap's exit 2 is untouched (it exits inside `Cli::parse()`).
- `--timeout` parses with `humantime::parse_duration` (default `60m`); `humantime` added to `sb` via `cargo add`.
- stdout: the final snapshot via `output::emit`, nothing per poll. Errors go through the eyre hook on stderr; DEBUG entry/exit logs per `rules/logging.md`.
- `sb/AGENTS.md`: Wait command entry and an Exit codes invariant.

### Deviations
- Exit 5 when the deadline passes before the daemon EVER answered (the "stub accepts and never responds" criterion): there is no snapshot to print, so stdout is empty and stderr says `timeout <t> reached before the daemon at <host:port> answered`. The doc's "exactly one final snapshot on 0/3/4/5" cannot hold when no snapshot exists; on exit 5 after at least one answer, stdout carries the last snapshot received.
- `sb borg queue` and `sb borg wait` are now inspection verbs in `sb/src/logger.rs::verb_logs_to_file` (stderr-only logging, opt-in to `borg.log` via `cli.yml`). Before, `queue` (Phase 4) fell through to `AlwaysFile`. The doc says wait's logs go to stderr; `queue` changed with it so the two siblings log the same way.
- No test-only poll-interval override: the 2s interval is fixed and the subprocess suite runs in ~8s in parallel, so no seam was added.
- `otto ci` was run with the Bash sandbox off. Inside the sandbox the pre-existing `vault` test `fabric::tests::test_is_available_returns_bool` (spawns `fabric --version`) ran past 60s and the run passed the 600s tool cap; the first sandboxed run had also failed only on `cargo fmt`, since fixed. Unsandboxed: exit 0, all 10 `sb/tests/wait.rs` tests and the 8 new unit tests green.
- `cargo add humantime -p sb` resolved 2.4.0 and re-resolved several Windows-only `windows-sys` lockfile entries (no Linux build impact).

### Tradeoffs
- Fixed 2s poll in tests vs a hidden `--poll-interval` flag: no production surface exists only for tests; suite stays fast enough.
- `wait::run` returns `eyre::Result<WaitOutcome>` vs a typed error: the only error consumer is `main` (exit 1 + message), so a typed enum would add nothing; the deadline/no-deadline split happens inside `run`.
- Stub daemon is a hand-rolled `std::net` HTTP responder in `sb/tests/wait.rs` vs adding axum as an sb dev-dep: no new dependency, and it records request targets so the pinned `?batch=` polling is asserted directly.

### Open questions
None.

## Phase 6: oracle `ingest_queue` + docs

### Design decisions
- `oracle/src/queue.rs` holds the client: `BorgView { hotkey: HotkeyConfig, server: ServerView { auth_token } }` (no `deny_unknown_fields`; the rest of borg.yml is borg's), `load_view` through `vault::config::load_config` (a no-op `Normalize` impl), and `fetch(view, timeout)`. The tool body is `load_view(None)` then `fetch(.., REQUEST_TIMEOUT = 10s)`, returning the daemon JSON verbatim via `Content::json`. Registered in both `#[tool_router]` and the manual `dispatch` match.
- Every failure mode is an eyre error naming `host:port`: connect/timeout ("unreachable"), plain 404 ("predates /queue; run otto deploy there", same wording as `borg::queue::fetch`), other non-2xx (status + 200-char body preview), unparseable body. No path returns idle.
- `IngestQueueRequest {}` carries `deny_unknown_fields` like every oracle request type, so a stale client sending `batch` gets a loud error rather than a silently ignored filter.
- `reqwest` added with `cargo add reqwest -p oracle --no-default-features --features json,rustls` (borg's TLS shape; avoids a native-tls/openssl pull). Cargo resolved the lockfile entry to 0.13.5 and added `base64`.
- Docs: `CLAUDE.md` (new "Ingest-queue status" paragraph in the borg section), `borg/AGENTS.md` (invariant 8), `oracle/AGENTS.md` (tool list, invariant, module map), `config/templates/borg.yml.example` (`queue:` block copied from the doc; the existing `bootstrap` test that parses the template as `borg::config::Config` covers it). `sb/AGENTS.md` was already done in Phases 4/5.
- Tests: 9 in `oracle/src/queue/tests.rs` against a hand-rolled tokio stub (200 verbatim, bearer sent, closed port, 500, 404, garbage body, silent daemon hits the timeout, `load_view` happy + missing-file), plus 2 in `server/tests.rs` (registered with the exact `sb borg wait` description text; unknown argument rejected). `ingest_queue` added to `NON_NOTE_TOOLS` in the trace-block classification test. Mutation check: making `fetch` return `{"state":"idle"}` on a send error failed `closed_port_is_an_error_naming_the_address_not_idle` and `silent_daemon_hits_the_request_timeout`.

### Deviations
- Closed-port and success criteria proven with the freshly built `target/debug/sb` against a temp `XDG_CONFIG_HOME` (borg.yml with `hotkey.port` on a closed port, then a python stub daemon) rather than a live daemon. Observed closed port: `Error: borg daemon at 127.0.0.1:37489 unreachable: ...`, exit 1. Stub: the JSON snapshot, exit 0.
- No server-level dispatch test for the closed-port case: `load_view(None)` reads the global config chain, and an env-var seam in a unit test would race other tests. The `fetch` tests carry the contract and the binary run proves the wiring.
- The tool description's blocking hint is lowercase mid-sentence ("; for blocking until a batch drains, run `sb borg wait` in the background instead of polling this") so it contains the doc's exact string.

### Tradeoffs
- A small oracle-local `fetch` vs depending on `borg::queue::fetch`: oracle stays independent of borg (the doc says oracle shares only the vault contract); the duplicated surface is ~30 lines and the shared parts (address type, token resolver, loader, snapshot type) are already in vault.
- Returning `serde_json::Value` verbatim vs deserializing into `vault::queue::QueueSnapshot` and re-serializing: the doc says "return the JSON", and verbatim cannot drop a field a newer daemon adds.

### Open questions
- PENDING DEPLOY: "with one `sb borg ingest <url>` in flight, `sb oracle call ingest_queue | jq .batch.id` equals `sb borg queue --format json | jq .batch.id`", and all live acceptance criteria (`curl desk.lan:8181/queue`, receipts `schema_version` 5, `sb borg queue --format yaml` idle) need the redeployed desk daemon. Not run: `otto deploy`/bump/push are the parent's finalization.

## Implementation audit round 1 (review-panel, synthesis `/tmp/review-panel/dphauOyg/synthesis.md`)

### Design decisions
- Must-fix 1, config fail-open (`vault/src/config.rs:load_first_existing`): the first EXISTING candidate (`~/.config/sb/borg.yml`, then `./borg.yml`) is the config, and a parse error there is a loud error naming the file; defaults apply only when no candidate exists. Before, a parse error warned and fell back, so `wedged-after: soonish` silently ran the client (and the daemon, whose `ExecStart` has no `--config`) on defaults, breaking the doc's "fails the YAML load with the key named". Live desk `borg.yml` parses under the new binary (Phase 4's run resolved `desk.lan:8181` from it). Tests: `load_first_existing_bad_yaml_errors_instead_of_falling_back`, `load_first_existing_skips_missing_and_defaults_when_none_exist`. Folded into the Phase 2 commit.
- Must-fix 2, batch id instability (`borg/src/queue.rs:snapshot`): `?batch=<id>` matches the batch CONTAINING that trace, not only the batch whose first member it is. A same-second arrival whose trace id sorts lower became the first member and the pinned lookup returned None (route 404, `wait` exit 1 mid-drain). The reported `batch.id` is still the earliest member. Test: `snapshot_pinned_id_survives_same_second_arrival_sorting_ahead` (fails against the first-member lookup, verified). `snapshot_unknown_batch_id_is_none`'s "only the earliest member names a batch" assert is rewritten to the membership rule. Folded into the Phase 2 commit.

### Deviations
- Cheap-win 3: a finished row with no `terminal_at` ends its interval at `received_at` (`QueueRow::interval_end`), a gap the doc's "terminal_at or now" did not cover; `now` would hold a batch open forever. Doc amended to say so (desk has 0 such rows).
- Cheap-win 4: doc's "exactly one snapshot on exits 0/3/4/5" amended to carve out exit 5 before any answer (empty stdout), matching Phase 5's disclosed behaviour.

### Tradeoffs
- Membership lookup vs. sub-second timestamps or an arrival-sequence tiebreak: membership needs no schema change and is a strict superset of the old lookup.

### Open questions
None.
