# Design Document: `sb borg distill`

**Author:** Scott Idler
**Date:** 2026-10-25
**Status:** Implemented
**Review Passes Completed:** 5/5, plus review-panel round 1. The pass log is at the end of this doc.

## Summary

`sb borg distill <URL> --output DIR` runs the ingest pipeline on the daemon host and writes everything it produces to `DIR` on the calling machine:

- the rendered note
- the staged stage files
- the frames tree: every extracted frame, plus OCR for every slide run
- the figures

It writes nothing to the vault and nothing to the ledger.

It shares one pipeline entry with `sb borg ingest` through a `PublishPolicy`. Ingest passes `Vault`, which is exactly today's behavior. Distill passes `Scratch { root }`.

## Problem Statement

### Background

- **Session `c7cd46e1`** (2026-10-25, on ripr): Scott asked for every artist and song in a YouTube Short, the Billboard 1989 year-end top 40.
  - Scott: "can you use any of the machinery from sb borg? I dont necessarily want it ingested in as a note."
  - Claude skipped borg because "`sb borg ingest` writes a note". It rebuilt the work by hand in a scratchpad with `yt-dlp`, `ffmpeg` and `tesseract`.
  - OCR missed 15 of the 40 songs, and Wikipedia filled the gaps.
- **Borg already has the pieces:** the engine choice, the yt-dlp breaker, frame extraction, slide segmentation and OCR (`borg/src/youtube/engine.rs`, `borg/src/slides.rs`, `borg/src/ocr.rs`). None of it can run without publishing.

### Problem

`sb borg ingest` has exactly one ending: a note in the vault, plus these side writes.

| Write | Where |
|---|---|
| note -> inbox | `borg/src/pipeline.rs:1090-1095` -> `Config::inbox_dir` (`borg/src/config.rs:1270`) |
| ledger append | `borg/src/pipeline.rs:1204` |
| ledger lookup -> reingest-in-place (old note's date, fields, attachments) | `borg/src/pipeline.rs:667-701` |
| raw-input sidecar `<vault>/system/intake/<trace>.txt` | `borg/src/intake.rs:120` -> `vault/src/intake.rs:108` |
| slide JPEGs -> `<vault>/system/attachments/YYYY-MM/` | `borg/src/slides/publish.rs:64-87`, called at `borg/src/pipeline.rs:1007` |
| figures -> `<vault>/system/attachments/images/` | `borg/src/pipeline/publish.rs:429` -> `borg/src/pipeline/figures.rs:328-346` |
| durable source archive `archive.root/<trace>/` | `borg/src/pipeline/publish.rs:347` |
| old-note delete + orphan cleanup through rkvr | `borg/src/pipeline.rs:1183-1196`, `borg/src/pipeline/publish.rs:448-457` |
| rkvr cleanup of a partial store on failure | `borg/src/pipeline/figures.rs:351`, `borg/src/pipeline/publish.rs:439` |
| receipts row (received -> terminal) | `borg/src/intake.rs:121`, `borg/src/pipeline.rs:426-456` |
| Stage 0 staging (envelope, body) | `borg/src/pipeline.rs:254` -> `borg/src/stages/raw.rs:177-179` |
| Stage 1/2 staging (fetched, transcript, `distilled.yml`) | `borg/src/stages/distill.rs:1090` and callers |

Four facts that are not visible from the CLI:

- **Ingest is fire-and-forget.** `POST /ingest` spawns the pipeline and returns `Queued{trace_id}` (`borg/src/routes.rs:172,204`). The only way to see the outcome is to poll `GET /trace/{id}` (`borg/src/replay.rs:354`).
- **The frames work dir is thrown away.** Frames, slide JPEGs and `slides.yml` live in `/tmp/borg-youtube-frames/<video_id>/` (`borg/src/pipeline/handlers.rs:245,351`).
  - On a text-only or failed slide path, `try_extract_slides` returns early. `WorkDirGuard` then removes the dir inside the handler (`handlers.rs:222-231`, `:357-363`).
  - On the slide-note path, `publish_slides` copies only the selected slides (`borg/src/slides/publish.rs:55-60`). Then `borg/src/pipeline.rs:1032` removes the dir.
  - The code comment at `handlers.rs:203-206` says most videos are text-only, so for most videos the frames die in the handler.
- **OCR runs only on frames the content filter keeps.** The vision classifier sorts each run (a stretch of near-identical frames) into a category. `classify_and_filter_slides` then OCRs only the runs whose category is in `keep` (`handlers.rs:376-381,445-454`).
  - ripr's `borg.yml` has `content-filter.enabled: true` and `keep: [architecture-diagram]`.
  - Chart cards like the Billboard Short's are dropped, so they get no OCR.
- **The ledger lookup never rejects.** A ledger hit turns the run into a reingest-in-place. The only duplicate rejection is the in-memory inflight guard (`borg/src/pipeline.rs:638-661`).

### Goals

All from Scott, 2026-10-25:

- A `sb borg distill <URL> --output DIR` verb, separate from `ingest`.
- It runs the same pipeline code as ingest, through the daemon.
- Zero vault writes: no note, no sidecar, no attachments, no archive.
- No ledger append and no ledger lookup.
- A receipts row is still written, marked scratch.
- `DIR` gets the rendered note plus the raw stage output: staged files, frames, OCR and figures.

Constraint: ingest's behavior and output stay byte-identical to today.

### Non-Goals

**Excluded:**
- Non-URL inputs. Distill is URL-only, and a non-URL input under `Scratch` is refused before Stage 0.
- Running the pipeline locally without the daemon (Scott chose the daemon).
- An `--output` flag on `ingest` (Scott chose a separate verb).
- Changes to the Firefox extension or hotkey (`IngestRequest` is untouched).
- The downloaded `.mp4`. The frames tree keeps every extracted frame, not the source video.

**Parked:**
- A `--note` capture-intent flag. Revisit when a distill needs to steer the distiller.
- "Promote this scratch run into the vault". Revisit if re-running `ingest` right after a distill becomes a habit.
- `sb borg distill --trace <id>`, to download a scratch trace again after an interrupted download or the poll ceiling. Revisit on the first such interruption.
- An oracle MCP tool for distill. Revisit when an agent needs distill without a shell.
- A config key for the scratch root. Revisit when a second host needs a different path.

## Proposed Solution

### Overview

```
sb borg distill URL --output DIR            (desk | lappy | ripr)
  GET  /health                    -> refuse unless features ∋ "distill"
  POST /distill {url}             -> Queued{trace_id}   (daemon: intake + spawn, PublishPolicy::Scratch)
  poll GET /trace/{id}            -> Terminal | CeilingReached
  GET  /distill/{id}              -> manifest of files under the scratch root
  GET  /distill/{id}/file/{path}  -> bytes, one per manifest entry
  write DIR/<path>
```

### Architecture

#### `PublishPolicy`: one enum, one pipeline entry

```rust
pub enum PublishPolicy {
    /// Today's behavior: inbox, ledger, archive, vault attachments, vault sidecar.
    Vault,
    /// Everything under `root`; no vault, ledger, lookup, or archive.
    Scratch { root: PathBuf },
}
```

- **Why an enum.** The switches (ledger, lookup, archive, rkvr cleanup, vault writes) always move together. Booleans would allow combinations nobody wants. Two signals must not encode the same meaning.
- **Where it enters.** `process_content` (`borg/src/pipeline.rs:195`) gains a `policy: &PublishPolicy` parameter.
  - All seven production callers pass `&PublishPolicy::Vault`: `dispatch.rs:49`, `client.rs:49,135`, `signal.rs:596`, `discord.rs:234,314`, `harvest/publish.rs:206`.
  - The policy goes on down to `process_url`, `process_url_inner` and `process_youtube`.

#### The effective config, resolved before Stage 0

At the top of `process_content`, before `stage_0_init` (`pipeline.rs:254`):

- A non-URL `ContentKind` under `Scratch` returns `Failed` with no write at all.
- Under `Scratch`, `process_content` clones the config once. The clone sets `staging.root = root/stages`, `staging.enabled = true` and `staging.layout = PerTrace`. This doc calls it the effective config.
- Every later stage, Stage 0 included, reads the effective config. For the run, `staging.root` is the staging root.
- Under `Vault`, the config passes through untouched.

#### What `Scratch { root }` does, site by site

| Site | `Vault` | `Scratch { root }` |
|---|---|---|
| non-URL kind | runs | `Failed` before Stage 0, no write |
| Stage 0 + all staging | `staging.root` | `root/stages` (effective config) |
| staging persistence failure (`stages/distill.rs:433-435`) | logged, run continues | `Failed`, so `succeeded` always means `distilled.yml` exists |
| ledger lookup `pipeline.rs:667` | runs | skipped, never a reingest |
| inflight guard `pipeline.rs:638` | key `<canonical-url>` | key `distill:<canonical-url>`, so a distill never makes an ingest return `Duplicate` |
| inflight hit | `Duplicate`, recorded `succeeded` (`pipeline.rs:435`) | `Failed`, stage `intake-rejected`, reason `distill of this URL already in flight` |
| note dest `pipeline.rs:1090` | inbox or reingest dest | `root` |
| `vault_dirs` collision set `pipeline.rs:1138` | vault dirs | `[root]` |
| selected slides `pipeline.rs:1007` | `vault_root` | `root` |
| frames work dir: `WorkDirGuard` drop (`handlers.rs:222`) and `pipeline.rs:1032` | removed | copied to `root/frames/` minus the `.mp4`, then removed, on every path: slide note, text-only, slide-path error |
| content-filter OCR (`handlers.rs:445`) | kept runs only | kept runs, as today, for the note. Plus every run's best frame OCR'd into `root/frames/runs.yml` |
| figures `publish.rs:429` | `vault_root` | `root` |
| archive `publish.rs:347` | written | skipped |
| rkvr cleanup on partial failure `figures.rs:351`, `publish.rs:439` | runs | skipped; partial output stays for debugging |
| old-note delete / orphan cleanup | runs | unreachable (no lookup, no reingest) |
| ledger append `pipeline.rs:1204` | runs | skipped |
| sidecar | `<vault>/system/intake/` | `root/system/intake/` |
| receipts | `scratch = 0` | `scratch = 1`, set in the INSERT |

- **Every vault write already takes its root as a parameter.** That includes `publish_slides(vault_root, …)`, `figures::store(vault_root, …)` and `UrlPublish { vault_root, vault_dirs, desired_path }`. Under `Scratch`, `root` takes the vault root's place, so no new write paths are added.
- **Intake.**
  - The sidecar core `record_received_with_sidecar_to(conn, vault_root, …)` (`borg/src/intake.rs:110`) is private today.
  - A new public `record_scratch_with_sidecar(config, root, …)` calls it with `root` and a scratch flag.
  - The flag lands in the INSERT that creates the row, as `replay-of` lineage does. A crash can therefore never leave a scratch run counted as an ingest.
- **Frames.** `WorkDirGuard` gains an optional preserve destination.
  - Under `Scratch`, its `Drop` copies the work dir to `root/frames/`, skipping the `.mp4`, then removes the work dir.
  - It copies rather than renames: `/tmp` and `~/.local/share` may be on different filesystems.
  - The success-path removal at `pipeline.rs:1032` goes through the same helper.
- **`runs.yml`.**
  - The content-filter path already computes every run and its best frame (`prepare_runs`, `handlers.rs:415`), and classifies each one (`classes`, `:426`).
  - Under `Scratch`, it also calls `ocr::ocr_extract` on every best frame, with `pipeline.ocr-timeout-secs`.
  - It writes one entry per run: window start and end, best-frame path, category, confidence and `ocr`.
  - The note still comes from the kept runs only, so ingest fidelity is untouched.
  - With the content filter off, `slides.yml` already OCRs every segmented slide (`handlers.rs:331-349`), and `runs.yml` is not written.
- **Structural exclusion.** Every staging write lands under `root/stages`, so readers of the shared `staging.root` never see a scratch trace. That covers cortex proposals (`cortex/src/proposals.rs:86,141`), cortex embed (`cortex/src/embed/source.rs:88`), `eval --staged` and `sb borg replay`.
- **Per-run temp paths.** Splitting the inflight key lets an ingest and a distill of one video run at once. Two shared temp paths would then collide:
  - Frames work dir: `handlers.rs:245` changes `<video_id>` to `<trace_id>`. The `remove_dir_all` at `:246` can no longer wipe another run's frames.
  - Whisper audio: `whisper_transcript` (`borg/src/youtube/engine.rs:138-149`) writes `$TMPDIR/obsidian-borg/<video_id>.<ext>`. It changes to a `tempfile::TempDir` per call.
- **Notifications.** Distill posts with `method = cli`, and `pings_operator` is already false for `cli` (`dispatch.rs:65`). No Telegram, Desktop or Signal message fires.

#### Scratch root and layout

`vault::paths::borg_distill_dir()` returns `~/.local/share/sb/borg/distill`, beside `borg_stages_dir()` (`vault/src/paths.rs:313`). `DIR` mirrors each run's `distill/<trace>/` byte for byte:

```
<trace>/
  <note-filename>.md                 rendered note, same bytes ingest would publish
  frames/                            YouTube only (no .mp4)
    *.jpg                            every extracted frame
    slides.yml                       the kept slides with OCR (today's manifest)
    runs.yml                         content filter on: every run, category, confidence, OCR
  system/attachments/YYYY-MM/...     selected slide JPEGs, as embedded by the note
  system/attachments/images/...      figures
  system/intake/<trace>.txt          raw-input sidecar
  stages/<trace>/envelope.yml body.txt fetched.* transcript.* distilled.yml
```

The note's embeds use vault-relative paths, so opening `DIR` as an Obsidian vault resolves them.

#### Retention

- Staging is operator-swept today, because a first automatic sweep of an old stages tree would be a surprise deletion (`borg/src/retention.rs:252-255`). The distill tree is new, so that does not apply.
- A new daemon background task, `run_distill_sweep`, removes each `distill/<trace>/` older than `staging.retention_days` (default 14), by mtime. `0` keeps everything, matching the sidecar convention (`retention.rs:171`).
- It is a sibling of `run_sidecar_sweep`, not folded into it, because that task returns early when `intake.retention-days` is `0` (`retention.rs:258`).
- `sb borg retention sweep` covers the same tree.
- The client never deletes anything on the daemon.

### Data Model

**Receipts schema v8** (`SCHEMA_VERSION` is 7 today, `borg/src/receipts.rs:42`):

- `ALTER TABLE receipts ADD COLUMN scratch INTEGER NOT NULL DEFAULT 0`. Guarded by `has_column`, the same pattern as v4 `degraded`, v5 `started_at` and v6 `replay_origin`.
- `method` keeps telling the truth (`cli`). I rejected a new `vault::schema::Method::Distill`: `Method` names the transport, not the policy.

**Inventory of every receipts reader** (`rg "FROM receipts"`), and what each does with `scratch = 1`:

| Reader | Scratch rows | Why |
|---|---|---|
| `borg/src/eval/backlog.rs:26` | excluded | no note to grade |
| `eval::remediate::plan` (`borg/src/eval/remediate.rs:202`) | excluded | the one reader that POSTs to `/ingest`. Scratch staging already sits outside `staged_root`; this filter is the fail-closed guard at the narrowest chokepoint |
| `triage::audit_health_stats` + `count_*` (`borg/src/receipts.rs:962-1061`) | excluded | `/health/audit` and `sb doctor` measure ingest health |
| oracle `failure_history` (`oracle/src/server.rs:642`) | excluded | it reports vault ingest failures |
| `borg/src/backfill.rs:377` | excluded | it stamps note timestamps; scratch rows have no note |
| `replay_one` (`borg/src/replay.rs`) | refused with a named error | replaying a scratch trace re-POSTs to `/ingest` and publishes |
| `receipts::get` behind `GET /trace` (`receipts.rs:811`) | included | the distill client polls it; no filter in `get` |
| watchdog `list_stale` (`receipts.rs:742`) | included | a wedged distill must still be reaped |
| queue snapshot (`borg/src/queue.rs:382`) | included | scratch runs hold permits; they are work in flight |
| `sb borg log` | included | the durable record of every input |
| `cortex/src/proposals.rs:315` | included, harmless | trace-to-note identity only; a scratch trace has no staged dir under `staging.root` to join |
| `eval/weekly/remediated.rs:140` | cannot match | scoped to remediation traces |
| `count_kind_since` (`receipts.rs:1064`) | cannot match | Session kind only |

**`GET /distill/{trace}` manifest:**

```yaml
trace-id: cl-1a2b3c4d
status: succeeded        # succeeded | failed (from receipts)
failure-stage: null
failure-reason: null
files:
  - path: billboard-1989-year-end-top-40.md
    size: 18342
  - path: frames/runs.yml
    size: 9120
  - path: stages/cl-1a2b3c4d/distilled.yml
    size: 40211
```

### API Design

#### Daemon routes

All routes sit behind `require_auth` (`borg/src/server.rs:49-69`).

- **`POST /distill`**, body `{ "url": "..." }`:
  - `record_scratch_with_sidecar` writes the `scratch = 1` row and the sidecar under the scratch root.
  - The pipeline is spawned with `PublishPolicy::Scratch`.
  - Returns `Queued{trace_id}`, the same shape as `/ingest`.
- **`GET /distill/{trace}`:**
  - 404 if the trace is unknown or `scratch = 0`, so it can never serve a vault ingest's staging.
  - 409 if the trace is not terminal.
  - 200 with the manifest otherwise. That includes a `failed` trace, because its partial output is what you debug the failure with.
- **`GET /distill/{trace}/file/{*path}`:**
  - The same 404/409 rules as the manifest.
  - Bytes are served only for a regular file found by a fresh `symlink_metadata` walk of `distill/<trace>/`. Symlinks and special files are never listed or served.
  - Any `..` component or absolute path gets 400. Anything not in the walk gets 404.
- **`health::DAEMON_FEATURES`** gains `FEATURE_DISTILL = "distill"` (`borg/src/health.rs:8`).
- **Why a separate route.** An old daemon silently drops unknown `/ingest` fields, so a distill sent to it would publish into the vault. A field on `IngestRequest` would also land in the extension's JSON schema (`borg/src/extension/schema.rs:8`).

#### Polling

`replay::poll_trace_terminal` (`borg/src/replay.rs:354`) returns `Ok(Failed)` both for a failed trace and for its own ceiling. It changes to a typed outcome:

```rust
pub(crate) enum PollOutcome {
    Terminal(IngestResult),
    CeilingReached,
}
```

- Its two callers (`replay.rs:336` and `reingest_one` at `client.rs:465`) map `CeilingReached` to the same `Failed` reason they produce today, so their behavior is unchanged.
- The ceiling is `hard_timeout_secs + 90`. That bounds the pipeline, not the wait for a permit (`pipeline.rs:242`).
- Under load, the ceiling can arrive while the trace is still queued or running. The client exits 1 and prints the trace id, and the files stay on the daemon for the retention window.

#### CLI

```
sb borg distill <URL> [--output DIR]
```

- **`DIR`:**
  - `--output` defaults to `./<trace-id>/`. A relative `DIR` resolves against the current directory.
  - An explicit `DIR` that exists and is non-empty is refused before any POST.
  - The default `DIR` gets the same check after the POST returns the trace id, before any download.
  - Fail closed: the client never merges into or overwrites a directory.
- **Daemon check.** The client checks `GET /health` first and refuses a daemon without `distill`, following the `DaemonPoster::connect` precedent (`borg/src/eval/remediate.rs:609-630`).
- **Manifest paths are untrusted.**
  - The client rejects any manifest path that is absolute or has a `..` component, before joining it onto `DIR`.
  - It also checks that every joined path stays under `DIR`.
  - A rejected path is a download error (exit 1). A buggy daemon cannot write outside `DIR`.
- **Download timeout.**
  - Each file GET uses the new `hotkey.download-timeout` (humantime, default `300s`, in `vault::daemon::HotkeyConfig` beside `request-timeout`).
  - It is passed through `DaemonClient::get`'s per-call override (`vault/src/daemon/client.rs:126-147`).
  - The 10s `request-timeout` (`vault/src/daemon.rs:20`) is too short for a frames tree over Tailscale.
- **Output.**
  - On success, stdout prints `DIR` and the note path.
  - On exit 1 after the POST, stderr names the trace id. A partial `DIR` is left in place.

| Exit | Meaning |
|---|---|
| 0 | trace succeeded, every manifest file written |
| 1 | daemon unreachable, missing `distill` feature, `DIR` non-empty, a download error or rejected path, or `CeilingReached` |
| 2 | clap usage |
| 3 | `Terminal` with a failed trace (pipeline error, inflight hit, crash); whatever the manifest listed was still written to `DIR` |

Exit 2 matches clap. Exit 3 matches `sb borg wait`'s "drained with failures".

### Implementation Plan

#### Phase 1: `PublishPolicy` threaded through, no behavior change
**Model:** sonnet
- Add `PublishPolicy` to `borg/src/pipeline.rs`.
- Add the `policy` parameter to `process_content`, `process_url`, `process_url_inner` and `process_youtube`. All seven callers and the tests pass `&PublishPolicy::Vault`.
- Key the frames work dir by trace id (`handlers.rs:245`).
- Move whisper's audio to a per-call `TempDir` (`youtube/engine.rs:138`).
- **Success criteria:**
  - `cargo test --workspace` passes, with no existing test edited beyond adding the new argument.
  - `rg -n 'join\(&video_id\)' borg/src/pipeline/handlers.rs` returns nothing.
  - `rg -n '"obsidian-borg"' borg/src/youtube/engine.rs` returns nothing.
  - Observed on main: both commands match (`handlers.rs:245`, `engine.rs:139`), as expected before this phase.

#### Phase 2: honor `Scratch` in the URL pipeline
**Model:** opus
- In `process_content`, before Stage 0: reject non-URL kinds and build the effective config.
- Implement every site-table row except the two frames rows (Phase 3).
- Add `vault::paths::borg_distill_dir()`.
- **Success criteria:**
  - New test `distill_policy_writes_nothing_to_vault`, in `borg/src/pipeline/tests/article.rs` with the existing axum stub:
    - Under `Scratch`, the tempdir vault tree, the shared `staging.root`, `borg-ledger.md` and `archive.root` are byte-identical before and after.
    - `root` holds one top-level `.md`, `stages/<trace>/envelope.yml` and `stages/<trace>/distilled.yml`.
    - Swapping in `Vault` makes the test fail.
  - New test `distill_policy_matches_ingest_note`: with clock and trace pinned, the note `.md` under `Scratch` is byte-identical to the one `Vault` publishes from the same stub.
  - New tests:
    - `distill_policy_refuses_non_url`: nothing is written anywhere, shared staging included.
    - `distill_inflight_hit_fails`: the run is `Failed`, not `Duplicate`, and a concurrent `Vault` run of the same URL is not blocked.
    - `distill_persist_failure_fails`: an unwritable `root/stages` gives `Failed`.

#### Phase 3: preserve frames and OCR every run
**Model:** opus
- Add the preserve destination to `WorkDirGuard` (copy minus `.mp4`, then remove), and route `pipeline.rs:1032` through it.
- Under `Scratch`, OCR every run's best frame and write `frames/runs.yml`.
- **Success criteria:**
  - New test `distill_policy_keeps_frames_on_text_only`, in `borg/src/pipeline/tests/youtube.rs`, with the stub's classifier dropping every run:
    - `root/frames/` holds every stub frame and no `.mp4`.
    - `runs.yml` has one entry per run, each with an `ocr` key.
    - The work dir under `/tmp` is gone.
    - The same test under `Vault` leaves no `root/frames/`.
  - New test `distill_policy_keeps_frames_on_slide_error`: when the stub fails the slide path after frame extraction, `root/frames/` is still populated.
  - The existing youtube publish tests pass unchanged, which shows the note's bytes did not move.

#### Phase 4: receipts `scratch` column and reader handling
**Model:** sonnet
- Add the v8 migration, and `record_scratch_with_sidecar` with the flag in the INSERT.
- Add the five `excluded` filters and the `replay_one` refusal from the inventory table.
- **Success criteria:**
  - Migrating a v7 fixture DB keeps every row, and `select count(*) from receipts where scratch = 1` returns 0.
  - One test per excluded reader, each seeded with a scratch row that would otherwise qualify for it:
    - `succeeded` for backlog, health and backfill.
    - `failed` for `failure_history`.
    - A scored `succeeded` for remediate.

    Each test shows the reader's result unchanged and fails when its filter is removed.
  - `replay_one` on a scratch trace returns an error naming the trace, and the stub records zero `POST /ingest`.

#### Phase 5: daemon routes, feature flag, polling, retention
**Model:** opus
- Add `POST /distill`, `GET /distill/{trace}` and `GET /distill/{trace}/file/{*path}`.
- Add `FEATURE_DISTILL`.
- Add `PollOutcome` and adapt its two callers.
- Add the `run_distill_sweep` task, and extend `sb borg retention sweep` to cover the distill tree.
- **Success criteria:**
  - Route tests:
    - No token, or a wrong one, gets 401 with no receipts row.
    - `../` or an absolute path gets 400, and a symlink in the tree gets 404.
    - An unknown trace gets 404, and a `scratch = 0` trace gets 404.
    - A `received` trace gets 409 on both the manifest and the file route.
  - Retention tests:
    - One pass removes a `distill/<trace>/` older than `staging.retention_days` and keeps a younger one.
    - With `intake.retention-days: 0`, the distill sweep still runs.
  - The existing `reingest_one` and replay tests pass unchanged.

#### Phase 6: `sb borg distill` client
**Model:** sonnet
- Add the clap arm in `sb/src/cli/borg/args.rs`.
- Add the client in `borg/src/client.rs`: health check -> POST -> poll -> manifest -> path check -> per-file GET -> write.
- Add `hotkey.download-timeout`.
- Map exits through `ExitWith`.
- **Success criteria:**
  - Against `borg/src/stub.rs`, `DIR` matches the stub's scratch tree file for file, and the exit code is 0.
  - A stub without the `distill` feature gives exit 1, and the stub records zero `POST /distill`.
  - Other client tests:
    - A non-empty explicit `DIR` gives exit 1 with zero requests.
    - A stub that never reaches terminal gives exit 1, with the trace id on stderr.
    - A failed trace gives exit 3, with the manifest files written.
    - A manifest path `../x` gives exit 1, and nothing is written outside `DIR`.

#### Phase 7: docs
**Model:** sonnet
- `CLAUDE.md`: in the durable-capture section, distill is the one input whose sidecar and output live outside the vault.
- `borg/AGENTS.md`: correct "cross-restart dedup checks the ledger" (it reingests in place, it does not reject), and add distill.
- `borg/src/pipeline/AGENTS.md`: `PublishPolicy` and the site table.
- `sb/AGENTS.md`: the verb.
- **Success criteria:**
  - `rg -l 'sb borg distill' CLAUDE.md borg/AGENTS.md sb/AGENTS.md` lists all three files.
  - `rg -l 'PublishPolicy' borg/src/pipeline/AGENTS.md` lists that file.
  - Observed on main: both commands list nothing (exit 1).

## Acceptance Criteria

- [ ] `sb borg distill --help` exits 0 and lists `--output`.
  - Observed on main (desk, v0.19.1): `error: unrecognized subcommand 'distill'`, exit 2.
- [ ] `curl -s http://ripr.lan:8181/health | jq -c .features` contains `"distill"`.
  - Observed on main: `{"status":"ok","service":"obsidian-borg","version":"v0.19.1","features":["replay-of"]}`.
- [ ] The vault-untouched shakedown. Run it while `sb borg queue` reports `state: idle` before and after, with no other capture in flight.
  - Before, on ripr: `touch /tmp/distill-marker`.
  - Run `sb borg distill <article-url> --output DIR`. It exits 0.
  - After, on ripr: `find ~/repos/scottidler/obsidian/inbox ~/repos/scottidler/obsidian/system/intake ~/repos/scottidler/obsidian/system/attachments -newer /tmp/distill-marker -type f` prints nothing.
  - After, on ripr: `sha256sum ~/.local/share/sb/borg/borg-ledger.md` is unchanged.
  - After, on ripr: `ls ~/.local/share/sb/borg/stages/<trace>` fails with "No such file or directory".
  - After: `DIR` holds exactly one top-level `*.md` and `stages/<trace>/distilled.yml`.
  - Observed on main: cannot run until Phase 6 ships (the verb does not exist; see the first criterion).
- [ ] On ripr, `sqlite3 ~/.local/share/sb/borg/receipts.db "select scratch from receipts where trace_id = '<trace from the shakedown>'"` returns `1`.
  - Observed on main (desk): `select count(*) from receipts where scratch=1` -> `Error: in prepare, no such column: scratch`.
- [ ] `sb borg distill https://www.youtube.com/shorts/y0HOOCNxjO4 --output DIR` (the motivating Short) exits 0, and `DIR/frames/runs.yml` exists.
  - Its entry count equals `classified=` in that trace's `slide-filter tally[<trace>]` line in `~/.local/share/sb/borg.log` on ripr.
  - Every entry carries an `ocr` key.
  - Observed on main: cannot run until Phase 6 ships. Today ripr keeps only `architecture-diagram` runs (`youtube.slides.content-filter.keep`), and the frames work dir is removed at `handlers.rs:222` or `pipeline.rs:1032`.

## Resolved Decisions

**2026-10-25, Scott:**
- Separate verb, not `ingest --output`.
- The verb is named `distill`, after the last stage it runs. I rejected `extract`, which collides with the `stages/extract` stage while running more than it.
- Runs through the daemon.
- Ledger and dedupe off; zero vault writes; receipts row kept and marked scratch; raw stage output beside the note.

**2026-10-25, author, from code reading:**
- A receipts `scratch` column, not `Method::Distill`.
- Scratch staging under a separate root.
- A separate `/distill` route.
- An enum policy.
- Manifest plus per-file GET.
- A separate inflight namespace.
- A failed trace still downloads.
- Retention reuses `staging.retention_days`.
- **OCR every run under `Scratch`.** The content filter still decides the note. `runs.yml` adds OCR for every run so the motivating case works under ripr's `keep: [architecture-diagram]`. The alternative was to turn the filter off under `Scratch`, but then the note would differ from ingest's.

**2026-10-25, review-panel round 1** (synthesis `~/.cache/review-panel/runs/mxyMUgPM/synthesis.md`), all folded in:
- **Must-fix:**
  - Resolve the effective config before Stage 0, not at `process_url` (both seats).
  - Preserve the whole frames work dir whatever the note's shape (Staff). Pass 2 found that the fix as first written covered only the success path; it now covers the guard's drop too.
  - Split exit 1 from exit 3 with `PollOutcome` (Staff).
  - Give whisper audio a per-call `TempDir` (Staff).
- **Cheap wins:**
  - The distill tree is auto-swept, and the false `max-size-gb` alerting claim is removed.
  - A staging persistence failure fails the run.
  - `PerTrace` layout is forced.
  - The default `DIR` gets the non-empty check.
  - The Phase 4 tests seed qualifying rows.
  - The shakedown asserts its own trace's row, plus a `-newer` check on the vault write targets.
- **Defers, resolved in the doc instead:**
  - `replay_one` refuses scratch traces, with a negative test.
  - The file route serves regular files only.
  - `--trace` re-download is parked.
  - Rollback needs nothing, because the client refuses a daemon without the feature.
- **Severity pushback, accepted in the panel's own synthesis:** the Stage-0 leak never inflated tag proposals, since proposals read only `distilled.yml`. It did leak trace dirs that `sb borg replay` could see, and the Stage-0 fix plus the replay refusal close both.
- **Unrequested scope:** `--note` is dropped (Scott did not ask for it) and parked.

## Alternatives Considered

### `ingest --output DIR`
- **Pros:** no new verb.
- **Cons:** "ingest" means "lands in the vault" everywhere else in borg. An old daemon drops the unknown field and publishes.
- **Why not chosen:** Scott chose a separate verb.

### Run the pipeline locally in the client
- **Pros:** no routes, no transport.
- **Cons:** it bypasses the daemon's yt-dlp breaker and permits, and it depends on the calling shell's secrets (Gemini, fabric).
- **Why not chosen:** Scott chose the daemon.

### `Method::Distill` instead of a receipts column
- **Pros:** the trace prefix identifies scratch runs without a DB join.
- **Cons:** a vault-schema change for a borg-only concern; it mixes transport and policy; the note would read `method: distill`.
- **Why not chosen:** a column is a local, additive migration.

### Shared `stages/` root with receipts-joined filters
- **Pros:** no second root.
- **Cons:** every staged-dir reader needs a filter. The panel showed how a single missed site (Stage 0) leaks.
- **Why not chosen:** a structural exclusion beats N filters.

### Override the config at `process_url` entry
- **Pros:** narrower change.
- **Cons:** Stage 0 runs earlier, in `process_content` (`pipeline.rs:254`), and would write to the shared root.
- **Why not chosen:** the panel found the leak.

### Turn the content filter off under `Scratch`
- **Pros:** `slides.yml` alone would carry OCR for every slide; no `runs.yml`.
- **Cons:** the note would differ from what ingest publishes, which breaks "same pipeline", and it makes `distill_policy_matches_ingest_note` impossible for YouTube.
- **Why not chosen:** `runs.yml` gets the motivating data without changing the note.

### Single JSON response with base64 bodies
- **Pros:** one GET.
- **Cons:** base64 bloat on a frames tree, one large allocation, and one slow request racing a timeout.
- **Why not chosen:** a manifest plus per-file GET is simpler to bound and needs no new crate.

## Technical Considerations

### Dependencies
- No new crates: axum, reqwest, serde_yaml and tempfile are already in the workspace.
- No cross-repo blast radius: everything lives in `second-brain`.

### Performance
- Distill competes with ingest for `GENERAL_PERMITS` and `HEAVY_PERMITS` (`borg/src/pipeline/permits.rs:100`). That is intended: one daemon host, one budget.
- `runs.yml` adds one tesseract call per run, under the heavy permit `process_youtube` already holds (`handlers.rs:87`). Each call is bounded by `pipeline.ocr-timeout-secs`. Under `Scratch` only.
- The disk cost per run is the note, the frames (no `.mp4`), attachments and staging. `run_distill_sweep` bounds it at `staging.retention_days`.

### Security
- Every distill route sits behind the existing bearer token (`require_auth`, `constant_time_eq`).
- The file route serves only regular files from a `symlink_metadata` walk of `distill/<trace>/`, and rejects `..` and absolute paths before any filesystem call.
- `GET /distill/{trace}` returns 404 for any `scratch = 0` trace.
- The client treats manifest paths as untrusted and never writes outside `DIR`.

### Testing Strategy
- Pipeline tests use the existing article and youtube stubs with `receipts::sandbox::enter()` and a tempdir `XDG_DATA_HOME`.
- Route tests use the existing router harness. Client tests use `borg/src/stub.rs`.
- Every exclusion and every zero-write assertion has a mutation partner: swap the policy or drop the filter, and the test must fail.

### Rollout Plan
- Ship order: the daemon on ripr first, then the clients. A client refuses a daemon without `distill`, so the wrong order fails loudly instead of publishing.
- `bump`, then `otto deploy` on ripr, desk and lappy.
- Shakedown: acceptance criteria 3-5. The YouTube one runs on the motivating Short.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| A missed vault or shared-staging write under `Scratch` | Med | High | `distill_policy_writes_nothing_to_vault` checks the vault tree, shared staging, ledger and archive, with a mutation partner; the shakedown repeats it live |
| `eval --remediate` or `sb borg replay` re-POSTs a scratch trace to `/ingest` | Low | High | no staged dir under `staging.root`; remediate filters `scratch`; replay refuses; each has a test |
| Frames lost on a path the guard does not cover | Low | Med | the guard's `Drop` is the single exit; text-only and slide-error tests |
| The frames tree inflates disk | Med | Low | no `.mp4`; `run_distill_sweep` |
| The poll ceiling arrives before a queued distill starts | Med | Low | exit 1 with the trace id; files stay on the daemon; `--trace` is parked |
| The effective config drifts from the live config | Low | Med | one clone per run, in `process_content`; only `staging.root`, `staging.enabled` and `staging.layout` are overridden |

## Open Questions

- None.

## References

- Session `c7cd46e1-9bf8-4f79-8607-af7b70edb37a` (motivating case, ripr, 2026-10-25)
- Review-panel round 1 synthesis: `~/.cache/review-panel/runs/mxyMUgPM/synthesis.md`
- `docs/design/2026-10-04-ingest-queue-status.md` (`/queue`, `sb borg wait` exit-code precedent)
- `docs/design/2026-06-03-receipts-log-legacy-markdown-excision.md` (receipts as the sole failure store)
- `docs/design/2026-09-21-staged-tag-proposals.md` (proposals read staged `distilled.yml`)

## Pass Log

1. **Draft.** Shape from the conversation and the design-research brief.
2. **Correctness.** Checked each claim against the code. Fixed:
   - The frames work dir dies in `WorkDirGuard`'s drop on the text-only path (`handlers.rs:222,357-363`), not only at `pipeline.rs:1032`. The panel's fix covered only the success path.
   - The content filter OCRs kept runs only, and ripr keeps only `architecture-diagram`, so the motivating case got no OCR. Added `runs.yml`.
   - `record_received_with_sidecar_to` is private. Added the public `record_scratch_with_sidecar`.
   - `run_sidecar_sweep` returns early on `intake.retention-days: 0`. Distill gets its own sweep task.
   - The `poll_trace_terminal` caller cite moved from `client.rs:443` to `:465`.
3. **Clarity.**
   - One term, "frames work dir", replacing three.
   - "Effective config" is defined once.
   - The old Phase 2 is split into the URL policy (Phase 2) and frames (Phase 3), giving seven phases.
   - The Problem section now names all four hidden facts.
4. **Edge cases.**
   - The `.mp4` is excluded from the frames tree.
   - The client treats manifest paths as untrusted.
   - The file route follows the manifest's 404/409 rules.
   - The scratch flag is set in the INSERT, so a crash cannot miscount.
   - The download timeout is config (`hotkey.download-timeout`), not hardcoded.
   - A relative `DIR` is resolved, and a partial `DIR` is left on exit 1.
5. **Excellence.**
   - Acceptance criterion 5 now runs on the motivating Short and is checked against the daemon's own tally line, so the feature is judged on the case that asked for it.
   - Added an alternative for turning off the content filter.
   - Risk rows cover the guard path.
