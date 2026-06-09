# Implementation Notes: Codebase Review Remediation

Running, append-only record of decisions, deviations, tradeoffs, and open
questions made while executing `2026-06-09-codebase-review-remediation.md`.

## Phase 1: UTF-8-safe truncation helper and replacements

### Design decisions
- `vault/src/text.rs:char_cut` — extracted a private `char_indices().nth(max_chars)` helper shared by both public functions, so the char-boundary logic lives in one place.
- Tests placed in `vault/src/text/tests.rs` with `#[cfg(test)] mod tests;` in `text.rs`, per `rust.md` (no inline `mod tests {}` blocks). The design doc's "unit tests in `vault/src/text.rs`" is loose wording, not a directive for an inline block.
- `borg/src/pipeline.rs` title fallback (formerly `:2288`): made the guard char-based (`first_line.chars().count() > 80`) and replaced only the truncation arm with `truncate_with_ellipsis(first_line, 77)`, preserving the `"Quick Note"` else branch. 77+`...` keeps the original ≤80-char visible budget.
- Added `log::trace!` entry lines to both helpers per the function-level logging rule; demoted to TRACE because these fire on every preview/log line.

### Deviations
- `borg/src/assets.rs:21` (`&hash[..8]`) was intentionally **not** converted. `hash` is a SHA-256 hex digest (`format!("{:x}", ...)`), guaranteed ASCII and ≥64 chars, so the byte slice is provably panic-free and is not user-controlled text. The doc's grep step says "convert any stragglers"; this is not a straggler of the panic class.

### Tradeoffs
- `truncate` returns `&str` (borrows) vs. always returning `String`. Kept the borrowing signature from the doc's API so non-allocating callers (e.g. the no-fabric extract path) stay allocation-free until they choose to `.to_string()`.

### Open questions
- None.

## Phase 2: Oracle DB path single source of truth

### Design decisions
- `cortex/src/classify.rs:run` hardcoded a third copy of the oracle DB path (`dirs::data_local_dir()/oracle/oracle.db`), not just the two the doc named. Repointed it at `config.oracle_db_path()` (which now delegates to the helper) so test redirection via `XDG_DATA_HOME` keeps working.
- New helper placed in `vault/src/paths.rs` next to `borg_signal_bootstrap_marker`, matching the existing `dirs::data_local_dir().expect(...)` idiom (the module already uses `dirs` directly, so the `rust.md` `xdg_data_dir` ban does not apply here — the doc explicitly directs the `dirs` pattern).
- Verified path identity before editing: cortex hardcode and oracle `default_db_path` both resolved to `~/.local/share/oracle/oracle.db`; the helper reproduces exactly that byte-for-byte. Live `~/.config/sb/oracle.yml` carries no `db-path`, so removing the field moves no data.

### Deviations
- Removed `shellexpand` from `oracle/Cargo.toml` (`cargo remove`) — it was used only by the deleted `db_path()` tilde expansion and is now dead.
- The `config/templates/oracle.yml.example` never contained a `db-path` field, so there was nothing to delete; added a comment documenting that the path is fixed and owned by `vault::paths` per the doc's intent.
- Updated `oracle/src/config/tests.rs` `config_without_retrieval_block_defaults_pipeline`: its sample YAML used the now-removed `db-path` key; switched to `inbound-recompute-interval-secs` (the test only asserts retrieval defaults, so any other field serves the same purpose).

### Tradeoffs
- Removed oracle's configurable `db-path` entirely (doc Alternative 2) rather than teaching cortex to read oracle's config. Eliminates the desync by construction at the cost of operator relocation, which no current install uses.

### Open questions
- Doc Open Question stands: is DB relocation a real operator need worth a shared-config field later, or is the fixed path sufficient indefinitely? Left as fixed for now.

## Phase 3: Borg port default

### Design decisions
- Changed `ServerConfig::default()` port `8080 -> 8181` (`borg/src/config.rs`), the test assertion in `test_default_config`, and the explicit fixture YAML in `test_config_without_bot_sections` (the latter asserts nothing about the port, but keeping a stale `8080` in fixtures is misleading).

### Deviations
- Also fixed `borg/obsidian-borg.example.yml` (`port: 8080 -> 8181`), a straggler the doc did not name. It is the same defect class (the operator-facing example disagreeing with every other surface); leaving it would reintroduce the inconsistency the phase exists to remove.

### Tradeoffs
- None.

### Open questions
- None.

## Phase 4: Authentication on HTTP write routes (opt-in, topology-preserving)

### Design decisions
- Implemented the gate as an axum `route_layer` middleware (`routes::require_auth`) applied only to the protected router (`/ingest`, `/ingest/file`, `/note`), merged with the open `/health*` router. A layer runs strictly before the handler, which is the cleanest way to guarantee the doc's "before any intake write" requirement (the handler that calls `record_received_with_sidecar` never executes on a 401). Handler signatures and return types are unchanged.
- `AppState` carries the **resolved** token (`Option<String>`), not the reference. Resolution happens once at startup in `serve_init` via `config::resolve_secret`, mirroring `telegram.bot-token`.
- Added `axum::http::header::AUTHORIZATION` to the CORS `allow_headers` so the browser extension's `Authorization` header is not stripped by preflight.
- Non-loopback detection for the startup WARN uses an explicit `matches!(host, "127.0.0.1" | "::1" | "localhost")`, not `config::is_local_host` - the latter matches the machine's *hostname* (for service host-gating), which is a different question than "is this bind loopback-only?".

### Deviations
- **Fail-closed on unresolvable configured token (gap-fill, not a spec change).** The doc specifies the startup WARN only for the non-loopback + `None` case and says to mirror `telegram.bot-token` resolution. Telegram's mirror behavior on resolve failure is WARN-and-disable. For auth I deliberately did **not** mirror that: if `server.auth-token` is set but `resolve_secret` fails, `serve_init` returns the error and the daemon refuses to start. Silently disabling a security control the operator explicitly opted into would be a downgrade. This is the safe reading of a case the doc left unspecified; it is not the "hard-fail on non-loopback without token" that Alternative 3 rejected.
- Fixed a stale comment in `popup.js` (`// mirror background.js guard`) that referenced `background.js`, a file retired in the popup-capture redesign (`docs/design/2026-06-03-extension-popup-capture.md`). Reworded to `// guard only on missing URL`. Cheap to do while editing the file; overlaps Phase 5's stale-comment cleanup.

### Tradeoffs
- Token comparison is a plain `==` on `&str`, not constant-time. For a self-hosted LAN/Tailscale daemon the timing-attack surface is negligible; a constant-time compare would add a dependency for no practical gain here.
- Auth tests live in the existing inline `mod tests` in `borg/src/lib.rs` (peers to the pre-existing router tests), rather than a new `tests.rs` file. `rust.md` prefers extracted test modules, but this module already exists inline with the `build_router` tests; adding peers keeps the Phase 4 diff focused. Extracting the whole module is a separate mechanical pass, not this phase's job.
- The positive "correct token passes" test asserts only `status != 401` (the handler then runs against a vault-less default config and returns a Failed body at HTTP 200). It does not assert full ingest success, to avoid running real fetch/pipeline work in a unit test.

### Open questions
- Doc Open Question stands: should `sb doctor` surface the auth posture (token-set/unset on a public bind)? Not implemented here; the startup WARN is the current surfacing. Flagged for the finalization summary.

## Phase 5: Docs, comments, and vault reconciliation

### Design decisions
- Verified every claim against the running system before rewriting it: `ledger_path()` resolves to `~/.local/share/sb/borg/borg-ledger.md` (not `system/views/borg-ledger.md`); the live vault has five top-level dirs (`entities/` is real); `system/borg-ledger.md` and `system/borg-dashboard.md` are gone; `system/views/borg-ledger.base` exists. The repo CLAUDE.md and vault CLAUDE.md edits describe what the code actually does.
- In `borg/src/pipeline.rs`, rewrote (rather than merely deleted) the dual-write comments at `record_terminal_to_receipts` so the chokepoint is still documented, but as "receipts is the sole authoritative store" with a pointer to the excision design doc.
- Repointed `cortex::sweep`'s footer cross-reference from the retired `[[borg-dashboard]]` to `[[borg-ledger]]` (which now resolves to `borg-ledger.base`), and regenerated the cold-report snapshot fixture (`cargo test ... regenerate_cold_report_snapshot -- --ignored`) - the only line that changed was the footer.
- Added a new vault note `system/borg-ingest-model.md` (`origin: assisted`) documenting receipts-authoritative / ledger-as-operational-history / `.base`-as-view / dashboard-retired.

### Deviations
- The live-vault files (`~/repos/scottidler/obsidian/CLAUDE.md`, `home.md`, the new `system/borg-ingest-model.md`) are in a **separate git repo** and are intentionally NOT staged into the second-brain Phase 5 commit. They propagate to other hosts via Syncthing; the obsidian repo is committed manually per that vault's own rule ("Git commits are manual").
- `home.md`: the doc said "repoint `[[borg-ledger]]`," but that wikilink already resolves to `borg-ledger.base` by basename (the `.md` is gone from the vault), so it was left intact; only the dead `[[borg-dashboard]]` line was removed.

### Tradeoffs
- `entities/` description in the vault CLAUDE.md was written from inspecting actual contents (`type: entity`, `ontotype: creator`, 683 files) rather than a generic guess.

### Open questions
- None.

## Phase 6: Typed failure-stage classification

### Design decisions
- **Found the doc's premise was false:** `PipelineError` (`borg/src/pipeline/error.rs`) is defined with tests but **entirely unused** - `process_url_inner` returns plain `eyre::Result`, and the module doc comment claiming it is "the only mechanism for receipts failure-stage classification" is aspirational fiction. The substring matcher `classify_terminal_failure` was the only real classifier.
- **Did not add a field to `IngestStatus::Failed`.** That variant is constructed/matched at ~30 sites across borg (routes, router, discord, replay, migrate, lib, notify) and `FailureStage` wasn't `Serialize`. Instead added an optional `failure_stage: Option<FailureStage>` to the `IngestResult` *struct*, which is built with `..Default::default()` almost everywhere - near-zero churn at the match sites. Added `Serialize`/`Deserialize` (kebab-case, matching `as_str`) to `FailureStage` so it can live on the serialized struct.
- Classified each failure at its origin: `stage_0_init` reject -> `IntakeRejected`; timeout -> `PipelineTimedOut`; bubbled-up `eyre` errors -> `FetchFailed` (the default). Converted the three sites the substring matcher distinguished (`router::classify_url` -> `ClassifyFailed`, quality gate -> `QualityBlocked`, `write_atomic` publish -> `PublishFailed`) from `bail!`/`?` into early `Ok(IngestResult { failure_stage: Some(...) })` returns inside `process_url_inner`, so the typed stage flows through the Ok path. Deleted `classify_terminal_failure`.
- Extracted `terminal_failure_stage(&IngestResult) -> FailureStage` (reads the typed field, defaults `FetchFailed`) and unit-tested it: every `FailureStage` round-trips, and `None` defaults to `FetchFailed`. This is the doc's "variant maps to its FailureStage" test, adapted to the typed-field design since `PipelineError` is not the mechanism.

### Deviations
- **Did not wire `PipelineError` through the pipeline.** The doc says to use "the typed `PipelineError` already defined," implying it was wired up; it wasn't, and threading `Result<_, PipelineError>` through `process_url_inner`'s many `?` sites would be a large refactor (eyre's blanket `From` conflicts with a `PipelineError: Error` impl, so every site needs an explicit `map_err`). The `failure_stage`-on-result design achieves the same goal - typed classification, no substring matching - with a fraction of the blast radius, and Phase 9 will decompose this file anyway. `PipelineError` is left in place, unused, as it was.
- **Media-pipeline failures (image/audio/document/text) classify as `FetchFailed`** (their catch-alls use `..Default`, so `failure_stage` is `None` -> default). The old substring matcher could have tagged a media publish failure as `PublishFailed` if its reason contained "publish"; that narrow case now reads `FetchFailed`. Acceptable: the substring guess was fragile, the URL path (the dominant one) is precisely classified, and media failures are overwhelmingly fetch/transcribe/extract.

### Tradeoffs
- `Option<FailureStage>` on the result vs. a non-optional field everywhere: optional keeps success results and the 30 `Failed`-match sites untouched, at the cost of a `unwrap_or(FetchFailed)` default at the single read site.

### Open questions
- Should `PipelineError` (now confirmed dead) be removed in a later cleanup, or kept for a future full typed-error refactor of the pipeline? Left in place for this phase.

## Phase 7: Cold-note sweep scoping

### Design decisions
- The exclusion lives in `vault::search::SearchIndex::cold_notes` (the SQL `WHERE`), not in `cortex/src/sweep.rs`. Reasons: (1) `ColdNote` does not carry `note_type`, so a sweep-side filter could only test the path; (2) `cold_notes` is the cold-sweep candidate-selection query and is called by exactly one caller (cortex sweep), so changing it does not alter semantics for any other consumer. Added `AND (note_type IS NULL OR note_type != 'daily') AND path NOT LIKE 'journal/%'`. The `IS NULL OR` form keeps untyped notes (SQL `NULL != 'daily'` is `NULL`, which would otherwise drop them).
- Mirrored the same two predicates into `count_pinned_excluded`, which documents that it uses "the identical age predicate as `cold_notes` so the two numbers describe the same population" - so the pinned-excluded count stays consistent with the surfaced set.
- Belt-and-suspenders by design: `type: daily` excludes a daily note misfiled outside `journal/`; `path NOT LIKE 'journal/%'` excludes an untyped note inside the journal subtree.
- Added `cold_notes_excludes_daily_and_journal_notes` (a `notes/` knowledge note surfaces; a `type: daily` note and a `journal/` note do not).

### Deviations
- Doc said "in `cortex/src/sweep.rs`"; implemented in the `vault::search` query instead, for the correctness reasons above. Functionally identical (sweep is the only caller).

### Tradeoffs
- None.

### Open questions
- The live `system/views/cold-notes.md` still contains journal entries from the pre-fix runs. Regenerating it requires running `sb cortex sweep --cold` against the live oracle index after install - a runtime step, not a CI/code step. Flagged for the finalization summary; it will also self-correct on the next daemon cold tick.

## Phase 8: SQLite immediate-transaction honesty

### Design decisions
- Replaced `self.conn.transaction()? + tx.execute_batch("BEGIN IMMEDIATE;").ok()` with `self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?` in `upsert_embeddings_batch` and `swap_transcript_chunks`. The old form opened a DEFERRED transaction (lock acquired at first write, not at BEGIN) and then ran a *second* `BEGIN IMMEDIATE` whose error was swallowed by `.ok()` - so it never actually took the write lock up front and hid any failure. The comments now describe what the code does.
- Added `use rusqlite::TransactionBehavior;`.

### Deviations
- Left the third write transaction, `set_active_embedding` (`vector.rs`), as a plain `self.conn.transaction()?`. The doc scoped Phase 8 to the two sites with the false-IMMEDIATE-claim-plus-swallowed-error pattern; `set_active_embedding` makes no IMMEDIATE claim and swallows nothing, so it is not the defect. (cortex is the sole writer, so its deferred lock is harmless.)

### Tradeoffs
- This is hygiene, not a concurrency fix - cortex is the sole writer, so the deferred lock never deadlocked. The value is the code no longer claims IMMEDIATE semantics it lacked or swallows a SQL error. The held-lock window is unchanged (inference still runs outside the transaction), so the existing `write_transaction_for_batch_64_stays_under_200ms` regression test still passes.

### Open questions
- None.

## Phase 9: Module decomposition

### Design decisions
- **search.rs (2004 -> 529):** split the single `impl SearchIndex` across `search/{schema,index,query,cold,stats}.rs` via `impl super::SearchIndex` blocks. Submodules are descendants of `search`, so methods keep access to the parent's private field (`conn`) and free helpers (`normalize_*`, `extract_cortex_*`) with no visibility change. Four private methods called cross-module (`ensure_schema`, `fts_has_claims_column`, `remove_stale_notes`, `resolve_wikilink`) were bumped to `pub(crate)`.
- **pipeline.rs (3562 -> 780):** moved free-function clusters into `pipeline/{tags,handlers,text,publish}.rs`. The submodule was named `tags` (not `canonical`) to avoid colliding with the `vault::canonical` import. Moved private fns are `pub(crate)`; shared structs/enums (`CanonicalState`, `YouTubeResult`, `SlidePayload`, `DocumentKind`, `TextPattern`) stay in pipeline.rs bumped to `pub(crate)` (private-interface lint). Parent re-exports each submodule (`pub(crate) use` for tags/handlers/text, `pub use publish` since it carries the public `expand_vault_root`). The orchestrators (`process_content`, `process_url`, `process_url_inner`, terminal handlers) stay in pipeline.rs.

### Deviations
- **The bloat ceiling is set to the real 1500, not bumped.** The first attempt raised `BLOAT_MAX_LINES` to 2100 to clear the tallest remaining files - that defeats the gate's purpose (it enforces the 1500-line rule) and was wrong. Corrected by decomposing **every** file over 1500, not just the two the design doc named: extracted inline test modules from `borg/src/pipeline.rs` -> `pipeline/tests.rs`, `borg/src/lib.rs` -> `src/tests.rs`, split `vault/src/search/tests.rs` -> `search/tests/{group_a,group_b}.rs`, and `oracle/src/server.rs` -> `server/tests.rs`. `oracle/src/server.rs` (2068) and the test files were outside the design doc's Phase 9 scope (which named only pipeline.rs and search.rs), but honoring the 1500 rule required them. `server.rs` is now 1485; its `#[tool_router]`/`#[tool_handler]` macro impls were left intact (splitting them risks breaking the rmcp macro), so only its inline tests were extracted.
- Decomposition by extraction script (column-0 `}` boundary detection) rather than by hand, after an initial brace-counting attempt was fooled by `{`/`}` inside string literals in `looks_like_code` and truncated a function.

### Tradeoffs
- Pure code-move decomposition; no behavior change. `otto ci` run after each split (search, then pipeline, then test/server extractions).
- `server.rs` at 1485 has little headroom under 1500; further growth will require splitting its helper `impl OracleMcpServer` block (the non-macro methods) into a `server/` submodule, which needs `pub(crate)` bumps for helpers called by the tool handlers.

### Open questions
- None.

## Post-audit follow-up (Codex implementation audit, 2026-06-09)

A Codex read-only audit (full-repo access, unlike the sandbox-jailed Gemini run)
confirmed all seven invariant checks (auth-before-intake, secret-reference token,
extension wiring, cold-note exclusion, typed failure-stage, 1500 ceiling, oracle
db path) but surfaced two classes of real gaps the original execution missed.
Both were fixed in the follow-up commit.

### Phase 1 residual UTF-8 panic sites (the material miss)
The original Phase 1 grep only matched numeric `[..N]` byte slices, so
**variable-indexed** byte slices survived - the same daemon-crash class Phase 1
was meant to eliminate "across all production truncation sites". Fixed:
- `vault/src/fabric.rs::truncate_input` - `input[..max_chars]` behind a `.len()`
  guard, in the live Fabric path -> routed through `vault::text::truncate`
  (preserving the `max_chars == 0` "no limit" sentinel).
- `borg/src/fabric.rs::{split_with_overlap, find_break_point}` - byte-arithmetic
  chunk offsets -> snapped to `floor_char_boundary` before every slice.
- `distillers/src/video.rs` and `distillers/src/voicenote.rs` transcript chunkers
  - the `find_boundary` fallback returned a raw byte index -> snap with
  `floor_char_boundary`, with a `ceil_char_boundary` guard so a single codepoint
  wider than `target_chars` still makes progress (no infinite loop).
- `vault/src/hygiene.rs::sanitize_filename` - `&slug[..MAX_FILENAME_LEN]` where
  `sanitize_slug` keeps non-ASCII alphanumerics -> snapped to `floor_char_boundary`.
  Added a regression test (`sanitize_filename_does_not_panic_on_multibyte_at_cut`).

### Phase 5 residual stale references
The doc named specific sites; these stragglers remained:
- `vault/src/ledger.rs` - the ledger-file header template **emitted**
  `See also: [[borg-dashboard]]` (retired) -> repointed to `[[borg-ledger]]`.
- `borg/src/lib.rs`, `borg/src/backfill.rs`, `borg/src/pipeline.rs` - comments
  said `borg-dashboard.base`; the live view is `borg-ledger.base` -> corrected.
- `borg/src/pipeline/permits.rs` - stale "ledger XOR DLQ" / "orphan DLQ rows"
  comments -> reworded to the receipts `crashed`-promotion model.
- `docs/design/2026-03-21-oracle-mcp.md` - added a "Superseded" note that the
  `db-path` field was removed and the path is now fixed in `vault::paths`.

### Dead `PipelineError` removed
Codex flagged `pipeline/error.rs` and `receipts.rs::default_catchall_stage` as
contradictions (their comments claimed `PipelineError` was the active classifier;
Phase 6 had replaced it with `IngestResult.failure_stage`). Both were entirely
unused. Rather than correct lying comments on dead code, the dead `PipelineError`
module (`borg/src/pipeline/error.rs` + its tests) and `default_catchall_stage`
were removed (rkvr-archived), per the "dead code must be removed" convention.
This resolves the Phase 6 open question (remove vs. keep) in favour of remove.
