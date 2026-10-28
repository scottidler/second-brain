# Design Document: Multi-target `sb borg ingest` with one bulk summary

**Author:** Scott Idler
**Date:** 2026-10-27
**Status:** Implemented
**Review Passes Completed:** 5/5 (+ review-panel round 1 folded)

## Summary

`sb borg ingest` takes exactly one target today. This makes it take 1+ targets, each a URL or a file path. When the count is over `ingest.bulk-threshold` (default 5) the run is a bulk: its URL items carry a batch id, and the daemon makes one best-effort summary (desktop + Telegram) when that batch drains. Per-item CLI ingests stay silent, as they already are.

## Problem Statement

### Background

- `sb borg ingest [OPTIONS] [URL]`: one optional URL, or `--clipboard`, or `--file <FILE>` (`sb/src/cli/borg/args.rs:33-54`).
- URL ingest is fire-and-forget: the client POSTs `/ingest`, the daemon answers `Queued` + `trace_id`, the client drops the trace id and prints "Queued for processing." (`borg/src/client.rs:523-566`, `sb/src/cli/borg/render.rs:511-528`).
- When that POST itself fails, the client sends a desktop `Error` toast, unconditionally (`borg/src/client.rs:539-551`).
- `--file` runs the whole pipeline in-process on the invoking host, synchronously (`borg/src/client.rs:74-159`). No daemon, no notify calls.
- Daemon-side notifications: CLI ingests have pinged nobody since a44e1f3 (2026-10-17, first shipped in v0.18.0). The gate is `pings_operator` (`borg/src/dispatch.rs:75-77`): `method != Cli && replay_origin != AutoRemediation`. Clipboard, extension, `/ingest/file`, Telegram, Signal and Discord still ping per item.
- The ingest guard (`scottidler/claude`, `HOME/.claude/hooks/intent-guard.sh`) denies loops and `xargs` around `sb borg ingest`. A bulk is allowed only as ONE statement led by `BULK_INGEST_ORDERED_BY_SCOTT=<n>`.

### Problem

- On 2026-10-27 Scott ordered 62 scoured URLs ingested. Three shapes were denied by the guard (a `for` loop, `bash -c`, `xargs`) because the CLI cannot take more than one target in one statement.
- Scott (2026-10-27): "I just dont want to spam me with toasts when ingesting a lot all at once." A multi-target ingest must not toast per item, and a run that big should tell him once when it is done.

### Goals

- `sb borg ingest <target>...`: 1+ targets, each an `http(s)` URL or an existing file path. (Scott: "1+ items that can be urls or filepaths")
- Over `ingest.bulk-threshold` targets (default 5) is a bulk. (Scott: "more than some config default (say 5) then it is a bulk ingest")
- A multi-target run never toasts per item, client side or daemon side. A bulk makes one summary attempt when its URL items drain. (Scott: "toasts are temporarily turned off during the ingest and then toggled back on"; restated 2026-10-27 as "I just dont want to spam me with toasts")
- `BULK_INGEST_ORDERED_BY_SCOTT=<n> sb borg ingest <n targets>` is one guard-legal statement, and the guard counts every target, URL or path, against `<n>`.

### Non-Goals

- **Excluded: a daemon-global notification mute.** Scott's literal wording was "turned off ... then toggled back on". The intent he restated is no spam. A global mute silences unrelated phone captures (the Telegram/Signal ack is his only confirmation there) and sticks off if the client dies mid-run. Per-batch marking gets the same outcome with no shared state. See Alternatives.
- **Excluded: changing per-item behavior of non-CLI transports.** Clipboard, extension, Telegram, Signal, Discord keep their per-item acks.
- **Excluded: durable retry of the summary.** The summary is one best-effort attempt per batch, logged per sink. A crash between claiming the summary and sending it loses it; the per-target lines and `sb borg log` remain the record.
- **Parked: routing file targets through the daemon** (`/ingest/file`). File targets keep today's local, synchronous path. Revisit when a file-only bulk is a common case.
- **Parked: skipping ledger hits in a bulk.** A URL already in the ledger re-distills today (`borg/src/pipeline.rs:780-786`). Unchanged here. Revisit if bulk re-distill cost shows up.
- **Parked: the `--force` help text**, which says "already in the ledger" while `--force` only bypasses the in-flight guard (`args.rs:45-47`). A separate targeted fix.
- **Parked: `sb borg replay` pinging per item** when replaying Telegram- or extension-captured notes (`borg/src/replay.rs:198`, `:530`). Not part of this ask.

## Proposed Solution

### Overview

```
sb borg ingest t1 t2 ... tN [-t tags...] [--note ..] [--force]
   |
   +-- classify every target first (URL | existing path | error -> exit 2, zero submits)
   |
   +-- N > bulk-threshold AND at least one URL target ?  -> batch = new uuid  : batch = none
   |
   +-- URL targets:  POST /ingest {url, method: cli, batch}; one line each with trace id; no per-target toast
   +-- batch set AND >=1 Queued ?  -> POST /batch/{id}/seal {trace_ids: [the Queued trace ids]}
   +-- file targets: local sync pipeline (today's ingest_file), one line each, after the seal

daemon: every terminal write of a batched trace, and every seal
   -> maybe_summarize: drained?  -> claim (UPDATE ... WHERE summarized_at IS NULL)
   -> winner sends a BatchSummary on the daemon's notifier channel
notifier task (holds the desktop + Telegram sinks): one send per sink, outcome logged with the batch id
watchdog tick (backstop, seal-missing only): unsealed batch, no member still `received`, last intake older
   than hard_timeout_secs + 60s -> claim and summarize, marked "seal missing"
```

### Architecture

- **CLI (`sb`)**: parse, classify, decide bulk, submit, seal, render, exit code. Libraries return typed data; `sb` prints (house rule).
- **Client (`borg::client`)**:
  - `ingest` returns the trace id.
  - The per-failure `Error` toast (`client.rs:539-551`) stays for the single-target path (hotkey/clipboard). A multi-target run suppresses it per target and sends at most one aggregated client toast, `borg: <k> of <n> submits failed`, only when k > 0.
  - New `seal_batch(id, trace_ids)`.
  - Refuses a bulk when the daemon's `/health` `features` lacks `bulk-batch` (precedent: `replay-of`, `borg/src/health.rs:9`), so an old daemon never silently drops the field.
- **Daemon**:
  - `IngestRequest.batch` recorded on the receipts row at intake.
  - New `batches` table.
  - One sync drain check, `batch::maybe_summarize(conn, batch_id) -> Option<BatchSummary>`. It never touches a sink; sink latency never sits inside a receipts write.
  - Called after each terminal transition of a batched trace (`mark_succeeded`, `mark_failed`, `mark_rejected`, the watchdog's `promote_single_to_crashed`), on seal, and on the watchdog backstop tick.
  - A `Some(summary)` goes on an `mpsc` channel to one notifier task spawned at daemon start, which holds the desktop + Telegram sinks from `AppState`. The watchdog (`borg/src/watchdog.rs:38`, `:86`) gets the channel sender at spawn; it holds no sinks.
- **Summary**: lands where today's acks land (desktop host-pinned to `desktop.host`, Telegram host-pinned). It reports URL items only; a mixed run's file lines are in the terminal.
- **Why the threshold still means something**: per-item CLI silence already exists. The threshold decides whether a run earns the one summary. One to five targets: per-target lines in the terminal are the feedback. Six or more: plus one summary when done.

### Data Model

- `IngestRequest` (`borg/src/types.rs:312`): new `#[serde(default)] pub batch: Option<String>`. Optional, so additive-only per `borg/AGENTS.md` invariant 5; the extension needs no re-sign.
- Receipts schema v9, one step in `run_migrations` using the `has_column` precedent of v8 `scratch` (`borg/src/receipts.rs:42`, `:270-383`):
  - `receipts.batch_id TEXT NULL`, plus index `receipts(batch_id)`.
  - `batches(id TEXT PRIMARY KEY, created_at TEXT NOT NULL, sealed_trace_ids TEXT NULL, sealed_at TEXT NULL, summarized_at TEXT NULL)`. `sealed_trace_ids` is a JSON array.
- The batch row is created at intake with `INSERT OR IGNORE` (concurrent intakes of one batch race here).
- Membership is the daemon's: every receipts row carrying the `batch_id`. The seal adds the client's view so the daemon knows submission is over.
- Stored statuses (verified): `received`, `succeeded`, `failed`, `rejected`. "Crashed" is `status='failed', failure_stage='crashed'` (`promote_single_to_crashed`, `borg/src/receipts.rs:810`). `rejected` is harvest-only today (`harvest.rs:375`) and is treated as terminal for safety.
- **Drained** = `sealed_at` set AND no row with this `batch_id` has `status='received'` AND every id in `sealed_trace_ids` has a row. Why both halves:
  - A POST can time out after the daemon wrote the row (`hotkey.request-timeout`, `vault/src/daemon/client.rs:66-74`). That row is a member the client never counted; the "no member still `received`" half waits for it.
  - The client's sealed ids cover the inverse: a member whose row the client saw acknowledged must exist before the batch reads as drained.
- The summary counts every member row: `succeeded` (with `degraded`), `failed` (with `crashed` broken out), and the number of sealed ids the daemon has no row for (`unknown`, should be 0).
- A member row written after the summary was claimed is ignored for notification; it is still in `sb borg log`.

### API Design

CLI:

```
sb borg ingest [OPTIONS] [TARGETS]...
  TARGETS   1+ http(s) URLs or file paths, space-separated
  --clipboard          read one URL from the clipboard (conflicts with TARGETS; never a bulk)
  -t, --tags <TAGS>... operator tags (unchanged; space-separated, greedy, so it goes after the targets)
  --note <NOTE>        applies to every URL target
  --force              unchanged
```

- `--file` is removed: a positional path says the same thing, and two signals never encode one meaning. No operational caller uses it. References to update: the guard clause `intent-guard.sh:1942`, `intent-guard-test.sh:408`, and `scottidler/claude` `docs/design/2026-09-15-intent-guards.md` (historical, left as written); in this repo, only `docs/shakedown-sb-v0.8.2.md` (historical).
- `--note` applies to URL targets and is still rejected for file targets (today `--note` conflicts with `--file`, `sb/src/cli/borg.rs:55`). A run mixing files and `--note` exits 2 before any submit.
- A tag that is itself an `http(s)` URL or an existing path exits 2: that is the greedy `-t` swallowing a target (`sb borg ingest -t ai https://x`). Tradeoff, accepted: a tag that happens to name a file in the CWD now errors.
- Output: one line per target, `<target> -> queued <trace>` | `-> ingested <note path>` | `-> failed: <reason>`. A bulk adds a final line: `batch <id>: sealed <k> URL items; one summary when they drain`, or on seal failure `batch <id>: seal failed (<reason>); the watchdog will summarize as "seal missing"`.
- Exit codes, plumbed through `ExitWith` (precedent: `sb borg wait`, `sb borg distill`), since an eyre error exits 1 by default (`sb/src/main.rs:29`):
  - 0: every URL target `Queued`, every file target ingested, seal (if any) accepted.
  - 1: any target failed, or the seal failed. File targets still run after a seal failure.
  - 2: usage or classification error; nothing submitted.

Daemon routes (bearer-gated like `/trace`):

- `POST /ingest` body gains optional `batch`.
- `POST /batch/{id}/seal` body `{"trace_ids": [..]}`. 404 for an unknown id; 409 if already sealed with a different id set; an identical re-seal is 200.

Config (`borg.yml`, read on the invoking host like `hotkey`):

```yaml
ingest:
  # More targets than this in one `sb borg ingest` is a bulk: one summary
  # toast when its URL items drain. 0 = every run with a URL target is a bulk.
  # --clipboard is never a bulk.
  bulk-threshold: 5
```

### Implementation Plan

Ship order: Phase 0 lands in `scottidler/claude` first. The new CLI shape does not exist until Phase 1 ships, so a stricter guard first opens no window.

#### Phase 0: Guard counts every `borg ingest` target (repo `scottidler/claude`)
**Model:** opus
- Today's counting has three holes, each confirmed by probe on this host:
  - Counting runs over a quote-masked string (`intent-guard.sh:1592`), and only `https?://` tokens add to `stmt_ops` (`:1747-1751`). File operands are never counted.
  - The door exempts the statement (`~:1768`) before any glob check, so `BULK_INGEST_ORDERED_BY_SCOTT=1 sb borg ingest ./*.pdf` passes.
  - The literal-URL gate (`:1952`) checks only the first operand.
- New rule:
  - `stmt_ops` = count of non-flag operands of `borg ingest`, URL or path, quoted or not, including operands after `--`. Values of `-t`/`--tags` and `--note` are not operands.
  - A glob, `$(...)`, or stdin/`--from` source as an operand denies, door or not: the count is unknowable.
  - One literal target (URL or path) needs no door. Two or more need the door with `<n>` >= the count.
  - The `*"ingest --file"*` clause (`:1942`) is retired; a path operand is now the file form.
- Cases in `intent-guard-test.sh` for every bullet above.
- **Success criteria:**
  - `BULK_INGEST_ORDERED_BY_SCOTT=1 sb borg ingest ./a.pdf ./b.pdf` -> deny; `=2` -> allow.
    Observed on main: `allow` for `=1`.
  - `BULK_INGEST_ORDERED_BY_SCOTT=9 sb borg ingest ./*.pdf` -> deny; `sb borg ingest https://a.example ./b.pdf` -> deny; `sb borg ingest ./a.pdf` -> allow; `sb borg ingest --note "see https://x.example" https://a.example` -> allow.
    Observed on main: glob at `=9` -> `allow`; URL+path -> `allow`; lone path -> `deny`; URL-in-`--note` -> `deny` (the quoted URL counts as a second target).
  - `bash intent-guard-test.sh` exits 0.

#### Phase 1: Multi-target CLI and classification
**Model:** sonnet
- `Ingest { targets: Vec<String>, clipboard, tags, force, note }`; `--file` removed; `--clipboard` conflicts with targets.
- Classify all targets before any submit: `http(s)` URL | existing path | error. Tag-swallow check. `--note` + file target check.
- Submit order: URL targets first, then file targets (local, synchronous), so a slow PDF never delays the daemon's queue.
- `IngestOutcome::Queued { trace_id }`; per-target render lines; per-target client error toast suppressed in multi-target runs, one aggregated toast if any failed.
- Exit 0 | 1 | 2 through `ExitWith`.
- Fix the pattern match in `sb/src/cli/tests.rs:50`.
- **Success criteria:**
  - `sb borg ingest --help` usage line reads `sb borg ingest [OPTIONS] [TARGETS]...`.
    Observed on main: `Usage: sb borg ingest [OPTIONS] [URL]`.
  - Tests against a stub daemon: a nonexistent path, a `--note` + file run, and a tag-swallow run each exit 2 with zero POSTs; a mixed URL+file run POSTs every URL before the first file starts; one failed POST of three exits 1 with exactly 1 client toast call.

#### Phase 2: `ingest.bulk-threshold` config
**Model:** sonnet
- `IngestConfig { bulk_threshold: u32 }`, kebab-case, `#[serde(default)]` = 5, in `borg::Config`; annotated block in `config/templates/borg.yml.example`.
- **Success criteria:**
  - Unit test: an absent `ingest:` block deserializes to `bulk_threshold == 5`; with `5`, 5 URLs are not a bulk and 6 are; with `0`, a 1-URL run is a bulk and `--clipboard` is not; 6 file-only targets never open a batch.
  - `rg -c 'bulk-threshold' config/templates/borg.yml.example` prints `1`.
    Observed on main: no match (exit 1).

#### Phase 3: Batch identity, seal, and the one summary
**Model:** opus
- `IngestRequest.batch`; receipts v9 (`batch_id` + index, `batches` table) via `has_column`; intake records it with `INSERT OR IGNORE` on the batch row.
- `POST /batch/{id}/seal`; `/health` feature `bulk-batch`; client refuses a bulk against a daemon without it.
- `batch::maybe_summarize` with the Drained predicate above; claim by `UPDATE batches SET summarized_at = ? WHERE id = ? AND summarized_at IS NULL`, acting only when one row changed.
- Notifier task + channel; watchdog gets the sender. Each sink's outcome logged: `batch <id>: desktop sent|failed|disabled, telegram sent|failed|disabled`.
- Watchdog backstop, seal-missing only: unsealed, no member `received`, newest member `received_at` older than `hard_timeout_secs + 60s`.
- Summary text: `borg bulk <id>: <s> succeeded (<d> degraded), <f> failed (<c> crashed) of <total> URL items` with `, seal missing` or `, <u> unknown` appended when nonzero.
- **Success criteria:**
  - Test with counting fake sinks: a sealed batch of 6 whose items all reach terminal sends 1 desktop and 1 Telegram summary and 0 per-item sends; a 7th member row still `received` (lost response) holds the summary until it is terminal.
  - Test: an unbatched `/ingest` with `method: http` during an open batch still sends its own ack; a second seal, a late terminal write after the claim, and a backstop tick on a sealed batch each send nothing.
  - `cargo test --workspace --test extension_body_matches_ingest_request` passes unchanged.

#### Phase 4: Docs, ship, live check
**Model:** sonnet
- `borg/AGENTS.md` (invariant 5 field list, client description, notifications), `sb/AGENTS.md`, CLAUDE.md notifications paragraph, template.
- Bump, `otto deploy` on ripr, then a live bulk of 6 fresh URLs with their trace ids captured from the per-target lines.
- **Success criteria:**
  - `sb --version` on ripr reports the bumped version, and `curl` of the daemon `/health` lists `bulk-batch` in `features`.
  - `dbus-monitor --session "interface='org.freedesktop.Notifications'"` on ripr, captured from the first submit until the summary, shows exactly 1 `Notify` whose body contains `borg bulk <id>`, and it arrives after all 6 `sb borg log --trace <id>` rows are terminal.
  - Telegram shows exactly 1 message containing `borg bulk <id>`.

## Acceptance Criteria

- [ ] `BULK_INGEST_ORDERED_BY_SCOTT=6 sb borg ingest <6 URLs>` passes the guard as one statement; `=1` with two file paths, and any glob operand, are denied.
  Observed on main: the 3-URL door case allows; the 2-file `=1` case and `./*.pdf` at `=1` allow (Phase 0 fixes).
- [ ] `sb borg ingest --help` shows `[TARGETS]...`; a run with an unclassifiable target exits 2 with zero submits.
  Observed on main: `Usage: sb borg ingest [OPTIONS] [URL]`.
- [ ] A bulk over the threshold produces 0 per-item notifications (daemon or client) and one summary per sink, sent after its last member is terminal: unit test with counting fake sinks, and bounded `dbus-monitor` live on ripr.
  Observed on main: `cargo test --workspace --lib dispatch::tests` -> `cli_ingests_do_not_ping_the_operator ... ok` (3 passed); no batch summary exists to observe.
- [ ] Unrelated ingests during an open batch keep their per-item acks (test).
  Observed on main: `human_transports_still_ping_the_operator ... ok`.
- [ ] `borg.yml` with no `ingest:` block behaves as `bulk-threshold: 5`, and the template documents the key.
  Observed on main: `rg -c 'bulk-threshold' config/templates/borg.yml.example` -> no match.

## Resolved Decisions

- 2026-10-27, Scott + author: the threshold gates the one summary; per-item CLI stays silent. Scott: "I just dont want to spam me with toasts when ingesting a lot all at once."
- 2026-10-27, author: per-batch marking over a global mute (no shared state, no stuck-off window, phone acks untouched).
- 2026-10-27, author: the summary fires daemon-side from receipts state, so it survives the client exiting and a daemon restart. This reverses one excluded item in `docs/design/2026-10-04-ingest-queue-status.md:55` ("Push notifications on drain ... Excluded: `wait` plus the agent's background-task wakeup covers the use case"). That reason assumed an agent waiting in the background; Scott's 2026-10-27 requirement is a human-facing signal when a large ingest finishes, which `wait` does not give him.
- 2026-10-27, author: `--file` removed in favor of positional paths.
- 2026-10-27, review-panel round 1 (both seats, author agreed): membership is daemon-owned plus a sealed trace-id list; backstop is seal-missing only; the summary is one best-effort attempt per batch with per-sink logging; a single literal file target needs no guard door.

## Alternatives Considered

### Alternative 1: Daemon-global mute toggled by the client
- **Description:** client POSTs "mute", ingests, POSTs "unmute".
- **Pros:** matches Scott's literal wording.
- **Cons:** needs new mutable state threaded through every transport; silences unrelated Telegram/Signal acks during the run; a killed client leaves it muted until a lease or restart; overlapping bulks need refcounting.
- **Why not chosen:** per-batch marking reaches the same outcome with none of those failure modes.

### Alternative 2: Client polls and toasts itself
- **Description:** CLI keeps trace ids, polls `/trace/{id}` until all terminal, then sends one toast.
- **Pros:** no schema change.
- **Cons:** the CLI must stay alive for the whole bulk (a 62-URL run is long); Ctrl-C loses the summary; a client-side desktop toast lands on the invoking host, not `desktop.host`.
- **Why not chosen:** the daemon already owns terminal state and the sinks.

### Alternative 3: Reuse `/queue` time-gap batches
- **Description:** summarize when the `queue.batch-gap` batch drains.
- **Cons:** the heuristic absorbs unrelated ingests (`borg/src/queue.rs:378-386`) and has no notion of "all submitted".
- **Why not chosen:** wrong identity.

### Alternative 4: Seal carries a count, not trace ids (round-0 draft)
- **Description:** drained when terminal rows == the count of `Queued` responses.
- **Why not chosen:** a POST that times out after the daemon wrote its row makes the count wrong in either direction (review-panel round 1, must-fix 1).

### Alternative 5: Backstop on batch age (round-0 draft)
- **Why not chosen:** a leased item legitimately outlives `created_at + 1860s`; an age backstop would claim the summary early and suppress the true one (review-panel round 1, must-fix 2).

## Technical Considerations

### Dependencies
- None new. `uuid`, `notify-rust`, the Telegram sink, rusqlite and tokio `mpsc` are already in `borg`.

### Performance
- One indexed query on `receipts(batch_id)` per terminal write of a batched trace. Unbatched traces skip the check.

### Security
- Seal route bearer-gated like `/trace` and `/queue`. Batch id is a client uuid, not a secret.

### Testing Strategy
- Unit: classification, threshold, Drained predicate (lost response, missing sealed id), claim exactly-once under concurrent callers, backstop scope.
- Counting fake sinks for desktop, Telegram, and the client error toast (the sinks already short-circuit under test via `real_notifications_disabled`; the fakes count calls).
- Guard: `intent-guard-test.sh` cases.
- Live: bounded `dbus-monitor` on ripr during a 6-URL bulk.

### Rollout Plan
- Phase 0 ships in `scottidler/claude` first. Phases 1-4 ship as one `sb` release; `otto deploy` on ripr. Clients on talos/lappy refuse a bulk until their daemon reports `bulk-batch`; single-target ingest keeps working against any daemon.
- After the release: run the held 62-URL ingest from 2026-10-27 as `BULK_INGEST_ORDERED_BY_SCOTT=62 sb borg ingest <62 URLs>`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Client dies before seal | Med | Low | Watchdog backstop fires a "seal missing" summary once no member is `received` |
| Summary claimed, then daemon crashes before send | Low | Low | Accepted (best-effort); per-sink outcome logged; `sb borg log` is the record |
| Two callers race to claim | Med | Low | `summarized_at IS NULL` conditional UPDATE; only the row-changing caller sends |
| Daemon down mid-bulk: one client toast per failed POST | Med | Med | Per-target toast suppressed in multi-target runs; one aggregated toast |
| Greedy `-t` eats targets | Med | Med | Exit 2 when a tag classifies as a URL or existing path |
| Old daemon drops `batch` silently | Low | Med | `/health` feature `bulk-batch`; client refuses |
| Ctrl-C during the file half of a mixed run | Low | Low | The URL summary still arrives; it says "URL items", never "run finished" |

## Open Questions

(none)

## References

- `docs/design/2026-10-04-ingest-queue-status.md`
- `docs/design/2026-05-21-desktop-notifications.md`
- a44e1f3 `fix(borg): stop pinging desktop and Telegram for CLI ingests`
- `scottidler/claude` `HOME/.claude/hooks/intent-guard.sh`
- Review panel round 1: `/home/saidler/.cache/review-panel/runs/Wj8tHd1B/synthesis.md`
