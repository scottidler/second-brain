# YouTube Gemini Engine: Implementation Notes

## Phase 1: Config enum and validation
### Design decisions
- `YoutubeEngine`, `YoutubeGeminiConfig`, `YoutubeDataApiConfig` and the four new `YoutubeConfig` fields live in `borg/src/config.rs` beside `YoutubeConfig`, per the Data Model. `YoutubeEngine::as_str` gives validate messages the same kebab spelling serde uses.
- Defaults are named consts (`DEFAULT_GEMINI_*`, `MIN_GEMINI_*`) so Phase 3/4 and validate share one source.
- Per-key `deserialize_gemini_{window,timeout,budget}` wrappers pass the key name to `deserialize_humantime`, the same pattern as `queue:`/`pause-for`, so a bad duration names its key.
- Validation split into `Config::validate_youtube_engine` (called last from `validate`); `secret_resolves` treats an unset var, a missing file, and a blank value all as "empty".
- Duration bounds and `fallback == engine` are hard errors unconditionally (even when gemini is unused), per the doc.

### Deviations
- The WARN also fires when gemini is the `engine` WITH a `fallback` and a key is empty. The doc names only the gemini-as-fallback case; the engine-with-fallback case is neither an error nor specified, and a silent startup there would hide a guaranteed per-ingest auth failure.
- `cargo test -p borg config::tests::youtube_engine` as written does not compile on main or on this branch: `borg/src/pipeline/tests.rs:902,926` use `vault::search`, gated behind vault's `search` feature, which a bare `-p borg` build does not enable (verified by stashing and building main). The 14 tests pass with `cargo test -p borg --features vault/search --lib config::tests::youtube_engine`. Not fixed here (unrelated to this phase).

### Tradeoffs
- Test keys use never-set env var names rather than mutating process env: no `unsafe set_var`, no cross-test races.
- Warn-path test asserts via `vault::capture` on a unique key name so parallel tests cannot collide.

### Open questions
- Should the pre-existing `-p borg` test build failure (missing `vault/search` feature on `borg`'s dev-dependencies) be fixed, so the Phase 1 criterion command works verbatim?

### Success criteria
- `config::tests::youtube_engine`: 14 passed (with `--features vault/search --lib`, see Deviations).
- `XDG_CONFIG_HOME=<tmp> sb borg log --limit 1` with `youtube.engine: bogus`: exit 1, text:
  ```
  Error: Failed to load configuration
  Caused by:
      1: Failed to load config from <tmp>/sb/borg.yml
      2: Failed to parse config file
      3: youtube.engine: unknown variant `bogus`, expected `yt-dlp` or `gemini` at line 2 column 11
  ```

## Phase 2: Data API metadata client
### Design decisions
- `FetchError` lives in its own shared module `borg/src/youtube/error.rs` (re-exported as `youtube::FetchError`) so Phase 3's `gemini.rs` and Phase 4's `engine.rs` use it without depending on `dataapi`. Single-word filename per the naming rule.
- Variants carry a reason string (`NotFound(video_id)`, `NotLive(video_id)`, `Auth`, `Quota`, `Service`, `Network`, `Response`); `Empty` and `Timeout` are unit; `YtDlp(eyre::Report)`. `is_permanent()` matches `NotFound | NotLive` only. NotFound's Display is the doc's reason, `youtube data api: video <id> not found`.
- `youtube/dataapi.rs`: `DataApiClient::{new(key, timeout), from_config(&YoutubeDataApiConfig, timeout), #[cfg(test)] with_base_url}` and `fetch_metadata(video_id)`. `from_config` resolves via `vault::config::resolve_secret`; an unresolvable key becomes an empty key, which fails every fetch with `Auth` before any request.
- Mapping runs before `channels.list`: a NotLive or malformed video never spends a second request.
- 403/429 classification: 429, and 403 with reason in {quotaExceeded, rateLimitExceeded, dailyLimitExceeded, userRateLimitExceeded} -> `Quota`; other 401/403 and 400 keyInvalid -> `Auth`; 5xx -> `Service`; anything else -> `Response`.
- The key is sent only in `x-goog-api-key`; transport errors go through `reqwest::Error::without_url()`.
- A present-but-unparseable `publishedAt` is `Response` (loud); an absent one is `None`.
- Fixtures in `borg/src/youtube/fixtures/dataapi-*.json`: live video, `items: []`, upcoming, 403 quota, two channels bodies (handle, legacy).

### Deviations
- Fixtures are shaped by the API docs, not recorded from a live call: recording needs a real key.
- The doc says 403/429 -> `Quota | Auth` "classified by the error reason"; the reason list above is my reading of Google's documented reasons.

### Tradeoffs
- Payload variants vs unit variants: payloads cost nothing and make the fallback log line self-explanatory.
- The parser returns 0 for `P0D`; the zero-on-finished-video check lives in the mapper so NotLive detection stays separate.
- No retries in the client: Phase 4's fallback is the retry policy.

### Open questions
- None.

## Phase 3: Gemini transcript client
### Design decisions
- `borg/src/youtube/gemini.rs`, registered as `pub mod gemini` in `youtube.rs`. Public pieces, each testable alone: `plan_windows`, `build_request`, `extract_text` (returns `ModelOutput { served_model, text }`), `parse_window`, and `GeminiClient::fetch_transcript`. Tests in `gemini/tests.rs`, fixtures `fixtures/gemini-*.json`.
- `GeminiClient::{new(key, &YoutubeGeminiConfig), from_config(&YoutubeGeminiConfig), #[cfg(test)] with_base_url}`, mirroring `DataApiClient`. The client takes model, window, per-call timeout and budget straight from `YoutubeGeminiConfig`, so tests inject millisecond durations through the same struct and nothing waits 5 minutes.
- Budget seam: `fetch_transcript(video_id, duration_secs, started: Instant)`. The engine passes the `Instant` it took before the Data API request; the deadline is `started + budget`. Every attempt is bounded by `min(timeout, deadline - now)`. When the budget is the bound and it elapses, or it is already spent before an attempt (including a retry), the result is `FetchError::Timeout`.
- Output: `GeminiTranscript { segments: Vec<(f64, String)> }` plus `plain_text()` (segment texts joined with newlines), which is exactly `VideoFetch.{segments, transcript}` for Phase 4.
- Window planner: whole seconds, duration rounded up. A video no longer than `w` (including exactly `w`) is one window with `clipped: false`, and `build_request` omits the offsets for it. Zero or negative duration plans nothing, and `fetch_transcript` turns that into `Response` before any request.
- Steps extractor joins the `model_output` text parts with `\n`, so a line split across parts still parses (the tail becomes a continuation). Status absent or not `completed` -> `Response`. The served `model` is logged at DEBUG in `extract_text` and returned in `ModelOutput.served_model`.
- Line parser: lines are trimmed, blank ones skipped. Regex `^\[(?:(\d{1,2}):)?(\d{1,2}):(\d{2})\](?: (.*))?$`. Seconds must be < 60; minutes must be < 60 only when an hour field is present, so `[75:00]` from a clipped window past the hour is accepted. Invalid fields -> `Response`.
- Bounds check runs on the raw timestamp before order repair. The order ratio counts timestamped lines only: `out_of_order > 2% of timestamped` fails, so 2 in 100 passes and 3 in 100 fails.
- Retry: one per window. Retryable: HTTP 400 whose body contains `Please retry` (mapped to `Response`), 429 (`Quota`), 5xx (`Service`), and every `Response` from a 2xx body (malformed JSON, status not completed, bounds, order). Not retryable: 401/403 and 400 `API_KEY_INVALID` / `API key not valid` (`Auth`), other 4xx (`Response`), transport errors and the per-call timeout (`Network`). The retry is immediate, with no backoff, and still budget-bounded.
- Key: empty or whitespace -> `Auth` before any HTTP. It goes only in `x-goog-api-key`, transport errors pass through `reqwest::Error::without_url()`, and error details carry only the API's `error.message`/`status`, excerpted to 300 chars.

### Deviations
- The DEBUG log of the served model is not asserted by a test. `vault::capture` only captures WARN and above. Instead, `extract_text` returns the served model and a test asserts that. Same effect, correct seam. Widening `vault::capture` was out of scope.
- A per-call timeout that fires before the budget is `FetchError::Network("... per-call timeout ...")`, not `Timeout`, and it is not retried. The doc's retry list does not include it, and `FetchError::Timeout` is documented as "the gemini budget ran out". The "stub that never answers -> Timeout at budget" criterion is tested with `timeout > budget`. With the shipped defaults (5m timeout, 15m budget), a hung Gemini fails `Network` at 5m, and the fallback still runs because both are non-permanent.
- An untimestamped non-empty line before the first timestamp has no previous segment to join, so it is `Response`, which is retried. The doc does not cover that case; failing closed matches "the parser cannot trust the format".
- A timestamped line with no text (`[00:01:00]`) is accepted and takes continuation lines. If it is still empty at the end of the window it is dropped. It still counts toward the order ratio.
- The fixtures were written from the shapes in the doc's Phase 0 table, not recorded from live calls. The 46s Short and the 58-min responses were not kept.

### Tradeoffs
- Every check returns `FetchError::Response`, and the retry decision is a separate `retryable` bit on an internal `AttemptError`. The alternative was a new variant per failure kind. This way `FetchError` stays the Phase 2 set.
- `tokio::time::timeout` around each request, with reqwest's own total timeout set to the same per-call value as a backstop. The alternative was rebuilding the client per call with the remaining budget. A reqwest timeout maps to the same per-call `Network` error.
- Immediate retry vs backoff: the budget, not a delay, bounds the retry, and Phase 0's `Please retry` failures were model-side, not rate-related.

### Open questions
- Should a per-call timeout be retried, or be reported as `Timeout`? As built, the per-call timeout is the only bound that fires first with the defaults, so a hang never reaches the 15m budget.

## Phase 4: Engine seam and fallback in the pipeline
### Design decisions
- `borg/src/youtube/engine.rs`: `VideoFetch` and `fetch(kind, url, &Config) -> Result<VideoFetch, FetchError>`, a `match` on the kind. The `YtDlp` arm is the old `process_youtube` body moved: `fetch_metadata` concurrent with fabric `-y`, then the subtitles -> Groq whisper chain (`whisper_transcript`). Its error is `FetchError::YtDlp(report)`.
- The fallback policy lives in `pipeline::handlers::fetch_video`. A permanent error fails with no fallback. Any other error runs `youtube.fallback` when it is set, and sets `fell_back = true`. Otherwise the error fails the ingest. When the fallback also fails, the reason names both engines: `youtube engine gemini failed (<err>); fallback yt-dlp failed: <err>`.
- `FetchError::into_report` (`youtube/error.rs`) hands a yt-dlp arm error back as its original report. Under the default `engine: yt-dlp`, receipt reasons still read `yt-dlp metadata failed: ...` exactly as before, and a test pins that.
- `YouTubeResult.fell_back` is ORed into the receipt through `pipeline::receipt_degraded(distilled, tags_degraded, fell_back)`. It replaces the inline expression at the `IngestResult` (previously `pipeline.rs:1213`, now about 1218).
- `distill_for_publish_video(fabric, staging, trace_id, url, &VideoFetch, capture_note)`. The metadata and VTT re-fetch is deleted. With segments it uses `render_timestamped_transcript(segments)`; with empty segments it uses `video.transcript`, the old `transcript_fallback` rule. The unused `pipeline` and `title_hint` params are gone too: the only caller always passed `metadata.title`.
- `try_extract_slides(url, &[(f64, String)], duration, config)` binds `VideoFetch.segments`. Its `fetch_subtitles_raw` call is deleted, and the mp4 download stays.
- `borg/src/youtube/spawn.rs`: `spawn::yt_dlp(args) -> io::Result<Run>`, where `Run` is a boxed `wait_with_output` future. Callers keep their own timeout, and `kill_on_drop` is unchanged. All four ingest-path `yt-dlp` spawns go through it: `run_metadata`, `fetch_subtitles_raw`, `run_audio_extraction` and the slides download.
- Test counter: under `cfg(test)`, `spawn::yt_dlp` never starts a process. Inside `spawn::scope(stub, fut)`, a tokio task-local, each call is recorded and answered by the stub's canned `Output`. Outside a scope it returns an error. The count is per scope, so parallel tests cannot race it, and a test asserts that two concurrent scopes see 3 and 5 independently.
- Gemini-arm base URLs in tests: `engine::bases::scope(ApiBases, fut)`, also task-local. Under `cfg(test)` the arm builds its clients with `with_base_url` and fails with `Network` outside a scope, so no test can reach Google. Keys come from temp files, read through `resolve_secret`'s file path, with no env mutation.
- yt-dlp-arm runs in tests use `engine::stub::yt_dlp`, a canned argv responder. `--dump-json` prints the recorded `dump-json-53xycpcgqwe.json`. `--write-auto-sub` writes a VTT into the test tempdir and prints `{"en-orig":{"filepath":...}}`. Everything else (the mp4, audio) exits 1. `engine::stub::serve` is one loopback server that plays the Data API and Gemini, with per-route reply sequences and hit counts.
- `video_metadata_from_yt_dlp` is renamed to `video_metadata_from_youtube` (and its 3 test call sites), because it now maps metadata from either engine. `VideoMetadata` gains `Clone` and a doc line naming both sources.
- `config::default_links` is now `pub(crate)`, so the test config classifies a YouTube URL as YouTube (`Config::default()` carries no link rules).

### Deviations
- The yt-dlp arm fetches the raw VTT once, after metadata, and derives both the segments (`parse_vtt_segments`) and the subtitle transcript (`clean_vtt`) from that one fetch. Before, the handler's `fetch_subtitles` and the distiller's `fetch_subtitles_raw` were two separate yt-dlp calls, and the slides path made a third. Same output, fewer calls: "one fetch per ingest". `youtube::fetch_subtitles` (only `clean_vtt(fetch_subtitles_raw)`) had no remaining caller and was deleted. As before, a raw-VTT error is fatal only when fabric gave no transcript; with a fabric transcript it is logged and the segments are empty.
- The Data API client's HTTP timeout is `youtube.gemini.timeout`. The doc does not specify one, and adding a config key was out of this phase's scope.
- `youtube/search.rs` (channel search, `GET /channels/search`) keeps its own `std::process::Command::new("yt-dlp")`. The doc scopes the seam to `youtube.rs` and `handlers.rs`. Search is not the ingest path, and it already takes the `Command` as a parameter for its own tests.
- AC bullet 3 / "receipt is degraded" for the gemini-5xx fallback is verified in two pieces. `process_youtube` returns `fell_back = true` with the yt-dlp arm run. `receipt_degraded` is true for `fell_back` alone with a non-degraded `Distilled`. A whole-pipeline run cannot isolate the flag, because with no fabric in tests the distiller always falls back, so the receipt is degraded for that reason too. The NotFound case and the gemini publish case do run through `process_content` end to end.
- The Phase 4 criterion `cargo test -p borg` still needs `--features vault/search` to compile (pre-existing, see Phase 1).

### Tradeoffs
- A task-local scope vs a global `AtomicUsize` counter: a global races under parallel tests, and the task-local needs no lock. It works because the whole YouTube handler runs on one task. The fabric `spawn_blocking` never spawns yt-dlp.
- Stub-in-the-seam (no process under `cfg(test)`) vs counting real spawns: counting real spawns would start real yt-dlp against YouTube in tests. Failing closed outside a scope makes that impossible.
- The fallback policy in `handlers::fetch_video` vs in `engine`: the doc puts it in `process_youtube`, and keeping it out of `engine::fetch` leaves that function a pure kind dispatch.

### Open questions
- Should the Data API get its own timeout key (for example `youtube.data-api.timeout`) instead of sharing `youtube.gemini.timeout` (5m)? A Data API call is normally sub-second.

### Success criteria
- `engine: gemini`, slides off, against the stubs: spawn counter 0. `pipeline::tests::youtube::gemini_engine_slides_off_spawns_zero_yt_dlp` and, through the whole `process_content` pipeline with a published note, `gemini_engine_publishes_a_note_with_zero_yt_dlp_spawns`.
- Slides on: exactly 1, the mp4 download (`--merge-output-format` = 1, `--write-auto-sub` = 0). `gemini_engine_slides_on_spawns_only_the_mp4_download`.
- Gemini 5xx twice + `fallback: yt-dlp`: 2 `/interactions` hits, the yt-dlp arm ran (`--dump-json` = 1), `fell_back = true`, and `receipt_degraded` is true. `gemini_5xx_twice_falls_back_to_yt_dlp`.
- Data API `items: []` + `fallback: yt-dlp`, through `process_content`: receipt `failed` / `fetch-failed`, reason contains `not found`, spawn counter 0, `/interactions` 0. `data_api_not_found_fails_the_receipt_without_fallback`. Mutation check: disabling the permanent-error early return makes this test fail (the fallback published a note).
- Existing YouTube and distill tests pass, modified only by the `video_metadata_from_youtube` rename.
