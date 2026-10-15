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
