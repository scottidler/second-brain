# Design Document: YouTube Gemini Engine

**Author:** Scott Idler
**Date:** 2026-10-16
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Add a `youtube.engine` enum (`yt-dlp | gemini`) plus an optional `youtube.fallback` to borg. The `gemini` engine fetches metadata from the YouTube Data API and the timestamped transcript from Gemini, with zero yt-dlp calls. When it errors and a fallback is set, the `yt-dlp` chain runs and the receipt is marked degraded. Phase 0 is already run and measured below. The gemini engine recovers live videos that yt-dlp currently fails on, and its transcript matches captions at 98.6% word precision.

## Problem Statement

### Background

- Borg ingests a YouTube URL through yt-dlp in up to six separate calls per video:
  - metadata: `youtube.rs:159`, `--dump-json`; fatal on error at `handlers.rs:119`
  - a second metadata fetch in `distill_for_publish_video`: `stages/distill.rs:738`
  - captions three ways:
    - `fabric -y`: `fabric.rs:48`
    - `fetch_subtitles`: `handlers.rs:128`
    - `fetch_subtitles_raw`: `distill.rs:739`, `handlers.rs:343`
  - audio for the Groq whisper fallback: `youtube.rs:407`
  - a 720p mp4 for slides: `handlers.rs:276`
- v0.17.2 -> v0.17.5 hardened the yt-dlp path with a failure-streak breaker (`borg/src/youtube/breaker.rs`). The breaker makes the failures cheaper. It does not make them succeed.
- `bradautomates/claude-video` v0.3.2 (`skills/watch/scripts/gemini.py`) sends a YouTube URL directly to Gemini's Interactions API. Nothing is downloaded. It is the wire-shape reference for this doc.

### Problem

yt-dlp metadata is the dominant failure, and most of those videos are alive.

- Failed receipts, last 14 days, 818 traces (`sb borg log --status failed --since 14d`, one `--trace` lookup each):
  - 709 `yt-dlp metadata failed`:
    - 543 `Video unavailable`
    - 139 `Sign in to confirm you're not a bot`
    - 37 `This video is unavailable`
    - 10 `has been removed`
    - 4 `youtube paused`
  - 28 `yt-dlp audio extraction failed`
- Data API `videos.list` check of a random sample of those failures:
  - `Video unavailable`: 12/12 public and alive
  - bot-check: 11/12 alive
  - `This video is unavailable` / `has been removed`: 4/4 gone (zero items)
- The failures are intermittent. `NEz_gCNwt-Q` failed with `Video unavailable` at 2026-10-16T04:57Z (`cl-c14ee1cc`) and succeeded on reingest at 15:06Z (`cl-3cec5418`).
- So the 682 `Video unavailable` + bot-check failures are, on that sample, YouTube refusing yt-dlp on live videos. The 47 `This video is unavailable` / `has been removed` failures are dead videos.

### Goals

- `youtube.engine: yt-dlp | gemini` selects the implementation (Scott: "an option to enable the gemini path ... should probably be in an enum").
- `youtube.fallback` runs the other engine when the selected one errors (Scott: "or fallback to the yt-dlp path").
- Under `engine: gemini`, a successful ingest spawns zero yt-dlp processes for metadata and transcript.
- The note is shaped the same under either engine:
  - the same `VideoMetadata` fields
  - the same `[HH:MM:SS] text` transcript feeding `distill-video` and cortex transcript embedding
- A fallback run is visible: the receipt is `degraded`.

### Non-Goals

- **Slides frames.** The slide-aware path keeps its own yt-dlp mp4 download under both engines and stays best-effort as today. A failed download falls back to a text-only note (`handlers.rs:171`). Gemini cannot return frames.
  - Its transcript binding moves to `VideoFetch.segments` (Architecture), so the mp4 is the only yt-dlp call left under `engine: gemini`.
- **Channel backfill** (`channels.rs:36`) keeps calling `fetch_metadata`. It is a separate maintenance verb, not the ingest path.
  - Parked. Revisit when slide-bearing notes measurably drop under `engine: gemini`.
- **Per-request engine override** (a reingest flag, an `IngestRequest` field). Config only.
  - Parked until Scott asks.
- **Gemini summaries replacing the distill patterns.** Gemini returns a transcript only. `distill-video` stays the distiller.
- **Non-YouTube video** (Loom, local files, the Files API upload path). This engine is YouTube-only.
- **Breaker changes.** The breaker keeps its yt-dlp-stderr signal and gates only yt-dlp calls.
- **Frontmatter recording which engine ran.** The receipt's `degraded` flag and the logs carry it.
  - Parked until a quality comparison needs it.

## Phase 0 Results (run 2026-10-16, desk, keys from `keep/.secrets`)

Zero code. curl against both APIs. The numbers are pasted here and the scripts died with the scratchpad.

| Probe | Result |
|---|---|
| `GEMINI_API_KEY`, `YOUTUBE_API_KEY` | both decrypt and authenticate; both already in ripr's `borg.env` |
| Data API `videos.list?part=snippet,contentDetails,status` | returns title, channelTitle, channelId, publishedAt (`2026-07-02T13:43:53Z`), duration (`PT8M32S`), description, tags; a dead id returns `items: []` with HTTP 200 |
| Data API `channels.list?part=snippet&id=UC...` | `snippet.customUrl` = `@bradbonanno` (the `channel_handle`) |
| Interactions, `processing: "agentic"`, 8.5 min | 1 of 3 calls 200; 2 of 3 HTTP 400 `Model generated invalid JSON syntax ... Please retry` |
| Interactions, `processing: {"type":"static"}`, 8.5 min | 200 in 41s; 46,513 video tokens; 3,350 output tokens |
| Static transcript vs staged captions (`ht-37006094`) | 1,737 vs 1,746 words; word-level precision 0.986, recall 0.981 (difflib longest-match over lowercased tokens) |
| Static, 58.5 min (`4l8MXYUqGaA`) | 200 in 314s; 319,687 video tokens; 26,995 output tokens; 1,036 lines, all `[HH:MM:SS]`; last line `[00:58:31]` of 58:33; 1 line out of order (`[00:01:01]` between `00:04:57` and `00:05:04`) |
| Static with `start_offset: 3000s`, `end_offset: 3300s` | 200 in 27s; 27,301 video tokens; timestamps absolute (`[50:00]` matches the full run's `[00:50:00]`), but formatted `[MM:SS]` despite the prompt |
| Static, Short `BJq73Hecbbo` (46s), sent as `watch?v=<id>` | 200 in 14s; 19 `[MM:SS]` lines through `[00:44]` |
| Static, Short `jTWbGuJcSI8` (7s, no speech) | 200 in 5s; `status: completed`, `total_output_tokens: 0`, no `steps` text |

What Phase 0 settles:

- **Static, never agentic.** Agentic failed 2 of 3 calls, and static succeeded 3 of 3.
- **Video tokens scale at about 5.4k per minute.** One call per video does not scale to multi-hour videos, so the client splits the video into fixed clip windows.
- **The parser cannot trust the format.**
  - Accept `[MM:SS]`, `[H:MM:SS]` and `[HH:MM:SS]`, and normalize to `[HH:MM:SS]`.
  - Repair a timestamp that goes backwards.
  - Fail closed on out-of-window and badly ordered timestamps.

## Proposed Solution

### Overview

```
process_youtube(url)
  -> engine = config.youtube.engine
  -> fetch = engine.fetch(url)                 // VideoFetch { metadata, transcript }
       Err(e) if e.is_permanent()  -> fail (fetch-failed), no fallback
       Err(e) if fallback.is_some() -> fallback.fetch(url), receipt degraded
       Err(e)                      -> fail (fetch-failed)
  -> slides path (best-effort, yt-dlp mp4, transcript binding from fetch.segments)
  -> distill_for_publish_video(.., &fetch)     // no re-fetch
```

### Architecture

- New module `borg/src/youtube/engine.rs` (the `youtube/` dir already exists; `breaker.rs` lives there):
  - `pub async fn fetch(kind: YoutubeEngine, url, &Config) -> Result<VideoFetch, FetchError>`
  - It matches on the kind, the same way `distillers::tags::build_kind` does (`distillers/src/tags.rs:249-285`).
- `YtDlp` arm: today's calls, moved, not rewritten:
  - `fetch_metadata`
  - the transcript chain at `handlers.rs:128-155` (fabric `-y`, `fetch_subtitles`, Groq whisper on audio)
  - plus `fetch_subtitles_raw` for the timestamped form `distill.rs:739` uses today
  - Breaker behavior is unchanged:
    - `fetch_metadata` both checks the breaker and records into it (`youtube.rs:161-172`), the only call that records.
    - `fetch_subtitles_raw` (`youtube.rs:214`), audio (`:458`) and the slides download (`handlers.rs:275`) only check it.
    - fabric `-y` does neither.
- `Gemini` arm:
  - `youtube/dataapi.rs`: `videos.list` + `channels.list` -> `VideoMetadata`.
  - `youtube/gemini.rs`: Interactions calls over clip windows -> timestamped transcript.
  - It does not consult the breaker. Breaker state is yt-dlp's.
  - The Data API call finishes before the first Gemini window starts, because windows need `duration_secs`. That is one extra serial round trip that the yt-dlp arm, with its parallel metadata and fabric calls, does not pay.
- `FetchError`, the full set:
  - `NotFound`, `NotLive`: permanent, below.
  - `Auth`: covers a key env var resolving empty, raised before any HTTP request.
  - `Quota`, `Service`, `Network`.
  - `Response`: malformed body, status not `completed`, or a parse check failed.
  - `Empty`: no speech in any window.
  - `Timeout`: the gemini budget ran out.
  - `YtDlp(eyre::Report)`: the yt-dlp arm's existing errors, wrapped unchanged.
- **Permanent errors skip the fallback.** `FetchError::is_permanent()` is true for exactly two variants:
  - `NotFound`: Data API `items: []`, so the video is gone. yt-dlp would only re-fail and add to the breaker streak. Reason: `youtube data api: video <id> not found`.
  - `NotLive`: `snippet.liveBroadcastContent` is `upcoming` or `live`. There is no finished recording to transcribe; yt-dlp's `This live event will begin` is already on the breaker's `PERMANENT` list (`breaker.rs:41`).
  - Both -> `failure_stage = fetch-failed`.
- Shorts and `youtu.be` URLs are sent to Gemini as `https://www.youtube.com/watch?v=<id>`, using the id from `youtube::extract_video_id`.
- `distill_for_publish_video` (`distill.rs:737-780`) takes the `VideoFetch` instead of re-fetching metadata and VTT. Both engines drop the duplicate fetch.
  - It keeps today's rule: no subtitles, a subtitle error, or empty segments -> `transcript_fallback` (`distill.rs:761-780`).
  - It loses the `yt-dlp-metadata-error` degraded path (`distill.rs:741-751`). Metadata already succeeded by the time the distiller runs, so that path can no longer fire. That is a deliberate improvement.
- `try_extract_slides` (`handlers.rs:254`) takes `&VideoFetch.segments` and drops its own `fetch_subtitles_raw` call (`handlers.rs:343`).
  - Slide transcript binding then uses the same segments under both engines.
  - The mp4 download stays (Non-Goals).
- `YouTubeResult` (`pipeline.rs:55`) gains `fell_back: bool`, ORed into the receipt's `degraded` at `pipeline.rs:1213` beside `tags_degraded`.

### Data Model

```rust
// borg/src/config.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum YoutubeEngine {
    #[default]
    YtDlp,
    Gemini,
}

// added to YoutubeConfig (#[serde(default, rename_all = "kebab-case")])
pub engine: YoutubeEngine,          // default yt-dlp: today's behavior
pub fallback: Option<YoutubeEngine>, // default None: today's behavior
pub gemini: YoutubeGeminiConfig,
pub data_api: YoutubeDataApiConfig,

#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct YoutubeGeminiConfig {
    pub model: String,           // "gemini-3.8-flash": the model that served every Phase 0 call
    pub api_key: String,         // env var NAME, default "GEMINI_API_KEY"
    #[serde(deserialize_with = "deserialize_humantime", serialize_with = "serialize_humantime")]
    pub window: Duration,        // 30m: clip length per Interactions call; >= 5m
    #[serde(deserialize_with = "deserialize_humantime", serialize_with = "serialize_humantime")]
    pub timeout: Duration,       // 5m per call; >= 30s
    #[serde(deserialize_with = "deserialize_humantime", serialize_with = "serialize_humantime")]
    pub budget: Duration,        // 15m for all windows + retries; >= timeout
}

#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct YoutubeDataApiConfig {
    pub api_key: String,         // env var NAME, default "YOUTUBE_API_KEY"
}

// borg/src/youtube/engine.rs
pub struct VideoFetch {
    pub metadata: VideoMetadata,
    pub transcript: String,                  // plain, for the existing fallback consumers
    pub segments: Vec<(f64, String)>,        // timestamped, what parse_vtt_segments yields today
    pub engine: YoutubeEngine,               // which engine produced it
    pub fell_back: bool,                     // true when the fallback engine produced it
}
```

- `yt-dlp` arm:
  - `transcript` is today's chain result (fabric, then subtitles, then whisper).
  - `segments` is `parse_vtt_segments(fetch_subtitles_raw)`, or empty when there are no subtitles.
  - Empty `segments` keeps today's `transcript_fallback` behavior in the distiller (`distill.rs:764-771`).
- `gemini` arm:
  - `segments` comes from the parser.
  - `transcript` is the segment texts joined with newlines.

- Base URLs are not config. Each client gets a `#[cfg(test)]` `with_base_url` constructor, the `TranscriptionClient::with_groq_url` precedent (`transcription.rs:55`).
- An unknown `engine` / `fallback` value fails at every config load: serde rejects an unknown enum variant.
- The rest of validation lives in `Config::validate` (`config.rs:1260`). It runs only at daemon start; the doctor skips it by design. Hard errors that name the field:
  - `fallback == Some(engine)`
  - `window < 5m`, `timeout < 30s`, `budget < timeout`
  - gemini is the `engine`, there is no `fallback`, and the gemini or data-api key env var resolves empty
- Daemon-start WARN: gemini is only the `fallback` and its key resolves empty. The primary still works, so the daemon still starts.
  - The daemon is the only process that gets `borg.env`, which is why the check lives in `validate` and not in the doctor.
- At runtime a key that resolves empty is `FetchError::Auth`, raised before any HTTP request.
- `YoutubeConfig` gains `deny_unknown_fields`.
  - Today `youtube.engine: bogus` loads silently (observed below).
  - Ripr's live `youtube` block holds only `slides`, `ffmpeg-threads` and `ffmpeg-filter-threads`, all known keys.
- YAML, as ripr will run it:

```yaml
youtube:
  engine: gemini
  fallback: yt-dlp
  gemini:
    model: gemini-3.8-flash
    window: 30m
```

### API Design

**Data API** (`youtube/dataapi.rs`), header `x-goog-api-key`:

- `GET https://www.googleapis.com/youtube/v3/videos?part=snippet,contentDetails&id={id}`, mapped into `VideoMetadata`:
  - `title`, `channelTitle` -> `uploader`
  - `description`, `tags` (absent -> empty), `channelId`
  - `channel_url` = `https://www.youtube.com/channel/{channelId}`
  - `contentDetails.duration` -> `duration_secs`.
    - The ISO-8601 parser accepts `P#DT#H#M#S` with any part omitted.
    - `P0D` / zero is legal only for `NotLive`. A zero duration on a finished video -> `Response`.
  - `publishedAt` (RFC 3339, UTC) -> its UTC date, `YYYY-MM-DD`.
    - Divergence, accepted: this is the public publish time, while yt-dlp's `upload_date` is the upload date.
    - The two differ for scheduled releases and premieres, and by a day near midnight UTC.
    - `published_at` is the content date in `cortex-video-published-at`; publish time is the closer match to what a viewer sees.
- `GET https://www.googleapis.com/youtube/v3/channels?part=snippet&id={channelId}`:
  - `snippet.customUrl` -> `channel_handle`, only when it matches `HANDLE_PATTERN` (`youtube/search.rs:24`). Otherwise `None`.
    - Legacy custom URLs (no `@`) never become a handle.
  - If this call fails, `channel_handle` is `None`, logged at WARN. Not fatal: yt-dlp's `uploader_id` is `Option` today too.
- Errors:
  - `items: []` -> `NotFound`
  - 403 or 429 -> `Quota | Auth`, classified by the error reason
  - 5xx -> `Service`
  - The key never appears in any error `Display`.

**Gemini** (`youtube/gemini.rs`), header `x-goog-api-key`, `POST https://generativelanguage.googleapis.com/v1beta/interactions`:

```json
{"model": "<model>",
 "input": [{"type": "video", "uri": "https://www.youtube.com/watch?v=<id>",
            "processing": {"type": "static", "start_offset": "<s>s", "end_offset": "<e>s"}},
           {"type": "text", "text": "<TRANSCRIPT_PROMPT>"}]}
```

- Windows:
  - `[0, w), [w, 2w), ...` up to `duration_secs`, with `w = gemini.window`.
  - A video shorter than `w` is one window, and offsets are omitted.
  - Calls run sequentially inside the existing `HEAVY_PERMITS` hold (`handlers.rs:86`).
- Deadlines:
  - Each call: `gemini.timeout` (5m). The longest Phase 0 call was 314s, on a 58.5 min single call; a 30 min window sits between that and the 41s for 8.5 min.
  - All windows plus retries: `gemini.budget` (15m), measured from the first Data API request.
    - Exhaustion -> `Timeout`, which is non-permanent, so the fallback runs.
    - 15m is half of `pipeline.hard-timeout-secs` (1800, `config.rs:419`). The yt-dlp fallback keeps the other half.
  - The hard timeout wraps all of `process_url_inner`, including the wait for a heavy permit (`pipeline.rs:531`).
    - If it fires first, the receipt is `pipeline-timed-out` exactly as today, and no fallback runs.
    - A video too long for 15m of windows fails over to yt-dlp. It does not run into the hard timeout.
- Response text: `steps[type == "model_output"].content[type == "text"].text`, joined.
- The served `model` field is logged at DEBUG. Every Phase 0 call requested `gemini-3.7-flash` and was served by `gemini-3.8-flash`.
- Per-window parse. Every check fails closed to `Response`:
  - `status` other than `completed` -> `Response`.
  - Each line matches `^\[(\d{1,2}:)?\d{1,2}:\d{2}\] ` and is normalized to `[HH:MM:SS]`.
  - A non-empty line with no timestamp is appended to the previous segment.
  - Window bounds: any timestamp outside `[window_start - 5s, window_end + 5s]` -> `Response`.
  - Order: a timestamp below the previous one is set to the previous one.
    - More than 2% of a window's lines out of order -> `Response`.
    - Phase 0 measured 1 in 1,036 lines (0.1%).
  - `completed` with no `model_output` text: that window has no speech, so zero segments and `Ok`.
- Whole fetch: zero segments across every window -> `Empty`.
  - `Empty` is non-permanent, so with a fallback set the yt-dlp chain decides. Its Groq step is the authoritative check for speech.
  - Without a fallback, the receipt is `fetch-failed`. A silent video under gemini-only fails visibly instead of publishing an empty note on one 7s sample's evidence.
- No truncation check:
  - The Interactions response carries no finish or stop signal. Phase 0 responses hold only `status`, `usage`, `steps` (`thought`, `model_output`), `model`.
  - Windows prevent truncation by construction. A 30 min window produces about 14k output tokens, half the 26,995 the 58.5 min single call returned whole.
- Retry:
  - One retry per window on HTTP 400 containing `Please retry`, 5xx, 429, or a parse `Response` error, within `budget`.
  - Then the window's error fails the fetch, and the fallback decides.
- `TRANSCRIPT_PROMPT` is the Phase 0 prompt:
  - verbatim, no summarizing
  - one line per segment, `[HH:MM:SS] text`
  - nothing else

### Implementation Plan

#### Phase 0: Prove both APIs from desk
**Model:** none (done)
- Results table above.
- **Success criteria:** met. Static Interactions returns a 58-minute transcript covering 58:31 of 58:33. The Data API returns every `VideoMetadata` field.

#### Phase 1: Config enum and validation
**Model:** sonnet
- `YoutubeEngine`, `YoutubeConfig.{engine, fallback, gemini, data_api}`, `YoutubeGeminiConfig`, `YoutubeDataApiConfig`, defaults.
- Add `deny_unknown_fields` to `YoutubeConfig`.
- The `Config::validate` rules and the daemon-start WARN, as specified in Data Model.
- Annotated block in `config/templates/borg.yml.example`.
- Tests in `borg/src/config/tests.rs`:
  - omitted `engine` -> `YtDlp` with `fallback: None`
  - `engine: gemini` + `fallback: yt-dlp` parses
  - `engine: bogus` errors naming `yt-dlp`, `gemini`
  - `validate`: `fallback == engine` errors
  - `validate`: `window: 0s` errors
  - `validate`: gemini engine + no fallback + empty key errors
- **Success criteria:**
  - `cargo test -p borg --features vault/search --lib config::tests::youtube_engine` passes.
    - Amended at finalization: the bare `cargo test -p borg ...` does not compile on main (34b6012). `borg/src/pipeline/tests.rs` uses `vault::search`, which sits behind vault's `search` feature. The doc defect is in the command, not the code.
  - `XDG_CONFIG_HOME=<tmp> sb borg log --limit 1`, with `youtube.engine: bogus`, exits non-zero naming the variants.

#### Phase 2: Data API metadata client
**Model:** sonnet
- `youtube/dataapi.rs`: `videos.list` + `channels.list` -> `VideoMetadata`. ISO-8601 duration parse, `HANDLE_PATTERN` gate.
- `FetchError`, the full set from Architecture.
- Recorded fixtures: one live video, one `items: []`, one `upcoming`, one 403 quota body.
- A stub server through a `#[cfg(test)]` `with_base_url`, the same shape as `TranscriptionClient::with_groq_url` (`transcription.rs:55`) and its tokio TCP stub (`transcription/tests.rs`).
- **Success criteria:**
  - The live fixture fills all nine `VideoMetadata` fields.
  - Duration: `PT8M32S` -> 512, `P1DT2H` -> 93600, `P0D` -> 0.
  - A legacy `customUrl` without `@` -> `channel_handle: None`.
  - The empty fixture -> `NotFound`; a `liveBroadcastContent: upcoming` fixture -> `NotLive`.
  - A test asserts that the key string is absent from every error's `Display`.

#### Phase 3: Gemini transcript client
**Model:** opus
- `youtube/gemini.rs`:
  - window planner
  - request builder (static, offsets)
  - `steps` extractor
  - line parser and normalizer
  - bounds and order checks
  - per-call timeout, budget, retry policy as specified
- Fixtures:
  - a Phase-0-shaped response with `[HH:MM:SS]` lines
  - a `[MM:SS]` clip response
  - one out-of-order line in 100
  - a timestamp outside its window
  - a `completed` no-text response (the 7s Short)
  - the HTTP 400 `Please retry` body
- **Success criteria:**
  - Window planner: 3,513s with `w = 1800` -> `[(0,1800),(1800,3513)]`; 512s -> one window with no offsets.
  - Parser:
    - `[50:00] x` -> `(3000.0, "x")`
    - 1 out-of-order line in 100 -> `Ok`, no decreasing timestamp
    - 3 in 100 -> `Response`
    - `[40:00]` inside a `(0,1800)` window -> `Response`
    - `completed` no-text -> zero segments, `Ok`
  - Stub: a 400 `Please retry` then a 200 -> success after exactly 2 requests. A stub that never answers -> `Timeout` at `budget`.

#### Phase 4: Engine seam and fallback in the pipeline
**Model:** opus
- `youtube/engine.rs`, `fetch` + `VideoFetch`. Move the yt-dlp metadata and transcript calls out of `process_youtube` into the `YtDlp` arm.
- `process_youtube` calls the engine and applies the fallback.
- `YouTubeResult.fell_back` (`pipeline.rs:55`) is ORed into the receipt's `degraded` at `pipeline.rs:1213`, beside `tags_degraded`.
- Thread `VideoFetch` into `distill_for_publish_video` (single caller, `handlers.rs:187`), and delete its metadata and VTT re-fetch (`distill.rs:737-780`).
- Thread `VideoFetch.segments` into `try_extract_slides`, and delete its `fetch_subtitles_raw` call (`handlers.rs:343`).
- Add a yt-dlp spawn seam. Every `Command::new("yt-dlp")` in `youtube.rs` and `handlers.rs` goes through one fn, with a `#[cfg(test)]` counter. None exists today.
- Update `borg/AGENTS.md` and the root `CLAUDE.md` YouTube notes: the engine seam, gemini never touches the breaker, one fetch per ingest.
- **Success criteria:**
  - `engine: gemini`, slides disabled, against the stubs: a pipeline test reads the yt-dlp spawn counter as 0.
  - The same with slides enabled reads exactly 1 (the mp4 download). Transcript binding made no yt-dlp call.
  - Gemini stub 5xx twice + `fallback: yt-dlp` -> yt-dlp arm runs and the receipt is `degraded`.
  - Data API `NotFound` + `fallback: yt-dlp` -> `fetch-failed`, and the yt-dlp arm never runs.
  - `cargo test -p borg` passes with the existing YouTube and distill tests unmodified apart from the call-signature change.

**Operator step** (not a phase), on ripr after ship:
1. Set `engine: gemini`, `fallback: yt-dlp` in `~/.config/sb/borg.yml`.
2. Run `otto deploy`, which restarts `borg.service`.
3. Check the daemon log for the startup line. No validate error and no key WARN.
4. Reingest `NEz_gCNwt-Q` and `SDLfjrUyBw4`.
5. Check `sb borg log --degraded --since 24h`.

**Rollback**, config only:
1. Set `engine: yt-dlp`.
2. Delete the `fallback` key, or set `fallback: gemini`. Leaving `fallback: yt-dlp` makes `engine == fallback`, which `validate` rejects at daemon start.
3. Run `systemctl --user restart borg.service`.

## Acceptance Criteria

- [ ] `youtube.engine: bogus` in `borg.yml` is a config-load error naming both variants.
  - Command: `XDG_CONFIG_HOME=<tmp> sb borg log --limit 1`
  - Observed on main (v0.17.5): `Loaded config from: .../borg.yml`, table printed, exit=0. The key is silently ignored.
- [ ] `rg -n 'enum YoutubeEngine' borg/src/config.rs` returns exactly one line.
  - Observed on main: zero lines, exit 1.
- [ ] Under `engine: gemini` with `youtube.slides.enabled: false`, a pipeline test against the Data API and Gemini stubs publishes a note, and the yt-dlp spawn counter reads 0.
  - With slides enabled, the counter reads exactly 1: the mp4 download, which Non-Goals keeps.
  - Observed on main: cannot run; depends on Phase 4.
- [ ] On ripr with `engine: gemini`, `sb borg ingest https://www.youtube.com/watch?v=SDLfjrUyBw4` ends `succeeded` and not degraded.
  - Command: `sb borg log --source '%SDLfjrUyBw4%'`
  - Observed on main: that id is in the 14-day failed set with `yt-dlp metadata failed: ... Video unavailable`, while the Data API reports it public, `PT6M59S`.
- [ ] With the Data API returning `items: []`, the receipt is `fetch-failed` with reason containing `not found`, and no fallback runs.
  - Observed on main: cannot run; depends on Phases 2 and 4.

## Resolved Decisions

- 2026-10-16, author, from Phase 0: **static processing only.** Agentic failed 2 of 3 calls with a server-side JSON error.
- 2026-10-16, author, from Phase 0: **the gemini engine sources metadata from the Data API.** Without that, the gemini engine keeps the yt-dlp metadata call behind 709 of the 737 yt-dlp failures (709 metadata + 28 audio) over 14 days.
- 2026-10-16, author: **`fallback` is `Option<YoutubeEngine>`, symmetric.**
  - `engine: yt-dlp, fallback: gemini` is legal too.
  - One enum, not a second fallback enum that repeats its variants.
  - Precedent: `tags.classifier.fallback: ClassifierKind`.
- 2026-10-16, author: **defaults preserve today**: `engine: yt-dlp`, `fallback: None`. Ripr opts in through config.
- 2026-10-16, panel round 1 (architect + staff-engineer), all 15 findings folded in:
  - **Slides:** transcript binding moves to `VideoFetch.segments`.
  - **Coverage:** the 90% check is replaced.
    - The API has no finish signal, so windows prevent truncation by construction.
    - Bounds and order checks fail closed.
    - Whole-video empty -> `Empty` -> fallback.
  - **Rollback:** also drops `fallback` and restarts the daemon.
  - **Breaker:** recorder vs checker described accurately.
  - **Deadlines:** a `budget` with a 5m per-call timeout.
  - **Wiring:** named (`YouTubeResult.fell_back`, `Config::validate`) and lower bounds added.
  - **Key check:** moved to `validate`, the daemon-only process with `borg.env`. Phase 5 dropped.
  - **Test URLs:** base URLs are `#[cfg(test)]` constructors, not config.
  - **Mapping:** `publishedAt` divergence accepted and stated; `customUrl` gated by `HANDLE_PATTERN`; day and zero durations parsed.
  - **Notes:** the removed distill degraded path, Data API serial latency, and the channel backfill are stated.
  - **Panel answers:** Q1 safe; Q2 yes, both seats; Q3 no, now replaced; Q4 three divergences, now handled.
  - Synthesis: `~/.cache/review-panel/runs/9uzIUO9B/synthesis.md`.

## Alternatives Considered

### Gemini for transcript only, keep yt-dlp metadata
- **Description:** Swap only the caption and whisper chain for Gemini.
- **Pros:** No Data API client.
- **Cons:** Leaves the fatal metadata call in place. Phase 0 shows that call is the failure.
- **Why not chosen:** Solves nothing measured.

### Agentic processing (claude-video's default)
- **Description:** `processing: "agentic"` as `gemini.py` sends it.
- **Pros:** Gemini picks frames itself.
- **Cons:** 2 of 3 calls failed with HTTP 400 in Phase 0. It also spent 12k tool-use tokens on an 8.5 min video.
- **Why not chosen:** Measured unreliable for a daemon.

### Single Interactions call per video
- **Description:** No windows.
- **Pros:** Simpler.
- **Cons:** About 5.4k video tokens per minute, so a 3-hour video is about 970k input tokens in one call, with output that long in one response.
- **Why not chosen:** Windows behave the same at every length. A short video collapses to one window with no offsets.

### Separate `fallback` enum with a `none` variant
- **Why not chosen:** Two enums carrying the same engine names. `Option` says "none" without a second type.

## Technical Considerations

### Dependencies
- No new crates.
  - `reqwest` 0.13.2, `serde_json` and `humantime` are already in `borg/Cargo.toml`.
  - Durations use the existing `deserialize_humantime` / `serialize_humantime` (`config.rs:1`, `:199`).
- Secrets: `GEMINI_API_KEY` and `YOUTUBE_API_KEY` already ride ripr's `borg.env` through `daemon.env-bootstrap`. Config holds env-var names only (`resolve_secret`, `config.rs:1`).

### Performance
- Gemini time per window, from Phase 0: 41s for 8.5 min, 314s for 58.5 min single-call, and 27s for a 5 min clip.
- A 30 min window lands between those. The 5m per-call timeout sits just under the 314s single-call maximum, and windows halve that load.
- `gemini.budget` (15m) bounds the whole gemini fetch, leaving half of the 1800s hard timeout to the yt-dlp fallback.
- Worst case for a 3-hour video: 6 windows sequential, inside `pipeline.hard-timeout-secs` (1800).
  - That is measured only to 58 min.
  - Videos long enough to breach 1800s fail `pipeline-timed-out` exactly as yt-dlp + whisper would today.

### Security
- Keys travel only as the `x-goog-api-key` header to `generativelanguage.googleapis.com` and `www.googleapis.com`.
- The URL sent to Gemini is a public YouTube URL. No local media leaves ripr.
- The transcript is untrusted content, the same as captions today. It flows only into fabric patterns, never into a command.

### Testing Strategy
- Recorded fixtures plus a tokio TCP stub per client, with no network in tests (borg convention).
- The pipeline test counts yt-dlp spawns through a seam rather than asserting on logs.

### Rollout Plan
- Ship with the defaults unchanged, then run the operator step on ripr.
- Rollback is config only: `engine: yt-dlp`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Gemini paraphrases instead of transcribing | Low (0.986 precision measured) | Claim anchors and recall drift | Prompt pinned. Bounds and order checks catch garbled timestamps. The fallback is available |
| Model id retired or silently remapped | Seen in Phase 0 (3.7 -> 3.8) | Behavior shifts without a config change | Model is config. The served model is logged. `fallback: yt-dlp` keeps ingest alive and degraded, and `degraded_24h` in `sb doctor` surfaces it |
| Data API quota exhausted | Low | Gemini engine fails at metadata | `Quota` error -> fallback runs. Two quota units per video (`videos.list` + `channels.list`) |
| Output timestamp format drift | Seen in Phase 0 | Bad anchors | Parser accepts three shapes and repairs order |
| Non-English video: Gemini transcribes the spoken language, yt-dlp requests `en-orig,en` captions | Low (vault is English-heavy) | Transcript language differs by engine | The distill patterns already run on whatever the transcript language is. Not addressed further |
| Slides mp4 download still calls yt-dlp under `engine: gemini` | Certain | That download keeps 403ing; it only checks the breaker and never records into it | Non-Goal by design. A failed download degrades to a text-only note, as today |

## Open Questions

- None.

## References

- `bradautomates/claude-video` v0.3.2, `skills/watch/scripts/gemini.py`, CHANGELOG 0.3.0
- `docs/design/2026-08-30-video-distill-token-budget.md`
- `distillers/src/tags.rs:164-285` (kind + fallback precedent)
- `borg/src/youtube/breaker.rs`
