## Phase 1: PublishPolicy threaded through, no behavior change
### Design decisions
- `PublishPolicy { Vault, Scratch { root } }` is `pub` in `borg/src/pipeline.rs` (derive Debug, Clone, PartialEq, Eq) - public so every transport and integration test can name `pipeline::PublishPolicy::Vault`; being `pub` in a lib means the unread `Scratch` variant raises no dead_code warning.
- `policy: &PublishPolicy` sits right after `config` on `process_content`, `process_url`, `process_url_inner`, `process_youtube`, plus `process_text` / `process_text_inner` - the text handler redirects a URL-bearing text to `process_url`, so the policy must reach it or that path could not pass one.
- `process_youtube` names its parameter `_policy` with a comment: nothing reads it until the Scratch phases; `process_url_inner` only forwards it.
- Frames work dir is keyed by trace id: `try_extract_slides` (`borg/src/pipeline/handlers.rs`) gains `trace_id: &str` and the dir is `borg-youtube-frames/<trace_id>`; the `.mp4` inside keeps its `<video_id>.mp4` name.
- Whisper audio: `whisper_transcript` (`borg/src/youtube/engine.rs`) uses `tempfile::Builder` `borg-whisper-` TempDir per call, removed on drop, replacing the shared `$TMPDIR/obsidian-borg/`.
### Deviations
- `try_extract_slides` and `process_text`/`process_text_inner` also gained parameters; the doc names only four functions. Same effect, correct seam (the trace id has to reach the dir name; the text path has to reach `process_url`).
- The doc says "All seven production callers"; `process_content` has an extra `attachment_note` argument today, so the policy is inserted before `trace_id`, not appended.
### Tradeoffs
- Policy after `config` vs. appended last: after `config` keeps the "what to do with this run" arguments together and leaves `trace_id`/`attachment_note` trailing as they were.
- No new test: Phase 1 adds no behavior and no public function; the existing suite pins that the signatures change and nothing else moved.
### Open questions
- `cortex daemon::tests::test_duration_until_weekly_returns_valid_duration` fails on main on the Sunday evening before the US fall-back DST change (`Sun 22:00` is 7d+1h away in wall-clock time, over the test's `7 * 24h` bound). It is a clock-dependent test bug unrelated to this work; it self-clears once the Sunday 22:00 hour passes. Not touched here. Worth a fix (bound `7d + 1h`) whenever convenient.
