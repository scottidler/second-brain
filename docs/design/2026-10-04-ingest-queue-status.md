# Design Document: Ingest Queue Status

**Author:** Scott Idler
**Date:** 2026-10-04
**Status:** Implemented
**Review Passes Completed:** 5/5 (draft, correctness, clarity, edge cases, excellence; run 2026-10-04)

## Summary

Add one agent-friendly answer to "is borg still chewing on what I just sent it?". The daemon gets a read-only `GET /queue` that reports the current ingest batch (done, remaining, elapsed, wedged items). `sb borg queue` prints it, `sb borg wait` blocks on it and exits with a code an agent can branch on, and oracle exposes it as the `ingest_queue` MCP tool. All three go through the daemon, so they answer the same on desk and lappy.

## Problem Statement

### Background

- Scott clicks 5-15 YouTube videos in quick succession; the Firefox extension POSTs each to the borg daemon on desk (`http://desk.lan:8181/ingest`).
- They drain through the pipeline over minutes. Measured on desk's receipts DB (last 60 days, non-harvest bursts of 5+ receipts each within 120s of the previous):

  ```
  start                | n   | yt | method | arrival_span_s | first_recv->last_terminal_s | failed
  2026-08-30T17:41:00Z | 25  | 25 | http   | 80             | 430                         | 0
  2026-09-19T21:19:49Z | 6   | 4  | http   | 16             | 195                         | 0
  2026-09-26T02:28:33Z | 12  | 12 | http   | 34             | 344                         | 1
  2026-09-29T03:49:18Z | 7   | 7  | http   | 149            | 273                         | 1
  2026-09-29T05:54:14Z | 8   | 8  | http   | 25             | 295                         | 0
  2026-09-30T23:17:40Z | 5   | 5  | http   | 18             | 183                         | 0
  2026-10-03T01:55:40Z | 14  | 14 | cli    | 42             | 579                         | 0
  2026-10-04T21:59:43Z | 6   | 6  | http   | 36             | 305                         | 0
  ```

- He then wants to tell an agent "once everything is ingested, do X". Today the agent has nothing to ask.

### Problem

Nothing answers "is a backlog draining, and is any of it stuck":

- `GET /health/audit` (`borg/src/routes.rs:83-98`, `borg/src/triage.rs:26-45`): lifetime and 24h counts, no notion of a current batch.
- `GET /trace/{id}` (`borg/src/routes.rs:211-243`): one trace, and the agent does not know the trace ids.
- `sb borg log` (`sb/src/cli/borg.rs:492-509`): reads the LOCAL receipts DB, so on lappy (client-only, no DB) it shows nothing.
- oracle `inbox_status` (`oracle/src/server.rs:990`): vault notes needing review, unrelated to ingest.
- The receipts row cannot tell queued from processing: `lease_until` is written at trace entry and renewed once at the general permit grant (`borg/src/pipeline.rs:196-222`), and nothing records when work began.
- A wedged trace stays invisible until the watchdog reaps it to `failed/crashed` at `hard_timeout_secs + 60` = 1860s (`borg/src/watchdog.rs:28,40`).

### Goals

- One snapshot that says `idle`, or: batch start, elapsed, done, remaining, and which items are queued | processing | wedged | failed. (Scott, this session)
- `idle` means nothing else is printed. (Scott: "it would report nothing queued/ingesting if there wasnt anything")
- Wedged = in flight longer than a configured threshold, reported before the watchdog reaps it. (Scott)
- A blocking `wait` an agent can run in the background and branch on by exit code, no output parsing required. (Scott: "make this new interface EASY for it to use and understand")
- Same answer from desk and lappy. (Scott chose Option B, this session)

### Non-Goals

- **ETA / time-remaining prediction.** Excluded: not requested.
- **Push notifications on drain** (ntfy, desktop toast). Excluded: `wait` plus the agent's background-task wakeup covers the use case.
- **Fixing the existing lease gap** where a trace queued for a general permit longer than 1860s is reaped while still queued (`borg/src/receipts.rs:597-618`). Excluded: pre-existing, never observed (longest measured burst drain 860s). Recorded in Risks.
- **Fixing `replay::poll_trace_terminal` not handling `status: "rejected"`** (`borg/src/replay.rs:303-320`). Excluded: separate bug, found during research.
- **Harvest visibility.** Excluded by Scott 2026-10-04 (see Resolved Decisions). Parked: revisit if he wants to watch harvest progress.
- **Retrofitting `emit` / `--format` onto other `sb` commands.** Excluded: not requested.

## Proposed Solution

### Overview

```
Firefox / cli / telegram ──POST /ingest──> borg daemon (desk) ──> receipts.db
                                               │
                              GET /queue[?batch=<id>] (computed on desk)
                                               │
              ┌────────────────────────────────┼─────────────────────────────┐
        sb borg queue                     sb borg wait                 oracle ingest_queue
     (desk or lappy, yaml|json)       (polls, exits 0|3|4|5)          (MCP, user-scoped)
```

- The snapshot is computed in ONE place: `borg::queue::snapshot` on the daemon host. Every client renders the same struct.
- Clients find the daemon through the existing `hotkey.host` / `hotkey.port` in `borg.yml` (`borg/src/config.rs:1173-1187`; live: `desk.lan:8181`), the same address `reingest` and `replay` already use (`borg/src/lib.rs:813-930`, `borg/src/replay.rs:249-330`).
- **Caller contract:** `wait` answers for work already submitted. Call it after the last click (or the last `sb borg ingest`) has returned, not before.

### Architecture

| Piece | Crate / path | Role |
|---|---|---|
| `started_at` column | `borg/src/receipts.rs`, `borg/src/receipts/schema.sql` | When the trace got its general permit |
| `QueueSnapshot` + item types | `vault/src/queue.rs` (new) | Wire contract shared by borg (server + CLI client) and oracle |
| `HotkeyConfig`, `client_auth_token` | move `borg/src/config.rs` -> `vault/src/daemon.rs` | One daemon-address type and one token resolver (on `vault::config::resolve_secret`, `vault/src/config.rs:57`) |
| `load_config::<T>` + `Normalize` | move `borg/src/config.rs:30-76` -> `vault/src/config.rs` | One borg.yml loading chain (explicit -> `~/.config/sb/borg.yml` -> `./borg.yml` -> defaults) for borg and oracle |
| `queue::snapshot(rows, now, cfg, batch)` | `borg/src/queue.rs` (new) | Pure batch/partition logic |
| `queue::load(conn, now, cfg, batch)` | `borg/src/queue.rs` | SQL read + `snapshot` |
| `GET /queue` | `borg/src/routes.rs`, `borg/src/lib.rs` router (`protected` group) | `spawn_blocking` around `load` |
| `queue::fetch(config, batch, timeout)` | `borg/src/queue.rs` | HTTP client with explicit per-request timeout |
| `sb borg queue`, `sb borg wait` | `sb/src/cli/borg.rs` | Render / poll |
| `emit(&T, Option<Format>)` | `sb/src/cli/output.rs` (new) | TTY yaml, piped json, `--format` override |
| `ExitWith(u8)` | `sb/src/error.rs`, `sb/src/main.rs:28-35` | Typed exit code, decided in main's one match |
| `ingest_queue` tool | `oracle/src/server.rs`, `oracle/src/tools.rs` | GET /queue, return JSON verbatim |

### Data Model

**Receipts v5** (`SCHEMA_VERSION` 4 -> 5, `borg/src/receipts.rs:41`):

- `started_at TEXT DEFAULT NULL`, `TIMESTAMP_FMT` (`%Y-%m-%dT%H:%M:%SZ`).
- Added to `schema.sql` (fresh DBs) AND as a `has_column`-guarded `ALTER TABLE ... ADD COLUMN` placed AFTER the v3 rebuild block (`receipts.rs:208-238`), next to the v4 lease columns (`:247-260`). The v3 block's fixed 12-column `INSERT...SELECT` would drop a column added before it.
- Stamped inside `renew_lease`'s existing UPDATE: `SET lease_until=?, started_at=COALESCE(started_at, ?) WHERE trace_id=? AND status='received'`. `renew_lease` is called exactly once, at the general permit grant (`pipeline.rs:214-222`), which every trace (light and heavy, daemon and harvest via `harvest/publish.rs:203`) passes through.
- `TraceLeaseGuard::renew()` warns and continues if that write fails (`pipeline/permits.rs:170-182`). That stays: the wedged rule below ages a NULL-`started_at` row from `received_at`, so a missed stamp cannot hide a stuck row.
- Never stamped on a terminal row (same `status='received'` guard). Never cleared at terminal time; it is history, not liveness.
- `TIMESTAMP_FMT` gets one shared definition; `pipeline/permits.rs:109` and `dedupe.rs:331` duplicate the literal today and switch to it.
- Downgrade: an older binary ignores the extra column; no action.

**Item partition.** For in-flight rows, `age = now - COALESCE(started_at, received_at)`. Every row in the batch is in exactly one state:

| State | Rule |
|---|---|
| `queued` | `status='received' AND started_at IS NULL AND age <= wedged-after` |
| `processing` | `status='received' AND started_at IS NOT NULL AND age <= wedged-after` |
| `wedged` | `status='received' AND age > wedged-after` |
| `succeeded` | `status='succeeded'` |
| `failed` | `status='failed'` (includes watchdog `crashed`) |

- `wedged` means "in flight past the age threshold", not proof of a deadlock. `processing` includes time spent waiting inside a handler for a HEAVY permit (see Resolved Decisions).
- Queued rows age from `received_at`, so rows stranded with a NULL `started_at` (failed stamp, daemon restart before grant) still reach `wedged`. Measured worst per-item receipt->terminal, permit waits included, is 537s, under the 15m default.

**Batch.**

- Candidate rows: `method != ?` bound to `vault::schema::Method::Harvest.as_str()` (`vault/src/schema.rs:402`, never a literal) `AND (status = 'received' OR received_at >= now - 24h)`. In-flight rows are visible at any age; the 24h bound only trims finished history (indexes `idx_receipts_status`, `idx_receipts_received_at`).
- Each row is an interval `[received_at, terminal_at or now]`. A finished row with no `terminal_at` (none exist on desk today) ends at `received_at`, never `now`, so it cannot hold a batch open (implementation audit round 1).
- Sort by `received_at`; merge intervals whose gap is `<= queue.batch-gap`. Each merged group is a batch.
- **Batch id** = trace id of the batch's earliest-received row. Stable: later rows only append, finished rows stay members.
- Plain `/queue`: the batch containing the newest in-flight row. No in-flight rows -> `state: idle`, nothing else.
- `/queue?batch=<id>`: the batch containing trace `<id>`, whether in flight or finished. Matched by membership, not first member: timestamps are 1s and ties sort by trace id, so a later same-second arrival can become the earliest member (16 same-second pairs on desk in 60 days); the pinned id must still resolve (implementation audit round 1). `state` is `draining` if that batch has in-flight rows, else `idle`, and `batch` + `items` are always present. Unknown id (aged past the 24h bound or never existed) -> 404.
- Why intervals and not "received_at >= earliest in-flight received_at": that anchor moves forward as early rows finish, so `done` would shrink and the first-finished videos of the burst would drop out.

**Config** (`borg.yml`, new `queue:` block). Durations are humantime strings deserialized straight into `std::time::Duration` by a `deserialize_with = "deserialize_humantime"` helper in `borg/src/config.rs` (on the existing `humantime` dep, `borg/Cargo.toml:48`). A bad value fails the YAML load itself with the key named; nothing downstream re-parses a string. This holds on every load path: an existing `borg.yml` (primary or `./borg.yml`) that fails to parse is a loud error, not a warning plus defaults (`vault::config::load_first_existing`; implementation audit round 1, since the daemon's `ExecStart` passes no `--config`).

```yaml
queue:
  # In flight longer than this (since permit grant, or since receipt if never
  # granted) is reported wedged. Measured worst single item (receipt ->
  # terminal, incl. permit wait), last 60 days: 537s (cli youtube). The
  # watchdog reaps at hard-timeout-secs + 60 = 1860s.
  wedged-after: 15m
  # Receipts whose [received, finished] spans are this close join one batch.
  # Covers clicks that arrive after an earlier fast item already finished.
  batch-gap: 2m
```

### API Design

**`GET /queue[?batch=<id>]`**: in the `protected` route group next to `/trace/{id}` (`borg/src/lib.rs:112-127`), so it carries the bearer token when `server.auth-token` is set and passes freely when it is not (`require_auth`, `routes.rs:57-59`). 200 with `QueueSnapshot` JSON.

- DB open or query error -> 500 with `{"error": "..."}`. Never `idle`. (The `health_audit` handler's `unwrap_or_default()` at `routes.rs:96`, which turns a DB error into zero counts, is the pattern NOT to copy.)

**`QueueSnapshot`** (`vault::queue`, `#[serde(rename_all = "kebab-case")]`, `batch` and `items` skipped when absent):

```yaml
state: draining            # idle | draining
batch:
  id: 20261004-215943-9f01 # trace id of the earliest member
  started: 2026-10-04T21:59:43Z
  elapsed-secs: 192        # now - started while draining; last terminal - started when idle
  total: 6
  done: 3                  # succeeded + failed
  remaining: 3             # queued + processing + wedged
  succeeded: 3
  failed: 0
  queued: 1
  processing: 2
  wedged: 0
items:                     # every in-flight or failed item in the batch; succeeded omitted
  - trace: 20261004-215950-ab12
    state: processing      # queued | processing | wedged | failed
    source: https://www.youtube.com/watch?v=...
    received: 2026-10-04T21:59:50Z
    started: 2026-10-04T22:01:02Z
    age-secs: 113
  - trace: 20261004-215944-cd34
    state: failed
    source: https://www.youtube.com/watch?v=...
    failure-stage: fetch-failed
    failure-reason: yt-dlp HTTP 403
```

Plain idle is exactly `state: idle` (yaml) / `{"state":"idle"}` (json).

- `source` is `raw_input`, truncated to 200 chars; `failure-reason` truncated to 200 chars.
- The `done`/`remaining` sums are what Scott asked for; they are computed in the same function as their parts, so they cannot drift.

**`sb borg queue [--format yaml|json]`**: one plain snapshot. yaml on a TTY, json when piped, `--format` overrides. Exit 0 on any snapshot; any error exits 1 with the message on stderr.

**`sb borg wait [--timeout <humantime>] [--format yaml|json]`**:

1. `GET /queue`. `idle` -> print it, exit 0. `draining` -> pin `batch.id`.
2. Every 2s (`replay::POLL_INTERVAL_SECS` precedent, `borg/src/replay.rs:247`): `GET /queue?batch=<pinned>`. Each response is one atomic read of that batch, so a new click joining it extends the same batch and a later separate batch cannot be confused with it.
3. Per response, in order: any item `wedged` -> exit 4; `state: idle` -> exit 0 if `failed == 0`, else 3.
4. One monotonic deadline (`--timeout`, default 60m) covers requests, body reads and sleeps. Each request's timeout is `min(10s, time left)`. Deadline reached -> exit 5. A request failing before the deadline (connect, 4xx/5xx, request timeout) -> exit 1.

| Exit | Meaning |
|---|---|
| 0 | nothing in flight at start, or the pinned batch drained with no failures |
| 1 | `wait` could not get an answer (daemon unreachable, HTTP error, request timeout, old daemon) |
| 2 | clap usage error (bad flag); clap's own exit from `Cli::parse()` at `sb/src/main.rs:21`, untouched |
| 3 | pinned batch drained with `failed > 0` |
| 4 | an item in the pinned batch is `wedged`; exits at once, does not wait for the watchdog |
| 5 | `--timeout` reached while still draining |

- stdout carries exactly one thing on exits 0/3/4/5: the final snapshot, in the chosen format. Sole exception: exit 5 when the deadline passes before the daemon ever answered has no snapshot to print, so stdout is empty and stderr names the address (implementation audit round 1). Nothing is printed per poll. Errors and logs go to stderr, so `sb borg wait --format json | jq` always parses.

**oracle `ingest_queue`** (no params): loads `borg.yml` through `vault::config::load_config` into a view struct holding `hotkey: HotkeyConfig` and `server.auth-token`, resolves the token with `vault::daemon::client_auth_token`, `GET /queue`, returns the JSON. Any error -> MCP error naming the address (never an idle-looking result). Tool description: "for blocking until a batch drains, run `sb borg wait` in the background instead of polling this".

### Implementation Plan

#### Phase 0: Prove lappy reaches the daemon
**Model:** sonnet
- Zero code, run ON lappy: `curl -s -o /dev/null -w '%{http_code}\n' http://desk.lan:8181/health/audit`.
- From desk this returns `200` (observed 2026-10-04); the daemon binds `0.0.0.0:8181`. Desk proves nothing about lappy's route.
- **Success criteria:** on lappy the command prints `200`, not a connect error.

#### Phase 1: `started_at` column (receipts v5)
**Model:** sonnet
- `schema.sql` + guarded ADD COLUMN after the v3 block; `SCHEMA_VERSION = 5`.
- Fold `started_at = COALESCE(started_at, ?)` into `renew_lease`.
- `TIMESTAMP_FMT` becomes `pub` in `receipts.rs`; `pipeline/permits.rs:109` and `dedupe.rs:331` use it (test files may keep the literal).
- Tests copied from `borg/src/receipts/tests.rs:722,737,871`.
- **Success criteria:**
  - `v5_migration_adds_started_at_surviving_v3_rebuild_from_pre_v3_db` passes.
  - `renew_lease_stamps_started_at_once`: two renews keep the first value; a terminal row is never stamped.
  - `rg -c '"%Y-%m-%dT%H:%M:%SZ"' borg/src --glob '!**/tests.rs'` lists only `borg/src/receipts.rs:1`. Observed on main: 3 files (`receipts.rs:50`, `pipeline/permits.rs:109`, `dedupe.rs:331`).

#### Phase 2: Shared config + snapshot contract and logic
**Model:** opus
- Move `HotkeyConfig`, `client_auth_token`, `load_config` + `Normalize` into vault; borg re-exports so call sites do not churn.
- `vault::queue::{QueueSnapshot, BatchSummary, QueueItem, ItemState, QueueState}`.
- `QueueConfig { wedged_after: Duration, batch_gap: Duration }` with the humantime deserializer.
- `borg::queue::snapshot(rows, now, cfg, batch: Option<&str>)` pure; `borg::queue::load(conn, now, cfg, batch)`. Entry/exit DEBUG logs per `rules/logging.md`.
- **Success criteria:**
  - `snapshot_idle_when_nothing_in_flight` serializes to exactly `{"state":"idle"}`; `queue_config_rejects_bad_duration` fails the load naming `wedged-after`.
  - `snapshot_partitions_and_ages`: one row per state; counts and `done`/`remaining` correct; `started_at` 16m old -> `wedged`; `started_at` NULL and `received_at` 16m old -> `wedged`.
  - `snapshot_batch_is_stable_and_ignores_harvest`: an early-finished row stays counted as later rows finish; 100 harvest rows inside the window change nothing; a row whose gap to the batch is 3m is a separate batch; a `received` row 30h old is still visible; `batch=<id>` returns a finished batch with `state: idle` and its counts.

#### Phase 3: `GET /queue`
**Model:** sonnet
- Route in the `protected` group of `build_router` (`borg/src/lib.rs:104-134`), handler in `routes.rs`, DB work in `spawn_blocking` (`watchdog.rs:93-100` precedent), `?batch=` supported, errors -> 500.
- **Success criteria:**
  - `queue_route_returns_snapshot` (tower `oneshot`, `borg/src/tests.rs:18` pattern) gets 200 + parseable `QueueSnapshot`; `?batch=nope` gets 404.
  - `queue_route_requires_token_when_configured` gets 401 without a bearer and 200 with it.
  - `queue_route_db_error_is_500_not_idle`.

#### Phase 4: `sb borg queue` + output helper
**Model:** sonnet
- `borg::queue::fetch(config, batch, timeout)`: `hotkey.host/port`, bearer via `client_auth_token`, explicit request timeout, connect-error mapping as `replay.rs`, 404 on plain `/queue` -> "daemon at <host:port> predates /queue; run otto deploy there".
- `sb/src/cli/output.rs::emit` (TTY yaml, piped json, `--format`).
- **Success criteria:**
  - After `otto deploy` on desk (which restarts the daemon): `sb borg queue | jq -e .state` exits 0.
  - `sb borg queue --format yaml` while idle prints exactly `state: idle`.

#### Phase 5: `sb borg wait` + typed exit codes
**Model:** opus
- Pinned-batch poll loop, monotonic deadline, `ExitWith(u8)` in `sb/src/error.rs` mapped in `sb/src/main.rs`'s match.
- Exit decision is a pure fn over (snapshot, deadline-reached) unit-tested per table row; the loop is tested end to end as a subprocess against a stub HTTP server.
- **Success criteria (subprocess exit codes against the stub):**
  - idle -> 0; draining then idle with `failed: 0` -> 0; draining then idle with the last item failed -> 3; an item wedged -> 4; stub never drains with `--timeout 3s` -> 5; stub accepts the connection and never responds, `--timeout 3s` -> 5 (not a hang).
  - stub returns 500 -> 1; closed port -> 1; `sb borg wait --bogus` -> 2.
  - a new item joins the pinned batch mid-wait -> `wait` keeps waiting and the final `batch.total` includes it.

#### Phase 6: oracle `ingest_queue` + docs
**Model:** sonnet
- `cargo add reqwest -p oracle` (json feature); tool in `#[tool_router]` AND the manual `dispatch` match (`oracle/src/server.rs:68-160`).
- Docs: `CLAUDE.md` (borg section), `borg/AGENTS.md`, `oracle/AGENTS.md:16` tool list, `sb/AGENTS.md`, `config/templates/borg.yml.example` (new `queue:` block).
- **Success criteria:**
  - `sb oracle call --list | rg -c ingest_queue` returns 1.
  - With one `sb borg ingest <url>` in flight: `sb oracle call ingest_queue | jq .batch.id` equals `sb borg queue --format json | jq .batch.id`.
  - With `hotkey.port` pointed at a closed port: `sb oracle call ingest_queue` returns an error naming the address, not `idle`.

## Acceptance Criteria

- [ ] `curl -s http://desk.lan:8181/queue | jq -e .state` exits 0 against the deployed daemon.
  - Observed on main (2026-10-04, unsandboxed from desk): `GET /queue` -> `404`. `GET /health/audit` -> `200`.
- [ ] `sb borg queue --format yaml` while nothing is in flight prints exactly `state: idle`.
  - Observed on main: `error: unrecognized subcommand 'queue'`, exit 2.
- [ ] `sb borg wait; echo $?` with nothing in flight prints `state: idle` then `0`. After three separate single-URL `sb borg ingest <url>` calls have each returned, and `sb borg queue` shows `state: draining`, `sb borg wait; echo $?` prints a snapshot with `batch.total: 3` and `0` once the third note lands.
  - Observed on main: `error: unrecognized subcommand 'wait'`, exit 2.
- [ ] `sqlite3 -readonly ~/.local/share/sb/borg/receipts.db "select max(version) from schema_version"` returns `5`, and `pragma_table_info('receipts')` lists `started_at`.
  - Observed on main: version `4`; `started_at` absent (query returned no row).
- [ ] `sb oracle call --list | rg -c ingest_queue` returns `1`.
  - Observed on main: `0`.

## Resolved Decisions

- **2026-10-04, Scott: Option B.** Every surface goes through the daemon's `GET /queue`, so lappy works. Local-DB-only was rejected (desk-only).
- **2026-10-04, Scott: harvest excluded (panel M4).** `method=harvest` rows are filtered out of every batch. ~2,500 rows land nightly in ~4 minutes (2026-10-04: 2542 rows, 10:00:09-10:04:04 UTC), almost all `rejected` in <=1s; counted, they turn "my 6 videos" into ~2,548 items. Harvest runs in its own process with its own permits, so it never delays click ingests. The round-0 Goals line claiming harvest coverage was the author's pitch for Option B, not a requirement, and is removed.
- **2026-10-04, author + both seats: one `started_at`, stamped at the general permit grant.** Every trace passes that grant; the heavy permit is taken only in four handlers (`pipeline/handlers.rs:85,466,893,1116`). Consequence: with 15 YouTube URLs, up to 8 show `processing` while only 4 hold a HEAVY permit. `wedged-after: 15m` sits above measured per-item receipt->terminal times that already include that wait (worst 537s; p99 518s cli YouTube, 305s http YouTube). Two-stamp option under Alternatives.
- **2026-10-04, panel round 1 (staff; architect disagreed, author sided with staff): `/queue` is auth-gated like `/trace`.** The round-0 reason for leaving it open ("same class as `/health/audit`") was false: `AuditHealth` is integer counts (`routes.rs:80-90`); `/queue` returns URLs and failure reasons. Gating costs nothing today (`require_auth` passes everything when `server.auth-token` is unset, `routes.rs:57-59`), matches the sibling `/trace`, and oracle needs no duplicated code because `resolve_secret` already lives in vault (`vault/src/config.rs:57`).
- **2026-10-04, panel round 1: exit codes 0/1/3/4/5, 2 left to clap.** `sb` exits 1 on any eyre error and clap exits 2 on a usage error before `main`'s match runs (`sb/src/main.rs:21`; observed: `unrecognized subcommand` -> exit 2). An agent must be able to tell "a video failed" from "wait never ran" from "bad flag".
- **2026-10-04, panel round 1: `wait` pins the batch id and polls `?batch=<id>`.** Replaces the round-0 "plain idle, then `?last=1`" protocol, which could read a different batch that arrived between the two calls. One response answers one batch atomically.
- **2026-10-04, panel round 1: queued rows age from `received_at`.** Without it a row with a NULL `started_at` (warn-and-continue renew failure, daemon restart before grant) never reaches `wedged` and `wait` polls until the watchdog reaps it at 1860s.
- **2026-10-04, author + both seats: daemon address = `hotkey.host/port`; type, token resolver and loader move to vault.** One address and one loading chain for every first-party client; oracle reads the same `borg.yml` the same way instead of a second `oracle.yml` key that could drift. The `hotkey` name undersells it (it is the client-side daemon address); renaming is out of scope.
- **2026-10-04, author: config durations are humantime strings, typed at deserialize.** Matches `--timeout 60m` on the CLI and `harvest.thread-window`; `pipeline.*-secs` u64 knobs stay as they are.

## Alternatives Considered

### Read the receipts DB directly from each client
- **Pros:** no HTTP route, no reqwest in oracle.
- **Cons:** lappy has no DB, so it would report idle forever; oracle's `failure_history` already has this blind spot (`oracle/src/server.rs:554-630`).
- **Why not chosen:** Scott chose Option B.

### Anchor the batch at the earliest in-flight `received_at`
- **Pros:** one SQL predicate.
- **Cons:** the anchor moves forward as early rows finish; `done` shrinks mid-batch.
- **Why not chosen:** wrong numbers for the exact use case.

### `sb borg wait --since <t>` pinned by the caller
- **Pros:** exact, no gap heuristic.
- **Cons:** the agent has to know when Scott started clicking.
- **Why not chosen:** fails Scott's "EASY for it to use" requirement.

### Second stamp at the HEAVY permit grant
- **Description:** a `heavy_started_at` stamped at the four `HEAVY_PERMITS.acquire` sites (`pipeline/handlers.rs:85,466,893,1116`); `processing` would then mean "doing heavy work", and handler-internal waits would read `queued`.
- **Pros:** `processing` count matches the 4 traces running yt-dlp/fabric.
- **Cons:** five stamp sites instead of one; light traces never take the permit so the column is NULL for them, and the partition rule forks per handler kind.
- **Why not chosen:** the age threshold already sits above the measured worst including that wait, so the extra precision changes no `wait` outcome. Revisit if `wedged` false positives show up.

### Plain `idle` then `?last=1` on drain (round 0)
- **Why not chosen:** races a new arrival between the two reads (panel round 1, M2). Replaced by the pinned `?batch=<id>` poll.

## Technical Considerations

### Dependencies
- oracle: new direct `reqwest` (via `cargo add`; transitive 0.12 via `hf-hub` today, borg is on 0.13.2).
- vault: no new deps (`HotkeyConfig`, the token resolver and `load_config` use serde, serde_yaml and `resolve_secret`, all already there).
- Ship order: one repo, one binary (`sb`); `otto deploy` restarts the daemon. During rollout an old daemon answers `/queue` with 404, which the client reports as "predates /queue".

### Performance
- `/queue`: one indexed scan over 24h of non-harvest rows plus any in-flight rows (tens of rows), merge in Rust. `wait` at 2s from one or two agents is negligible next to the pipeline.

### Security
- `/queue` sits behind the existing bearer gate; open today only because no token is configured, same as `/trace`. No writes, no new secret storage.

### Testing Strategy
- Pure `snapshot` and the exit decision carry the logic, tested with injected `now` like `list_stale` (`borg/src/receipts.rs:597`).
- Route tests via tower `oneshot`; `wait` as a subprocess against a stub server (Phase 5 list).
- Shakedown: `/cli-shakedown` on `sb borg queue` / `sb borg wait` with a live 3-5 URL burst from desk and lappy.

### Rollout Plan
- Branch, phases 1-6 (one commit each, `otto ci` green), bump rides the PR, `otto deploy` on desk then lappy, live check with a click burst.

## Edge Cases

- **Upgrade mid-batch:** in-flight rows from the old binary have `started_at` NULL -> `queued`, aging from `received_at`. They finish normally or reach `wedged`.
- **Item finishes before the next click arrives:** joined by `batch-gap` (2m). Measured arrival spans had gaps well under that.
- **Gap longer than `batch-gap` mid-session:** the later clicks form a new batch. `wait` reports the batch in flight when it started; per the caller contract, call it after the last submit.
- **Everything already drained before the agent calls `wait`:** `wait` sees idle first and exits 0 without a batch. Click bursts take 183-579s to drain, so calling `wait` right after the last click catches the batch. The door write is synchronous before the extension gets its 200 (`routes.rs:110-124`), so a clicked URL is never missed.
- **Watchdog reaps a wedged item:** it becomes `failed` (`failure-stage: crashed`); `wait` would already have exited 4.
- **Replays:** `sb borg replay` rows are ordinary non-harvest receipts and count.
- **Duplicates:** written `succeeded` (`pipeline.rs:408`), counted as done.
- **Daemon restarts mid-`wait`** (e.g. `otto deploy`): the poll fails and `wait` exits 1 naming the address. In-flight traces die with the process but stay `received` until the watchdog reaps them (1860s). A re-run `wait` sees them and exits 4 once they pass `wedged-after`, so the stranded work surfaces.
- **Old daemon, new client:** plain `/queue` returns 404; the client says "daemon at <host:port> predates /queue; run otto deploy there" and exits 1.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| `wedged-after` false positive on a long video waiting for a HEAVY permit | Low | `wait` exits 4 early | Default 900s vs measured per-item worst 537s; configurable |
| Pre-existing: trace queued for a general permit past 1860s reaped while queued | Low | False `crashed` | Out of scope (Non-Goals); never observed (longest burst drain 860s) |
| Two click bursts merge into one batch | Med | Larger `total` than expected | Correct by definition: the agent waits for everything in flight |
| `/queue` exposes ingested URLs to anyone reaching port 8181 while no token is set | Low | Info disclosure on Scott's own LAN/Tailscale | Same posture as `/trace` today; setting `server.auth-token` closes both |

## Open Questions

None.

## References

- `docs/design/` harvest-watchdog-cross-process-reaping (lease columns, v4 migration)
- `borg/src/replay.rs:245-330` (daemon polling precedent)
- Panel round 1 synthesis: `/tmp/review-panel/byouVFGn/synthesis.md`
- `~/repos/.claude/rules/taste.md`
