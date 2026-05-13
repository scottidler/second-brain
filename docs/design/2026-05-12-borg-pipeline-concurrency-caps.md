# Design Document: Borg Pipeline Concurrency Caps

**Author:** Scott Idler (drafted by Claude)
**Date:** 2026-05-12
**Status:** Implemented
**Review Passes Completed:** 5/5

**Revision history.**
- **v1:** Single pool, top-of-`process_content` acquire, semaphore on `Config`.
- **v2:** After Architect review: split into general/heavy pools, watchdog skips active traces, semaphores moved out of `Config` into process-wide statics with the `PermitPool` newtype.
- **v2.1:** After author review: added shared `startup::init_permits` helper for daemon + CLI paths; refactored `watchdog::run_once` to take the active-trace predicate as a parameter so tests can use local fixtures instead of touching the global static.
- **v3:** After second Architect pass on heavy-permit placement: per-handler acquire (in `process_youtube`, `process_article_fabric`, `process_audio_inner`, `process_document_file_inner`), not top-of-`process_content`. The `is_heavy_content` classifier is removed - it would have missed any URL whose article path delegates to `fabric -u`, which internally invokes `yt-dlp` on media URLs.

## Summary

borg has no concurrency cap on in-flight ingest traces. The 2026-05-12 incident showed a 36-URL replay batch fanning out into 20+ simultaneous ffmpeg processes, load 159, requiring external intervention. This doc proposes two process-wide `tokio::sync::Semaphore` pools - `GENERAL_PERMITS` (default 8) and `HEAVY_PERMITS` (default 4) - wrapped in a `PermitPool` newtype. The general permit is acquired at the top of `pipeline::process_content`; the heavy permit is acquired *per-handler* by exactly the four functions that shell out to subprocess-heavy work (`process_youtube`, `process_article_fabric`, `process_audio_inner`, `process_document_file_inner`). Cheap paths (text, vocab, code snippet, image, Jina-only articles) acquire general only. A new in-memory `ACTIVE_TRACES` set tracks every trace inside `process_content` so the watchdog can skip them and preserve the `ledger XOR DLQ` invariant.

## Problem Statement

### Background

borg accepts ingest requests from six input surfaces - HTTP (`/ingest`, `/note`, `/ingest/file`), Telegram, Discord, ntfy, CLI, and clipboard. Every surface ultimately calls `pipeline::process_content`, which dispatches to typed handlers (`process_url`, `process_youtube`, `process_article_*`, `process_image`, `process_audio`, `process_document_file`, `process_text`, `process_vocab`, `process_code_snippet`). Each dispatch site does its own `tokio::spawn` for fire-and-forget processing.

The `process_youtube` handler is the heaviest path. For each YouTube URL it runs:

- `yt-dlp --dump-json` for metadata (now bounded by `youtube.rs:50-86` after the v0.5.44 fix);
- `yt-dlp` audio extraction with internal ffmpeg post-processor (`youtube.rs:268`);
- `yt-dlp --write-auto-sub` for captions (`youtube.rs:114`);
- direct `ffmpeg` frame extraction for the slides pipeline (`youtube.rs:400`), which applies `fps=...,mpdecimate=hi:lo:frac,scale=...` and writes up to 100 JPEGs;
- per-slide vision calls (configurable);
- fabric for transcript summarization and tag synthesis.

A single trace spawns one ffmpeg child for the slides extraction. The fan-out we observed during the incident came from running many traces concurrently, not from a single trace spawning many ffmpegs.

### Problem

There is no upper bound on how many `pipeline::process_content` calls can be in flight simultaneously. Each input surface's `tokio::spawn` site has no awareness of how many other traces are already running, and `process_content` itself imposes no cap. Bulk operations - replay batches, mass HTTP submissions, Telegram bursts - therefore translate directly into N concurrent slides pipelines, N concurrent fabric calls, N concurrent vision calls.

Observed failure mode (2026-05-12 17:47-18:11):

- 36 URLs POSTed to `/ingest` at 2s intervals.
- 36 `tokio::spawn` tasks all entered `process_youtube` near-simultaneously.
- Load average climbed to 159.67; memory hit 53 GiB used + 5 GiB swap on a 94 GiB system.
- 20+ concurrent ffmpeg children were observed; another agent killed them externally to recover.

The 2s submission pacing did not slow down work - it only paced *enqueueing*. The HTTP handler returns `Queued` immediately and the work runs concurrently regardless of submission rate.

### Goals

- Cap the number of `pipeline::process_content` invocations that can be running concurrently inside one borg process.
- Apply the cap uniformly across all six input surfaces (HTTP, Telegram, Discord, ntfy, CLI, clipboard).
- Make the caps configurable via `borg.yml` under `pipeline.max-concurrent-traces` and `pipeline.max-concurrent-heavy-traces` with sensible defaults.
- Prevent head-of-line blocking: cheap content kinds (text, vocab, code snippet) must not queue behind heavy ones (YouTube, audio, document OCR).
- Preserve the existing fire-and-forget HTTP semantics: `POST /ingest` must return `Queued` immediately, not block on a permit.
- Preserve the intake invariant from `docs/design/2026-05-11-borg-intake-log-and-dlq.md`: every trace ID appears in ledger XOR DLQ. The watchdog must not write a DLQ orphan row for a trace that is alive in the permit queue or actively executing.
- Add structured DEBUG logging so an operator can tell from logs how many permits are in use, which pool, and when a trace is queued.

### Non-Goals

- Per-stage concurrency caps (e.g., a separate ffmpeg-only semaphore, separate vision-only semaphore). The pool split (general/heavy) subsumes this for the failure mode we hit. Per-stage caps may be useful later as a third tier; out of scope here.
- Cross-process / cross-host coordination. Single borg process only.
- Cap on yt-dlp/ffmpeg subprocess CPU/memory via `nice`, `ionice`, cgroups, or `RLIMIT_*`. Out of scope.
- Priority queues, fair scheduling across input surfaces, or per-surface caps. All surfaces share the same two pools; first-come first-served within each pool.
- Replacing `tokio::spawn` dispatch with an mpsc worker pool. Considered as an alternative; see below.
- Backpressure visible to the *submitter* (e.g., HTTP 429). The HTTP API stays `Queued` semantics; queueing is invisible to the client.
- Three or more pools (e.g., cheap / moderate / heavy). Two pools is the minimum complexity that fixes head-of-line blocking; more tiers add tuning surface area without clear benefit.

## Proposed Solution

### Overview

Introduce two process-wide permit pools, each a `tokio::sync::Semaphore` wrapped in a small newtype:

- `GENERAL_PERMITS` (default cap **8**) - acquired at the top of `process_content` for *every* trace.
- `HEAVY_PERMITS` (default cap **4**) - acquired *per-handler* by the four functions that actually spawn subprocess-heavy work: `process_youtube`, `process_article_fabric`, `process_audio_inner`, `process_document_file_inner`. Acquired *after* the general permit and *immediately before* the heavy work (subprocess spawn or Groq API call).

Why per-handler instead of a top-level classifier? `fabric::fetch_article` (used by `process_article_fabric`) shells out to `fabric -u <url>`, which internally invokes `yt-dlp` for any URL it recognizes as media. A URL classified as "article" at the top of `process_content` (because it lacks a YouTube video-ID pattern) can therefore still fan out to yt-dlp/ffmpeg under the article path. Top-level classification would let those traces bypass the heavy cap entirely. Per-handler acquisition has no such gap: the heavy permit is held iff the heavy code actually runs.

A second benefit: cheap work (URL canonicalization, ledger dedup lookup, content-kind classification) runs only under the general permit. Heavy slots are spent on actual subprocess execution, not on bookkeeping for duplicates that will be rejected anyway.

Cheap paths (text, vocab, code snippet, image, articles routed to Jina) acquire general only. This prevents a burst of YouTube ingests from starving cheap traces: even with `HEAVY_PERMITS` saturated, four more cheap traces can still acquire from `GENERAL_PERMITS` and complete.

To preserve the `ledger XOR DLQ` invariant, a process-wide `ACTIVE_TRACES: Mutex<HashSet<String>>` set tracks every trace currently inside `process_content` (queued for a permit OR actively executing). The watchdog (`borg/src/watchdog.rs`) consults this set before emitting an orphan DLQ row.

**Concrete before / after, using the 2026-05-12 incident shape (general=8, heavy=4):**

| | Before | After (v2) |
|---|---|---|
| Submit 36 YouTube URLs to `/ingest` | 36 `tokio::spawn`s, all racing through `process_content` | 36 `tokio::spawn`s; 4 hold heavy permits, 32 wait in the heavy queue |
| Concurrent ffmpeg children (slides) | 20+ observed | 4 max (one per active heavy trace) |
| Submit 10 text snippets while 4 YouTubes are heavy-running | 14 racing | All 10 snippets acquire general permits and finish promptly; YouTubes proceed independently |
| HTTP response time on `/ingest` | Immediate `Queued` | Immediate `Queued` (unchanged) |
| Watchdog scan during queue saturation | DLQ-orphans every trace whose intake age > 31min, even if the trace is still queued or running | Watchdog reads `ACTIVE_TRACES`, skips any trace currently held by the pipeline |

### Architecture

**Where the cap goes.** Verified by inspection of every dispatch site (`grep "pipeline::process" borg/src/`): every input surface - HTTP routes (`routes.rs`), Telegram (`telegram.rs`), Discord (`discord.rs`), ntfy (`ntfy.rs`), CLI (`lib.rs`), triage/replay (`triage.rs`) - calls `pipeline::process_content` inside its own `tokio::spawn`. The dispatch surfaces do not `await` `process_content` inline, so capping at `process_content` does not block input-surface poll loops (e.g., the Telegram bot continues to receive new messages even when all permits are held). `process_content` itself does not recurse into `process_content`; the internal dispatch chain (`process_content` → `process_url` → `process_youtube`, or → `process_text` → `process_url`) holds permits for the lifetime of the outermost call. The existing `InflightGuard` (`borg/src/pipeline/inflight.rs`) handles per-URL dedup and is orthogonal; it stays as-is.

**The five concrete changes:**

1. **A new module `borg/src/pipeline/permits.rs` introduces the `PermitPool` newtype and two static instances.** `PermitPool` wraps `OnceLock<Arc<Semaphore>>` and exposes `init(cap: usize)` and `acquire() -> OwnedSemaphorePermit`. Two static instances live at the module top: `GENERAL_PERMITS` and `HEAVY_PERMITS`. The newtype hides the three-layer stack (`OnceLock<Arc<Semaphore>>`) behind a clean API and gives the typedef-equivalent ergonomics the call sites want.

2. **A shared `borg::startup::init_permits(cfg: &Config)` helper.** Both daemon and CLI entry points call it before any `process_content` invocation. This is critical: borg has two execution modes (long-running daemon and one-shot CLI commands like `borg replay`/`borg ingest`), each in its own process. The CLI process must also init the pools or `acquire()` panics on `OnceLock::get().expect(...)`. Callers found in `borg/src/lib.rs:280`, `:373`, `borg/src/triage.rs:423`, `:444`, plus every input surface dispatch site - all of which today call `pipeline::process_content` directly. The helper runs once per process startup, validates `1 <= cap <= 64` for each pool, and bails if out of range.

3. **`pipeline::process_content` registers the trace as active and acquires only the general permit.** Order: register in `ACTIVE_TRACES` (immediately, so the watchdog sees the trace as live before any await), then acquire the general permit, then dispatch. Both are released on scope exit via RAII (`ACTIVE_TRACES` removal uses a guard exactly like `InflightGuard`). The heavy permit is acquired further down the call chain by the four heavy handlers themselves.

4. **`watchdog::run_once` consults `ACTIVE_TRACES` before declaring orphan.** Today the watchdog writes a DLQ orphan row for any intake row older than `hard_timeout_secs + WATCHDOG_BUFFER_SECS` (1860s) that has no ledger and no DLQ entry. The change: also skip the row if its trace ID is in `ACTIVE_TRACES`. A trace stuck in the permit queue or executing past the watchdog deadline remains alive in memory; the watchdog defers to that state. If the daemon crashes, `ACTIVE_TRACES` vanishes with it; on restart the watchdog correctly orphans the abandoned traces.

5. **`watchdog::run_once` takes an `active_traces: &dyn Fn(&str) -> bool` parameter, not a hardcoded reference to the static.** Production passes `&permits::is_trace_active`; tests pass a closure that consults a fixture set. This decouples the watchdog from the global static, which is required for deterministic concurrent test execution (see Testing Strategy below).

Permits are acquired *after* intake durability (`record_intake` already ran in the HTTP/Telegram/etc. handler before `process_content` is called) and *before* any pipeline work. Intake durability stays at the door, unaffected by this change.

### Data Model

**Config change** (`borg/src/config.rs::PipelineConfig`):

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PipelineConfig {
    pub hard_timeout_secs: u64,
    pub subtitle_fetch_timeout_secs: u64,
    pub yt_dlp_timeout_secs: u64,
    pub ocr_timeout_secs: u64,
    pub jina_timeout_secs: u64,
    pub max_concurrent_traces: usize,        // NEW: general pool cap
    pub max_concurrent_heavy_traces: usize,  // NEW: heavy pool cap
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            hard_timeout_secs: 1800,
            subtitle_fetch_timeout_secs: 30,
            yt_dlp_timeout_secs: 600,
            ocr_timeout_secs: 60,
            jina_timeout_secs: 60,
            max_concurrent_traces: DEFAULT_MAX_CONCURRENT_TRACES,
            max_concurrent_heavy_traces: DEFAULT_MAX_CONCURRENT_HEAVY_TRACES,
        }
    }
}

const DEFAULT_MAX_CONCURRENT_TRACES: usize = 8;
const DEFAULT_MAX_CONCURRENT_HEAVY_TRACES: usize = 4;
```

`Config` itself remains unchanged. No `#[serde(skip)]` field, no `Default` derive collision, no `Config::load` rewrite. The runtime state lives outside `Config` in the new permits module.

**New module** (`borg/src/pipeline/permits.rs`):

```rust
use std::sync::{Arc, Mutex, OnceLock};
use std::collections::HashSet;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Process-wide permit pool. One instance is initialized in main()
/// from the loaded config value; all dispatch sites call .acquire().await.
pub struct PermitPool {
    inner: OnceLock<Arc<Semaphore>>,
    name: &'static str,
}

impl PermitPool {
    pub const fn new(name: &'static str) -> Self {
        Self { inner: OnceLock::new(), name }
    }

    /// Initialize from a cap. Idempotent: a second call after the first wins is a no-op.
    pub fn init(&self, cap: usize) {
        if self.inner.set(Arc::new(Semaphore::new(cap))).is_err() {
            log::warn!("PermitPool {}: init called twice; second call ignored", self.name);
        }
    }

    pub async fn acquire(&self) -> OwnedSemaphorePermit {
        let sem = self.inner.get()
            .expect("PermitPool::init must be called before acquire");
        log::debug!(
            "permits[{}]: acquiring (available={}, total={})",
            self.name, sem.available_permits(), /* total computed elsewhere */
        );
        sem.clone().acquire_owned().await.expect("semaphore never closed")
    }
}

pub static GENERAL_PERMITS: PermitPool = PermitPool::new("general");
pub static HEAVY_PERMITS: PermitPool = PermitPool::new("heavy");

/// Trace IDs currently inside process_content (queued for a permit or running).
/// Watched by the watchdog to suppress false orphan DLQs.
static ACTIVE_TRACES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn active_traces() -> &'static Mutex<HashSet<String>> {
    ACTIVE_TRACES.get_or_init(|| Mutex::new(HashSet::new()))
}

/// RAII guard: insert on construction, remove on Drop. Mirrors InflightGuard.
pub struct ActiveTraceGuard { trace_id: String }

impl ActiveTraceGuard {
    pub fn acquire(trace_id: &str) -> Self {
        active_traces().lock().expect("not poisoned").insert(trace_id.to_string());
        Self { trace_id: trace_id.to_string() }
    }
}

impl Drop for ActiveTraceGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = active_traces().lock() {
            set.remove(&self.trace_id);
        }
    }
}

pub fn is_trace_active(trace_id: &str) -> bool {
    active_traces().lock().map(|s| s.contains(trace_id)).unwrap_or(false)
}
```

The `Mutex<HashSet>` for `ACTIVE_TRACES` is `std::sync::Mutex`, not `tokio::sync::Mutex` - the lock is held for microseconds (one insert or one remove) and is never held across an `.await`. This matches the existing `borg/src/pipeline/inflight.rs` precedent.

### API Design

**`pipeline::process_content`** (in `borg/src/pipeline.rs`) — top-level general permit only:

```rust
pub async fn process_content(
    content: ContentKind,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: Option<String>,
) -> IngestResult {
    let tid = trace_id.clone().unwrap_or_else(|| trace::generate(method));

    // 1. Register as active *before* any await, so the watchdog sees us
    //    even while we wait in the permit queue.
    let _active_guard = permits::ActiveTraceGuard::acquire(&tid);

    // 2. Acquire the general permit (every trace).
    log::debug!("process_content[{tid}]: acquiring general permit");
    let _general = permits::GENERAL_PERMITS.acquire().await;
    log::debug!("process_content[{tid}]: general permit acquired");

    // 3. Existing dispatch logic, unchanged.
    // ...
}
```

No `is_heavy_content` classifier. The heavy permit is acquired by each heavy handler instead.

**Heavy permit acquire sites** — each guarded inline immediately before the subprocess-heavy work:

```rust
// borg/src/pipeline.rs::process_youtube
async fn process_youtube(url: &str, config: &Config) -> Result<YouTubeResult> {
    let use_fabric = fabric::is_available(&config.fabric);
    log::debug!("process_youtube: acquiring heavy permit");
    let _heavy = permits::HEAVY_PERMITS.acquire().await;
    log::debug!("process_youtube: heavy permit acquired");

    // ... existing yt-dlp metadata + fabric transcript join ...
}

// borg/src/pipeline.rs::process_article_fabric
async fn process_article_fabric(url: &str, config: &Config, trace_id: &str)
    -> Result<(String, String, ContentType)>
{
    log::debug!("process_article_fabric[{trace_id}]: acquiring heavy permit");
    let _heavy = permits::HEAVY_PERMITS.acquire().await;
    // fabric -u may delegate to yt-dlp internally; this acquire is why.
    fabric::fetch_article(url, &config.fabric).await
    // ... rest unchanged ...
}

// borg/src/pipeline.rs::process_audio_inner
async fn process_audio_inner(...) -> Result<...> {
    log::debug!("process_audio_inner: acquiring heavy permit");
    let _heavy = permits::HEAVY_PERMITS.acquire().await;
    // Groq transcription + any ffmpeg pre-processing happens under this permit.
    // ... existing client.transcribe call ...
}

// borg/src/pipeline.rs::process_document_file_inner
async fn process_document_file_inner(...) -> Result<...> {
    log::debug!("process_document_file_inner: acquiring heavy permit");
    let _heavy = permits::HEAVY_PERMITS.acquire().await;
    // OCR / document::extract_text below.
    // ... existing logic ...
}
```

`process_article_jina` does **not** acquire the heavy permit — it is a plain HTTP GET to `r.jina.ai` with no subprocess fan-out.

**`watchdog::run_once`** (in `borg/src/watchdog.rs`): one additional skip predicate inside the orphan-detection loop.

```rust
for row in &intake_rows {
    let Some(age) = intake_age_secs(row) else { continue };
    if age < deadline { continue; }
    if ledger_traces.contains(&row.trace_id) { continue; }
    if dlq_traces.contains(&row.trace_id) { continue; }

    // NEW: skip traces still alive in the pipeline's permit queue or runtime.
    if permits::is_trace_active(&row.trace_id) {
        log::debug!(
            "watchdog: trace {} aged {}s but still active in pipeline; skipping",
            row.trace_id, age
        );
        continue;
    }

    // ... existing orphan DLQ row append ...
}
```

No external API change. HTTP handlers continue to return `Queued` immediately; the wait, if any, happens inside the spawned task.

### Implementation Plan

#### Phase 1: Config fields and the `permits` module
**Model:** sonnet

- Add `max_concurrent_traces: usize` and `max_concurrent_heavy_traces: usize` to `PipelineConfig` in `borg/src/config.rs`. Defaults via consts: `DEFAULT_MAX_CONCURRENT_TRACES = 8`, `DEFAULT_MAX_CONCURRENT_HEAVY_TRACES = 4`.
- Create `borg/src/pipeline/permits.rs` with `PermitPool` newtype, the two static instances (`GENERAL_PERMITS`, `HEAVY_PERMITS`), `ActiveTraceGuard` RAII type, and `is_trace_active(trace_id)` helper.
- Register the new module in `borg/src/pipeline.rs` (`mod permits; pub use permits::is_trace_active;`).
- `cargo check -p borg`.

#### Phase 2: Init helper and `process_content` general acquire
**Model:** opus

- Create `borg/src/startup.rs` with a `pub fn init_permits(cfg: &Config) -> Result<()>` function: validates each cap, then calls `permits::GENERAL_PERMITS.init(...)` and `permits::HEAVY_PERMITS.init(...)`.
- Wire `startup::init_permits(&cfg)?` into every borg entry point that ends up calling `pipeline::process_content`:
  - Daemon `run`/`start` in `borg/src/lib.rs`, before any input surface starts.
  - CLI commands in `borg/src/lib.rs` (currently calling `process_content` directly at lines 280, 373).
  - Replay paths in `borg/src/triage.rs` (lines 423, 444).
- In `pipeline::process_content`, add the `ActiveTraceGuard::acquire(&tid)` and the general permit acquire, all before existing dispatch.
- DEBUG logs as shown in the API Design section.
- `otto ci`.

#### Phase 2b: Per-handler heavy permit acquires
**Model:** opus

- In `pipeline::process_youtube`, acquire `HEAVY_PERMITS` at the top of the function before the `tokio::join!(metadata_future, transcript_future)`.
- In `pipeline::process_article_fabric`, acquire before `fabric::fetch_article` (the call that delegates to the fabric subprocess, which can internally invoke `yt-dlp` for media URLs).
- In `pipeline::process_audio_inner`, acquire before the Groq `transcribe` call.
- In `pipeline::process_document_file_inner`, acquire before the OCR / `document::extract_text` dispatch.
- Do NOT acquire in `process_article_jina` (HTTP GET only).
- DEBUG logs at each acquire site as shown in API Design.
- `otto ci`.

#### Phase 3: Watchdog integration
**Model:** opus

- Change `watchdog::run_once`'s signature to take an `active_traces: &dyn Fn(&str) -> bool` parameter. Existing callers pass `&permits::is_trace_active`.
- Inside `run_once`, before appending an orphan DLQ row, call `active_traces(&row.trace_id)` and `continue` if true.
- Add the DEBUG log shown in API Design.
- Add a test in `borg/src/watchdog/tests.rs`: build a `HashSet<String>` fixture, pass `&|t: &str| set.contains(t)` to `run_once`, fixture intake row aged past deadline with its trace_id in the set, assert no orphan row appended. Then remove from set, run again, assert orphan row IS appended.
- `otto ci`.

#### Phase 4: Concurrency tests
**Model:** sonnet

- **Critical: tests never touch the production statics (`GENERAL_PERMITS`, `HEAVY_PERMITS`, `ACTIVE_TRACES`).** With multiple tests in the same `cargo test` process, the first `init()` call wins permanently and all subsequent tests see that cap. Same hazard for `ACTIVE_TRACES` cross-test pollution. Instead, every test constructs a **local** `PermitPool::new("test-<name>")` instance and tests against it.
- New `borg/src/pipeline/permits/tests.rs` (declare `#[cfg(test)] mod tests;` in `permits.rs`):
  - Local pool, cap = 2: spawn 5 acquire+sleep+release tasks; assert max-watermark <= 2 via `AtomicUsize::fetch_max`.
  - Local pool, cap = 1: two concurrent acquires; second waits for first to drop.
  - Local pool, `init()` returns OK on first call, logs warning on second (idempotent).
  - `ActiveTraceGuard` Drop test: use a local `Mutex<HashSet>` passed into a test-only `ActiveTraceGuard::acquire_in(&set, "t1")` variant. Verify Drop removes the trace_id. Run a panicking-future variant to verify panic-unwind also removes.
- Config tests in `borg/src/config/tests.rs`: defaults populate the new fields; YAML overrides are read.
- `cargo test -p borg`.

#### Phase 5: Ship and observe
**Model:** sonnet

- `bump`, `otto deploy` (auto-restarts borg + cortex per `reference_otto_deploy`).
- Watch 1-2 organic Telegram-driven ingests, confirming the DEBUG lines `permits[general]:` and `permits[heavy]:` fire with sensible `available=` values, and that `process_content[<trace>]:` logs include the trace id.
- Do *not* run a multi-trace replay batch as a verification step. The first real stress is whatever organic traffic Scott generates next.

## Alternatives Considered

### Alternative 1: Permit acquired at each input-surface dispatch site
- **Description:** Have each input surface (routes.rs, telegram.rs, etc.) acquire a permit before its `tokio::spawn`.
- **Pros:** Permit acquisition visible at the surface; per-surface caps possible later.
- **Cons:** Six sites to change, easy to miss one when adding a seventh surface (already adding more is plausible). Repetitive. Easy to put the acquire in the wrong place and block the input loop.
- **Why not chosen:** The single funnel through `process_content` is exactly the right cap point and stays correct as we add surfaces.

### Alternative 2: Replace `tokio::spawn` dispatch with bounded mpsc worker pool
- **Description:** A single `mpsc::channel(N)` and a fixed pool of N `tokio::spawn`ed workers consuming `process_content` jobs. Submission becomes `tx.send(...).await`.
- **Pros:** Classic bounded-work pattern. Backpressure on submitters falls out naturally. Clean shutdown story.
- **Cons:** Larger refactor - every input surface stops calling `process_content` directly. Changes shutdown semantics, observability, and the `IngestResult` return path for synchronous-ish callers like CLI/triage. The `IngestStatus::Queued` semantics already imply "fire and forget"; this would tighten it but is more invasive than needed for the failure mode.
- **Why not chosen:** Solves the same problem but at higher refactoring cost. Worth revisiting if we need per-surface fairness, queue introspection, or graceful shutdown.

### Alternative 3: `Arc<Semaphore>` field on `Config` (v1 of this doc)
- **Description:** Add `pipeline_permits: Arc<Semaphore>` to `Config` behind `#[serde(skip, default = "default_permits")]`, rebuild in `Config::load` from the deserialized cap value.
- **Pros:** Reuses the existing `Config` plumbing that already flows to every dispatch site.
- **Cons:** `Config` derives `Default`, which `Arc<Semaphore>` does not satisfy without a hand-rolled wrapper; `Config::load` does not exist (the codebase uses a generic `load_config<T>` function), so there is no clean post-deserialize hook to size the semaphore from the YAML value.
- **Why not chosen:** Architect review of v1 surfaced both issues. The static-pool approach in this doc (v2) avoids them entirely.

### Alternative 4: Single pool (no general/heavy split)
- **Description:** v1 of this doc proposed one shared semaphore for every `process_content` call.
- **Pros:** Simpler config (one knob), simpler call site (one acquire).
- **Cons:** Head-of-line blocking. A burst of YouTube ingests fills the pool and a cheap text snippet from Telegram waits seconds-to-minutes behind them. For an interactive capture system, that breaks the "instant capture" UX.
- **Why not chosen:** Architect review of v1 surfaced this. Two pools is the minimum complexity that protects cheap traces from heavy ones without adding tuning surface area.

### Alternative 4: OS-level cgroup or `RLIMIT_NPROC` on the borg.service unit
- **Description:** Have systemd cap borg's process count or CPU shares.
- **Pros:** Defends against any code path missing the cap. Independent of Rust correctness.
- **Cons:** Coarse - kills processes rather than queueing them, no visibility from Rust, mixes deployment config with application logic, doesn't differentiate ffmpeg from yt-dlp from notify.
- **Why not chosen:** Useful as a separate hardening layer later, but not a substitute for an in-process queue.

## Technical Considerations

### Dependencies

- `tokio::sync::Semaphore` and `tokio::sync::OwnedSemaphorePermit` (already in the dependency tree via `tokio` full feature).
- No new crates.

### Performance

- Permit acquisition on the hot path: one or two async waits (general, plus heavy if applicable). When permits are available it returns immediately. When saturated, parks the task on a Tokio waiter list; cost is negligible relative to a YouTube pipeline (seconds-to-minutes of yt-dlp + fabric + vision).
- `ACTIVE_TRACES` insert/remove: a `std::sync::Mutex<HashSet<String>>` operation; microseconds in the worst case, well under any await-boundary cost.
- Heavy cap = 4 on a 32-core machine leaves headroom for ffmpeg/yt-dlp/fabric children plus everything else the user runs (browser, Claude, IDE). The 2026-05-12 incident hit load 159 at ~36 concurrent heavy traces; cap 4 corresponds to ~3-5 concurrent ffmpegs worst-case, well within sustainable budget.
- General cap = 8 lets cheap content (text snippets, vocab) drain quickly even when the heavy pool is full.
- Submission rate stays unchanged: HTTP handlers still return `Queued` immediately. Permits shape work, not enqueue.

### Security

- No external surface change. Internal-only concurrency knob.
- Caps as positive integers. `PermitPool::init(0)` would deadlock all subsequent acquires; the daemon startup path validates `1 <= max_concurrent_traces` and `1 <= max_concurrent_heavy_traces` (with a soft upper bound of 64 to catch accidents) and refuses to start on out-of-range values, alongside the existing config validation pattern.

### Testing Strategy

**Key constraint: tests must not touch the production statics.** `cargo test` runs multiple tests in a single process; `OnceLock::set` succeeds exactly once per process; `ACTIVE_TRACES` is a single shared set. Without isolation, the first test's `init()` cap wins forever and concurrent tests race on the shared `ACTIVE_TRACES`. Two design decisions enforce isolation:

1. **`PermitPool` is instantiable.** Production code uses `GENERAL_PERMITS`/`HEAVY_PERMITS` statics; tests build their own `PermitPool::new("test-<name>")` and verify against it. The statics are never touched in `#[cfg(test)]` code paths.
2. **`watchdog::run_once` takes the active-trace predicate as a function parameter.** Production passes `&permits::is_trace_active`; tests pass closures over a local `HashSet`. The watchdog logic is testable without ever populating the global `ACTIVE_TRACES`.

The `ActiveTraceGuard` itself stores a reference (or has a test-only variant) to its backing set so tests can use a local one. Concrete shape:

```rust
pub struct ActiveTraceGuard<'a> {
    trace_id: String,
    set: &'a Mutex<HashSet<String>>,
}
impl<'a> ActiveTraceGuard<'a> {
    pub fn acquire_in(set: &'a Mutex<HashSet<String>>, trace_id: &str) -> Self { ... }
}
// production helper - acquires against the static
pub fn acquire(trace_id: &str) -> ActiveTraceGuard<'static> {
    ActiveTraceGuard::acquire_in(active_traces(), trace_id)
}
```

Concrete tests:

- Config tests in `borg/src/config/tests.rs`: defaults populate both new fields (`max_concurrent_traces = 8`, `max_concurrent_heavy_traces = 4`); YAML overrides are honored; out-of-range values rejected.
- `PermitPool` tests in `borg/src/pipeline/permits/tests.rs`:
  - Local pool, cap = 2: spawn 5 acquire+sleep+release tasks; assert max-watermark <= 2 via `AtomicUsize::fetch_max`.
  - Local pool, cap = 1: two concurrent acquires; second waits for first to drop.
  - `init()` is idempotent on second call.
- `ActiveTraceGuard` tests against a local `Mutex<HashSet>`: insert+drop removes trace_id; panic-unwind drop also removes.
- Watchdog tests in `borg/src/watchdog/tests.rs`: pass a closure over a fixture HashSet; aged intake row is NOT orphaned while predicate returns true; IS orphaned when predicate returns false.
- Pure tokio runtime behavior; no external systems mocked.

### Rollout Plan

- Land via standard `bump` + `otto deploy`. `otto deploy` already restarts borg + cortex daemons per `reference_otto_deploy`.
- Defaults: general = 8, heavy = 4. Observe a real ingest day. If DEBUG logs show heavy queue waits stretching past a few minutes, raise `max-concurrent-heavy-traces`. If general saturates during text/vocab bursts, raise `max-concurrent-traces`.
- Add `pipeline.max-concurrent-traces` and `pipeline.max-concurrent-heavy-traces` to `~/.config/borg/borg.yml` only when a non-default value is wanted; serde fills defaults otherwise.

### Rollback Plan

- If the caps misbehave in production, set both fields to large numbers (e.g., 1024) in `~/.config/borg/borg.yml` and restart borg. Effectively neuters the caps without reverting code; the `ACTIVE_TRACES`/watchdog change remains active (it is correctness-preserving regardless of permit pressure).
- Full revert: `git revert` the ship commit, `bump`, `otto deploy`. Change is contained to four files (`borg/src/config.rs`, `borg/src/pipeline.rs`, new `borg/src/pipeline/permits.rs`, `borg/src/watchdog.rs`) plus tests. Revert is mechanical.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Cap set too low; ingests visibly slow during normal traffic | Medium | Low | DEBUG log shows queue waits; easy to raise via config without rebuild. Restart daemon to apply. |
| Permit leak (panic / abnormal exit inside `process_content`) | Low | Medium | `OwnedSemaphorePermit` drops on panic-unwind. Borg uses `eyre::Result` not panic-based control flow; verify with `panic = "unwind"` default (not `"abort"`). |
| Cap of 4 not protective enough for very heavy single traces (e.g., a 4h video with 100-frame slides + vision) | Medium | Medium | Single-trace cost is already bounded by `hard_timeout_secs = 1800`. If a single trace can crush the system we have a per-trace problem to fix separately; the cap is the right tool only for cross-trace fan-out. |
| Cross-binary interaction: cortex/oracle running their own work while borg holds permits | Low | Low | Each binary has its own process and pool. cortex runs sync rayon work; oracle is MCP read-side. No shared resource competition that this cap should address. |
| Telegram input surface back-pressured behind a saturated cap | Medium | Low | Telegram's `pipeline::process_content` call runs inside the bot's poll loop. With cap = 4 and typical processing of ~1-3min per YouTube trace, sustained backpressure is unlikely; verify in observation phase. If problematic, move the await off the poll loop with another `tokio::spawn`. |
| Replay command (`borg replay`) hits the same cap | Low | Low | Desired behavior - replay should respect the cap exactly like fresh ingests. |
| Watchdog (intake-orphan, 1860s) falsely DLQs traces stuck in the permit queue | ~~Medium~~ Resolved | ~~Medium~~ Resolved | Solved by design in v2: `ACTIVE_TRACES` tracks every trace inside `process_content` from the moment of entry; watchdog skips any trace ID in that set. Daemon crash discards the set, so abandoned traces correctly orphan on restart. |
| `ACTIVE_TRACES` mutex contention under heavy load | Low | Low | Lock is held for microseconds (one HashSet insert or remove) and never across an await. Matches the existing `InflightGuard` pattern. |
| `ActiveTraceGuard` insert happens before permit acquire; if the daemon dies mid-await, the in-memory set vanishes - correct behavior. If the daemon survives but the trace's future is dropped (e.g., explicit cancellation), `Drop` removes the trace_id. | Low | Low | RAII covers both await-cancellation and panic-unwind paths. |
| Heavy permit acquire site is forgotten when a new subprocess-heavy handler is added | Medium | Medium | Code review checklist: any new `process_*` function that calls `yt-dlp`, `ffmpeg`, `fabric -u`, `ocrmypdf`, or external transcription APIs MUST acquire `HEAVY_PERMITS` immediately before the work. Add a comment to `permits.rs` listing the current four acquire sites so reviewers know the surface. Long-term, a clippy lint or grep-based CI check could enforce this, but is out of scope here. |

## Open Questions

- [ ] Should `max-concurrent-traces` and `max-concurrent-heavy-traces` be auto-derived from `num_cpus::get()` (e.g., `general = max(4, n/4)`, `heavy = max(2, n/8)`)? Adds a `num_cpus` dep and varies behavior across hardware. Fixed defaults (8/4) are predictable; auto-tuning is a follow-up.
- [ ] Are there content paths that should be considered "very heavy" and need a *third* pool (e.g., long YouTube videos doing 100-frame slides + vision)? Probably no - the heavy cap of 4 should be small enough to handle the worst case. Revisit only if observation shows sustained over-saturation.

## References

- 2026-05-12 incident: load avg 159.67, 53 GiB RAM + 5 GiB swap, 20+ concurrent ffmpegs. Recovered via external `pkill -u saidler ffmpeg` from a second Claude session.
- `borg/src/youtube.rs:400` - direct ffmpeg frame extraction (slides pipeline).
- `borg/src/youtube.rs:268` - yt-dlp audio extraction with internal ffmpeg postprocessor.
- `borg/src/pipeline.rs:186` - `process_content` (proposed cap point).
- `borg/src/routes.rs:83`, `:151`, `:372` - HTTP dispatch sites.
- `borg/src/telegram.rs:299..518`, `borg/src/discord.rs:212..266`, `borg/src/ntfy.rs:186..212` - other dispatch sites.
- `docs/design/2026-05-11-borg-intake-log-and-dlq.md` - intake durability invariant (preserved by this change).
- `docs/design/2026-05-08-borg-pipeline-resilience.md` - prior resilience work.
- Memory: `feedback_no_unbounded_fanout.md` - operator-side counterpart rule for batch submissions.
