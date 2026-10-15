# Implementation notes: note sources, intent, tools, channels

## Phase 1: VTT overlap dedupe
### Design decisions
- Overlap check is a separate `overlap_remainder` helper, run only when `rolling_dedupe_action` returns Push (borg/src/youtube.rs, `parse_vtt_segments`). Today's prefix-growth rule is untouched and the new check runs in addition to it.
- A matching cue is pushed as a new segment carrying only the non-overlapping remainder, with its own start time; a fully covered cue is dropped. This keeps timestamps accurate for the new words.
- Overlap must be whole words and at least `MIN_OVERLAP_WORDS` (3), so a legitimately repeated short phrase is not eaten.
### Deviations
- None.
### Tradeoffs
- Fixed 3-word minimum vs. no minimum: avoids false merges at the cost of missing 1-2 word overlaps.
- `clean_vtt` (plain-text path) left unchanged vs. also fixing it: the phase names only `parse_vtt_segments`.
### Open questions
- None.

## Phase 2: Source archive write, archive-first reads, durability on `/trace`
### Design decisions
- New `borg::archive` module (`SourceArchive::write`, `is_durable`, `trace_dir`) owns the archive layout: `<root>/<trace>/distilled.yml`, plus `transcript.md` for repo (rendered README transcript) and thread (thread markdown). Video and article write `distilled.yml` alone, since its `transcript` is the source. `transcript.md` is written first and `distilled.yml` last, each temp-then-rename, so `distilled.yml` presence means a complete archive.
- The archive root is config, not a hardcoded path: borg `archive.root` and cortex top-level `archive-root`, both defaulting to `vault::paths::borg_sources_dir()`, mirroring the `staging.root` / `staging-root` pair. Both example configs document it.
- The write sits in one seam, `pipeline::archive_then_publish` (borg/src/pipeline/publish.rs), called by `process_url_inner` in place of the bare `atomic::publish_unique`. It writes the archive, then the note; `PublishError::ArchiveWrite` (and `NoteWrite`) both map to `FailureStage::PublishFailed` through `PublishError::failure_stage`, and `publish_failure_result` builds the terminal `IngestResult`. This made the "read-only archive dir fails the trace, no note" criterion testable without a live fetch.
- `distill_for_publish_repo` now returns `RepoDistillation { distilled, readme_transcript }` so the README the distiller saw reaches the archive whether or not staging is enabled (borg/src/stages/distill.rs).
- `vault::trace::dir(root, trace)` is the one per-trace path join, refusing anything but a single plain component. borg's archive writer, the `/trace` route and cortex's reader all go through it, so a `..` trace id from the route or from frontmatter never resolves outside the root.
- `vault::distilled::FILENAME` is the shared `distilled.yml` name; borg's `DISTILLED_FILENAME` now aliases it and cortex imports it.
- Cortex: `embed::source` (new file, split out to stay under the 1,500-line bloat limit) holds `SourceRoots { archive, staging }`, `read_source_transcript` (archive first, then staging) and `revive_archived_sentinels`. `process_batch` takes `SourceRoots` in place of `staging_root`.
- Sentinel retry: `revive_archived_sentinels` clears the transcript-chunk examined row of a staging-sourced note whose archived `distilled.yml` mtime is newer than the sentinel watermark (new `SearchIndex::sentinelled_notes` / `clear_embedding_examined_batch`). A re-sentinel raises the watermark to the archive mtime (`sentinel_watermark`), so an archive that exists but has no transcript cannot revive the same note every tick (the oscillation the sentinel exists to stop; pinned by `archive_without_transcript_does_not_revive_the_sentinel_every_tick`).
- `/trace/{id}` computes `source-durable` from `archive::is_durable` for both the found and the 404 response.
- Oracle's `trace_block` stays filesystem-free; its doc comment and the MCP server instructions now say `within-window` reflects staging retention only and point at the daemon's `/trace` `source-durable` for durability (the Architecture bullet on the trace block description).
### Deviations
- `PublishError` did not exist; it was created in `borg/src/pipeline/publish.rs` with two variants (`ArchiveWrite`, `NoteWrite`) rather than only the one the doc names, so the note-write failure also flows through the typed seam. Same `publish-failed` stage as before for the note write.
- The archive is written immediately before the note's atomic write, after the slide-aware path has already copied slide JPEGs into the vault. On an archive failure those attachments remain, exactly as they already do when the note-size gate fails at the same point. The note itself is never written, which is what the doc requires.
- Archiving covers the URL kinds the per-kind Resolved Decision names (video, article, repo, thread). Image, voicenote, idea/text, document and session paths are not archived: session is skipped by decision, and the others keep their verbatim text in the note body (`## Transcript`), so the decision does not list them.
- `TraceStateResponse` keeps its existing snake_case keys (`trace_id`, `failure_stage`) and adds `source-durable` (kebab) as the doc's API section spells it, with `#[serde(default)]` so a client reading an older daemon still parses.
### Tradeoffs
- Sentinel revival keyed on the archive file's mtime vs. a new "source appeared" column: the mtime needs no schema change and is in the same unit as `notes.modified_at` (unix seconds). Cost: an archive written in the same second as the sentinel does not revive it; the next archive rewrite or note edit does.
- Revival runs at the top of every transcript batch (one indexed query plus a stat per sentinelled note) vs. once per daemon tick: simpler wiring, and the sentinel set is small (~hundreds).
- Archive-first falls back to staging when the archived `distilled.yml` has no transcript, rather than treating an archive hit as final: the staged copy is the same bytes when present, and the fallback costs one read.
### Open questions
- None.

## Phase 3: Archive backfill verb
### Design decisions
- `borg::archive::backfill::run(config, apply) -> BackfillReport` (borg/src/archive/backfill.rs) walks `staging.root`, and for each trace dir holding `distilled.yml` and no durable archive, builds a `SourceArchive` and calls `.write()`. It is the same writer the live publish seam uses, so the per-kind file layout lives in one place.
- Kind comes from the staged `distilled.yml`'s `meta.extractor` (`distill-repo-v1`, `distill-thread-v1`, `distill-session-v1`). Repo and thread take the staged `transcript.md` as `source_md`; repo falls back to `readme_from_stage0` on `fetched.html` (the Resolved Decision's order); session is skipped and counted; everything else archives `distilled.yml` alone.
- "No archive dir yet" is `archive::is_durable` (archived `distilled.yml` exists), so an existing complete archive is never rewritten, and a crash-partial dir (only `transcript.md`) gets completed.
- Unreadable or unparseable staged `distilled.yml` is collected in `failed` and printed; the CLI exits non-zero after processing the rest, rather than silently skipping.
- CLI: `sb borg retention archive [--apply]`, dry-run by default (sb/src/cli/borg.rs, args.rs).
### Deviations
- The ripr success criterion (`fd ... | wc -l` >= 1,361 after `--apply`) is an operator step and is not run here. Covered by unit tests: dry run reports N to copy and writes nothing, `--apply` copies them, a second run reports 0 to copy.
### Tradeoffs
- Kind detection by extractor id vs. inferring from file presence: the extractor id is explicit; file-presence would archive Stage-1 `transcript.md` for article traces too, which the live path does not.
- A repo whose `fetched.html` has no readme gets `distilled.yml` alone, as the live path does when the README is unavailable, rather than failing the trace.
### Open questions
- Operator step still owed: run `sb borg retention archive --apply` on ripr before anyone runs `sb borg retention sweep`.

## Phase 4: Capture-note boilerplate filter
### Design decisions
- `router::extract_capture_note` filters `Source: <site>` lines (case-insensitive prefix, trimmed, non-empty remainder) BEFORE the whitespace-collapsing token pass — borg/src/router.rs — the line boundary is gone once whitespace is collapsed. Prose that merely starts with the word "Source" (`Source code is...`) is kept because the rule requires the `Source:` colon form.
- `router::drop_note_equal_to_title(note, title)` — borg/src/router.rs — pure, case-insensitive, whitespace-collapsed equality; returns `None` for empty/blank notes.
- Publish seam: applied where `NoteContent` is built in `process_url_inner` (borg/src/pipeline.rs), against `scraped_title` (the title the fetcher returned, not the owner/repo or thread override). That drops `## Why Captured` and the `capture-note:` frontmatter key.
- Measurement: the 58 `capture-note:` values from ~/repos/scottidler/obsidian/notes (YAML-decoded, each fed through `extract_capture_note` with a trailing URL line) via a throwaway test, reverted and not committed. Observed: `MEASURE survivors=9 of 58`. The 9 survivors are 1 real intent note and 8 page titles, which the title-equality rule handles at publish (needs the fetched title, not measurable offline).
### Deviations
- The title-equality drop runs at the `NoteContent` construction seam, after the distiller has already received the note (the title only exists after the fetch and distill runs inside the same fetch step). So a title-equal note can still reach the single-call distill context; it is dropped from `## Why Captured` and frontmatter only.
### Tradeoffs
- Prefix-line rule vs. a per-site allowlist: the share sheet emits `Source: <any site>`, so an allowlist would never keep up.
- Title equality by collapsed case-insensitive match vs. fuzzy match: exact is predictable and cannot discard real intent that merely overlaps the title.
### Open questions
- None.

## Phase 5: Borg-owned preserve key, reader and writer
### Design decisions
- `markdown::BORG_PRESERVE_KEYS = &[CAPTURE_NOTE_KEY]` and `markdown::CAPTURE_NOTE_KEY` (borg/src/markdown.rs), beside `RENDER_NOTE_KEYS`: borg owns the field, so the list lives with borg's renderer. `CORTEX_PRESERVE_KEYS` is untouched; `publish/tests.rs::borg_preserve_keys_are_not_cortex_keys` pins the two lists disjoint.
- Reader: `publish::read_preserved_fields` collects a borg key's line plus every following indented or blank line and decodes that one entry with `serde_yaml` (`decode_borg_scalar`), so `|-` block scalars and multi-line quoted strings decode exactly. Cortex keys keep the existing line-based reader, whose doc comment explains why a full-frontmatter parse is avoided.
- Fail closed: an undecodable `capture-note` entry is an `Err` that aborts the reingest, matching the reader's existing contract for an unreadable note.
- Writer: `atomic::apply_borg_field` writes `{key}: {yaml_scalar(v)}`; `markdown::yaml_scalar` became `pub(crate)` for it.
- Precedence: a fresh render that already carries `capture-note` wins and the preserved value is dropped (debug log). The newest stated intent is the operator's current one; Phase 6's `--note` on a `--force` reingest is the case this protects.
- Renamed `read_cortex_fields` -> `read_preserved_fields`, `apply_cortex_fields` -> `apply_preserved_fields`, and the `cortex_fields` local -> `preserved_fields` (borg/src/pipeline.rs, publish.rs, atomic.rs, tests, pipeline/AGENTS.md): they now carry borg keys too, and the old names would lie. pipeline/AGENTS.md gains invariant 12 for the borg list.
- Orchestrator decision (resolves this phase's open question): the preserve path runs the same `Source: <site>` boilerplate rule as ingest. `router::strip_share_boilerplate` is the one shared function; `extract_capture_note` and `publish::read_preserved_fields` both call it. A preserved `capture-note` that is boilerplate only is not preserved at all. No vault cleanup verb. Test: `publish/tests.rs::preserved_boilerplate_capture_note_is_dropped_and_intent_survives`.
### Deviations
- Cortex scalars still write raw (`{key}: {v}`), not through `yaml_scalar`. Re-encoding them would quote values like `cortex-confidence: 0.85` into strings and change their on-disk type; the doc's `yaml_scalar` bullet is scoped to the borg key that can carry free text.
- New tests live in borg/src/pipeline/publish/tests.rs, not pipeline/tests.rs: the latter would exceed the 1500-line `otto ci` bloat limit.
### Tradeoffs
- Per-entry YAML decode vs. whole-frontmatter parse: per-entry decodes the borg key exactly while a malformed unrelated key elsewhere in an old note still cannot block the reingest.
- Fresh-wins vs. preserved-wins for `capture-note`: cortex scalars are preserved-wins because cortex's work is newer than a refetch; for a borg key the refetch IS the operator's newer statement.
### Open questions
- None. (Resolved: whether the preserve path filters share boilerplate. The orchestrator answered yes; see Design decisions.)

## Phase 6: --note through CLI, client, intake, replay
### Design decisions
- `sb borg ingest --note <TEXT>` (sb/src/cli/borg/args.rs, borg.rs) feeds `borg::ingest(.., note)`, whose body comes from the pure `client::ingest_body`: the note is trimmed and omitted when blank, so a bare capture sends the same bytes as before. The daemon's existing `IngestRequest.note` path does the rest; the field is additive, `extension_body_matches_ingest_request` still passes.
- Intake record: the note is stored in the Stage-0 envelope's `extra` map (borg/src/stages/raw.rs `capture_extra`, read back by `envelope_capture_note`), keyed by `markdown::CAPTURE_NOTE_KEY` so the "capture-note" string is not hardcoded again. Staging is what `sb borg replay` already reads, and Stage 0 runs before any gate or fetch, so a failed trace holds the note.
- Replay: pure `replay::replay_body(url, method, note)` builds the POST body (note only when present). `replay_note` prefers the envelope note (failed traces), else the published note's `capture-note:` found through `vault::identity::NoteIndex::resolve_trace` and read with Phase 5's `read_preserved_fields` (so block scalars and the boilerplate rule apply). `--bootstrap-from-vault` reads `capture-note:` straight from the note it was given.
- `--note` with `--file` is a loud error (a file capture has no URL note seam).
### Deviations
- The design says "Intake records the note with the URL". The raw-input sidecar and receipts `raw_input` stay the bare URL (other readers treat them as the URL); the note is recorded in the staged envelope instead, which is the store replay reads. Same effect, correct seam.
### Tradeoffs
- Published-trace replay does a vault scan per replayed trace that has no envelope note, vs. trusting the receipts `note_path`: that path is a location snapshot, stale once `cortex classify` moves a note. Phase 5's fresh-vs-preserved rule means a force reingest keeps `capture-note` even without this lookup; the lookup makes the POST body honest.
### Open questions
- None.

## Phase 7: tools contract
### Design decisions
- `Tool { name, url, command }` with `PartialEq` added beyond the doc's derive set, no `kind` field (vault/src/distilled.rs): equality lets the round-trip test assert whole values.
- `Distilled.tools` is `#[serde(default, skip_serializing_if = "Vec::is_empty")]` (vault/src/distilled.rs): empty tools stay out of serialized yml, so pre-tools staged files and fresh no-tools files share a shape.
- Bounds `MAX_TOOLS = 20`, `MAX_TOOL_COMMAND_CHARS = 500` in `enforce_tool_bounds`, called from `enforce_bounds` (distillers/src/validate.rs). Records `tools:N>20` and `tool-command:idx:orig>500` in `bounds_truncations`.
- Command overflow is cut at a char boundary, not a sentence boundary: a command is verbatim text, not prose.
- Every existing `Distilled { .. }` literal (distillers, plus tests in vault/borg/cortex/distillers) gained `tools: Vec::new()`; the compiler enforced it.
- Superseded: the char-boundary-cut bullet above (and the `tool-command:idx:orig>500` record format) no longer applies.
- Orchestrator decision: an over-limit `command` is dropped (set to `None`, tool kept), recorded as `tool-command:idx:orig>500 dropped`. Why: the doc pins the ceiling to `cortex-repo-install`, which drops over-limit values; a cut shell command is a different, possibly harmful command; Phase 9 grades commands by exact substring.
### Deviations
- None. Observed eval output: `sb borg eval --staged --no-judge` exit 0, line `  note-size: 1361/1361 within the 65536-byte ceiling` (built from this tree with `cargo build --release -p sb`, run as `./target/release/sb`). Flag spelling matches the doc; the line carries a trailing "within the 65536-byte ceiling" clause.
### Tradeoffs
- Skip-serialize-when-empty vs always emitting `tools: []`: chose skip to keep staged yml diffs quiet.
### Open questions
- None.

## Phase 8: Tools render section, repo install in body, Links dedup
### Design decisions
- `## Tools & Commands` sits after Claims, before Links, omitted when empty (`distillers/src/render.rs:push_tools`, per Part 3). A tool with an empty name is skipped.
- Bullet shape is `- name url `command``. A multiline command becomes a fenced block indented under the bullet (`push_tools`). The code span/fence delimiter is one longer than the longest backtick run in the command, padded per CommonMark when it touches a backtick, so a command cannot break out.
- `RepoPayload.install` joins the section as an `Install` entry (`tools_to_render`), skipped when a tool already carries that exact command. The body has no length cap; frontmatter `cortex-repo-install` keeps its 500-char cap.
- Links dedup lives in `render` for every kind (`push_links`). URLs compare trimmed, trailing-slash-insensitive, case-insensitive. `## Links` disappears if every entry was a tool URL. `merge_description_links` untouched.
- `tempfile` added as a distillers dev-dependency (workspace version) for the indexing test.
### Deviations
- None.
### Tradeoffs
- Case-insensitive URL compare vs exact: a case-only-different path could be dropped wrongly, but that risk is smaller than leaving duplicates.
### Open questions
- FTS criterion proven by `rendered_install_command_is_found_by_bm25_after_indexing` (temp `SearchIndex`, bm25 `search` on `quuxtool`). The live check, `sb oracle call knowledge_search --json '{"query":"<that command>","mode":"bm25"}'` after reindex, is owed after deploy. Nothing live was touched.

## Phase 9: Eval harness for Parts 2 and 3
### Design decisions
- A fixture's optional `capture-note.txt` loads into `Fixture.capture_note`, trimmed, and is `None` when absent or blank (`borg/src/eval/fixtures.rs:read_capture_note`). A blank note counts as a bare capture, the same as on ingest. Staged fixtures always carry `None`.
- `--synth-intents` lives in `borg/src/eval/intents.rs:synth_intents`. The writer sits behind the `IntentWriter` trait, and production uses `FabricIntentWriter`, which calls `vault::fabric::run_pattern` (the same seam the judges use) with a new pattern, `borg/patterns/synth-capture-intent.md`. The pattern is added to `bootstrap::PATTERNS`, which brings CLAUDE.md to 29 pattern files.
- Notes go only to `INTENT_KINDS` = article, repo, session, thread, video, the kinds whose distill patterns take a capture note (Phase 10's list). Files open with `create_new`, so an existing note is never overwritten, even one that appears mid-run. A writer error stops the run. Notes already written stay, and a rerun picks up where it stopped.
- `clean_intent` strips fences and wrapping quotes and collapses whitespace. An empty reply, or one over `MAX_INTENT_CHARS` (400), is an error and is never truncated into a note.
- `--regenerate` lives in `borg/src/eval/regenerate.rs`. `redistill` (async, through an injected `distillers::Dispatch`) distills each fixture twice, without and then with its note. `grade` then runs the existing sync `evaluate` once per arm. The judges and the SQLite cache never live across an await. Production builds the dispatcher with `borg::stages::distill::dispatcher_from_fabric_config(&config.fabric)`, the same one the pipeline uses.
- Both arms run over the same set: the fixtures that carry a `capture-note.txt`. Fixtures without one are skipped and counted, so the two arms' numbers compare directly (Phase 10's "no more unsupported with the note than without"). Both arms run uncalibrated, because the hand labels describe the golden distillations. If no fixture has a note, the run fails and points at `--synth-intents`. An unroutable kind label is an error (`distill_kind_for_fixture`).
- The `tools-grounding` metric is `calc::tools_grounding`. Names come from `Distilled.tools` and are graded by the quote shingle rule (`piece_grounded`). A name with no words is ungrounded. Commands come from `distillers::tools_to_render` (made `pub` and re-exported), so a repo's `install` line counts. They are graded by `command_grounded`: an exact, case-sensitive substring after whitespace collapse on both sides. The `Install` label render gives that line is not graded as a name.
- Report line: `tools-grounding: names g/n (p%), commands g/n (p%)  (k fixtures)`, plus per-kind rows and `LOW` rows (`report.rs:render_tools_grounding`). With no tools it prints `tools-grounding: N/A (no note renders a tool)`.
- `judge_note_text` appends a `TOOLS:` block (name, URL, `: command`) built from `tools_to_render`, so the judge sees what the note renders.
- Observed, golden fixtures, `cargo run -q -p sb -- borg eval --no-judge`:
  ```
    tools-grounding: names 0/0 (N/A), commands 2/2 (100.0%)  (2 fixtures)
      repo       names 0/0 (N/A), commands 2/2 (100.0%)
  ```
- Observed, `cargo run -q -p sb -- borg eval --staged --no-judge` (1364 fixtures):
  ```
    tools-grounding: names 0/0 (N/A), commands 47/64 (73.4%)  (64 fixtures)
      repo       names 0/0 (N/A), commands 47/64 (73.4%)
  ```
  Spot check: `repo/cl-05e26c9f` is ungrounded because its install is `git clone ... && cd ClaudeJSONLbrowser && npm install && npm run dev`, a chain the LLM assembled from separate README steps. That is the case the metric exists to flag.
- `--synth-intents` live run: `synth-intents: 18 written, 0 already present (kept), 5 fixtures outside article/repo/session/thread/video`. 18 `capture-note.txt` files are committed: 5 article, 3 repo, 1 session, 3 thread, 6 video. None was hand-written or edited.
- `--patterns-dir <DIR>` (orchestrator decision, answering open question 1: no deploy in the middle of the run). Every pattern the eval distills or judges through resolves from DIR. That covers the distill patterns behind `--regenerate`, `judge-distillation`, `judge-claims`, and the intent writer's `synth-capture-intent`. Phases 10 and 11 measure with `sb borg eval --regenerate --patterns-dir borg/patterns`.
  - `borg/src/eval/patterns.rs:resolve_patterns_dir` tilde-expands and canonicalizes DIR at the boundary and requires it to be a directory.
  - `pattern_in` maps a name to the absolute `DIR/<name>.md`, which must exist. A missing pattern is an error before fabric runs.
  - `PatternDirFabric` wraps the `FabricCaller` and rewrites each request's pattern. `vault::fabric::resolve_pattern` passes the absolute path through.
  - The dispatcher comes from `regenerate_dispatcher`, which calls the new generic `stages::distill::dispatcher_with_caller` (same per-kind config as production). The judges come from `eval::fabric_judges`, the writer from `eval::fabric_intent_writer`.
  - With no flag, behavior is unchanged: names resolve through `vault::paths::patterns_dir()`. Live ingest still goes through `dispatcher_from_fabric_config` -> `FabricShell`, and that path is untouched.
  - Tests: `eval::patterns::tests` (the dispatcher sends `DIR/distill-{article,repo,video,thread}.md` to fabric; a missing pattern never reaches fabric; the judges and writer resolve from DIR).
### Deviations
- The eval's fabric calls now pass borg's configured credential var. `EvalOpts.api_key` comes from `config.fabric.api_key`, and `FabricJudge::new` / `FabricClaimJudge::new` take it. Before this, they passed `""` and relied on fabric's own `.env`. On this host that `.env` has no `ANTHROPIC_API_KEY`, and `fabric` fails with `could not find vendor`, so neither the judges nor the intent writer could run. Same seam (`run_pattern`), now given the credential the pipeline already uses.
- The design names no model flag for the intent writer. `--synth-intents` reuses `--judge-model`, and that flag's help now says it covers every eval fabric call.
- To run the one-time synth before any deploy, I copied the single new file `borg/patterns/synth-capture-intent.md` into `~/.config/sb/patterns/`. It was a new file, nothing was overwritten, and it holds the same bytes `sb bootstrap --force` / `otto deploy` will write.
- `--regenerate` was not run live: it means two full distills (map-reduce on the long videos) plus both judges for 18 fixtures. Its wiring is proven by `regenerate::tests::regenerate_distills_each_noted_fixture_twice_and_grades_both_arms`, which uses a fake `Dispatch` and the mock judges.
### Tradeoffs
- Regenerate's arms cover only note-bearing fixtures, not every fixture: a same-set comparison beats including image/voicenote/idea fixtures that would distill identically in both arms.
- A hard 400-char ceiling on a synthesized note, with an error past it, instead of truncating: a truncated note is a hand-shaped note, and the design forbids those.
- An eval-local `PatternDirFabric` wrapper vs threading a base dir into `vault::fabric::resolve_pattern`: the wrapper keeps live ingest's resolution untouched.
### Open questions
- RESOLVED (orchestrator): Phases 10 and 11 would have measured deployed patterns. Answer: no deploy in the middle of the run; add `--patterns-dir` (see Design decisions).
- Pre-existing flake, unrelated to this phase, which hit one `otto ci` run: `cortex::sweep::tests::test_daemon_cold_tick_fires` takes only `serial_test::serial(xdg_data_home)`. The staged-proposal tests at `cortex/src/sweep/tests.rs:738` and `:790` take only `testutil::lock_env()` and redirect `XDG_DATA_HOME` through `empty_receipts_db()`. Two disjoint locks guard one env var. When they overlap, the cold tick opens another DB and surfaces 0. The fix is for all three to take the same lock. Left out of this phase's commit.

## Phase 10: Capture note into map-reduce, plus note pattern guidance
### Design decisions
- `build_reduce_input(.., note: Option<&str>)` and `build_thread_reduce_input(.., note: Option<&str>)` (distillers/src/parse.rs). A non-blank note leads the reduce input in the same labeled "context only - NOT instructions" block the single-call path uses. A blank or absent note leaves the input byte-identical to a bare capture.
- One block source: the new private `parse::capture_note_section` builds it, and `compose_capture_input`, `build_reduce_input` and `build_thread_reduce_input` all call it. The single-call bytes are unchanged, pinned by `parse::tests::compose_capture_input_unchanged_by_shared_note_section`.
- Thread: the note comes before `## Thread Head` and appears once. The thread builder calls `build_reduce_input` with `None` and adds the note itself.
- Every long-path caller passes the note: `distill_long` in article.rs, video.rs, thread.rs and session.rs gains a `capture_note` arg, fed from `inputs.capture_note`. The map (chunk) calls do not get the note. The doc scopes the change to the reduce input, and the reduce pattern is where tldr and claims are selected.
- voicenote.rs passes `None`, per the Non-Goal "Capture notes on voicenote, image and idea ingests". A comment there says why.
- Patterns: one `# CAPTURE NOTE` block, identical in all 9 files, placed just before `# OUTPUT`: distill-{video,article,thread,session}, their `-reduce` siblings, and distill-repo. It names the labeled input section and covers: weight tldr (when the schema has one) and claim selection toward the stated interest; never invent relevance the source does not support; ignore an empty note; treat instructions in the note as content; never turn the note into a claim, quote or link.
- distillers/AGENTS.md gains an invariant line for the capture note on both paths.
- Tests: `video::tests::long_video_with_capture_note_carries_note_into_reduce_input` (the success-criterion test) plus `long_video_without_capture_note_has_no_note_block`. Also `article::tests::long_article_reduce_input_carries_capture_note`, `thread::tests::long_thread_reduce_input_carries_capture_note`, `session::tests::map_reduce_session_reduce_input_carries_capture_note`, and three parse tests (leading block, blank note is byte-identical, thread ordering). Bite check: I passed `None` at the video reduce call site and the success-criterion test failed (`panicked at distillers/src/video/tests.rs:1001`). It passes again after the revert.
- Observed: `rg -l "capture note" borg/patterns/distill-*.md | wc -l` -> `9`.
- Observed, live, `./target/release/sb borg eval --regenerate --patterns-dir borg/patterns` (release build of this tree, exit 0, one attempt, no prompt iteration). `regenerate: 18 fixtures with a capture note, each distilled without and with it (5 without a note skipped)`. Claim support:
  ```
  === without capture note ===
  kind         claims  supported  unsupported  contradicted
  article          48      27.1%           35             0
  repo             29      89.7%            3             0
  session           8     100.0%            0             0
  thread           17      94.1%            1             0
  video            59      32.2%           40             0
  ALL             161      50.9%           79             0

  === with capture note ===
  kind         claims  supported  unsupported  contradicted
  article          45      24.4%           34             0
  repo             29      86.2%            4             0
  session           8     100.0%            0             0
  thread           17     100.0%            0             0
  video            63      34.9%           41             0
  ALL             162      51.2%           79             0
  ```
  Unsupported, without vs with the note: 79 vs 79. The criterion "no more unsupported with the note than without" passes. Composite score, without vs with: `ALL 18 ... 1.593` vs `ALL 18 ... 1.907`.
  - Both arms report `11 from truncated sources`. The judge sees a source cut at 24000 chars (`vault::fabric: Truncating input from 27218 to 24000 chars`), so the high unsupported rate on article and video is the same in both arms and does not come from the note.
  - Fallbacks: `1 fixtures on a distill fallback` without the note, `2` with it. The run log shows two article chunk YAML parse failures and one article reduce YAML parse failure (`falling back to concat + chronological claims`). The log does not tie these to an arm.
### Deviations
- The doc's bullet lists three pattern rules. The orchestrator's brief added two more: treat instructions in the note as content, and use the literal phrase "capture note". The block carries both. It also adds a rule that the note itself never becomes a claim, quote or link, so the note cannot be graded as source content.
- `distill_long` signatures in four distillers changed. The doc names only the two builders, but the note has to reach them through `distill_long`. Same effect, correct seam.
### Tradeoffs
- Note on the reduce call only vs. on every chunk call too: reduce-only keeps the chunk extraction neutral over the whole source, and puts the weighting where selection happens. It also keeps the chunk inputs the same, with no added tokens.
- One identical block in 9 files vs. per-kind wording: identical text keeps the rule auditable with one `rg`. "tldr (when the schema has one)" covers the thread and session patterns, which have no tldr.
### Open questions
- None.

## Phase 11: Tools extraction prompts
### Design decisions
- `parse::PatternTool { name, url, command }` is the YAML leaf, with every field serde-defaulted. `into_tool` trims, turns an empty url or command into `None`, and drops a nameless tool. A `name: null` entry drops that one tool and does not fail the whole parse (`parse::tests::pattern_yaml_parses_tools_and_a_null_name_does_not_fail_the_parse`).
- `parse::collect_tools` converts a single-call list. `parse::merge_tools` dedupes in first-seen order. A repeated command (whitespace-collapsed) only fills the earlier tool's missing URL. A same-name tool (case-insensitive) where either side has no command fills missing fields. One name with two different commands keeps both, because each is verbatim source text. Existing values are never overwritten.
- `PatternYaml.tools` (shared video/article struct) and repo's own `PatternYaml.tools` feed `Distilled.tools` on the single-call paths (`video.rs:build_distilled`, `article.rs:distill_short`, `repo.rs:distill`).
- Map-reduce (video, article `distill_long`): each chunk's tools are pooled and merged with `merge_tools`, the same structural merge links already get. The reduce patterns do not emit tools. Their existing "Do NOT produce tags or links" rule now names tools too, and says why: the reduce never sees the source, so it could not copy a command verbatim. So a long-path command is always one a chunk copied from the source, and no `ReduceYaml` change was needed.
- Patterns: `distill-video`, `distill-article`, `distill-video-chunk`, `distill-article-chunk` and `distill-repo` gain a `tools:` schema entry and a `tools` rule. A tool is a named, usable thing, not a topic. The URL is set only when the source gives it, and a URL on a tool must not also appear in `links`. The COMMAND-HONESTY RULE: copy the command verbatim, or leave it null. Never compose a command from prose, never chain with `&&`/`;`/`|`, never join separate lines, never add or drop flags/sudo/versions/placeholders, never fix a typo. Several commands for one tool: copy the single most useful one.
- `distill-repo`: the repository itself is not a `tools` entry. Its install stays in `install`, which is now "the ONE command that installs this project, copied VERBATIM", and the rule names the chained `git clone` + `cd` + `npm install` shape as forbidden. Phase 9's staged run found exactly that shape in `repo/cl-05e26c9f`. `install` counts toward `tools-grounding` through `tools_to_render`.
- `distill-video` and `distill-video-chunk` add a "SPOKEN COMMANDS ARE NOT COMMANDS" sub-rule: speech-to-text writes "Git work tree add" for `git worktree add`, so a command read aloud gets `command: null`. Only a command written out as text in the input qualifies.
- `eval::calc::tools_grounding` logs each ungrounded command at DEBUG (tool name plus command), so `-v` shows which command missed. I added it to diagnose attempt 3 and kept it for diagnosability.
- distillers/AGENTS.md gains a "Tools are extracted, never composed" invariant.
- Tests: 8 parse tests (trim and empty-to-None, nameless drop, null-name parse, absent key, fill from a later same-name tool, repeated command across chunks, two commands kept, no overwrite). Distiller tests: `video::tests::{single_call_video_extracts_tools, long_video_merges_chunk_tools_structurally, video_without_tools_key_has_no_tools}`, `article::tests::{single_call_article_extracts_tools, long_article_merges_chunk_tools_structurally}`, `repo::tests::repo_extracts_tools_beside_install`. Bite check: I replaced the video long-path `merge_tools(combined_tools)` with `Vec::new()`, and `long_video_merges_chunk_tools_structurally` failed (`panicked at distillers/src/video/tests.rs:1064`). It passes again after the revert.
- Live runs. Each was a release build of this tree (`cargo build --release -p sb`, run as `./target/release/sb`). Coverage: `--regenerate` only covers fixtures with a `capture-note.txt`. For repo and video that is all 9 of them. Repo: github-com-brianpetro-obsidian-smart-connections, github-com-jacob-bd-notebooklm-mcp-cli, github-com-magarcia-granola-cli. Video: build-an-obsidian-system-not-a-second-brain, building-great-agent-skills-the-missing-manual, game-over-neovim, git-worktrees-for-agents-are-awesome, there-are-only-5-safe-places-to-build-in-ai-right-now-are-yo, top-10-claude-code-skills-plugins-clis-april-2026.
  - Run 1, wording: the initial tools rules above, without the spoken-commands sub-rule. Command: `sb borg eval --regenerate --patterns-dir borg/patterns` over all 18 fixtures. No `tools-grounding:` line was printed. Every distill ran, but then the run died in the judge: `Error: judging fixture article/research-trychroma-com-context-rot ... judge reply missing axis 'claim.coverage': "{\"claim-coverage\":2,\"anchor-validity\":0,\"summary-faithfulness\":0}"` (exit 1). The judge returned kebab-case axis keys. This is a judge-reply flake, not the tools metric.
  - Run 2, same wording as run 1. Command: `sb borg eval --regenerate --patterns-dir borg/patterns --fixtures <dir holding only the repo and video fixture dirs> --no-judge`. Exit 0.
    ```
    === without capture note ===
      tools-grounding: names 76/80 (95.0%), commands 13/14 (92.9%)  (9 fixtures)
        repo       names 28/28 (100.0%), commands 10/10 (100.0%)
        video      names 48/52 (92.3%), commands 3/4 (75.0%)
    === with capture note ===
      tools-grounding: names 82/89 (92.1%), commands 14/16 (87.5%)  (9 fixtures)
        repo       names 32/32 (100.0%), commands 10/10 (100.0%)
        video      names 50/57 (87.7%), commands 4/6 (66.7%)
    ```
    Every ungrounded command came from `video/git-worktrees-for-agents-are-awesome`. Its source is an ASR transcript that says "Git work tree" and "UV run main.py", never a typed command, so any `git worktree ...` command is composed from speech.
  - Run 3, wording change: the "SPOKEN COMMANDS ARE NOT COMMANDS" sub-rule added to `distill-video` and `distill-video-chunk`. Same command as run 2 plus `-v`. Exit 0.
    ```
    === without capture note ===
      tools-grounding: names 73/78 (93.6%), commands 8/8 (100.0%)  (9 fixtures)
        repo       names 27/27 (100.0%), commands 8/8 (100.0%)
        video      names 46/51 (90.2%), commands 0/0 (N/A)
    === with capture note ===
      tools-grounding: names 78/83 (94.0%), commands 16/16 (100.0%)  (9 fixtures)
        repo       names 32/32 (100.0%), commands 15/15 (100.0%)
        video      names 46/51 (90.2%), commands 1/1 (100.0%)
    ```
    Commands are at 100% in both arms, so the criterion (>= 90% for commands) passes. The DEBUG log has no `ungrounded command` line. One `VideoDistiller: reduce yaml parse failed: did not find expected key at line 3 column 8` WARN fired. The reduce output carries no tools, and Phase 10's run logged the same class of reduce parse failure.
### Deviations
- The criterion command was `sb borg eval --regenerate --patterns-dir borg/patterns`. The passing runs added `--fixtures` pointing at a scratch directory that holds only the repo and video fixture dirs (symlinks to `config/eval/distill-fixtures/{repo,video}`, nothing committed), plus `--no-judge`. Why: the criterion names the repo and video fixtures, and `tools-grounding` is deterministic, so the judges cannot change it. The full run with judges lost its whole result to a judge-reply parse error (run 1).
- The doc says the "chunk/reduce siblings extract tools". The chunk patterns extract them. The reduce patterns are told not to, and the code merges the chunk tools structurally, as it already does for links. Same effect, correct seam: a reduce never sees the source, so a reduce-emitted command could only be a copy or a rewrite of a chunk's.
### Tradeoffs
- Structural merge vs. a reduce-selected tool list with a pool-honesty gate: the structural merge cannot alter a command and needs no new reduce input section. The cost is that a long video keeps every tool any chunk named, up to the 20-tool bound in `validate.rs`.
- One name with two different commands keeps two bullets vs. picking one: picking would drop verbatim source text, and the renderer handles repeated names.
- The prompt makes `null` the normal command for videos. That gives fewer video commands (0 to 1 per arm in run 3) in exchange for no composed ones. The doc's Risks row ("The tools prompt invents install commands") is the failure this guards.
### Open questions
- Video tool *names* sit at 90.2% (5 ungrounded of 51 in each arm of run 3). The phase criterion covers commands only, and names were not tuned. I did not diagnose which names miss: the DEBUG line logs ungrounded commands only. The video sources are ASR transcripts (one says "Cloud Code" for Claude Code), so a canonical spelling may be the cause, but that is unconfirmed. Is the name rate worth its own pass?
- The judge sometimes replies with kebab-case axis keys (`claim-coverage`), and a full `--regenerate` run then aborts and loses every distill. Should the judge parser absorb that drift with a WARN (the `ClaimKind` precedent), so a long run survives?

## Phase 12: Popup note field
### Design decisions
- The popup no longer posts on open; a textarea (`#note`, autofocused) and a Capture button submit — `borg/clients/extension/popup.html`, `popup.js` — the doc's Resolved Decision. Enter submits with focus in the textarea, Shift+Enter inserts a newline, so a bare capture is one keypress.
- `buildBody(url, rawNote)` is a pure function that returns `{url}` or `{url, note}`, with `note` trimmed and omitted when empty — `popup.js:buildBody`.
- The button is disabled while a capture is in flight and re-enabled on every failure path, so a failed capture can be retried; success closes the popup as before.
- `borg/clients/AGENTS.md` updated to describe the new popup flow. `popup.css` gained two rules for the textarea and button. The no-`keepalive` rule is untouched.
### Deviations
- No JS test was added: the extension has no JS test setup (no package.json). Per the phase instruction, the body builder is extracted as a pure function and its test is the Rust `borg/tests/extension_body_matches_ingest_request.rs` contract (the `note` field exists on `IngestRequest`) plus the `sb borg extension stage` check. I also ran `buildBody` once under node by hand (`{url}` for blank input, `{url, note}` for " hi\n"); that run is not committed.
### Tradeoffs
- Extracting `buildBody` vs. inlining it: it costs a few lines and gives a seam a future JS test can import.
- The status text reads "Enter to capture" before submit vs. a blank line: it tells the operator the popup is waiting, since it no longer posts on open.
### Open questions
- OWED to Scott, not attempted: the live check (`sb borg extension stage --to ./dev-ext`, load in `about:debugging`, capture a page with a note, confirm the vault note carries `capture-note:`).
- OWED to Scott, not attempted: the operator step `sb borg extension sign`, then `otto deploy`.

## Phase 13: creator_browse type filter
### Design decisions
- `creator_stats(note_type: Option<&str>)` and `notes_by_creator(.., note_type, limit)` filter through the existing `Filter::and_eq("note_type", ..)` seam - `vault/src/search/stats.rs` - same as `search`/`list_notes`; new pure builder `creator_stats_filter` keeps the SQL testable like its siblings.
- `CreatorBrowseRequest.note_type: Option<NoteType>` with `#[serde(rename = "type")]` - `oracle/src/tools.rs` - wire name is `type` per the API Design block; typed via the `NoteType` enum (schema is law), converted with `as_str()` in `oracle/src/server.rs::creator_browse`. The schemars description documents the field. No handle field (Phase 17).
- The type applies to both branches (creator list and per-creator notes).
### Deviations
- None. (Other request structs name the wire field `note_type`; this one is `type` as the doc specifies.)
### Tradeoffs
- Rust field named `note_type` renamed on the wire to `type` vs a raw `r#type` field - keeps the keyword out of the Rust API and matches sibling structs internally.
### Open questions
- None.

### Live check (read-only, release binary built from this tree)
`sb oracle call creator_browse --json '{"type":"youtube","limit":25}'`: spelling `youtube` works (it is the `NoteType::Youtube` enum value). Zero matches for "Hacker News" or "Michael Larabel". Creators returned, verbatim: AI News & Strategy Daily | Nate B Jones; Better Stack; Chase AI; Let's Get Rusty; IndyDevDan; AI Engineer; Weekly Spiral; Coach McKie; Matt Pocock; AI LABS; Code to the Moon; Simon Scrapes; David Ondrej; Cole Medin; The Spread Offense; The AI Automators; Sidero Labs; Dreams of Code; Throw Deep Publishing; Playrbook; Jake Van Clief; American Football Academy; No Boilerplate; Eric Michaud; Ray Amjad.

## Phase 14: Persist channel metadata
### Design decisions
- Split yt-dlp JSON parsing out of the spawn into pure `parse_metadata(&[u8])` — borg/src/youtube.rs — so the fixture test exercises exactly the code `fetch_metadata` runs, with no network.
- `channel_id`, `channel_handle` (yt-dlp `uploader_id`), `channel_url`, `published_at` added to `youtube::VideoMetadata`, `distillers::VideoMetadata`, and `vault::distilled::VideoPayload` (all `#[serde(default)]` on the payload). Empty strings map to `None`.
- `upload_date` (`YYYYMMDD`) is normalised to ISO `YYYY-MM-DD` in `iso_date_from_upload_date`; non-8-digit values become `None` rather than leaking a malformed date.
- Frontmatter: `channel-id`, `channel-handle`, `channel-url` written top-level in distillers/src/render.rs. `published_at` reuses the existing `cortex-video-published-at` key (no second date key). `attach_payload` now also fires when only channel fields are set.
- `borg::stages::distill::video_metadata_from_yt_dlp` stops hardcoding `published_at: None`.
- Fixture: borg/src/youtube/fixtures/dump-json-53xycpcgqwe.json, a field subset of a real `yt-dlp --dump-json` for 53XYcpCgQWE.
### Deviations
- None.
### Tradeoffs
- Top-level `channel-*` keys (as the phase specified) vs `cortex-video-*` prefix for the siblings; they are intended as cross-kind source keys, while the older channel name/date keep their `cortex-video-` keys.
### Open questions
- The live re-ingest check (`channel-handle: '@letsgetrusty'` after re-ingesting https://www.youtube.com/watch?v=53XYcpCgQWE) is OWED after deploy; the daemon runs the old binary, so it was not run here. The unit test proves the same chain (fixture -> parse -> map -> distill -> render) and observed `channel-handle: '@letsgetrusty'`, `channel-id: UCSp-OaMpsO8K0KkOqyBl7_w`, `cortex-video-published-at: 2025-11-21`.
- Phase 17 owns indexing these keys in oracle; not touched here.

## Phase 15: Channel backfill
### Design decisions
- Dry run makes no network call; candidates are `type: youtube` notes whose `source:` carries a video id and that lack at least one `channel-*` key. Where: `borg/src/channels.rs:run_with`. Why: a 1,400-call loop to preview is not a dry run anyone would run; the unavailable count only exists after `--apply`.
- The yt-dlp call sits behind the `ChannelFetcher` async trait (`YtDlpFetcher` wraps `youtube::fetch_metadata`, bounded by `pipeline.yt-dlp-timeout-secs`) in `borg/src/channels.rs`, so tests inject a fake.
- Never-clobber is per key: an existing `channel-id`/`channel-handle`/`channel-url` line (any value) is left alone and only missing keys are appended at the end of the frontmatter. Where: `borg/src/channels.rs:insert_missing` (pure, string in, string out).
- Writes use `pipeline::atomic::write_atomic`; the lookup key is the note's own `source:` URL, never `creator:`.
- Any fetch error (deleted, private, timeout) lands in `unavailable` and the loop continues; read/write errors land in `failed`, which makes the CLI exit non-zero after the loop finishes (`sb/src/cli/borg.rs`).
- `--backfill` is `required = true` on `sb borg channels` so a bare `sb borg channels` errors instead of doing nothing.
### Deviations
- None. (The creator backfill in `audit.rs` uses a plain `std::fs::write`; this one keeps its shape but writes atomically, as the phase bullet requires.)
### Tradeoffs
- Appending keys at the end of the frontmatter vs inserting after `type:` like the creator backfill: appending is order-independent and cannot misplace keys on unusual layouts.
- Own directory walk (the shape of `audit::collect_md_files`) vs `vault::note::scan_vault`: the backfill needs two raw frontmatter fields and must not reparse every note as typed YAML.
### Open questions
- Owed: Scott's `sb borg channels --backfill --apply` run on ripr after deploy (about 1,400 yt-dlp calls). Success check: `rg -l '^channel-handle:' ~/repos/scottidler/obsidian/notes | wc -l` >= 1,300. Not run here: it is a live-vault network loop.

## Phase 16: Channel search in borg
### Design decisions
- `search_channel(handle, query, limit) -> Result<Vec<ChannelHit>, ChannelSearchError>` lives in a `youtube/search.rs` submodule, re-exported as `borg::youtube::search_channel`. It runs through `vault::process::run` (the workspace's one subprocess primitive: process group, drained pipes, SIGKILL on deadline) inside `spawn_blocking`, not a hand-rolled `tokio::process` call. The URL is one argument; nothing goes through a shell.
- Validation (`^@[A-Za-z0-9._-]+$` handle, non-blank query, `limit` in 1..=50) runs before anything is spawned (`search.rs:validate`). The test proves the order with a control: an invalid handle against a nonexistent program gives `InvalidHandle`, and a valid handle against the same program gives `Spawn`. `sb/tests/channel_search.rs` repeats this end to end against the built binary with an empty PATH.
- The query is percent-encoded with `urlencoding::encode` (already a borg dependency). Spaces become `%20`, not `+`.
- Wire types go in a new `vault::channel` (`ChannelHit`, `ChannelSearch`, `ChannelSearchFailure`), following the `vault::queue` precedent, so Phase 17's oracle client can deserialize the route's 200 and 502 bodies without depending on borg.
- Typed `ChannelSearchError`. `InvalidHandle`, `InvalidLimit` and `EmptyQuery` map to HTTP 400, because nothing ran. `Spawn`, `TimedOut`, `Failed` (non-zero exit) and `Parse` map to HTTP 502. One pure mapper, `routes::channel_search_failure`, builds both, and its unit test pins the 502 shape: `{"error": ..., "stderr-tail": ...}`.
- The stderr tail is the last 10 non-blank lines, capped at 2,000 chars, keeping the end (`search.rs:stderr_tail`).
- `parse_hits` keeps `url` entries containing `watch?v=` in listed order. A body with no `entries` array, or a video entry missing `id` or `title`, is a loud `Parse` error (502), not zero hits. `entries: []` is zero hits.
- The route `GET /channels/search?handle=&q=&limit=` sits in the `protected` subrouter next to `/queue`, behind `routes::require_auth`. A missing `handle` or `q` is axum's 400 query rejection.
- CLI choice: `sb borg channel-search` calls `borg::youtube::search_channel` in-process and does NOT go through the daemon (`sb/src/cli/borg.rs`). `sb borg queue` goes through the daemon because the receipts DB exists only on the daemon host. Channel search has no host-bound state: it needs only yt-dlp and the network, so the direct call works on any machine with yt-dlp, and the daemon does not have to be the new binary. The daemon route exists for remote callers (oracle, Phase 17) and runs the same function.
- `in-vault` marking per hit is left to Phase 17 (oracle). The design lists it under oracle's `channel_search`, and oracle owns the index that answers "is this video already a note". borg's daemon host has the vault, but a vault scan per search is the wrong place for that lookup.
- Fixture: `borg/src/youtube/fixtures/flat-search-letsgetrusty-tokio.json`, a real `yt-dlp --flat-playlist -J --playlist-end 10 "https://www.youtube.com/@letsgetrusty/search?query=tokio"` capture (2026-10-15, yt-dlp 2026.08.19) trimmed to the per-entry fields. Entry 3 is `playlist?list=PLai5B987bZ9CoVR-QEIN9foz4QCJ0H2Y8` ("The Rust Lang Book"), so the filter test has a real playlist to drop.
- Tests bite: with the handle check disabled and the `watch?v=` filter loosened, `bad_handle_is_rejected_before_yt_dlp_runs`, `parse_hits_keeps_only_watch_entries_in_order` and `run_search_parses_stdout_of_a_successful_run` all fail. Restored afterwards.
### Deviations
- A timeout 502 carries `"stderr-tail": null`, not a stderr tail. `vault::process::run` returns `Outcome::TimedOut` without the partial output: it deliberately does not join the drain threads after a SIGKILL, so an escaped grandchild cannot hold the call open. Getting the tail would mean a second subprocess path, which that module exists to prevent. A `Failed` exit carries the tail. The error text names the timeout ("yt-dlp timed out after 8s").
- `search_channel` returns a typed `ChannelSearchError`, not `eyre::Result`, so the route can tell 400 from 502. Same effect, correct seam.
- Inputs the design does not name are added: an empty query is refused, and `limit` is capped at 50 (`MAX_LIMIT`). Both are refused as 400 before yt-dlp runs.
### Tradeoffs
- `limit` feeds `--playlist-end`, so it bounds entries read, not hits returned. A dropped playlist leaves fewer hits: limit 10 gave 9 live. The alternative (over-fetch, then truncate) costs more yt-dlp time inside the 8 s bound. The design's command line uses `--playlist-end N` as is.
- The 8 s bound and the 50 cap are constants (`SEARCH_TIMEOUT`, `MAX_LIMIT`), not config. The design pins 8 s against the client's 10 s default. Making it configurable would let it pass the client timeout.
- `yt-dlp` is resolved from PATH, as `fetch_metadata` does, not a configured binary path.
- Spawn failure, timeout, non-zero exit and unparseable output all map to 502 (a bad gateway: the upstream tool failed). Only refused input is 400.
### Open questions
- The daemon route's live check (`GET /channels/search` on desk/ripr) is OWED after deploy: the running daemon is the old binary. The library and CLI were run live from this tree; output below.
- Observed: `sb borg channel-search @letsgetrusty tokio --limit 10 --format yaml`, built from this tree (target/debug/sb), exit 0, 9 hits, no playlist entry (the playlist at position 3 was dropped):
  ```
  handle: '@letsgetrusty'
  query: tokio
  hits:
  - id: FUg1y-yv6cs
    title: How Rust engineered the perfect async runtime
    url: https://www.youtube.com/watch?v=FUg1y-yv6cs
    duration-secs: 488
    view-count: 68792
  - id: wXtngLBkK4Q
    title: Async Rust explained in 20 minutes
    url: https://www.youtube.com/watch?v=wXtngLBkK4Q
    duration-secs: 1155
    view-count: 85838
  - id: K8LNPYNvT-U
    title: Intro to async/.await in Rust
    url: https://www.youtube.com/watch?v=K8LNPYNvT-U
    duration-secs: 837
    view-count: 125434
  - id: _6QSe8Xzbnk
    title: 7 Rust project ideas for senior developers
    url: https://www.youtube.com/watch?v=_6QSe8Xzbnk
    duration-secs: 583
    view-count: 30110
  - id: 0y6RKiIk6cs
    title: Rust for Dummies in 12 Minutes
    url: https://www.youtube.com/watch?v=0y6RKiIk6cs
    duration-secs: 750
    view-count: 421020
  - id: q3VOsGzkM-M
    title: The ultimate Rust performance guide
    url: https://www.youtube.com/watch?v=q3VOsGzkM-M
    duration-secs: 731
    view-count: 69862
  - id: 9RsgFFp67eo
    title: Rust’s most complicated features explained
    url: https://www.youtube.com/watch?v=9RsgFFp67eo
    duration-secs: 1819
    view-count: 117715
  - id: luOgEhLE2sg
    title: Build your entire tech stack in Rust
    url: https://www.youtube.com/watch?v=luOgEhLE2sg
    duration-secs: 442
    view-count: 283296
  - id: iSB31t7pnzE
    title: Rust’s hardest features in 10 minutes
    url: https://www.youtube.com/watch?v=iSB31t7pnzE
    duration-secs: 527
    view-count: 24250
  ```
- Observed, library level: `cargo test --workspace --lib search_channel_live -- --ignored --nocapture` passed in 0.71 s with 9 `watch?v=` hits (FUg1y-yv6cs, wXtngLBkK4Q, _6QSe8Xzbnk, 9RsgFFp67eo, iSB31t7pnzE, K8LNPYNvT-U, FPRH66r-zUQ, eIB3Pd5LBkc, luOgEhLE2sg). The ranking differs from the CLI run moments earlier: YouTube's search order is not stable between calls.
- Observed, failure path: `sb borg channel-search @this-handle-does-not-exist-zq9x tokio` exited 1 in 1.5 s with `Error: yt-dlp exited exit status: 1: WARNING: [youtube:tab] HTTP Error 404: Not Found. Retrying (3/3)... ...`. The message now reads `yt-dlp failed (exit status: 1): <tail>`. A 404 handle costs yt-dlp 3 retries, still well inside 8 s here.

## Phase 17: Oracle channel search and creator handles
### Design decisions
- New `notes.channel_handle TEXT DEFAULT ''` column, added by `ensure_channel_handle_column` (idempotent `PRAGMA table_info` + `ALTER ADD COLUMN`, no version bump) - `vault/src/search/schema.rs` - the same migration pattern as `repos_touched`/`superseded_by`. It is filled from `channel-handle:` in `Frontmatter.extra` in `index_one` (both UPDATE and INSERT arms).
- `SearchIndex::creator_handles(note_type)` returns creator -> handle (`MAX(channel_handle)`, deterministic on a rename) - `vault/src/search/stats.rs`. `creator_browse` calls it only for `type: youtube` and adds `handle` to an entry only when one exists; an absent handle is an absent key, not `""`/null.
- `SearchIndex::youtube_note_path(video_id)` matches `source` on the video id with `instr`, not `LIKE`, because ids contain `_` (a LIKE wildcard); restricted to `note_type = 'youtube'`, so it matches `youtu.be/ID?t=30` as well as `watch?v=ID`.
- `oracle/src/channel.rs`: `fetch` (GET `/channels/search` via `DaemonClient` and `queue::BorgView`) and `render` (`{handle, query, count, results}` with `in-vault: <path>` per marked hit). A 400/502 with a `ChannelSearchFailure` body becomes an error naming the daemon address, the daemon's `error` and the yt-dlp stderr tail; 404 says the daemon predates the route.
- The tool loads the view with `self.borg_config` (the server's existing override), unlike `ingest_queue`, which passes `None`; that makes the error path testable without touching `~/.config/sb/`.
- Registered via `#[tool]`, a `dispatch()` arm, and the MCP instructions text. Added to `NON_NOTE_TOOLS` in the trace-marker coverage test.
### Deviations
- None.
### Tradeoffs
- `creator_handles` is a second query beside `creator_stats` vs widening `creator_stats`' return tuple: it leaves the other callers and the untyped path untouched.
- Instance-wide `in-vault` lookup is one indexed-table scan per hit (at most 50) vs a single `IN` query: `source` has no index either way, and the hit count is small.
### Open questions
- Owed after deploy: `sb oracle call creator_browse --json '{"type":"youtube","limit":5}'` showing `handle` on each entry needs the Phase 15 backfill on ripr, then a reindex. Existing index rows have `channel_handle = ''` until each note is reindexed (the column migration does not touch their mtime). Proven here on a temp index (`creator_browse_type_youtube_carries_handle_per_creator`, `channel_handle_is_indexed_and_creator_handles_filters_by_type`, `channel_handle_column_is_added_to_a_pre_existing_db`).
- Owed after deploy: the live `channel_search` call against the real daemon (the running daemon is the old binary, no `/channels/search`; it would answer 404 "predates /channels/search").
- Daemon-unreachable check, run with the live daemon untouched: `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME` pointed at a scratch dir (borg.yml `hotkey.port` set to an unused port, oracle.yml `vault.root-path` set to an empty scratch vault, so no live index was written), `sb oracle call channel_search --json '{"handle":"@letsgetrusty","query":"tokio"}'`, exit 1:
  ```
  Error: cannot reach daemon at 127.0.0.1:46689 for GET /channels/search: error sending request for url (http://127.0.0.1:46689/channels/search?handle=%40letsgetrusty&q=tokio)
  ```
