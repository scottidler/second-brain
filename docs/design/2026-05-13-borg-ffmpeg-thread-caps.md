# Design Document: Borg ffmpeg Thread Caps

**Author:** Scott Idler (drafted by Claude)
**Date:** 2026-05-13
**Status:** Implemented
**Review Passes Completed:** 1/5

**Revision history.**
- **v1:** Initial draft. Two `ThreadCount` knobs on `YoutubeConfig`, helper threads args through `extract_frames` only, `#[serde(untagged)]` enum for the parse/render.
- **v2:** After Architect review:
  - Scope extended to include `extract_audio`'s `yt-dlp --postprocessor-args` path, which indirectly spawns ffmpeg with default (auto) threading. The audio post-processor inherits `ffmpeg-threads` (the audio path uses no filter graph, so `ffmpeg-filter-threads` is N/A there).
  - `ThreadCount` serde shape switched from `#[serde(untagged)]` enum to a single newtype with hand-written `Deserialize` *and* `Serialize` impls. The untagged-enum form would have round-tripped `NprocOver { denom: 8 }` as a YAML map (`{denom: 8}`), not the symbolic string `"nproc/8"`, breaking `borg config dump` -> daemon reload.
  - Default population spelled out: field-level `#[serde(default = "...")]` functions, not a blanket `Default` impl on `ThreadCount`. Keeps the denominator (`8`) co-located with the field that uses it.
  - Clarified what `-threads` does and does not control in the slides pipeline (decoder + ffmpeg's worker pool; the `mjpeg` encoder is single-threaded regardless).

## Summary

The 2026-05-12 pipeline-concurrency-caps work bounded *how many* ingest traces can run concurrently (`HEAVY_PERMITS` default 4), but did not bound *how much CPU each heavy trace consumes*. On 2026-05-13 a YouTube backlog with `heavy=4` produced load 61 on a 32-core box because each `ffmpeg` frame-extract invoked `mpdecimate` with default threading (auto = use all cores), so 4 concurrent ffmpegs spent 20-24 cores on filter work alone. This doc adds two `ffmpeg`-threading knobs to `YoutubeConfig` -- `ffmpeg-threads` and `ffmpeg-filter-threads` -- with defaults expressed as `nproc`-derived expressions (`nproc/8` by default, with a floor of 2) so the same config behaves sensibly on a 32-core workstation, a 16-core laptop, and an 8-core dev box. Both the direct `extract_frames` ffmpeg invocation *and* the indirect ffmpeg spawned by `yt-dlp` inside `extract_audio` (via `--postprocessor-args`) are routed through the same resolved values, so neither path can fan out unboundedly. The change is internal -- the heavy-permit count, the slides pipeline, and the on-disk artifacts are unchanged.

## Problem Statement

### Background

After commit `66a218d` shipped the `GENERAL_PERMITS` / `HEAVY_PERMITS` pools, two ffmpeg entry points run under steady-state load:

1. **Direct invocation in `extract_frames`** (`youtube.rs:400`) -- the slides pipeline. The hot one.
   ```
   ffmpeg -hide_banner -loglevel error -y -i <video.mp4>
          -vf fps=<fps>,mpdecimate=hi=<hi>:lo=<lo>:frac=<frac>,scale=<px>:-2
          -frames:v <budget> -q:v 4 -vsync vfr <out_dir>/frame_%04d.jpg
   ```
   `mpdecimate` compares every decoded frame against the previous keep-frame in 8x8 blocks -- it dominates CPU. The `mjpeg` encoder writing the JPEG output is single-threaded by design (one image per output file); thread caps here govern the *decoder* and ffmpeg's worker pool, not the JPEG encoder.

2. **Indirect invocation via `yt-dlp` in `extract_audio`** (`youtube.rs:264`) -- the audio path used when subtitles are missing and we have to run Whisper. yt-dlp is invoked with `--postprocessor-args "ffmpeg:-vn -ac 1 -ar 16000 -b:a 64k"`, which yt-dlp passes verbatim to an internal ffmpeg subprocess for the m4a -> mp3 transcode. No filter graph is involved (no `-vf`), so `-filter_threads` does not apply here; `-threads` does.

ffmpeg's filter threading is governed by `-filter_threads` (and `-filter_complex_threads` for `-filter_complex` graphs); decoder/encoder threading by `-threads`. When none of these are set ffmpeg defaults to "auto" -- in practice, "use as many cores as you can." On a 32-core box that resolves to 8-12 effective worker threads per ffmpeg invocation, which `ps` reports as 400-600% CPU per process.

`HEAVY_PERMITS` defaults to 4. Four concurrent ffmpeg invocations each consuming 4-6 cores is 16-24 cores -- ~half the box. Combined with yt-dlp post-processors, fabric LLM calls, and vision API requests, the system's 1-minute load average climbed to 61.71 during the 2026-05-13 burst. The audio path is cheaper per-call (single-stream transcode with no `mpdecimate`), but `--postprocessor-args` provides no thread cap by default, so a burst of subtitle-less videos would expose the same uncapped fan-out shape.

### Problem

The concurrency cap counts processes, not cores. Each "heavy slot" is treated as a single accounting unit, but ffmpeg under that slot can spawn an arbitrary number of internal worker threads. The cap therefore underestimates per-slot CPU cost by roughly the ffmpeg per-process thread fan-out (currently 4-6x on a 32-core box).

Observed symptoms from the 2026-05-13 burst:

- `uptime`: load average 61.71 / 27.26 / 14.66 on 32 cores.
- `ps`: 4 active heavy permits, only 2 visible ffmpegs at sample time (others between invocations), each at 400-590% CPU.
- borg journal: `permits[heavy]: acquiring (available=0)` repeatedly -- the cap fired and queued work; load was driven by the 4 ffmpegs already running, not by uncapped fan-out.

So the cap is doing its job (load 61 instead of the previous incident's 159), but the per-slot CPU cost is uncapped.

### Goals

- Cap `ffmpeg`'s per-invocation CPU consumption via the `-threads` and `-filter_threads` command-line flags.
- Make the caps configurable in `~/.config/borg/borg.yml` under the existing `youtube:` block.
- Default values must scale with the host's core count -- the same config must behave reasonably on 8-core, 16-core, and 32-core hosts. Hardcoded integers are explicitly out.
- Default values expressed as `nproc`-derived expressions (`nproc`, `nproc/2`, `nproc/4`, `nproc/8`, ...) plus a floor, with a literal integer also accepted as an override.
- All production ffmpeg invocations -- direct (`Command::new("ffmpeg")`) *and* indirect via `yt-dlp --postprocessor-args` -- route through the same resolved thread values, so neither call shape can bypass the cap.
- Log the resolved thread counts at daemon startup so an operator can read the active values from the journal without inspecting config + nproc separately.

### Non-Goals

- Reducing `HEAVY_PERMITS` from 4 to 2 (or any other adjustment to the permit pools). That is a separate tuning knob; this doc bounds per-slot cost so the existing pool sizes stay correct.
- Capping yt-dlp's `--concurrent-fragments`, whisper threads, fabric subprocess threading, or any other tool. The 2026-05-13 incident is specifically ffmpeg-bound; broadening scope here delays the fix and introduces unrelated tuning surface.
- A general "core budget" abstraction that distributes a global core budget across all subprocesses. Conceptually attractive, operationally heavy; future work if a second tool joins the budget pressure.
- Linux `cgroup` / `nice` / `ionice` / `RLIMIT_*` controls on subprocess CPU. These exist but require root or systemd-slice plumbing; ffmpeg flags are universally portable.
- `-filter_complex_threads`. The slides pipeline uses `-vf` (simple filter graph), not `-filter_complex`. Adding the flag has no effect on current code; YAGNI.
- Dynamic tuning at runtime based on observed load. Static config + restart is sufficient for the operator workflow.

## Proposed Solution

### Overview

1. Introduce a `ThreadCount` newtype struct representing either a literal integer or an `nproc`-derived expression, with hand-written `Deserialize` and `Serialize` impls that parse/render YAML scalars (`4`, `"nproc"`, `"nproc/N"`). See "ThreadCount type" below for why this is a newtype, not a `#[serde(untagged)]` enum.
2. Add two fields to `YoutubeConfig`: `ffmpeg_threads: ThreadCount` and `ffmpeg_filter_threads: ThreadCount`, each populated via field-level `#[serde(default = "...")]` functions that return `ThreadCount::nproc_over(DEFAULT_FFMPEG_THREAD_DENOM)` where `DEFAULT_FFMPEG_THREAD_DENOM = 8`. No blanket `Default` impl on `ThreadCount` -- the denominator lives next to the field that uses it.
3. Resolution: `ThreadCount::resolve()` returns `max(MIN_FFMPEG_THREADS, computed)` where `MIN_FFMPEG_THREADS = 2`. `nproc` comes from `std::thread::available_parallelism()`, falling back to 1 on failure.
4. Add two helpers on `YoutubeConfig`:
   - `ffmpeg_thread_args(&self) -> [String; 4]` returns `["-threads", "<n>", "-filter_threads", "<m>"]` for the direct `extract_frames` call.
   - `yt_dlp_postprocessor_threads(&self) -> usize` returns just the resolved `ffmpeg_threads` value, for splicing into `--postprocessor-args` strings. The audio path has no filter graph, so `-filter_threads` is omitted there.
5. Refactor `extract_frames` to prepend `ffmpeg_thread_args` output to its existing ffmpeg argv.
6. Refactor `extract_audio` to inject `-threads <n>` into the `--postprocessor-args` string (prepended before `-vn -ac 1 -ar 16000 -b:a 64k`).
7. Log resolved values once at daemon startup: `ffmpeg thread caps: threads=4 filter_threads=4 (nproc=32, ffmpeg-threads=nproc/8, ffmpeg-filter-threads=nproc/8)`.

### Why these defaults

Default denominator `8` chosen by walking through three host shapes against `heavy=4`:

| Host    | nproc | resolved (nproc/8, floor 2) | 4 ffmpegs total cores | leaves N cores for everything else |
|---------|-------|------------------------------|-----------------------|-------------------------------------|
| 32-core | 32    | 4                            | 16                    | 16                                  |
| 16-core | 16    | 2                            | 8                     | 8                                   |
| 8-core  | 8     | 2 (floor)                    | 8                     | 0 (saturated, but cap will still hold load below ~10x prior incident) |
| 4-core  | 4     | 2 (floor)                    | 8 (oversubscribed)    | -                                  |

The 32-core target is the one we have measured. The smaller hosts inherit a sensible floor without an explicit per-host override. Anyone running borg on a 4-core box can drop `heavy=2` or set `ffmpeg-threads: 1` (floor is enforced at resolve, but config validation should warn on values below the floor, not refuse them).

The exposed YAML knob lets a 64-core host operator pick `nproc/4` (= 16 threads per ffmpeg) if they want fewer, fatter heavy slots, or `nproc/16` (= 4 threads per ffmpeg) for more concurrency at lower per-slot cost.

### Config shape

```yaml
youtube:
  # ... existing fields (slides:) ...
  ffmpeg-threads: nproc/8         # decoder/encoder threads. accepts: <int> | "nproc" | "nproc/N"
  ffmpeg-filter-threads: nproc/8  # filter graph threads (mpdecimate). same syntax.
```

Both fields hyphenated per the project's `kebab-case` serde convention; the Rust struct fields are `ffmpeg_threads` and `ffmpeg_filter_threads`.

### `ThreadCount` type

`ThreadCount` is a newtype struct over an internal `Spec` enum, with hand-written `Deserialize` and `Serialize` impls. The naive `#[serde(untagged)]` enum-with-struct-variant approach was considered and rejected: serde's default `Serialize` derive for `NprocOver { denom: 8 }` emits a YAML map `{denom: 8}`, not the symbolic string `"nproc/8"`, so config round-trips (e.g., `borg config dump` -> daemon reload) would silently break.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadCount(Spec);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spec {
    /// Literal thread count, e.g. `ffmpeg-threads: 4`.
    Absolute(usize),
    /// `nproc` divided by `denom` (denom == 1 means "all cores").
    /// Resolved at call time via `std::thread::available_parallelism()`.
    NprocOver { denom: usize },
}

impl ThreadCount {
    pub fn absolute(n: usize) -> Self {
        Self(Spec::Absolute(n))
    }

    pub fn nproc_over(denom: usize) -> Self {
        Self(Spec::NprocOver { denom })
    }

    pub fn resolve(self) -> usize {
        const MIN_FFMPEG_THREADS: usize = 2;
        let nproc = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let raw = match self.0 {
            Spec::Absolute(n) => n,
            Spec::NprocOver { denom } => nproc.saturating_div(denom.max(1)),
        };
        raw.max(MIN_FFMPEG_THREADS)
    }
}
```

**Deserialization** (`impl<'de> Deserialize<'de> for ThreadCount` via a `Visitor` that accepts both integer and string forms):
- YAML integer `4` -> `ThreadCount::absolute(4)`.
- YAML string `"nproc"` -> `ThreadCount::nproc_over(1)`.
- YAML string `"nproc/N"` for positive integer `N` -> `ThreadCount::nproc_over(N)`.
- `"nproc/0"`, negative integers, and any other string are config errors that fail config load (so a typo cannot silently default).

**Serialization** (matching `impl Serialize for ThreadCount`):
- `Spec::Absolute(n)` -> YAML integer `n`.
- `Spec::NprocOver { denom: 1 }` -> YAML string `"nproc"`.
- `Spec::NprocOver { denom: d }` -> YAML string `"nproc/d"`.

A unit test asserts YAML -> `ThreadCount` -> YAML round-trips byte-identically for each shape; this is the regression guard for the round-trip bug rejected above.

**No `Default` impl on `ThreadCount`.** Field-level defaults via `#[serde(default = "default_ffmpeg_threads")]` keep the denominator co-located with the field that uses it -- if a future config field needs a different denominator (or an absolute floor) there is no shared `Default` impl to fight against.

```rust
fn default_ffmpeg_threads() -> ThreadCount {
    ThreadCount::nproc_over(DEFAULT_FFMPEG_THREAD_DENOM)
}

fn default_ffmpeg_filter_threads() -> ThreadCount {
    ThreadCount::nproc_over(DEFAULT_FFMPEG_THREAD_DENOM)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct YoutubeConfig {
    pub slides: YoutubeSlidesConfig,
    #[serde(default = "default_ffmpeg_threads")]
    pub ffmpeg_threads: ThreadCount,
    #[serde(default = "default_ffmpeg_filter_threads")]
    pub ffmpeg_filter_threads: ThreadCount,
}
```

The current `#[derive(Default)]` on `YoutubeConfig` is replaced by an explicit `impl Default` that calls the same two functions; this keeps `cfg.youtube.default()` producing the same values whether borg lands here via `serde(default)` or via direct construction.

### Where the helpers live

Three ffmpeg-touching call sites exist in the borg tree:

| Site | Path | Treatment |
|------|------|-----------|
| Direct, slides extraction | `youtube.rs:400` (`extract_frames`) | Full `[-threads, -filter_threads]` injection. |
| Indirect via yt-dlp postprocessor | `youtube.rs:264` (`extract_audio`) | Splice `-threads N` into `--postprocessor-args` only. No filter graph used here, so `-filter_threads` is N/A. |
| Direct, version probe | `youtube.rs:726` (`ffmpeg -version`) | No change. One-shot, near-zero cost. |
| Direct, test fixture | `youtube.rs:737` (synth test video) | No change. Test code -- must not have its argv expectations broken. |

The helpers live as methods on `YoutubeConfig`:

```rust
impl YoutubeConfig {
    /// Argv tokens for a direct `Command::new("ffmpeg")` invocation that
    /// runs a filter graph (slides path).
    pub fn ffmpeg_thread_args(&self) -> [String; 4] {
        [
            "-threads".to_string(),
            self.ffmpeg_threads.resolve().to_string(),
            "-filter_threads".to_string(),
            self.ffmpeg_filter_threads.resolve().to_string(),
        ]
    }

    /// Thread count to splice into `yt-dlp --postprocessor-args`. Returns
    /// just the resolved integer; the caller is responsible for formatting
    /// the surrounding `ffmpeg:-threads N ...` postprocessor string.
    pub fn yt_dlp_postprocessor_threads(&self) -> usize {
        self.ffmpeg_threads.resolve()
    }
}
```

`extract_frames` and `extract_audio` each take only the resolved value(s) they need -- not the whole `&YoutubeConfig` -- so the call-chain change stays small:

- `extract_frames(...)` gains a `thread_args: [String; 4]` parameter; the call site in `process_youtube` passes `cfg.youtube.ffmpeg_thread_args()`.
- `extract_audio(...)` gains a `ffmpeg_threads: usize` parameter; the call site passes `cfg.youtube.yt_dlp_postprocessor_threads()`. The function formats its postprocessor string as `format!("ffmpeg:-threads {ffmpeg_threads} -vn -ac 1 -ar 16000 -b:a 64k")`.

### Startup log

In `borg::startup::init_permits` (or a sibling helper called from `main`) log a single line after permits init:

```
INFO  borg::startup: ffmpeg thread caps: threads=4 filter_threads=4 (nproc=32, config=nproc/8 / nproc/8)
```

This lets an operator confirm from journalctl alone that the cap is in effect and resolved as expected, without re-reading config + asking the OS for nproc.

### What this does NOT change

- `HEAVY_PERMITS` cap (still 4).
- `GENERAL_PERMITS` cap (still 8).
- The slides pipeline output (same JPEG count, same compression behavior).
- Any other subprocess (yt-dlp, fabric, whisper, vision API).
- On-disk artifacts, ledger schema, DLQ shape, watchdog behavior.

## Alternatives Considered

### Drop `HEAVY_PERMITS` from 4 to 2

Halves observed load but also halves YouTube ingest throughput. The replay-batch scenarios that motivated the original cap are exactly the case where we *do* want 4 concurrent traces -- the problem is per-trace cost, not concurrency. Rejected.

### Cap with `nice` / `ionice` / cgroups

Effective but platform-specific (Linux only via cgroup v2 + systemd slice, or `nice` everywhere with weaker semantics) and requires either root or careful systemd-user-slice configuration. The ffmpeg flag approach is universal, in-process, and requires zero ops setup. Reserved for a future "system-level resource isolation" doc if and when that becomes warranted.

### Single `ffmpeg-cores` knob instead of two

`-threads` and `-filter_threads` control different worker pools; ffmpeg does not collapse them. Setting only `-threads` leaves `mpdecimate` (the actually-expensive filter here) at its default auto behavior. Combining the two into one config field would force the same value for both -- workable for now, but a wart the first time a future operator wants to tune them independently. Two fields, same default expression -- the user types `nproc/8` twice in the rare case they need to override, and they keep the freedom to set different values without a config-shape migration later.

### Custom DSL like `nproc-1`, `nproc*0.75`

Tempting but every operator overrides would land in a different corner of the grammar. `nproc/N` covers the realistic tuning range (1, 2, 4, 8, 16) with one operator and one operand; richer arithmetic adds parser surface for marginal benefit. If a future host genuinely needs `nproc-1`, we add it then.

### Compute at startup, store resolved integer in config struct

Cleaner internally (no per-call resolve) but loses the ability to print the symbolic form (`nproc/8`) in the startup log and in `borg config dump` output. The per-call `resolve()` is `available_parallelism()` + a saturating divide + a `max` -- cost is negligible; ffmpeg invocation is the next thing to happen and it spawns a process.

## Implementation Plan

### Phase 1: types + config

1. Add `ThreadCount` newtype to `borg::config` with `Spec` internal enum, hand-written `Deserialize` and `Serialize` impls, and `resolve()` method.
2. Add `default_ffmpeg_threads` and `default_ffmpeg_filter_threads` free functions returning `ThreadCount::nproc_over(DEFAULT_FFMPEG_THREAD_DENOM)`.
3. Add `ffmpeg_threads: ThreadCount` and `ffmpeg_filter_threads: ThreadCount` fields to `YoutubeConfig` with `#[serde(default = "...")]` pointing at those functions. Replace the existing `#[derive(Default)]` with an explicit `impl Default for YoutubeConfig` so direct-construction matches deserialize-default.
4. Add unit tests covering:
   - Integer parse: `4` -> `ThreadCount::absolute(4)`.
   - String parse: `"nproc"`, `"nproc/8"` parse correctly.
   - Invalid parse: `"nproc/0"`, `"-1"`, `"4cores"`, `"nproc/abc"` all rejected.
   - Floor clamp: `ThreadCount::absolute(1).resolve() == 2`; `ThreadCount::nproc_over(999).resolve() == 2`.
   - Default value: `YoutubeConfig::default().ffmpeg_threads` matches `default_ffmpeg_threads()`.
   - **Round-trip:** for each of the three serialized shapes (`4`, `"nproc"`, `"nproc/8"`), assert that `serde_yaml::from_str -> serde_yaml::to_string` produces the same byte sequence. Guards against the regression rejected in "ThreadCount type".
5. Add `YoutubeConfig::ffmpeg_thread_args()` and `YoutubeConfig::yt_dlp_postprocessor_threads()` helpers.

### Phase 2: wire into ffmpeg call sites

1. Change `extract_frames` signature to accept `thread_args: [String; 4]`; prepend the args to the existing ffmpeg argv.
2. Change `extract_audio` signature to accept `ffmpeg_threads: usize`; format the postprocessor string as `format!("ffmpeg:-threads {ffmpeg_threads} -vn -ac 1 -ar 16000 -b:a 64k")`.
3. Update the call sites in `process_youtube` (and any other caller of `extract_audio` / `extract_frames` -- audit before editing) to pass `cfg.youtube.ffmpeg_thread_args()` / `cfg.youtube.yt_dlp_postprocessor_threads()`.
4. Add DEBUG-level entry logs in `extract_frames` and `extract_audio` recording the resolved thread args alongside the existing params (matches `rules/log.md` -- a DEBUG run should tell the full story without re-reading source).

### Phase 3: startup log

1. After `init_permits` in `main`, emit one `INFO` line with the resolved thread counts and the symbolic config form: `ffmpeg thread caps: threads=4 filter_threads=4 (nproc=32, ffmpeg-threads=nproc/8, ffmpeg-filter-threads=nproc/8)`.

### Phase 4: ship

1. `otto ci` (full pipeline -- lint, check, test).
2. `bump` for the next patch release.
3. `otto deploy` -- installs borg + cortex + oracle and restarts the daemons.
4. Observe next YouTube burst (mix of subtitled and non-subtitled videos to exercise both call sites); confirm load tops out near `heavy_cap * resolved_threads` rather than `heavy_cap * (auto-fan-out)`.

## Testing

Unit tests in `borg::config::tests`:

- `ThreadCount` parse:
  - `4` -> `ThreadCount::absolute(4)`.
  - `"nproc"` -> `ThreadCount::nproc_over(1)`.
  - `"nproc/8"` -> `ThreadCount::nproc_over(8)`.
  - `"nproc/0"`, `"-1"`, `"4cores"`, `"nproc/abc"`, `""` all rejected with a config error.
- `ThreadCount::resolve` honors floor `MIN_FFMPEG_THREADS = 2` for both `ThreadCount::absolute(1)` and `ThreadCount::nproc_over(999)`.
- `YoutubeConfig::ffmpeg_thread_args` returns `["-threads", "<n>", "-filter_threads", "<m>"]` in that order with the resolved values stringified.
- `YoutubeConfig::yt_dlp_postprocessor_threads` returns the same integer as `ffmpeg_threads.resolve()`.
- **YAML round-trip** (regression guard for the serde-untagged bug rejected above): for each of `4`, `"nproc"`, `"nproc/8"`, assert that loading the YAML into `ThreadCount`, then re-serializing, produces a byte-identical string. This is the test that would have caught the `{denom: 8}` map regression at compile-test time.
- `YoutubeConfig::default()` produces the same values as the `serde(default)` path on an empty YAML document.

Integration test (not new -- exists as ignored): `tests::test_extract_frames_synth_video` (the synth-video test in `youtube.rs:737`). It runs ffmpeg directly without the thread args helper; that's fine -- it's a test fixture, not a production call site. If the test passes a `thread_args: [String; 4]` parameter through, supply a fixed `["-threads", "2", "-filter_threads", "2"]` to keep the test hermetic.

No new end-to-end test. Manual verification after deploy:

```
sudo journalctl --user -u borg --since "1 minute ago" | grep "ffmpeg thread caps"
```

should show the configured values resolved against the host's `nproc`. During a subsequent YouTube burst, `ps -eo pcpu,cmd | grep ffmpeg` should show per-process CPU at roughly `resolved_threads * 100%`, not the previously-observed 400-600%. Confirming the audio path: queue a YouTube URL with no subtitles available, watch the journal for the `extract_audio` DEBUG line, then `ps` for the spawned ffmpeg child of yt-dlp -- it should also show the capped per-process CPU.

## Risks / Open Questions

- **Q:** Does `-threads 4` measurably slow frame extraction vs. `-threads 0` (auto) for a typical 1-hour YouTube video?
  **A:** Almost certainly yes -- ffmpeg's auto-threading is optimal *per process in isolation*. The whole point is that we're not in isolation: 4 concurrent ffmpegs each picking "auto" oversubscribes by ~4x. A 4-thread cap may add ~50-100% to single-process wall time but the *aggregate* throughput of 4 concurrent jobs goes up, not down, because the box stops thrashing. To be confirmed during shakedown.

- **Q:** Should `ffmpeg-threads` and `ffmpeg-filter-threads` ever be different defaults?
  **A:** Not until we have a measurement that says so. Keeping them coupled by default keeps the config surface narrow; the two-field shape preserves the option for future divergence without a migration.

- **Q:** Is the floor of 2 right, or should it be 1?
  **A:** ffmpeg's `-threads 1` runs a single decoder thread, which on long videos becomes meaningfully slower than `-threads 2` for negligible CPU savings. 2 is a better floor for the only call site we have today. Revisit if a low-power deploy target emerges.

- **Q:** Does the transcriber service (`transcriber.url = http://localhost:8090`) have its own unbounded-thread risk?
  **A:** Almost certainly yes -- any BLAS / OpenMP-backed local whisper implementation will saturate cores by default unless its startup arguments pin a thread count. That risk lives entirely inside the transcriber service's own process tree, not `borg.service`, so it is out of scope for this doc. Track as a follow-up: when the transcriber service is next touched, audit its threading model and either pin a thread cap at the service entry point or place it under a systemd slice with `CPUQuota=`. The `borg`-side caps proposed here are not affected either way -- borg only sends HTTP to the transcriber.
