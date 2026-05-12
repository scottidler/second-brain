# Design Document: Borg Pipeline Resilience

**Author:** Scott Idler
**Date:** 2026-05-08
**Status:** Draft (post-architect-review amendments)
**Review Passes Completed:** 5/5 + architect review (2026-05-08)

## Architect Review (2026-05-08)

This doc was reviewed by the Architect persona before implementation. Three findings were incorporated as amendments:

1. **Metadata patching breaks atomicity (Risk A).** `patch_note_date` and `patch_cortex_fields` (`pipeline.rs:2478` and `:2535`) each do `read_to_string + write`, called immediately after the initial `std::fs::write` at `pipeline.rs:542`. The original draft fixed only the first write; a SIGKILL between writes 2 and 3 would still desync the note body from its `original_date` and cortex fields. **Fix:** refactor `patch_*` into pure-string helpers (`apply_original_date`, `apply_cortex_fields`) and compose the final note in memory before a single `write_atomic` call. Phase 3 amended.
2. **Blocking child processes defeat the timeout backstop.** `youtube::fetch_subtitles_raw` calls `std::process::Command::new("yt-dlp").output()` synchronously from inside an `async fn` (`youtube.rs:79`); same pattern at `pipeline.rs:775`, `ocr.rs`, `fabric.rs`. A hung `yt-dlp` blocks the worker thread; `tokio::time::timeout` cancels the future but the child process keeps running. **Fix:** migrate these callsites to `tokio::process::Command` with per-call timeout + `child.start_kill()` on elapsed. The pipeline-wide `tokio::time::timeout` is now framed as a backstop, not a liveness guarantee. Phase 1 amended.
3. **Explicit dot-prefix for the atomic temp file.** Switched from `tempfile::NamedTempFile::new_in` to `tempfile::Builder::new().prefix(".borg-tmp-").tempfile_in(...)` so the temp's invisibility to Obsidian / git / other watchers is deterministic, not dependent on the tempfile crate's default naming.

Two findings were noted but **not** elevated to architectural risks of equal weight to the original data-loss bug:

- **Risk framing.** The metadata-patching gap is a real bug introduced by the proposed fix and is now corrected, but it is a localized refactor (replace two helpers with two pure functions), not a structural concern that should expand scope toward the staged-pipeline redesign. Tracking it as a Phase 3 amendment, not as a top-level risk.
- **`spawn_blocking` "permanent thread leak" framing.** `spawn_blocking` futures detach when the JoinHandle drops; the underlying thread is held only until the running task completes naturally. With the per-call timeout + `kill()` migration above, the child process is killed promptly and the thread releases within milliseconds. The fix is the same; the framing is "thread is held until child exits or is killed," not "thread leaks permanently."

The Architect concurred that the tactical scope is correct: ship these fixes ahead of the staged-ingestion redesign in `docs/design/2026-04-19-staged-ingestion-pipeline.md`.

## Summary

A YouTube ingestion failed mid-pipeline and silently destroyed an existing vault note. Three independent defects in `borg/src/pipeline.rs` combine to make this failure mode possible: the reingest path deletes the old note before writing the new one, the in-memory inflight dedup guard is not released when a task is dropped or hangs, and the subtitle fetcher has no timeout so a stuck request can leave the pipeline silently abandoned with no error log. This doc proposes three targeted fixes that restore the invariant "a failed ingestion never destroys data" and make pipeline failures observable.

## Problem Statement

### Background

`borg` runs as a long-lived systemd user daemon that ingests URLs into the Obsidian vault. When the same URL is re-ingested (e.g., a Telegram message resends a YouTube link), the pipeline supersedes the existing note: it locates the old note via the SQLite ledger, captures its cortex-owned frontmatter (tags, quality issues, curation state, dates), then writes a fresh copy that preserves those fields. This is the "reingest-domain-preservation" behavior.

The pipeline is structured as `process_url -> process_url_inner` in `borg/src/pipeline.rs`. The outer function provides the top-level `Result` boundary: on `Ok` it logs `Pipeline completed`, on `Err` it logs `Pipeline failed` and writes a `Failed` ledger entry. Concurrent ingestions of the same canonicalized URL are blocked via a process-wide `LazyLock<Mutex<HashSet<String>>>` of inflight URLs.

### Problem

On 2026-05-08, ingestion `ht-fb0810` for a YouTube video (the "5 million people are obsessed with Excalidraw" video) executed the reingest path: it deleted `notes/why-5-million-people-are-obsessed-with-excalidraw.md` at `pipeline.rs:404`, then never wrote a replacement. No error was logged; no `Failed` ledger entry was written. Four minutes later, `ht-8dd5d7` retried the same URL and was rejected as `Duplicate (inflight)` in 3.82ms - the inflight set still contained the canonical URL from `ht-fb0810`. The deleted note was recoverable from git only because it had been committed previously.

Three concrete defects:

1. **Non-atomic replace** (`pipeline.rs:404`): `std::fs::remove_file(old_path)?` runs immediately after the old note is located, ~134 lines before the new note is written at `pipeline.rs:538`. Any failure in the intervening fetch / transcribe / fabric / render path leaves a hole in the vault.
2. **Silent abandonment**: Between the subtitle fetch at `youtube.rs:119` and the next request, the journal contains no `Pipeline failed` line. The outer `match` in `process_url` (`pipeline.rs:255-305`) only fires when `process_url_inner` returns. If the inner future hangs forever (no timeout on `reqwest::get(sub_url)`) or panics in a way the runtime swallows, neither branch runs.
3. **Inflight guard leak**: Cleanup happens at `pipeline.rs:592` (success path) and `pipeline.rs:272` (failure path). Both depend on `process_url_inner` returning. A future that is dropped, panics, or hangs never releases the guard, and every subsequent retry of the same URL short-circuits with `Duplicate (inflight)`.

### Goals

- A failed ingestion must never destroy data the user previously had. Reingest is atomic from the user's perspective: either the new note replaces the old one, or the old note is preserved unchanged.
- Every pipeline run terminates with a logged outcome: `Pipeline completed`, `Pipeline failed`, or `Pipeline timed out`. There is no path that leaves a trace ID without a terminal log line.
- The inflight guard is released for every terminal outcome, including timeout, panic, and task cancellation. A retry of a previously-failed URL is never silently rejected.
- Subtitle fetches and other YouTube auxiliary requests cannot hang the pipeline indefinitely.

### Non-Goals

- Redesigning the pipeline as the staged, replayable architecture described in `2026-04-19-staged-ingestion-pipeline.md`. This doc is a tactical correctness fix that lands before that larger redesign.
- Adding cross-process dedup. The inflight set is process-local; that is unchanged.
- Recovering vault notes deleted by past instances of this bug. Git is the recovery mechanism.
- Changing the reingest-domain-preservation semantics (which cortex fields carry forward, which dates are preserved). Behavior is identical post-fix.

## Proposed Solution

### Overview

Three localized changes to `borg/src/pipeline.rs` and `borg/src/youtube.rs`:

1. **Atomic publish-or-revert.** Don't delete the old note in the middle of the pipeline. Capture the old note's metadata up front, run the full pipeline, write the new note bytes to a sibling temp file, fsync, rename over the destination, and only then (if the destination differs from the old path) delete the old note.
2. **Drop-safe inflight guard.** Replace direct `INFLIGHT.insert/remove` calls with a RAII guard struct whose `Drop` impl releases the entry. This makes cleanup automatic for every termination mode: success, error, panic, timeout, cancellation.
3. **Bounded waits.** Add `.timeout()` on the subtitle fetcher (and other unbounded `reqwest::get` calls in the YouTube extraction path). Wrap `process_url_inner` in `tokio::time::timeout(PIPELINE_HARD_TIMEOUT, ...)` to backstop any blocking call we missed.

### Architecture

#### Atomic publish (Bug 1)

The current flow has **three** non-atomic writes to the destination, not one (see Architect Review note below for the discovery context):

```
process_url_inner (today):
  find old note path -> capture cortex fields -> remove_file(old_path)
  ... 134 lines of fetch / transcribe / fabric / render ...
  std::fs::write(note_path, rendered)                 # write 1: pipeline.rs:542
  patch_note_date    -> read_to_string + write        # write 2: pipeline.rs:546 -> 2491
  patch_cortex_fields-> read_to_string + write        # write 3: pipeline.rs:552 -> 2559
```

A `SIGKILL` or panic between any of writes 1, 2, 3 leaves the note on disk with partial state: body present but `original_date` missing, or both present but `cortex-quality-issues` etc. missing. The atomic-publish proposal is incomplete unless the metadata patches are folded into the in-memory composition. Becomes:

```
process_url_inner (proposed):
  find old note path -> capture cortex fields -> remember old_path
  ... fetch / transcribe / fabric / render produces rendered:String ...
  rendered = apply_original_date(rendered, original_date)        # in-memory
  rendered = apply_cortex_fields(rendered, cortex_fields)        # in-memory
  write_atomic(dest_path, rendered.as_bytes())                   # single durable write
  if old_path.is_some() && old_path != dest_path: remove old_path
```

`apply_original_date` and `apply_cortex_fields` replace `patch_note_date` and `patch_cortex_fields`. Same parsing logic, same correctness; they take and return `String` instead of opening the file. The existing functions are deleted (their only callers are the publish path; tests at `pipeline.rs:3293`/`3316`/`3338` are migrated to the new in-memory shape).

This means: after `write_atomic` returns Ok, the file on disk contains the final body, the restored original date, and all preserved cortex fields. There is no "10ms after persist before patch" window where a SIGKILL can desync them.

Two cases:

- **Same path** (most common: reingest in place). `dest_path == old_path`. The atomic `rename` from a temp file in the same directory replaces the old note in a single inode operation. No separate delete needed.
- **Different path** (rare: the old note is in `notes/` and the heuristic now resolves to `inbox/`, or vice versa). Write the new note atomically to `dest_path`, then delete `old_path`. If the rename succeeds and the delete fails, the user has a transient duplicate but no data loss; cortex's existing duplicate detection surfaces it.

The pre-pipeline read of cortex fields and dates does not change. The only motion is: defer the delete until after the write.

A new helper `write_atomic(dest: &Path, bytes: &[u8]) -> Result<()>` lives in a new `pipeline/atomic.rs` submodule (the parent `pipeline.rs` is already at 3350 lines, so new code goes into a sibling file rather than the existing one; see Risks). Use the `tempfile` crate already in `borg/Cargo.toml`:

```rust
use std::io::Write;
use tempfile::Builder;

fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<()> {
    let parent = dest.parent().context("dest has no parent")?;
    // Explicit dot-prefix makes the temp file deterministically invisible to
    // Obsidian's indexer and well-behaved file-watching tools, regardless of
    // tempfile's default naming conventions.
    let mut temp = Builder::new()
        .prefix(".borg-tmp-")
        .tempfile_in(parent)
        .with_context(|| format!("create temp in {}", parent.display()))?;
    temp.write_all(bytes).context("write temp bytes")?;
    temp.as_file().sync_all().context("fsync temp")?;
    temp.persist(dest)
        .with_context(|| format!("persist temp -> {}", dest.display()))?;
    // Best-effort fsync of the parent directory so the new dirent is durable
    // across a power loss. Not required to defeat the failure mode in this doc,
    // but cheap insurance and standard practice for atomic-write helpers.
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}
```

`NamedTempFile` removes the temp file in its `Drop` if any of `write_all`/`sync_all` errors before `persist`, so an interrupted write does not leak. `persist` performs a same-FS rename and returns the `NamedTempFile` back on failure so we never lose the bytes silently.

The temp file's name (`.borg-tmp-<random>`) is dot-prefixed and does not end in `.md`, so it is invisible to the vault watcher (`vault/src/watcher.rs:134` filters on `extension() == Some("md")`), invisible to Obsidian's indexer (dotfile convention), and easy to ignore in `.gitignore`. Concurrent runs cannot collide on the same temp path because of the random suffix.

#### Drop-safe inflight guard (Bug 3)

The current `INFLIGHT` is `LazyLock<tokio::sync::Mutex<HashSet<String>>>` (`pipeline.rs:182`). Switch the mutex to `std::sync::Mutex` so `Drop` can lock synchronously without an `await`. The set is touched twice per ingestion for microseconds; the lock is never held across an `.await`, so a sync mutex is correct in async context. (`parking_lot::Mutex` is not a current direct workspace dep; staying on `std::sync::Mutex` avoids adding one.)

```rust
struct InflightGuard {
    canonical: String,
}

impl InflightGuard {
    fn try_acquire(canonical: &str) -> Option<Self> {
        let mut set = lock_inflight();
        if set.contains(canonical) {
            None
        } else {
            set.insert(canonical.to_string());
            Some(Self { canonical: canonical.to_string() })
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        // Tolerate a poisoned mutex (a prior panic while holding it).
        // Panicking from Drop during unwind aborts the process - we MUST NOT.
        let mut set = lock_inflight();
        set.remove(&self.canonical);
    }
}

fn lock_inflight() -> std::sync::MutexGuard<'static, HashSet<String>> {
    // Poisoning means a previous panic occurred while holding this lock.
    // The inner data is a HashSet<String> with no invariants to protect,
    // so recovering it via into_inner() is safe and we proceed to remove
    // our entry. The alternative - panicking from Drop - would abort the
    // process during unwind.
    match INFLIGHT.lock() {
        Ok(g)         => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}
```

Replace the `inflight.insert(canonical.clone())` at `pipeline.rs:367` with a named binding (the value is in use - its `Drop` does the cleanup work, so the binding must outlive the rest of the function; per `rules/rust.md`, no `_`-prefix on variables in production code):

```rust
let inflight_guard = match InflightGuard::try_acquire(&canonical) {
    Some(g) => g,
    None => {
        // Existing duplicate-inflight ledger entry, then return Duplicate.
        ledger::append_entry(&ledger_file, &LedgerEntry {
            date: log_date, time: log_time,
            method: method.into(), status: LedgerStatus::Skipped,
            filename: None, source: canonical.clone(),
            domain: None, trace_id: Some(trace_id.to_string()),
        })?;
        return Ok(IngestResult {
            status: IngestStatus::Duplicate { original_date: "inflight".into() },
            // ... existing fields ...
        });
    }
};
// inflight_guard lives until end of scope; its Drop releases the entry on
// every termination mode (success, error, panic-unwind, timeout-cancel).
```

Remove the explicit `INFLIGHT.lock().await.remove(...)` calls at `pipeline.rs:272` and `pipeline.rs:592`. The guard's `Drop` covers all paths, including the panic and timeout paths added in fix #2.

`--force` continues to bypass the dedup check (today's behavior at `pipeline.rs:341`). Two concurrent `--force` runs on the same URL is a user-driven race; the trace-id-suffixed temp path prevents temp collision but the final `rename` is last-writer-wins. Document and accept; out of scope.

#### Bounded waits (Bug 2)

Three layers (the third layer was added after architect review - see note below):

1. **Per-call timeouts on async network IO.** Replace `reqwest::get(sub_url).await` at `youtube.rs:119` with a `Client` that has `.timeout(SUBTITLE_FETCH_TIMEOUT)`:

   ```rust
   let client = reqwest::Client::builder()
       .timeout(Duration::from_secs(SUBTITLE_FETCH_TIMEOUT_SECS))
       .build()?;
   let response = client.get(&sub_url).send().await
       .with_context(|| format!("subtitle fetch failed: {sub_url}"))?;
   ```

   Audit other `reqwest::get` call sites (`youtube.rs`, `jina.rs`, `extraction.rs`, `description.rs`) and add the same. Define a shared `static CLIENT: LazyLock<reqwest::Client>` in `youtube.rs` so we don't construct a fresh client per request.

2. **Async child processes with explicit kill on timeout.** Today, `youtube::fetch_subtitles_raw` (`youtube.rs:79`) calls `std::process::Command::new("yt-dlp").output()` *inside an `async fn`*. This is a blocking syscall on a Tokio worker thread; it does not yield, and `tokio::time::timeout` on a parent future cannot preempt it - the worker thread is held until `yt-dlp` returns naturally. The same pattern appears at `pipeline.rs:775` (yt-dlp subtitle download) and in `ocr.rs` / `fabric.rs` (other `Command::new(...).output()` callsites).

   Migrate every `std::process::Command` callsite reachable from `process_url_inner` to `tokio::process::Command`, wrap each in a per-call `tokio::time::timeout`, and on elapsed call `child.kill().await`:

   ```rust
   use tokio::process::Command as TokioCommand;
   use tokio::time::{timeout, Duration};

   let mut child = TokioCommand::new("yt-dlp")
       .args([...])
       .stdout(Stdio::piped())
       .stderr(Stdio::piped())
       .spawn()
       .context("spawn yt-dlp")?;

   let output = match timeout(Duration::from_secs(YT_DLP_TIMEOUT_SECS), child.wait_with_output()).await {
       Ok(res) => res.context("yt-dlp wait failed")?,
       Err(_elapsed) => {
           if let Err(e) = child.start_kill() {
               log::warn!("failed to send kill to yt-dlp: {e}");
           }
           let _ = child.wait().await;  // reap the killed process
           return Err(eyre!("yt-dlp timed out after {YT_DLP_TIMEOUT_SECS}s"));
       }
   };
   ```

   Without this, layer 3 (the pipeline-wide `tokio::time::timeout`) cancels the *future* but the thread holding the blocking `output()` call continues running until the OS or the child exits on its own; the child process leaks. The per-call timeout + `kill()` is the actual liveness guarantee for child processes; layer 3 is a backstop only for awaitable code paths.

   Sites to migrate (verified by grep on `2026-05-08`):
   - `borg/src/youtube.rs:79` - yt-dlp subtitle fetch
   - `borg/src/pipeline.rs:775` - yt-dlp subtitle download in slide pipeline
   - `borg/src/ocr.rs:4` (`use std::process::Command;`) - tesseract OCR
   - `borg/src/fabric.rs:3` (`use std::process::Command;`) - fabric LLM call

   `fabric` and `ocr` already run inside `spawn_blocking` (`pipeline.rs:627`, `pipeline.rs:1020`), which means the inner `Command::output()` is allowed to block the blocking-pool thread. Migrating these to async-aware Command + per-call timeout is still required because **`tokio::task::spawn_blocking` futures do not propagate cancellation**: dropping the JoinHandle does not signal the running task. Today, a hung `fabric` or `ocr` invocation will tie up a blocking-pool thread until the child process exits naturally, even if the outer pipeline timed out 30 minutes ago. Per-call timeout + `kill()` inside the spawn_blocking closure (or, simpler, via `tokio::process::Command` outside `spawn_blocking`) is the fix.

3. **Top-level pipeline timeout.** Wrap the inner future in `process_url`:

   ```rust
   let outcome = tokio::time::timeout(
       Duration::from_secs(PIPELINE_HARD_TIMEOUT_SECS),
       process_url_inner(url, tags, method, force, config, trace_id),
   ).await;
   match outcome {
       Ok(Ok(result))   => { /* existing success branch */ }
       Ok(Err(e))       => { /* existing failure branch */ }
       Err(_elapsed)    => {
           log::error!(
               "[{trace_id}] Pipeline timed out after {PIPELINE_HARD_TIMEOUT_SECS}s for {url}"
           );
           // Mirror the existing failure-path ledger write at pipeline.rs:279-291,
           // except status stays Failed and the in-memory IngestStatus carries
           // reason = "timeout".
           let canonical = hygiene::normalize_url(url, &config.canonicalization.rules)
               .unwrap_or_else(|_| url.to_string());
           let _ = ledger::append_entry(&ledger::ledger_path(config), &LedgerEntry {
               date: now_date(config), time: now_time(config),
               method: method.into(), status: LedgerStatus::Failed,
               filename: None, source: canonical.clone(),
               domain: None, trace_id: Some(trace_id.to_string()),
           });
           IngestResult {
               status: IngestStatus::Failed { reason: "timeout".into() },
               // ... existing fields ...
           }
           // InflightGuard drops automatically with the cancelled inner future.
       }
   }
   ```

   When `tokio::time::timeout` fires, the inner future is dropped. The `InflightGuard` drops with it, releasing the entry. This is the load-bearing reason fix #3 must use RAII rather than explicit cleanup.

   Caveat (added after architect review): `tokio::time::timeout` cancels at `.await` points. It does **not** kill the underlying OS thread, child process, or `spawn_blocking` task. Layer 2's per-call timeout + `kill()` is the real liveness guarantee for child-process work; layer 3 is a backstop for awaitable code paths and a way to surface a `Pipeline timed out` log line for the operator.

   Pick `PIPELINE_HARD_TIMEOUT_SECS` as **1800** (30 minutes): long enough for a multi-hour video that still completes a Whisper transcription, short enough that a stuck ingestion is noticed within the same human session. Make it a `const` per `rules/rust.md` (no magic numbers); also expose it as a config field with the const as the default.

### Data Model

No schema changes. No ledger format changes. `LedgerEntry` has no `reason` field today; the timeout outcome writes the existing `LedgerStatus::Failed`. The reason string `"timeout"` lives on the in-memory `IngestStatus::Failed { reason }` returned to the caller, alongside existing reason strings.

Optionally, an `IngestStatus::TimedOut` variant distinct from `Failed { reason }` is cleaner type-wise (see Open Questions). It does not change the on-disk ledger format either way.

### API Design

No public API changes. CLI flags unchanged. Config additions:

```yaml
# borg.yml additions
pipeline:
  hard-timeout-secs: 1800
  subtitle-fetch-timeout-secs: 30
```

Both have sensible defaults via `serde(default = ...)`. Existing configs continue to work without edits.

### Implementation Plan

#### Phase 1: Bounded waits and structured timeout
**Model:** opus (was sonnet pre-architect-review; the child-process migration adds enough subtlety that opus is the right call)

- Add `PIPELINE_HARD_TIMEOUT_SECS`, `SUBTITLE_FETCH_TIMEOUT_SECS`, `YT_DLP_TIMEOUT_SECS`, `OCR_TIMEOUT_SECS`, `FABRIC_TIMEOUT_SECS` as module-level `const` (also exposed via config with defaults).
- Replace `reqwest::get(sub_url).await` at `youtube.rs:119` with a `Client` builder that sets `.timeout(...)`. Define a shared `LazyLock<Client>` in `youtube.rs`.
- Audit `youtube.rs`, `jina.rs`, `extraction.rs`, `description.rs` for other unbounded `reqwest::get` calls; add timeouts.
- **Migrate child-process callsites from `std::process::Command` to `tokio::process::Command`** with per-call `tokio::time::timeout` + `child.start_kill()` on elapsed:
  - `borg/src/youtube.rs:79` (yt-dlp subtitle fetch)
  - `borg/src/pipeline.rs:775` (yt-dlp subtitle download in slide pipeline)
  - `borg/src/ocr.rs` (tesseract)
  - `borg/src/fabric.rs` (fabric)
  Inside `spawn_blocking` closures, the migration is the same; the closure becomes async-shape via `Handle::current().block_on(...)` if needed, or the closure is hoisted out of `spawn_blocking` since async `Command` does not need a blocking thread.
- Wrap `process_url_inner(...)` in `tokio::time::timeout(...)`. Add the timeout-elapsed branch that logs `Pipeline timed out` and writes a `Failed` ledger entry with reason `timeout`.
- Unit tests:
  - A stub fetch that hangs for 60s with a 1s timeout returns an `IngestStatus::Failed` and emits the timeout log line.
  - A `tokio::process::Command` invoking `sleep 60` with a 1s timeout returns `Err` and the child process is killed (verify with `child.id()` no longer in `/proc` after 200ms).

#### Phase 2: Drop-safe inflight guard
**Model:** opus

- Switch `INFLIGHT` from `tokio::sync::Mutex` to `std::sync::Mutex`. No new direct dep. The lock is held microseconds and never across an `.await`, so this is correct in async context.
- Add `InflightGuard` with `try_acquire` and `Drop`. Place in a new `pipeline/inflight.rs` submodule alongside the existing `pipeline.rs` (the new code does not require decomposing the existing file; that is a separate refactor, see Risks).
- Replace `inflight.insert(...)` (currently `pipeline.rs:367`) with a call to `InflightGuard::try_acquire(...)`. Remove the explicit `INFLIGHT.lock().await.remove(...)` at the failure path (`pipeline.rs:272`) and the success path (`pipeline.rs:592`).
- Test: after a simulated timeout (Phase 1), a second `process_url` call for the same canonical URL is not rejected as `Duplicate (inflight)`.
- Test: panic injected mid-pipeline still releases the inflight entry (`Drop` runs during unwind; the test wraps in `std::panic::catch_unwind` to observe).

#### Phase 3: Atomic publish-or-revert
**Model:** opus

- In `process_url_inner`, retain the up-front capture of `original_date`, `cortex_fields`, `reingest_dest`, `old_slides_frontmatter`, and `old_path`. Replace the eager `std::fs::remove_file(old_path)?` at `pipeline.rs:404` with a stored `Option<PathBuf>` (`old_path_to_delete`).
- **Refactor `patch_note_date` and `patch_cortex_fields` to operate on `String` instead of `Path`**, returning new strings rather than reading and writing the file (mandated by architect review - the original three-write publish path was non-atomic across writes 2 and 3 even after fixing write 1):
  - `apply_original_date(rendered: &str, date: &str) -> String` (replaces `patch_note_date`)
  - `apply_cortex_fields(rendered: &str, fields: &[(String, String)]) -> String` (replaces `patch_cortex_fields`)
  - Migrate existing tests at `pipeline.rs:3293`, `:3316`, `:3338` to the new in-memory shape.
- After the pipeline produces the new note bytes (around `pipeline.rs:538`):
  1. Build `dest_path` as today.
  2. Compose the final bytes in memory:
     ```rust
     let mut final_str = rendered;
     if let Some(orig) = &original_date    { final_str = apply_original_date(&final_str, orig); }
     if !cortex_fields.is_empty()          { final_str = apply_cortex_fields(&final_str, &cortex_fields); }
     ```
  3. Call `write_atomic(&dest_path, final_str.as_bytes())?`. This is the single durable write.
  4. If `old_path_to_delete.is_some() && old_path_to_delete.as_ref() != Some(&dest_path)`, `std::fs::remove_file(p)`. Log a `warn!` with the trace ID if this remove fails (non-fatal: the new note exists).
- Delete the now-unused `patch_note_date` and `patch_cortex_fields` (no other callers exist; verified by grep).
- Add `write_atomic` helper (in `pipeline/atomic.rs`) using `tempfile::Builder::new().prefix(".borg-tmp-").tempfile_in(parent) -> write_all -> sync_all -> persist(dest)`. Unit test: a simulated error between `write_all` and `persist` leaves the destination unchanged and `tempfile::Drop` removes the temp.
- Integration test: invoke the reingest path with a fault injected after the old-note read but before the publish step. Assert the old note still exists on disk and contains its original content byte-for-byte.
- Integration test (architect's hardest question): a fault injected immediately after `write_atomic` but before any subsequent step does **not** corrupt the file because the file is now complete (date and cortex fields already baked in). Verify byte-for-byte that the persisted file contains the date and cortex fields.

#### Phase 4: Cleanup and observability
**Model:** sonnet

- Add `debug!("[{trace_id}] Acquired inflight guard for {canonical}")` and a matching release log so the lifecycle is visible at debug level (per `rules/rust.md` function-level instrumentation).
- Update `borg/src/cli.rs` `--help` text if it mentions config keys, to include the new ones.
- Run `otto ci`. Confirm green.
- Restore `notes/why-5-million-people-are-obsessed-with-excalidraw.md` from git in the same PR (was deleted by the bug being fixed).
- Bump version (`bump -m`); reinstall and restart `borg` per `CLAUDE.md`.

## Alternatives Considered

### Alternative 1: Wrap the entire pipeline in `tokio::task::JoinHandle`

- **Description:** Spawn `process_url_inner` as a separate task; the supervisor task awaits the join handle, observing cancellation, panic, and completion uniformly.
- **Pros:** Cleanly separates pipeline execution from supervision. Catches Tokio-internal cancellations.
- **Cons:** Adds a task layer for marginal benefit. `tokio::time::timeout` already exposes the timeout case; panics on the same task already propagate through the `await` boundary. The extra plumbing buys observability that RAII Drop + timeout already provide.
- **Why not chosen:** Simpler `timeout` + Drop-guard combination covers the same failure modes with less code.

### Alternative 2: Two-phase commit via a vault-side staging directory

- **Description:** Write the new note to `~/repos/scottidler/obsidian/.borg-staging/` first, run cortex validation against the staged copy, then rename into the vault only after validation passes.
- **Pros:** Catches more failure modes (broken frontmatter, invalid wikilinks). Aligns with the staged-pipeline architecture.
- **Cons:** Substantially larger surface area; the data-loss bug is an atomic-write problem, not a validation problem. Validation belongs in the larger redesign in `2026-04-19-staged-ingestion-pipeline.md`.
- **Why not chosen:** Out of scope for a tactical correctness fix.

### Alternative 3: Use a concurrent map (`DashMap` / `OnceCell`) for the inflight set

- **Description:** Different lock primitive; explicit remove at every termination site.
- **Pros:** No new struct.
- **Cons:** Doesn't solve the bug. The bug is "we forget to remove on some path"; a different lock primitive doesn't change that. Only RAII (or unconditional cleanup via `defer`-like patterns) closes the gap.
- **Why not chosen:** Doesn't address the root cause.

### Alternative 4: Leave atomicity as-is, rely on git for recovery

- **Description:** The vault is git-tracked; the deleted note can be `git restore`d.
- **Pros:** Zero code change.
- **Cons:** A note authored after the last commit and not yet committed is unrecoverable. Even when recovery succeeds, a silent data-loss bug erodes trust in the system.
- **Why not chosen:** Insufficient.

## Technical Considerations

### Dependencies

No new crates. `tempfile` (used by `write_atomic`) is already a direct dep at `borg/Cargo.toml`. `std::sync::Mutex` replaces `tokio::sync::Mutex` for the inflight set; both are stdlib / current dep.

### Performance

- Atomic write adds one `write` + one `fsync` + one `rename` per publish. The fsync is the only meaningful cost; on a desktop SSD it is single-digit milliseconds. Negligible vs. the 30s-multi-minute pipeline.
- Inflight guard has the same lock-cost profile as today (one acquire on entry, one release on Drop).
- Pipeline-level timeout is zero-cost in the success path: `tokio::time::timeout` registers a single timer.

### Security

No new attack surface. The temp file lives in the same directory as the destination and inherits the directory's permissions. The `trace_id` suffix on the temp file ensures concurrent ingestions cannot collide on the same temp path. The `.` prefix keeps Obsidian from indexing the temp file mid-write.

### Testing Strategy

Tests live in `pipeline/tests.rs` and `youtube/tests.rs` per `rules/rust.md` test placement. Inline `#[cfg(test)] mod tests` blocks present in those files today are extracted as part of this work.

- `write_atomic` survives a mid-write panic: assert old file content unchanged; `tempfile::TempDir` cleans the orphaned temp.
- `InflightGuard` releases on Drop including panic-unwind (test wraps in `std::panic::catch_unwind`).
- Pipeline timeout: stub `process_url_inner` that sleeps 60s with a 1s configured timeout returns `Failed` and the inflight set is empty after.
- Reingest fault injection: a mock pipeline step that returns `Err` after the old-note read leaves the old file on disk with original content (verified byte-for-byte).
- Subtitle fetcher: `wiremock` returning a slow response hits the timeout and returns `Err`, does not hang.

`otto ci` runs `cargo test --workspace`. Manual validation: trigger an ingestion, kill borg with `SIGKILL`, restart, retry the same URL; the retry proceeds (process restart clears the in-memory inflight set; the persistent test is "panic within the same process," which the unit tests cover).

### Rollout Plan

Single PR with phases 1-4. After merge:

1. `bump -m` (minor: behavior change, new config keys with defaults).
2. `cargo install --path borg && systemctl --user restart borg` per `CLAUDE.md`.
3. Watch the journal for `Pipeline timed out` lines over the following week. Each occurrence is a signal of an unbounded code path the per-call timeouts missed; investigate the underlying hang. The hard timeout is a backstop, not a feature.
4. The deleted excalidraw note is restored from git in the same PR (trivial `git restore notes/why-5-million-people-are-obsessed-with-excalidraw.md`).

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Atomic rename fails across filesystem boundary (vault on a separate FS) | Low | Med | Sibling temp path is always on the same FS as the destination (it shares `dest.parent()`), so `EXDEV` cannot fire from the temp -> dest rename. (`tempfile::NamedTempFile::persist_noclobber` is an alternative if we want belt-and-braces.) |
| Pipeline hard-timeout fires on legitimate long ingestions (multi-hour Whisper transcription) | Med | Med | Default 1800s covers all observed cases; configurable per install. Consider per-method timeouts in a follow-up if needed. |
| `parking_lot` dependency introduces transitive risk | Low | Low | Use `std::sync::Mutex` if `parking_lot` is not already a direct workspace dep. Same correctness. |
| Concurrent `--force` ingestions race on the same destination | Low | Med | Out of scope; document. `NamedTempFile`'s random suffix prevents temp-path collision; the final rename is last-writer-wins. |
| Stale temp files accumulate if a process is `SIGKILL`ed mid-write | Low | Low | `tempfile` Drop removes orphans on graceful unwind. SIGKILL bypasses Drop, but the file lives in the vault dir as `.borg-tmp-*` (dot-prefixed, no `.md` extension, invisible to cortex/Obsidian/watchers). Optional follow-up: a startup sweep that removes stale `.borg-tmp-*` files older than 1h. |
| `spawn_blocking` task holds a blocking-pool thread until its child process exits | Med | Low | Per-call timeout + `child.start_kill()` (Phase 1) terminates the child within seconds; the thread releases as soon as the killed child is reaped. Without this, a hung child holds a thread for the duration of the hang. Default Tokio blocking pool is 512 threads; saturation requires 512 simultaneous hangs, which is not a current threat. |
| `pipeline.rs` is already over the 1500-line threshold (3350 lines) and decomposition is a separate concern | Med | Low | New code lives in new submodules (`pipeline/inflight.rs`, `pipeline/atomic.rs`) so this PR does not depend on the larger decomposition. The 2018+ submodule pattern is already established in borg (`blocklist.rs` + `blocklist/`, `replay.rs` + `replay/`, `slides.rs` + `slides/`, `stages.rs` + `stages/`, `transcription.rs` + `transcription/`, `retention.rs` + `retention/`), so adding `pipeline/*.rs` siblings is consistent. Full decomposition of the existing `pipeline.rs` is a separate follow-up; do not bundle. See `rules/dealing-with-large-files.md` for why mixing decomposition with feature work is hazardous. |

## Open Questions

- [ ] Should `Failed { reason: "timeout" }` be a distinct `IngestStatus::TimedOut` variant? Distinct variant aligns with status-typing principles in `rules/rust.md`. Recommendation: yes, but small enough to defer if it bloats Phase 1.
- [ ] Should the startup sweep for stale `.borg-tmp-*` files be in this PR or a follow-up? Recommendation: follow-up; not load-bearing for the bug being fixed.
- [ ] Should this change land before or after the staged-ingestion redesign starts? Recommendation: before. The staged redesign is months of work; the data-loss bug should not wait.

## References

- Bug evidence: 2026-05-08 incident with `ht-fb0810` (Excalidraw video).
- Code: `borg/src/pipeline.rs:404` (delete-before-write), `borg/src/pipeline.rs:182,272,342,367,592` (inflight set lifecycle), `borg/src/youtube.rs:119` (subtitle fetch without timeout), `borg/src/pipeline.rs:255-305` (top-level error handler).
- Related: `docs/design/2026-04-19-staged-ingestion-pipeline.md` (longer-term redesign that subsumes this).
- Rules: `rules/rust.md` (function-level instrumentation, no magic numbers, RAII, test placement), `rules/git.md` (vault recovery via git restore), `rules/dealing-with-large-files.md` (decomposition of `pipeline.rs` if Phase 2 splits it).
