# Design Document: Discovery Remediation (second-brain + vault + host)

**Author:** Scott Idler (via agent)
**Date:** 2026-09-05
**Status:** Implemented (2026-09-06: phases 0-15 built, one commit each, `otto ci` green; three criteria amended post-build and one bullet blocked upstream, all recorded under Acceptance Criteria; the operator runbook and the deploy remain Scott's to run). Review panel closed 2026-09-05 after four rounds with zero blockers. Caveat on rounds 4-5: the staff seat (Codex) hung twice on the full prompt and returned only on a narrowed one; its round-5 re-run against the current text confirmed its own round-3 blocker closed and raised no new one. Rounds 1-3 ran both seats at full depth and every finding from them is dispositioned below. Run dir: `/tmp/review-panel/ZwHkq29D`.
**Review Passes Completed:** 5/5 + panel r1-r4 (pass 2: every line ref re-checked against `f97718f`, every runnable criterion executed on desk and recorded as `Observed on main`; pass 3: phase-count wording, `bin/agents-map` as the one script both otto and CI call, reverse-check scope narrowed; pass 4: excalidraw rename dropped because the daemon's auto-apply naming lint would undo it, `rkvr::remove` visibility, the stale-inbox Warn reconciled with the "no Warn" acceptance criterion, S2 reinstall ordered after the dotfiles phase; pass 5: voice lint, phase-count wording, panel dispatched)

## Summary

A six-agent discovery pass over the vault, the second-brain workspace, and the live desk host surfaced 25 defects and hygiene items. Scott asked for every one of them specced to executable precision. This doc is that spec: 16 phases (0-15), one commit each: 14 in second-brain (`otto ci` green), one in dotfiles, one in the vault; plus one operator runbook for the host-mutating steps on desk.

## Problem Statement

### Background

Discovery ran 2026-09-05 (brief: session scratchpad `discovery-brief.md`, 3,040 lines; verification: `research-brief.md`, 251 lines). Every item below was verified against code or host state before it was written here. Five items were corrected by verification and are marked as such.

### Problem

The system is healthy (both daemons up, 0 restarts, `sb doctor` exit 0) but carries drift in four layers:

1. **Docs contradict code.** Root `CLAUDE.md`, three `AGENTS.md` module maps, the vault `CLAUDE.md`, and the vault schema notes name files, fields, and rules that do not exist. Agents read these first.
2. **Config contradicts code.** The live `cortex.yml` `schema:` block hand-lists 17 types and 5 methods against 25 and 9 in the enum. Measured: 2,016 `frontmatter.enum.*` lint Errors per daemon tick, 1,927 of them false positives, masking 89 real ones.
3. **Host carries dead weight.** 858 MB of oracle DB backups, ~745 MB of rotated logs (borg unit hardcodes `--log-level debug`), five vestigial systemd files, three legacy config dirs, four orphan Fabric patterns.
4. **Small correctness holes.** Empty-slug publish wrote `inbox/.md`; the doctor schema-gap check counts 915 entity hubs as gaps; Claude Code state files are in the oracle index; the oracle data dir sits outside `~/.local/share/sb/`.

Plus one in-flight change (fabric `--maxTokens`) sitting uncommitted for 17 days while the doctor guard it adds is not deployed.

### Goals

Every item traces to Scott, 2026-09-05: "write each and every one of those in a design doc".

| ID | Item | Verification verdict |
|---|---|---|
| S1 | Delete four `oracle.db.pre-*` backups | confirmed, operator rm |
| S2 | Rotated logs: borg unit log level from config, rm rotations, rotate `oracle serve` log | confirmed, 4 knobs not 2 |
| S3 | Remove five vestigial systemd files | confirmed, operator rm (no code) |
| S4 | `sb bootstrap --prune-legacy-config` | confirmed, verb never specced |
| S5 | Delete `distillers/src/passthrough.rs` | confirmed, zero consumers |
| S6 | Delete vault `.github/workflows/ci.yml` | confirmed, fails every push |
| S7 | Legacy `## Transcript` sections | **corrected**: 1 note in scope, reingest it |
| S8 | Move two root orphans, remove `inbox/.md` | confirmed |
| F1 | Empty-slug publish fallback | confirmed, 9 unguarded sites |
| F2 | `origin: human` x87 + two strays | **corrected**: cortex migrate, not borg |
| F3 | Generate `system/schemas/*-values.md` from `vault::schema` | confirmed |
| F4 | `cortex.yml` `schema:` block | **corrected**: live bug, 1,927 false Errors/tick |
| F5 | Doctor schema-gap check | **re-scoped by panel r2**: doctor reports cortex lint's own required-field policy; the entity exemption goes into cortex.yml |
| F6 | `canonical-tags.yml` missing `system` group | confirmed, cosmetic |
| F7 | Templates use Templater syntax, Templater absent | confirmed, rewrite to core |
| F8 | `home.md` eleven dangling `[[domain-*]]` | confirmed |
| F9 | Vault `CLAUDE.md` "exactly five", README table | confirmed |
| F10 | Repo doc drift (root CLAUDE.md, three AGENTS.md) | confirmed, plus `graph.rs` omission |
| R1 | Oracle data dir -> `~/.local/share/sb/oracle/` | confirmed, operator move required |
| R2 | PATTERNS table drift | **corrected**: already test-guarded; 4 orphans to rm |
| R3 | AGENTS.md module-map lint | new |
| R4 | PR-time CI workflow | new |
| R5 | Doctor Warn on stale inbox | new |
| R6 | Doctor data-dir size section | new |
| R7 | Commit + deploy the `--maxTokens` diff | confirmed, needs decision record |

Five more items surfaced during verification. None was on Scott's list. Each is included because it shares a fix seam with a listed item and leaving it out would make that item's fix incomplete; the panel asked for explicit disposition, so here it is. Scott strikes any row he does not want.

| ID | Item | Coupled to | Disposition |
|---|---|---|---|
| X1 | `.claude/loop.md` and `inbox/.claude/loop.md` are in the oracle index (ignore-list gap) | F5, R5 | included: without it R5's stale-inbox Warn fires on Claude Code state |
| X2 | `oracle serve` log has no rotation (`sb/src/logger.rs:197` plain append `File`) | S2 | included: S2's "rm rotated logs" leaves the one unrotated log growing |
| X3 | `cortex.service` renders `ReadWritePaths={vault}` only while cortex writes under `~/.local/share` | R1 | included: R1 moves the DB cortex writes; the unit should name the data dir like borg's does |
| X4 | `cortex.yml:39` `path-exempt: "Inbox/**"` (capital I) never matches `inbox/` | F4 | included: same file, same phase, one-character fix |
| X5 | `system/templates/daily.md:35-38` carries cortex quality/duplicate stamps, re-applied every daemon tick | F7, F4 | included: F7 scrubs the file, Phase 14's cortex.yml `vault.ignore: templates` stops the daemon re-applying them |

### Non-Goals

- No architecture change. One-way data flow, schema-is-law, lib-only crates all stand.
- No change to the token-budget design (`2026-08-30-video-distill-token-budget.md`). R7 ships the plumbing that doc's Phase 0 measured through; the wave-split design stays Draft.
- No history rewrite of the vault `.git` (94 MB from 1,775 committed intake sidecars). Parked: revisit if clone time on a new device becomes a problem.
- No auto-commit of vault changes. Vault `CLAUDE.md:195`: commits are manual. Phase 15 stages by path and Scott commits.
- No separate fix for the daily-note `origin` gap. The 86 daily notes read as empty `origin` in the index because they carry `origin: human`, an invalid value (cortex lint reports it as `frontmatter.enum.origin`, not as a missing field); F2's `v4-legacy-values` renames it to `authored`, so it closes in Phase 15 on its own.
- No change to the daemon's write arms bypassing `vault.exclude`. Lint honors exclude/include (`cortex/src/lib.rs:127-133`); the daemon's quality, duplicates, auto-tag, and link arms take the raw scan (`daemon.rs:546-856`), so `exclude: system/**` never protected `system/` from those writers. Phase 14's `ignore` entry closes the template instance. Making the daemon arms honor exclude/include is a behavior change across the whole `system/` tree (hubs, links, duplicates) and is Scott's call, not this doc's.
- No cleanup of the other lint classes `sb cortex lint` prints today (`tags.non-canonical` 7,514, `type-field.*` 767, `broken-links.*` 535, `required.tags` 289). Observed while measuring F5; not on the list.
- No Templater install. Rewriting to core syntax removes a plugin dependency; the one Templater-only feature (`tp.user.workweek()` in `daily.md`) called a script that no longer exists in the vault.
- The 239 notes with an empty `distilled-extractor:` value. Observed, not on the list, not in scope.
- The 44 quarantined youtube notes with `## Transcript` under `system/quarantine/`. Invisible to every scanner by design; leave them.

## Proposed Solution

### Overview

Deterministic work first, host mutation last, one commit per phase. R7 ships first because it is the only diff in the tree and every later `otto ci` must run clean. R3 (the lint) lands before F10 (the doc fixes) so the lint proves them. F1 lands before `inbox/.md` is removed so the class is closed before the instance. R1 is guarded in code: a binary that resolves the new oracle path refuses to create it while the legacy DB exists, so deploy order cannot corrupt anything; the runbook still moves the DB before deploying so cortex does not sit in a restart loop.

### Architecture

Nothing new structurally. Additions are all at existing seams:

- `vault::schema`: `description()` on the five enums.
- `vault::paths`: two resolver changes (oracle under `sb/`), `dir_size(&Path) -> u64` (walkdir is already a vault dep).
- `vault::search::stats`: `compute_schema_gaps` drops `status` (raw counts, still oracle's `vault_overview`); `inbox_oldest()`.
- `sb doctor` calls `cortex::lint(vault_root, &config, &LintOpts { rule: vec!["frontmatter".into()], apply: false, format: LintFormat::Human, path: None })` (all four fields spelled out: `LintOpts` derives no `Default`, so `..` would not compile; `format` is inert for a library caller: `opts.format` is never read in `cortex/src/lib.rs`, and `:262` says formatting is the caller's job), the same entry point `sb cortex lint --rule frontmatter` uses, and counts the returned `Report` with a new `Report::count_by_rule_prefix(&str) -> BTreeMap<String, u64>`. Same scan, same exclude/include filter, same policy (`required` / `exempt` / `path-exempt` from cortex.yml); doctor grows no second copy of anything.
- `borg::hygiene`: `note_filename(title, trace_id) -> String`, the one fallback seam over `vault::hygiene::sanitize_filename`.
- `borg::service::render_systemd_unit`: log level from config, mirroring cortex.
- `sb::logger`: `oracle serve` writes through the shared `FileRotate` behind `tracing_appender::non_blocking` (background writer thread, so the tokio worker never blocks on a 50 MiB rename).
- `vault::search::SearchIndex::open`: fail-closed guard, refuses to create a fresh DB at the new oracle path while the legacy one still exists.
- `cortex::schema_docs` (new module): pure renderers for the four `*-values.md` files, snapshot-tested like `sweep::render_cold_report_at`.
- `sb cortex schema --render|--check` (new verb), `sb bootstrap --prune-legacy-config [--apply]` (new flag).
- `sb doctor`: four new findings (`data dir` section, stale inbox, legacy oracle path, schema-docs drift) plus one Info (unknown pattern files).
- `.otto.yml`: `agents-map` task wired into `ci`.
- `.github/workflows/ci.yml`: push/PR workflow cloned from `release.yml`'s container and cache.

### Data Model

No on-disk schema change. Path changes only:

| Before | After |
|---|---|
| `~/.local/share/oracle/oracle.db` | `~/.local/share/sb/oracle/oracle.db` |
| `~/.local/share/oracle/eval-cache.db` | `~/.local/share/sb/oracle/eval-cache.db` |

### API Design

```rust
// vault/src/schema.rs
impl Domain   { pub fn description(&self) -> &'static str }   // exhaustive match, all five enums
impl NoteType { pub fn description(&self) -> &'static str }
impl Origin   { pub fn description(&self) -> &'static str }
impl Status   { pub fn description(&self) -> &'static str }
impl Method   { pub fn description(&self) -> &'static str }
// cortex/src/lib.rs  (existing, unchanged: scan -> exclude/include filter -> rules)
pub fn lint(vault_root: &Path, config: &Config, opts: &LintOpts) -> Result<(Report, LintApplyReport)>;  // :90
// cortex/src/report.rs  (new helper on the existing Report)
impl Report {
    /// Violations whose `rule` starts with `prefix`, keyed by the remainder
    /// (`count_by_rule_prefix("frontmatter.required.")` -> {"domain": 920, "origin": 931, "tags": 289}).
    pub fn count_by_rule_prefix(&self, prefix: &str) -> BTreeMap<String, u64>;
}

// vault/src/paths.rs
pub fn oracle_db_path() -> PathBuf;          // xdg_data_dir()/sb/oracle/oracle.db
pub fn oracle_eval_cache_path() -> PathBuf;  // xdg_data_dir()/sb/oracle/eval-cache.db
pub fn legacy_oracle_dir() -> PathBuf;       // xdg_data_dir()/oracle  (guard + doctor)
pub fn dir_size(root: &Path) -> u64;         // walkdir sum, follows no symlinks

// vault/src/search.rs  (SearchIndex::open, existing fn, new guard as the FIRST statement,
// before the `create_dir_all(parent)` at search.rs:373, or the guard itself would mint
// `~/.local/share/sb/oracle/` and the runbook's directory rename would nest inside it)
// if path == oracle_db_path() && !path.exists() && legacy_oracle_dir().join("oracle.db").exists()
//     -> Err(LegacyOracleDb { legacy, new })   // typed, so doctor can map it to Error
// Every opener (cortex daemon, oracle serve, sb doctor, sb oracle index/call/stats/eval, cortex
// one-shots) goes through this fn, so no process can create an empty DB while the legacy one exists.
// classify.rs:60 swallows open errors with `.ok()` by design (Tier-2 context is optional);
// Phase 9 adds a `log::warn!` on that Err so the degradation is visible, not silent.

// vault/src/search/stats.rs
pub fn inbox_oldest(&self) -> Result<Option<(String, i64)>>;  // (path, modified_at epoch), path NOT LIKE 'inbox/.%'

// borg/src/hygiene.rs
/// `sanitize_filename(title)` or, when that is empty, `untitled-<trace_id>`.
pub fn note_filename(title: &str, trace_id: &str) -> String;

// cortex/src/schema_docs.rs
pub fn render_domain_values() -> String;   // and type_values, origin_values, status_values
pub fn render_all(vault_root: &Path, apply: bool) -> Result<Vec<SchemaDocOutcome>>;
pub enum SchemaDocOutcome { Unchanged(PathBuf), Drifted(PathBuf), Written(PathBuf) }
```

CLI:

```
sb cortex schema --render        # write system/schemas/{domain,type,origin,status}-values.md atomically
sb cortex schema --check         # diff against rendered; exit 1 on drift (default when neither flag)
sb bootstrap --prune-legacy-config [--apply]   # dry-run default, per sb cortex migrate
```

### Implementation Plan

Sixteen phases (0-15), one commit each; second-brain phases are `otto ci` green. Then one operator runbook. Line numbers are as of `f97718f`.

#### Phase 0: Ship the in-flight `--maxTokens` work (R7)
**Model:** sonnet
- This design doc is committed first, on its own, when the panel closes (`docs(design): discovery remediation`).
- Then commit the 9 modified files (`git diff --stat`: +172/-11) and the two untracked 2026-08-30 design docs.
- Append to `docs/design/2026-08-30-video-distill-token-budget.md` Resolved Decisions: "2026-09-05: the `scottidler/Fabric` fork with `--maxTokens` is accepted as the interim guard while the wave-split design stays Draft. Evidence: `borg.yml:63-75` (dotfiles) carries Scott's comment naming `scottidler/Fabric` (fork of v1.4.473, fixes open as `danielmiessler/Fabric#2207`), the mise re-sync caveat, and `sb doctor` as the guard, plus `max-tokens: 16384`; every `fabric` on desk reports v1.4.473 with the flag. Alternative 1 stays rejected as the *final* fix." Flip nothing else in that doc.
- `bump && otto deploy`.
- **Success criteria:**
  - `git status --porcelain` is empty after the commit.
  - `cargo test -p vault --lib fabric` passes, including `build_fabric_command_keeps_model_alongside_max_tokens`.
  - `sb doctor 2>/dev/null | grep -c 'maxTokens'` >= 1 on the deployed binary.
  - Observed on main: `git status --porcelain` = 9 ` M` + 3 `??` (the two 2026-08-30 docs and this doc); `cargo test -p vault --lib fabric` = `14 passed; 0 failed`; `grep -c maxTokens` = 0 (installed binary predates the diff).

#### Phase 1: Delete the passthrough stub (S5)
**Model:** sonnet
- `git rm distillers/src/passthrough.rs distillers/src/passthrough/tests.rs`.
- Remove `pub mod passthrough;` (`distillers/src/lib.rs:17`) and `pub use passthrough::PassthroughDistiller;` (`:37`).
- Rewrite the comment at `distillers/src/dispatcher.rs:169-174` to drop the stub mention; same at `borg/src/stages/distill/tests.rs:137`.
- Retag `config/eval/distill-fixtures/idea/linker-edge-from-capture-note/distilled.yml:12` extractor `distill-passthrough-v1` -> `distill-idea-v2` (the judge never reads it; honesty only).
- Do NOT touch `borg::stages::extract::PassthroughExtractor` (`borg/src/stages/extract.rs:25`), the live Stage-1 extractor.
- **Success criteria:**
  - `grep -rn 'PassthroughDistiller\|distill-passthrough' --include='*.rs' --include='*.yml' . | grep -v target | wc -l` = 0.
  - `cargo test -p distillers` passes.
  - Observed on main: 12 lines (`distillers/src/lib.rs:17,37`, `passthrough.rs`, `passthrough/tests.rs`, `dispatcher.rs:172`, `borg/src/stages/distill/tests.rs:137`, the idea fixture).

#### Phase 2: AGENTS.md module-map lint (R3)
**Model:** sonnet
- Script `bin/agents-map` (bash, executable), same shape as the `bloat` task body (`.otto.yml:22-47`: loop, `FAIL:` lines, `exit 1`). Forward check: for each `AGENTS.md` under a crate dir, extract backticked tokens matching `[A-Za-z0-9_./-]+\.rs`; a token resolves when `find <crate> -name <basename(tok)> -not -path '*/target/*'` returns at least one file (basename search, because nested `AGENTS.md` files name parent-dir modules like `pipeline.rs` and `sb/AGENTS.md` names `cli/*.rs` files by bare name); `FAIL: <agents.md>: <tok> not found` on a miss. Reverse check, crate-root `AGENTS.md` only: every top-level `<crate>/src/*.rs` except `lib.rs`, `main.rs`, `tests.rs`, `testutil.rs` must appear by basename somewhere in that file; `FAIL: <agents.md>: <file> undocumented`. Nested module files (`borg/src/config/harvest.rs`) are not required, since the maps list top-level modules. Token regex harvested from `~/.claude/skills/thorough-review/bin/census:264-300`.
- `.otto.yml` task `agents-map` runs `bin/agents-map`; wire into `ci: before` (`.otto.yml:181-183`) after `bloat`. Phase 13's CI workflow calls the same script, so there is one implementation.
- Nested `AGENTS.md` (`borg/src/pipeline/`, `borg/src/stages/`, `borg/clients/`, `vault/src/search/`) get the forward check only.
- **Success criteria:**
  - `otto agents-map` exits 1 at this phase's HEAD (it must catch `cortex/AGENTS.md:55` `hygiene.rs`).
  - After Phase 3 it exits 0.
  - Temporarily adding `` `nope.rs` `` to any module-map table makes it exit 1 (break-the-test check, recorded in implementation notes).
  - Observed on main: the task does not exist (`grep -c agents-map .otto.yml` = 0). Running the forward check by hand with the basename resolver: exactly one miss, `cortex/AGENTS.md: hygiene.rs`. Running the reverse check by hand: exactly 20 undocumented files (listed in Phase 3). Both seats confirmed a path-prefix resolver would false-FAIL on `borg/src/pipeline/AGENTS.md: pipeline.rs`, `borg/src/stages/AGENTS.md: retention.rs`, and seven `sb/AGENTS.md` `cli/*.rs` tokens; the basename resolver is what clears them.

#### Phase 3: Repo doc drift (F10)
**Model:** sonnet
- Root `CLAUDE.md:16` distillers list -> `(article, repo, video, thread, image, voicenote, session, idea)`.
- Root `CLAUDE.md:25` -> `borg::service::install_systemd` (`borg/src/service.rs:240`); pure renderer `render_systemd_unit` (`:186`).
- Root `CLAUDE.md:52` Distilled -> `{ summary, tldr, slug, enumeration, key_ideas, claims, tags, links, kind_specific, meta, transcript }`. Same at `vault/AGENTS.md:17,23`.
- Root `CLAUDE.md:55` L2 patterns -> "chunk/reduce triples for article, video, thread, session, voicenote; `distill-repo`, `distill-image`; nine support patterns; 26 files, `borg/patterns/` is the list".
- `borg/AGENTS.md:12` add `GET /trace/{trace_id}` (`routes.rs:211`) and the auth gate: `routes::require_auth` (`routes.rs:49`) as a `route_layer` over the write routes and `/trace`; health routes open.
- Module maps: add every top-level module the Phase 2 reverse check names, 20 files measured on `f97718f`, one row each with a one-line purpose:
  - `vault/AGENTS.md`: `text.rs`, `tombstone.rs`.
  - `borg/AGENTS.md`: `backoff.rs`, `byline.rs`, `dedupe.rs`, `dispatch.rs`, `eval.rs`, `harvest.rs`, `readability.rs`, `service.rs`, `thread.rs`.
  - `cortex/AGENTS.md:53-58`: remove `hygiene.rs`; add `association.rs`, `bridge.rs`, `entities.rs`, `graph.rs`, `hub.rs` (+`hub/render.rs`, `hub/asymmetry.rs`), `memgraph.rs`.
  - `oracle/AGENTS.md`: `eval.rs`.
  - `distillers/AGENTS.md:53-54`: remove `passthrough.rs`; add `session.rs`, `parse.rs`.
  - `sb/AGENTS.md`: none missing.
- **Success criteria:**
  - `otto agents-map` exits 0.
  - `grep -n 'borg/src/lib.rs`)\|passthrough' CLAUDE.md` is empty.
  - `grep -c 'tldr' CLAUDE.md vault/AGENTS.md` reports >= 1 for each file.
  - Observed on main: the `grep -n` hits `CLAUDE.md:16` (passthrough) and `:25` (`borg/src/lib.rs`); `grep -c tldr` = `CLAUDE.md:0`, `vault/AGENTS.md:0`.

#### Phase 4: Empty-slug publish fallback (F1)
**Model:** sonnet
- Add `borg::hygiene::note_filename(title, trace_id)`: `sanitize_filename(title)`; if empty, `format!("untitled-{trace_id}")`. Log at debug when the fallback fires.
- Replace the nine note-publish sites: `borg/src/pipeline.rs:896,986`; `borg/src/pipeline/text.rs:170,323,696`; `borg/src/pipeline/handlers.rs:798,1027,1243`; `borg/src/pipeline/session.rs:32-33` (keep its slug-then-title order, wrap the result). Leave `borg/src/assets.rs:15,26` (asset names, different contract).
- Tests in `borg/src/hygiene/tests.rs`: empty title -> `untitled-tg-2280a3`; title of ten U+2500 -> same; `"Hello World"` -> `hello-world` (negative). Publish integration test in `borg/tests/`: a `kind: text` ingest with a box-drawing title lands a file whose stem starts with `untitled-`.
- Leave `vault::hygiene::sanitize_filename` and its test `sanitize_filename_empty_input_stays_empty` (`vault/src/hygiene/tests.rs:42`) unchanged: the primitive is correct, the guard belongs one layer up.
- Contract, stated so it is not overclaimed: trace ids are always `vault::trace::generate` output, `{prefix}-{8 hex}` (`vault/src/trace.rs:12`), so the fallback is a valid slug by construction. Uniqueness of `untitled-<trace>` equals trace-id uniqueness, which the receipts DB already relies on (`trace.rs:16` states the birthday bound). The URL publish path writes `dest_path.join(filename)` without `resolve_publish_path` (`borg/src/pipeline.rs:995`) by design: reingest overwrites in place. That is pre-existing behavior for every title and is not changed here.
- **Success criteria:**
  - `cargo test -p borg note_filename` passes.
  - `grep -rn 'hygiene::sanitize_filename(' borg/src --include='*.rs' | grep -v '/tests' | grep -v assets.rs | wc -l` = 0 (every note-publish call goes through `note_filename`; the `(` excludes the doc comment at `session.rs:28`, which Phase 4 also rewords).
  - Observed on main: 10 call lines (`pipeline.rs:896,986`, `text.rs:170,323,696`, `handlers.rs:798,1027,1243`, `session.rs:32,33`), i.e. the nine sites with session's two match arms counted separately; without the `(` the count is 11 because of the comment.

#### Phase 5: borg unit log level from config (S2a)
**Model:** sonnet
- `borg/src/service.rs:217`: `--log-level debug` -> `--log-level {level}` where `level = config.log_level.as_deref().unwrap_or("info")`, mirroring `cortex/src/daemon.rs:896,936`.
- Renderer test: default config -> `--log-level info`; `log_level: Some("debug")` -> `--log-level debug`; the literal `log-level debug` never appears with default config.
- This makes `borg.yml` `log-level` (`borg/src/config.rs:224`, today read by nothing) a live knob.
- **Success criteria:**
  - `cargo test -p borg render_systemd_unit` passes.
  - `grep -c 'log-level debug' borg/src/service.rs` = 0.
  - Observed on main: 1 (`service.rs:217`); `systemctl --user cat borg` on desk shows `--log-level debug`.

#### Phase 6: Rotate the `oracle serve` log (S2b)
**Model:** opus
- `sb/src/logger.rs:190-215` `init_tracing_to_file`: replace the plain `File` with `vault::logging`'s `FileRotate<AppendCount>` (50 MiB x 5, `vault/src/logging.rs:15,19`) wrapped in `tracing_appender::non_blocking(writer)`. The non-blocking layer hands writes to one background thread, so a rotation `rename` never runs on a tokio worker and there is no shared `Mutex` to contend or poison (both seats flagged the `Mutex<FileRotate>` shape: `MakeWriter for Mutex<W>` exists at `tracing-subscriber-0.3.23/src/fmt/writer.rs:808` but locks inline and `expect("lock poisoned")`s). Expose a `rotating_log_writer` constructor from `vault::logging` rather than duplicating the constants. Add `tracing-appender` to `sb/Cargo.toml` (`RollingFileAppender` is time-based only, so `FileRotate` stays the rotator and `non_blocking` is the only piece used).
- Guard ownership at the process boundary, not in `oracle::serve` (which never sees the logger): `logger::init_for` (`sb/src/logger.rs:25`) returns `Result<Option<tracing_appender::non_blocking::WorkerGuard>>` (`Some` only on the serve path); `main.rs:21` binds it as `let _log_guard = logger::init_for(&cli)?;` so it lives across `cli.cmd.run().await` and the shutdown log at `oracle/src/lib.rs:119`. The `SilentFailure` arm at `main.rs:27` calls `drop(_log_guard)` before `std::process::exit(1)`, which otherwise skips destructors.
- Keep the default `lossy(true)`. Both r3 seats flagged `lossy(false)`: with a bounded 128,000-line channel it blocks the calling tokio worker when the writer thread stalls (disk hiccup mid-rotation), which is request-path backpressure from a log. An MCP request must never wait on a log line. The drop is made visible instead of silent: keep the `NonBlocking::error_counter()` (`tracing-appender-0.2.4/src/non_blocking.rs:178`) and log `dropped_log_lines=<n>` from `ErrorCounter::dropped_lines()` (`:313`) in the `MCP server shutting down` line. 128,000 lines is more than the whole `oracle.log` (11 MB) has accumulated since May.
- `WorkerGuard::drop` is bounded, not absolute: it signals shutdown with a 100 ms timeout and waits up to 1 s for the worker's ack (`non_blocking.rs:282`). Lines still queued past that are lost. Acceptable for a log; noted so nobody reads "flushes on shutdown" as a guarantee.
- Wording: one new direct dependency (`tracing-appender`), which pulls `crossbeam-channel` into `Cargo.lock`.
- Test: write past `LOG_ROTATE_MAX_BYTES` into a tempdir path through the same writer stack and observe `oracle.log.1`.
- **Success criteria:**
  - `grep -c 'OpenOptions\|File::create\|File::options' sb/src/logger.rs` = 0 and `grep -c 'non_blocking' sb/src/logger.rs` >= 1 and `grep -c '_log_guard' sb/src/main.rs` >= 2.
  - The rotation test passes.
  - `timeout 300 sb oracle serve </dev/null >/dev/null` exits 0 (stdin EOF ends the MCP transport after indexing) and the last line of `~/.local/share/sb/oracle.log` matches `MCP server shutting down .*dropped_log_lines=0` (the serve writer stack is live end to end and nothing was dropped). `sb oracle call` is NOT a valid smoke: `logger.rs:32` routes only `Serve` through the tracing writer.
  - Observed on main: 1 (`sb/src/logger.rs:197 std::fs::OpenOptions::new()`); `ls -la ~/.local/share/sb/oracle.log*` = `oracle.log` 11 MB + `oracle.log.1` 163 MB (the `.1` came from `sb oracle index` runs through `vault::logging`, not from `serve`).

#### Phase 7: Doctor reports cortex lint's frontmatter policy + `.claude`/`templates` ignore (F5)
**Model:** sonnet
- **One exemption engine.** Cortex already has the policy: `cortex::frontmatter::is_field_required` (`cortex/src/frontmatter.rs:187`) reads `required`, `exempt` (type -> fields), and `path-exempt` (glob -> fields) from cortex.yml, and `path_exempts_field` (`:210`) is already `pub` because classify needed the same answer and a second copy had drifted before ("the drift cost real money"). Panel r2 (both seats) caught this doc about to make a third copy as static enum lists. Rejected. Instead:
  - `cortex/src/report.rs`: add `Report::count_by_rule_prefix(&self, prefix) -> BTreeMap<String, u64>`. Nothing else in cortex changes: `lint_frontmatter` is already `pub` (`frontmatter.rs:29`), and the note-set filter (exclude/include -> `lintable_notes`) lives inside `lint_with_notes` (`lib.rs:126-133`), which is exactly why doctor must enter through `cortex::lint` and not call `scan_vault` + `lint_frontmatter` itself (panel r3: that shortcut lints 3,509 files where `sb cortex lint` lints 3,429).
  - `sb/src/cli/checks.rs vault_findings`: load cortex config (already does at `:166`), call `cortex::lint(&vault_root, &config, &LintOpts { rule: vec!["frontmatter".into()], apply: false, format: Human, path: None })`, then `report.count_by_rule_prefix("frontmatter.required.")` -> `Finding::info("frontmatter gaps (cortex lint policy): domain=<n>, origin=<n>, tags=<n>")` and `report.count_by_rule_prefix("frontmatter.enum.")` summed -> `Finding::warn("<n> frontmatter enum violations", "sb cortex lint --rule frontmatter")` when `n > 0`. Measured cost of that exact call on desk: 1.72 s (staff seat), against a doctor run dominated by the fabric live probe. Drop the `stats.schema_gaps` Info from doctor: doctor gets one signal, and it is the policy the daemon enforces.
  - `vault/src/search/stats.rs:170-184 compute_schema_gaps`: drop `status` (optional per `status-values.md` and `frontmatter.md`). Everything else unchanged. These raw counts stay oracle's `vault_overview` field; raw-over-index and policy-over-files are different questions and are labeled as such.
- **The entity exemption is config, not code.** Cortex lint today emits 920 `frontmatter.required.domain` and 931 `frontmatter.required.origin`, dominated by the 915 entity hubs, because cortex.yml exempts `daily` and `notes/ai/**` but never `entities/`. Phase 14 adds `path-exempt: "entities/**": [domain, origin]`. After that dotfiles commit, doctor's numbers drop with no rebuild, which is the proof the policy is config-driven. `Daily` is not origin-exempt: the 86 daily notes reading empty in the index carry `origin: human` (an enum violation, not a required gap; the 87th `origin: human` file is a journal `type: note`), and F2 renames all 87.
- `cortex/src/config.rs Config::load`: `log::warn!` when the YAML carries a `schema:` key ("schema: overrides the enum-derived vocabulary; delete it unless you mean to"). Lands here, not in the dotfiles phase, so each phase stays one repo, one commit.
- `vault/src/config.rs` `ScanConfig::default().ignore` (today `.git, .obsidian, .cortex, assets, attachments, quarantine`): add `.claude` and `templates`. `oracle/src/config.rs:416-418 default_ignore_dirs` (today `.git, .obsidian, templates`): add `.claude`. The two lists then agree. `templates` matters for oracle: `index_vault` scans `system/templates/*.md` today (the watcher already ignores it, the full index does not): those rows are 2 of the `note_type=5` gaps and the source of 1,190 `graph_note_rows: unparseable tags JSON for system/templates/...` WARN lines in `cortex.log`. It does NOT fix cortex: cortex builds its scan from cortex.yml `vault.ignore` (`cortex/src/vault.rs:11-16`), not from `ScanConfig::default()`, and the daemon's quality/duplicates arms pass that unfiltered scan straight to `apply_quality` (`cortex/src/daemon.rs:780`; no `is_excluded` call anywhere in `daemon.rs`), so cortex.yml `vault.exclude: system/**` protects lint only. The stamps in `daily.md` are re-applied every tick, idempotently, which is why their mtime still reads 2026-08-15. The panel caught this; the author's first reading was wrong. The cortex-side fix is one cortex.yml line in Phase 14 (`vault.ignore: + templates`), and Phase 15's criterion waits two ticks to prove the scrub sticks.
- Unit test: `count_by_rule_prefix("frontmatter.required.")` over a `Report` with two `frontmatter.required.domain`, one `.origin`, one `tags.non-canonical` returns `{domain: 2, origin: 1}`; the same call with `"frontmatter.enum."` ignores all four.
- **Success criteria:**
  - `cargo test -p cortex count_by_rule_prefix` passes.
  - Doctor's `frontmatter gaps` numbers equal cortex lint's own counts by an independent path (doctor calls the function, lint prints the rows): `sb cortex lint 2>/dev/null | grep -c '\[frontmatter.required.domain\]'` equals doctor's `domain=`; same for `origin`. Not circular: two consumers of one policy are compared, and Phase 14's config edit must move both numbers together with no rebuild.
  - Doctor's raw `schema gaps` Info line is gone from `sb doctor` output; `sb oracle call vault_overview` still returns `schema_gaps` without a `status` entry.
  - `sqlite3 <oracle.db> "select count(*) from notes where path like '%.claude/%' or path like 'system/templates/%'"` = 0 after `sb oracle index --force`; `grep -c 'system/templates' ~/.local/share/sb/cortex.log` stops growing after the next graph tick.
  - Observed on main: doctor prints `💬 [vault] schema gaps: domain=1048, note_type=5, origin=1195, status=1088` (raw index count). `sb cortex lint` prints 13,032 rows: `tags.non-canonical` 7,514, `frontmatter.enum.type` 1,428, `frontmatter.required.origin` 931, `frontmatter.required.domain` 920, `frontmatter.enum.method` 499, `frontmatter.required.tags` 289, `frontmatter.enum.origin` 88; 2,753 rows name `entities/`; `.claude/loop.md` appears twice (cortex.yml `vault.ignore` lacks `.claude`, fixed in Phase 14). Index rows under `.claude/` = 2 and under `system/templates/` = 11 (all eleven templates are indexed, not two; an earlier draft said 4); `cortex.log` carries 1,190 template WARN lines.

#### Phase 8: Doctor stale-inbox Warn and data-dir section (R5, R6)
**Model:** sonnet
- `vault/src/search/stats.rs`: `inbox_oldest()` = `SELECT path, modified_at FROM notes WHERE path LIKE 'inbox/%' AND path NOT LIKE 'inbox/.%' ORDER BY modified_at ASC LIMIT 1`.
- `sb/src/cli/checks.rs` vault section: `const INBOX_STALE_SECS: u64 = 48 * 3600` beside `FABRIC_PROBE_TIMEOUT_SECS` (`:265`, doctor thresholds are consts today); `Finding::warn("oldest inbox note <path> is <n>h old", "sb cortex classify, or assign a domain by hand")` when older; Ok otherwise. Rationale comment: the daemon classifies every 300 s, so anything past 48 h is stuck or low-confidence (`classify.rs:963` deliberately leaves low-confidence notes unmarked) and either way wants eyes. On desk this Warn fires on `inbox/barcode-label-on-clear-plastic-bag.md` until Scott triages it; that is the check working.
- `vault/src/paths.rs`: `dir_size(&Path) -> u64` via walkdir, no symlink follow.
- New doctor section `data dir` (register in `all_sections()`, `checks.rs:72`): Info lines for `sb/borg/stages`, `sb/borg/receipts.db`, `sb/*.log*` (sum), `sb/oracle`, and `legacy oracle/` while it exists; `Finding::warn` when logs exceed `const DATA_DIR_LOGS_WARN_BYTES = 512 MiB` or total exceeds `2 GiB`; `Finding::warn` for any file in the oracle dir other than `oracle.db{,-wal,-shm}` and `eval-cache.db` ("stray backup").
- **Success criteria:**
  - `cargo test -p vault inbox_oldest` passes.
  - `sb doctor 2>/dev/null | grep '\[data dir\]'` shows one line each for `stages`, `receipts.db`, `logs`, and `oracle`.
  - With `inbox/barcode-label-on-clear-plastic-bag.md` still present, `sb doctor` prints a Warn containing `oldest inbox note`.
  - Observed on main: `[data dir]` count = 0; sqlite `inbox_oldest` query returns `inbox/barcode-label-on-clear-plastic-bag.md` with `modified_at` = 2026-07-05 (the `inbox/.claude/loop.md` row, 2026-07-24, is what the `NOT LIKE 'inbox/.%'` clause excludes).

#### Phase 9: Oracle data dir under `sb/` (R1)
**Model:** opus
- `vault/src/paths.rs:362,373`: join `"sb"` then `"oracle"`. Add `legacy_oracle_dir()`. Update the doc comments and `config/templates/oracle.yml.example:6`.
- Doctor (`data dir` section from Phase 8): `Finding::warn("legacy oracle data dir present at ~/.local/share/oracle; see runbook R1", ...)` while `legacy_oracle_dir().join("oracle.db")` exists.
- `cortex/src/daemon.rs:945` `render_systemd_unit`: `ReadWritePaths={vault}` -> `ReadWritePaths={vault} {data}` where data is `xdg_data_dir()/sb`, matching borg's unit (`borg/src/service.rs`). Renderer test asserts both paths appear.
- Tests: `vault/src/paths/tests.rs`: `oracle_db_path().ends_with("sb/oracle/oracle.db")`; XDG-redirect precedent `cortex/src/sweep/tests.rs:160-190`.
- **Structural guard, not a runbook rule.** `vault/src/search.rs SearchIndex::open`: as the FIRST statement, before the `create_dir_all(parent)` at `:373` (both r2 seats: a guard placed after it would create `~/.local/share/sb/oracle/` and the runbook's directory rename would then nest the old dir inside it), when `path == oracle_db_path()`, `!path.exists()`, and `legacy_oracle_dir().join("oracle.db").exists()`, return a typed `Err` (`SearchError::LegacyOracleDb { legacy, new }`) naming both paths and the runbook step. Every opener goes through `SearchIndex::open` (cortex daemon and one-shots via `config.oracle_db_path()`, `oracle serve`/`index`/`call`/`stats`/`eval` via `Config::db_path`, `sb doctor` at `checks.rs:726`), so no process can create an empty DB while the legacy one exists. `sb oracle eval` opens the index (`eval.rs:155`) before the judgment cache (`:158`, whose `cache.rs:59` would `create_dir_all` the same parent), so the guard fires first there too. One opener degrades instead of failing: `cortex/src/classify.rs:60` does `SearchIndex::open(..).ok()` because Tier-2 similar-note context is optional; add `log::warn!` on the `Err` so it is visible. Deploying this binary before the move is therefore loud and harmless: cortex.service fails on start and `Restart=on-failure` retries every 5 s with the message in the journal, `sb doctor` prints an Error, `sb oracle serve` refuses. The move clears it. Both seats asked for exactly this; the previous draft relied on "never deploy first".
- `sb/src/cli/checks.rs:726`: today any open failure is `Finding::warn(.., "sb oracle index")`. Match the typed guard error and emit `Finding::error(<message>, "runbook R1: stop cortex, mv -T ~/.local/share/oracle ~/.local/share/sb/oracle")`; other open failures keep the Warn.
- No auto-migration at open: two processes can start concurrently with no lock; the guard makes the race harmless instead of racing it.
- Unit test for the guard: tempdir with `XDG_DATA_HOME`; legacy `oracle/oracle.db` present, `sb/oracle/` absent -> `open` errs AND `sb/oracle/` still does not exist afterwards; both absent -> `open` creates; new present -> `open` succeeds regardless of legacy.
- **Success criteria:**
  - `cargo test -p vault paths` passes with the new assertion.
  - `cargo test -p vault legacy_oracle_guard` passes all three cases.
  - `cargo test -p cortex render_systemd_unit` asserts `ReadWritePaths=` contains both the vault and the data dir.
  - After the runbook move + deploy: `sb doctor 2>/dev/null | grep 'note(s) indexed'` shows the count recorded in the runbook's pre-move step, never 0.
  - Observed on main: `✅ [vault] 3454 note(s) indexed across 12 domain(s)`; `grep -c 'sb/oracle' vault/src/paths.rs` = 0; `grep -c ReadWritePaths cortex/src/daemon.rs` = 1 (vault only); `pgrep -fc 'sb oracle serve'` = 2 (two Claude Code sessions hold the DB open right now, which is what the runbook gate is for).

#### Phase 10: `sb bootstrap --prune-legacy-config` (S4)
**Model:** sonnet
- `sb/src/cli/bootstrap.rs` `BootstrapArgs`: `--prune-legacy-config` and `--apply` (dry-run default, per `sb cortex migrate`, `sb/src/cli/cortex.rs:291`).
- `sb/src/cli/bootstrap/migrate.rs`: `prune_legacy(apply) -> Report`. Per dir in `LEGACY_DIRS` (`:20`): skip if absent; refuse (report, no delete) if `MARKER` (`:18`) absent; list every file; refuse the whole dir naming each stranger if any file is not in the known set (the seven plan basenames at `:74-100`, `patterns/*.md`, the marker); otherwise delete via `borg::rkvr::remove` (`borg/src/rkvr.rs:22`, recoverable through `rkvr rmrf` when installed, WARN + std removal when not), never `remove_dir_all`. `remove` is `pub(crate)` today; widen to `pub` (sb already depends on borg).
- Report lines per dir: `would remove` / `removed` / `refused: <strangers>`.
- Tests (tempdir + `XDG_CONFIG_HOME`, `serial_test`): marker + known files -> removed under `--apply`, listed under dry-run; marker + stranger -> refused with the stranger named; no marker -> untouched; dir absent -> silent.
- Update `migrate.rs:5-6` and root `CLAUDE.md:53` to say the verb exists.
- **Success criteria:**
  - `cargo test -p sb prune_legacy` passes all four cases.
  - `sb bootstrap --prune-legacy-config` (dry-run) on desk lists exactly `borg`, `cortex`, `second-brain`.
  - Observed on main: the flag does not exist; `ls -d ~/.config/{borg,cortex,second-brain} | wc -l` = 3, each holding `.migrated-to-sb` dated 2026-05-24.

#### Phase 11: `system` tag group (F6)
**Model:** sonnet
- `config/canonical-tags.yml`: add `system: []` beside `diy: []` (`:109`). The grouping is documentation; the flat set is what code reads (`vault/src/canonical.rs:34 all_tags`).
- `vault/src/canonical/tests.rs`: parse the embedded `config/canonical-tags.yml`; assert every group key parses as `Domain` and every `Domain::all()` variant has a key.
- **Success criteria:**
  - `cargo test -p vault canonical` passes; renaming a key to `sytem` fails it (break-the-test check, recorded).
  - After `sb bootstrap --force`, `sb doctor 2>/dev/null | grep 'canonical-tags.yml'` says `matches binary`.
  - Observed on main: `grep -c '^system:' config/canonical-tags.yml` = 0; 11 group keys for 12 `Domain` variants; doctor today says `canonical-tags.yml: matches binary`.

#### Phase 12: Schema docs rendered from `vault::schema` (F3)
**Model:** opus
- `vault/src/schema.rs`: `description(&self) -> &'static str` on all five enums, exhaustive `match` so a new variant cannot ship undescribed. Seed text from the current `system/schemas/*-values.md` tables; write descriptions for the ten types the vault doc lacks (entity, session, digest, review, image, pdf, reddit, audio, document, code) and the four methods (harvest, signal, discord, ntfy). `Entity`'s description states the hub contract: "carries no `domain`, `origin`, or `status`"; the enforcing rule is cortex.yml's `path-exempt: "entities/**"` (Phase 14), and this text is the human-readable statement of the same rule.
- `oracle/src/server.rs:635-650 schema_info`: emit `{value, description}` pairs.
- New `cortex/src/schema_docs.rs`: pure renderers for `domain-values.md`, `type-values.md`, `origin-values.md`, `status-values.md`. Each file: generated frontmatter (`type: system`, `domain: system`, `origin: generated`, `generated-at`, `generator: sb cortex schema`, `pinned: true`), a "regenerated by `sb cortex schema --render`; do not edit" line, the values table from the enum (value, description), and a fixed Rules block carried as a `const &str` per file (the current Rules prose, minus the stale "no hyphens" rule). Drop `domain-values.md`'s "Replaces folder" column and its Tag->Domain table (duplicates `cortex::classify::default_tag_domain_map`, `cortex/src/classify.rs:121`; the map is config-driven and the doc copy has already drifted). Precedent: `cortex/src/sweep.rs:352-430` cold-notes renderer + `cortex/src/sweep/fixtures/cold-notes-expected.md` snapshot + `#[ignore]` regen test (`sweep/tests.rs:85`).
- `render_all(vault_root, apply)`: for each file compare rendered bytes to disk; `Unchanged` / `Drifted` / `Written` (atomic via `vault::note::write_atomic`).
- `sb cortex schema --render|--check` in `sb/src/cli/cortex.rs`; `--check` exits 1 on any `Drifted`.
- Doctor `vault` section: `Finding::warn("system/schemas/*-values.md drifted from binary", "sb cortex schema --render")` using the same compare (mirrors `shared_config_findings`).
- `frontmatter.md` stays hand-written (field tables are not in code). Fix its naming-rule line ("single lowercase words, no hyphens" -> "prefer single word; hyphenate when needed, e.g. `cortex-quality`") and add an `entity` note under Universal Fields ("entity hubs carry no `domain`, `origin`, or `status`"). Done in Phase 15 (vault commit).
- **Success criteria:**
  - `cargo test -p cortex schema_docs` snapshot tests pass.
  - `sb cortex schema --check` exits 1 against the current hand-written files, 0 after `--render`.
  - `sb oracle call schema_info | grep -c description` >= 5.
  - Observed on main: `sb cortex schema` is not a subcommand; `sb oracle call schema_info 2>/dev/null | grep -c description` = 0; `grep -c 'fn description' vault/src/schema.rs` = 0.

#### Phase 13: PR-time CI (R4)
**Model:** sonnet
- `.github/workflows/ci.yml`: `on: push: branches: [main]`, `pull_request`. Same `container: debian:bookworm`, apt list, rustup pin (`RUST_VERSION: 1.96.0`), `Swatinem/rust-cache@v2` with `shared-key: ci` as `release.yml:17-54`. Steps mirror `otto check` + `otto test` (`.otto.yml:50-71`): `cargo fmt --all --check`; `cargo check --workspace --all-targets --features vec`; `cargo clippy --workspace --all-targets --features vec -- -D warnings`; `cargo test --workspace --features vec`; the `bloat` loop inlined; `bin/agents-map` from Phase 2. Skip `whitespace -r` (Scott's binary, not installable in CI).
- Default test path is offline: candle parity skips without `vault/tests/fixtures/`; the real-model test is gated on `CANDLE_TESTS_REAL=1`; `candle-bounded.rs` and `perf.rs` are `#[ignore]`; `hub_retrieval_contract.rs` uses the committed tokenizer.
- **Success criteria:**
  - `gh workflow view ci` lists the workflow after push.
  - The first run on `main` is green; warm-cache duration recorded in implementation notes.
  - Observed on main: `gh workflow list` = `Release` only.

#### Phase 14: dotfiles (F4, F2 config, S2 config, `Inbox/**` casing)
**Model:** sonnet
- Repo: `~/repos/scottidler/dotfiles`, files `HOME/.config/sb/{borg,cortex}.yml`.
- `cortex.yml:20-25`: delete the `schema:` block. `Config` is `#[serde(default)]` at struct level (`cortex/src/config.rs:8`), so the missing key falls to `SchemaConfig::default()` (`:414`), which is built from the enums. The symlink means the commit is live on the next daemon tick.
- `cortex.yml:39`: `"Inbox/**"` -> `"inbox/**"`.
- `cortex.yml` `actions.frontmatter.path-exempt`: add `"entities/**": [domain, origin]`. Entity hubs carry neither by contract (F3's `Entity` description says so); today lint flags 915 of each. This single line is what makes doctor's Phase 7 numbers drop.
- `cortex.yml` `vault.ignore` (today `.git .obsidian .cortex assets attachments`): add `.claude` and `templates`. Cortex lint currently reports `.claude/loop.md` (Claude Code state) as a note missing domain and origin, and the daemon's quality arm re-stamps `system/templates/daily.md` every tick because `vault.exclude` does not reach the non-lint arms (Phase 7 text). `ignore` matches directory names anywhere; the only `templates` dir in the vault is `system/templates/`.
- `cortex.yml` `migrations:` (after `v3-domain-expansion`, `:223-237`): add
  ```yaml
  - name: v4-legacy-values
    value-renames:
      origin: { human: authored, ai-generated: generated }
      status: { resolved-workaround: reviewed }
  ```
- `borg.yml:121`: `log-level: debug` -> `log-level: info`.
- **Success criteria:**
  - `sb doctor 2>/dev/null | grep '\[config\]'` shows all three parse.
  - `sb cortex lint 2>/dev/null | grep -c 'not valid'` = 89 (before F2 apply).
  - `sb cortex lint 2>/dev/null | grep -c '\[frontmatter.required.domain\]'` <= 10 and `grep -c '\[frontmatter.required.origin\]'` <= 30 (today 920 and 931; the residue is the handful of hand-written notes missing a field), and `grep -c '.claude/'` = 0 (today 2 rows). `sb doctor` shows the same two numbers with no rebuild.
  - `sb cortex migrate 2>/dev/null | grep -c "would rename origin: 'human'"` = 87, `grep -c "would rename origin: 'ai-generated'"` = 1, `grep -c "would rename status: 'resolved-workaround'"` = 1 (dry-run; message shape from `cortex/src/migrate.rs:366`).
  - Observed on main: lint count = 2016, first rows `type 'entity' is not valid; allowed: [youtube, article, ... digest, review]`; `sb cortex migrate` prints `No violations found.` (no `v4-legacy-values` entry yet).

#### Phase 15: Vault repo edits (S6, F8, F9, S8, F7, F2 apply, S7, F3 render)
**Model:** sonnet
- Repo: `~/repos/scottidler/obsidian`. Requires Phases 12 and 14 done and the Phase 12 binary deployed (F3 render needs the verb; F2 apply needs the `v4-legacy-values` entry).
- S6: `git rm .github/workflows/ci.yml`.
- F9: `CLAUDE.md:17` "Five" -> "Six", name `work/`; table `:19-26` add `| work/ | Scott's own work notes (domain: work, origin: authored), created by bin/wn; surfaced on home.md and system/views/work.base |`; `:193` "exactly five: inbox, journal, notes, entities, system" -> "exactly six: inbox, journal, notes, entities, work, system". `README.md:7-13` add `entities/` and `work/` rows; `:19` add `system` to the domain list; `:24` "Dataview queries" -> "Obsidian Bases (`.base`)".
- F8: `home.md:41` eleven `[[domain-*]]` -> one `[[domains.base]]` (the `By Domain` grouped view). Runbook step verifies in Obsidian and adds `.base` to the five existing extensionless base links (`:29,39,40`) if they dangle too.
- S8: `git mv the-promise-of-loopr.md notes/`; set `domain: tech`, `origin: authored`. `git mv loopr-architectureexcalidraw.md notes/`; set `title: Loopr Architecture (excalidraw)`; keep the filename as is, because the daemon runs the naming lint with `apply` on every tick (`cortex/src/daemon.rs:625-631`) and would re-slug a restored `.excalidraw.md` infix; the plugin recognises the drawing by `excalidraw-plugin: parsed`, not by name. `git rm inbox/.md` (F1 shipped in Phase 4; receipts row stays `succeeded`, staging and sidecar already aged out).
- F7: rewrite all 11 `system/templates/*.md`: `<% tp.date.now("YYYY-MM-DD") %>` -> `{{date:YYYY-MM-DD}}` (core Templates supports Moment format strings after a colon; Obsidian help, Templates: "Both `{{date}}` and `{{time}}` allow you to change the default format using a format string ... for example `{{date:YYYY-MM-DD}}`"); `<% tp.file.title %>` -> `{{title}}`; `daily.md` drop the `<%* %>` block and the two `<% exercises %>` (pushups/situps become plain `20` starting values); scrub `daily.md:35-38` cortex stamps (`cortex-quality`, `cortex-quality-issues`, `cortex-duplicate`, `cortex-duplicate-group`). Runbook writes `.obsidian/templates.json` and `daily-notes.json`.
- F2: `sb cortex migrate` (expect 89 rows) then `sb cortex migrate --apply`.
- S7: `sb borg reingest --source https://www.youtube.com/shorts/iDISCSQn6mI` (the `source:` of `notes/prompt-caching-cuts-claude-code-bills-by-80.md`, the one in-scope note; regenerates staging and a transcript-free body; reingest preserves the note's location, `borg/src/pipeline.rs:988`). Verify `## Transcript` is gone from that file and `sb borg log --since 1h --source shorts/iDISCSQn6mI` shows `succeeded` without `degraded`.
- F3: `sb cortex schema --render`; `frontmatter.md` hand edits from Phase 12.
- Staging is path-level, never `git add -A` and never a glob: the vault worktree carries 146 unrelated entries (daemon digests, link-pass edits). Stage exactly these paths: `.github/workflows/ci.yml` (rm); `CLAUDE.md`; `README.md`; `home.md`; `the-promise-of-loopr.md` -> `notes/the-promise-of-loopr.md` and `loopr-architectureexcalidraw.md` -> `notes/loopr-architectureexcalidraw.md` (both sides of each `git mv`); `inbox/.md` (rm); the eleven templates by name: `system/templates/{book,daily,frontmatter,idea,link,moc,note,presentation,slack-post,vocab,work-note}.md`; the five schema docs by name: `system/schemas/{domain-values,type-values,origin-values,status-values,frontmatter}.md`; `notes/prompt-caching-cuts-claude-code-bills-by-80.md`; and the 88 files (89 violations: 87 journal notes plus `notes/persona-oauth-callback-dns-local-tatari-tools.md`, which carries both stray values) that `sb cortex migrate --apply` prints, taken from its output. Scott commits (vault commits are manual).
- **Success criteria:**
  - `grep -rl --include='*.md' '^origin: human$' . | wc -l` = 0; `grep -c 'domain-ai' home.md` = 0; `grep -c 'exactly six' CLAUDE.md` = 1; `grep -c '^| work/' CLAUDE.md README.md` = 1 each; `grep -c 'entities/' README.md` >= 1.
  - `grep -rl '<%' system/templates | wc -l` = 0; `grep -c 'cortex-' system/templates/daily.md` = 0 both immediately and again after two daemon ticks (`sleep 660`: about eleven minutes, poll-interval is 300 s, not instant), which proves Phase 14's `templates` ignore stopped the re-stamp; `test -e inbox/.md` fails; `test -e .github/workflows/ci.yml` fails; `test -f notes/the-promise-of-loopr.md && test -f notes/loopr-architectureexcalidraw.md` succeeds.
  - `grep -c '^## Transcript' notes/prompt-caching-cuts-claude-code-bills-by-80.md` = 0.
  - `sb cortex schema --check` exits 0; `gh run list -R scottidler/obsidian -L 3` shows no run newer than the removal commit; `git show --stat HEAD | grep -c 'notes/ai/daily'` = 0 (no unrelated churn rode along).
  - Observed on main (vault at `08bdf0a1`): 87; 1; 0 (`exactly five`); 0; 0; 11; 4 cortex stamps; both files exist; both notes at root; transcript count 1; `README.md:19` domain list omits `system`; `README.md:24` says "Dataview queries"; `git status --porcelain | wc -l` = 146.

#### Operator runbook (desk, no commits)
Run after the phases named in each step. Every command is read-back-verified in the last column.

| Step | After | Command | Verify |
|---|---|---|---|
| S1 | any, and before R1 | `rm ~/.local/share/oracle/oracle.db.pre-*; rm -r ~/.local/share/oracle/logs` | `ls ~/.local/share/oracle` shows only `oracle.db`, `oracle.db-wal`, `oracle.db-shm`, `eval-cache.db` |
| S2 | Phase 5 deployed AND Phase 14 committed (`borg.yml` `log-level: info` is what the renderer reads) | `rm ~/.local/share/sb/{borg,cortex,oracle}.log.[1-9]`; `sb borg daemon --reinstall`; `systemctl --user restart borg` | `systemctl --user cat borg \| grep ExecStart` shows `--log-level info` |
| S3 | any, and before R1 (the legacy daily/weekly units run `sb cortex intel`, another DB opener if ever enabled) | `rm ~/.config/systemd/user/{cortex-daily.service,cortex-daily.timer,cortex-weekly.service,cortex-weekly.timer,cortex.service.bak}; systemctl --user daemon-reload` | `ls ~/.config/systemd/user \| grep -c cortex` = 1 |
| R2 | any | `rm ~/.config/sb/patterns/facet-*.md` | `ls ~/.config/sb/patterns \| wc -l` = 26 |
| R1 | S1 and S3 done; Phase 9 built (deploy order cannot corrupt: the guard fails closed; moving first avoids a cortex restart loop) | record `sb doctor 2>/dev/null \| grep 'note(s) indexed'`; `systemctl --user stop cortex`; `pgrep -af '[s]b oracle serve'` must print nothing (the bracket keeps pgrep from matching its own command line; close Claude Code sessions, it was 2 during discovery); `test ! -e ~/.local/share/sb/oracle && mv -T ~/.local/share/oracle ~/.local/share/sb/oracle` (`-T` refuses to nest if the destination exists; one directory rename, same device, so `oracle.db`, `-wal`, `-shm`, and `eval-cache.db` move atomically together); `otto deploy` (or `systemctl --user start cortex` if already deployed) | `test -d ~/.local/share/oracle` fails; `test -f ~/.local/share/sb/oracle/oracle.db` succeeds; `sb doctor` shows the recorded note count and no legacy finding |
| S4 | Phase 10 deployed | `sb bootstrap --prune-legacy-config` then `--apply` | `ls -d ~/.config/{borg,cortex,second-brain}` all fail |
| F7 | Phase 15 | write `.obsidian/templates.json` `{"folder":"system/templates"}` and `.obsidian/daily-notes.json` `{"folder":"journal","format":"YYYY/MM/YYYY-MM-DD","template":"system/templates/daily"}` | Ctrl-N + insert template expands `{{date}}` |
| F8 | Phase 15 | open `home.md` in Obsidian; click `[[domains.base]]` and the five existing base links | all six resolve; if the five dangle, append `.base` and recommit |

## Acceptance Criteria

Run against desk when all phases and the runbook are done.

- [ ] `otto ci` green on `main`, and `otto agents-map` exits 0.
- [ ] `sb doctor` exits 0; every Warn present, if any, contains `oldest inbox note` (R5 firing on a note awaiting human triage is the check working); output contains `maxTokens`, a `[data dir]` section, and a `frontmatter gaps` line whose `domain=` and `origin=` equal `sb cortex lint`'s `frontmatter.required.domain` / `.origin` row counts.
- [ ] `sb cortex lint 2>/dev/null | grep -c 'not valid'` = 0 (today: 2016).
- [ ] In the vault: `grep -rl --include='*.md' '^origin: human$' . | wc -l` = 0; `test -e inbox/.md` fails; `grep -c 'domain-ai' home.md` = 0; `test -e .github/workflows/ci.yml` fails.
- [ ] On desk: `test -d ~/.local/share/oracle` fails; `test -f ~/.local/share/sb/oracle/oracle.db` succeeds; `ls -d ~/.config/{borg,cortex,second-brain}` all fail; `ls ~/.config/systemd/user | grep -c cortex` = 1; `ls ~/.config/sb/patterns | wc -l` = 26.

Observed on main, 2026-09-05, desk: `sb doctor` exit 0 with 2 Info and 0 Warn, `maxTokens` count 0, `[data dir]` count 0, `schema gaps: domain=1048, note_type=5, origin=1195, status=1088`; lint `not valid` count 2016; vault 87 / exists / 1 / exists; host: `~/.local/share/oracle` exists (1.1 GB), `~/.local/share/sb/oracle` absent, 3 legacy config dirs, `grep -c cortex` = 6, patterns = 30.

### Post-build status, 2026-09-06

Measured after phase 15, before the deploy and before the operator runbook:

- `otto agents-map` exit 0; `otto ci` green on `main` at `8064ce6`. **PASS.**
- `sb cortex lint | grep -c 'not valid'` = 0 (was 2016). **PASS.**
- Vault: `origin: human` files 0 (was 87); `domain-ai` in `home.md` 0; `inbox/.md` absent; vault `.github/workflows/ci.yml` absent. **PASS.**
- `sb doctor` and the desk host row: **DEFERRED.** Both depend on the deploy and the operator runbook, neither of which has run. `sb doctor` currently prints the Phase 9 legacy-oracle Error by design and returns before its `frontmatter gaps` line; runbook R1 clears it.

### Criterion amendments (post-build)

Three criteria were written as commands that cannot pass as literally spelled. Each was executed, the failure reproduced against unmodified code, and the criterion corrected here rather than the implementation bent to fit it.

- **Phase 6, criterion 3** said `timeout 300 sb oracle serve </dev/null` exits 0 and writes the shutdown line. It exits 1: rmcp fails the MCP handshake on immediate stdin EOF (`Error: connection closed: initialize request`) at `server.serve(transport).await?`, before `service.waiting()`, so the shutdown line is never reached. Reproduced with the pre-Phase-6 binary, so it is not caused by the rotation work. **Amended to:** drive a real `initialize` + `notifications/initialized` on stdin, then EOF. Observed: exit 0, last line `MCP server shutting down dropped_log_lines=0`.
- **Phases 4, 5, 7-12** wrote test criteria as `cargo test -p <crate> <name>`. `-p <crate>` alone does not compile in this workspace: `vault::search` is behind the `vec` feature and the consumer crates do not forward it. **Amended to:** `cargo test --workspace --features vec <name>`, which is what `.otto.yml`'s test task already runs.
- **Phase 15**, the two-tick re-stamp criterion, assumed the running cortex daemon would pick up Phase 14's `vault.ignore: templates` without a restart. It does not: `cortex.service` has run since 2026-09-03 07:58:31 and reads its config at start, so at 2026-09-06 01:32:52 (twelve minutes after the config landed) it was still emitting `system/templates/*` lines. The `grep -c 'cortex-' system/templates/daily.md` = 0 reading after two ticks is therefore **not** evidence the ignore worked. **Amended to:** re-check after cortex restarts, i.e. after runbook step S2.

### Blocked

- **Phase 15, S7.** `sb borg reingest --source https://www.youtube.com/shorts/iDISCSQn6mI` returns `fetch-failed`; `yt-dlp` on the same URL returns `ERROR: [youtube] iDISCSQn6mI: This video is unavailable`. The source video no longer exists, so the transcript cannot be regenerated. `notes/prompt-caching-cuts-claude-code-bills-by-80.md` keeps its `## Transcript` section. The doc's own reason for choosing reingest over `bin/strip-transcripts` was that stripping would destroy the note's only copy; with the source dead that reasoning now argues for leaving the section in place. Scott's call.

### Follow-up found during the build, not fixed

- `sb/src/cli/checks.rs`: `frontmatter_policy_findings()` sits after the oracle-index open in `vault_findings`, so an index-open failure suppresses it even though it reads markdown through `cortex::lint` and never touches the index. Visible today because the Phase 9 guard is active. Self-clears at runbook R1; moving the call up beside `schema_docs_findings()` would decouple it.
- `~/.local/share/sb/oracle.log` is written by every concurrent `sb oracle serve` process, and Phase 6 put a `FileRotate` under it. Whichever process crosses 50 MiB renames the file under the others, which keep writing to the renamed inode until restart. Pre-existing shape for every sb log; Phase 6 is the first to add rotation to a multi-process one.
- Root `CLAUDE.md` does not mention `sb cortex schema` or that four of the five schema docs are now generated.

## Resolved Decisions

- 2026-09-05 **S7 re-scoped to one reingest.** Verification: 103 `## Transcript` sections; 53 keep transcripts by design (social/image/note), 44 are quarantined and unscanned, 5 are pre-cutoff legacy bodies `strip-transcripts` refuses. The one in-scope note has no staged copy, so stripping would delete its only transcript. Reingest regenerates both.
- 2026-09-05 **R2 is a delete, not a derivation.** `sb/src/cli/bootstrap/tests.rs:31 patterns_array_matches_source_tree` already guards the table; the explicit-list choice is documented at `bootstrap.rs:45-48`. The four `facet-*` files are a May 2026 experiment (commit `2594dd5`) with zero references anywhere.
- 2026-09-05 **S3 is an rm.** `cortex.service.bak` is operator-made (no code writes `.bak`); an uninstall/install cycle would delete the live unit mid-run.
- 2026-09-05 **F2 goes through `sb cortex migrate`, config-driven.** `sb borg migrate` skips `journal/` (`skip-folders`), where all 87 live. The two stray values ride the same `v4-legacy-values` entry.
- 2026-09-05 **F7 rewrites to core Templates syntax.** No Templater install. `tp.user.workweek()` called a script that no longer exists; the two computed fields become plain numbers.
- 2026-09-05 **R1 has no auto-migration.** Concurrent starters, no lock, and `otto deploy` bootstraps with daemons live. Operator move with cortex stopped.
- 2026-09-05 **R7 ships first with a decision record.** The token-budget doc rejects a fork as the final fix; the fork is accepted as the interim guard (Scott's `borg.yml` comment already says so and names `sb doctor` as the guard). The addendum lands in that doc's Resolved Decisions in Phase 0.
- 2026-09-05 **Doctor thresholds are consts.** Doctor has no config file; `FABRIC_PROBE_TIMEOUT_SECS` and the borg 24 h window are consts. Same for `INBOX_STALE_SECS` and the data-dir limits.
- 2026-09-05 **Schema docs: four generated, one hand-written.** `frontmatter.md`'s field tables are not in code; generating them would invent a second schema. The four `*-values.md` files are pure enum listings.
- 2026-09-05 (panel r1, both seats) **R1 gets a fail-closed guard in `SearchIndex::open`.** The runbook rule "never deploy first" was the only control on the highest-impact failure; both seats named `sb doctor`, `sb oracle call/index/stats`, and cortex one-shots as openers the `pgrep` gate misses. The guard makes every one of them refuse rather than create an empty DB. Folded in.
- 2026-09-05 (panel r1, both seats) **Phase 6 uses `tracing-appender::non_blocking`.** The earlier "do not add the crate" line assumed a `Mutex<FileRotate>` MakeWriter was enough; both seats verified it locks inline on the tokio worker and panics on poison. One new dependency, accepted. Folded in.
- 2026-09-05 (panel r1, both seats) **F5 exempt sets corrected.** `System` removed from domain-exempt (system notes carry `domain: system`); `Daily` removed from origin-exempt (cortex.yml requires it; 86 missing is a real gap). Sets now mirror the live cortex.yml exemptions. Folded in.
- 2026-09-05 (panel r1, staff) **Phase 2 resolver is basename search; Phase 3 lists all 20 undocumented modules.** Measured by hand; the seat's enumeration matched. Folded in.
- 2026-09-05 (panel r1, staff) **Phase 15 stages by path.** The vault worktree carries 146 unrelated entries. Folded in.
- 2026-09-05 (panel r1, staff) **The `Config::load` schema warn lives in Phase 7**, not "either phase". One repo, one commit per phase. Folded in.
- 2026-09-05 (panel r1, architect) **F7 `{{date:YYYY-MM-DD}}` stands.** The seat asserted core Templates has no per-use format. Obsidian help (Templates) documents the colon-plus-Moment-tokens syntax with that exact example. Pushed back with the citation.
- 2026-09-05 (panel r1, synthesis) **Daily stays origin-exempt-free because F2 fixes it.** The synthesis found the 86 empty-origin daily rows are `origin: human` in the files, which is F2's exact target. Folded in; the Non-Goal that called it "a separate decision" was wrong and is rewritten.
- 2026-09-05 (panel r1, synthesis) **`Digest`/`Review` stay domain-exempt.** The synthesis proposed `[Entity, Daily]` only. Rejected: the 31 digests without a domain are every digest since 2026-07-10, current intel output omits it, and cortex.yml `path-exempt: notes/ai/**` already exempts them for lint. Doctor and lint must not disagree on the same notes. Pushed back with the date breakdown.
- 2026-09-05 (panel r1 + r2, synthesis) **`templates` goes into BOTH ignore lists; the author's r1 pushback was wrong.** The r1 disposition said cortex does not re-stamp `daily.md` because cortex.yml excludes `system/**` and the stamps' mtime is 2026-08-15. Both facts were true and the conclusion was wrong: `vault.exclude` filters only `lintable_notes` in `lib.rs`; the daemon's quality arm (`daemon.rs:780`) writes over the unfiltered scan, idempotently, so mtime never moves. The exclude landed in dotfiles `490a54f` on 2026-05-21 and the stamps were written 2026-08-15, three months later, with the exclude in force. Retracted. `ScanConfig::default()` gets `templates` for oracle's index (Phase 7); cortex.yml `vault.ignore` gets `templates` for the daemon (Phase 14); Phase 15's criterion re-checks after two ticks.
- 2026-09-05 (panel r1, synthesis) **`borg.yml` does name the fork and the guard** (`borg.yml:63-75`). The synthesis said it does not. Pushed back with the line range.
- 2026-09-05 (panel r2, both seats) **F5 derives from cortex's policy engine; no enum exempt lists.** The r1 fold hardcoded `domain_exempt`/`origin_exempt` in `vault::schema`, a third copy of a rule cortex.yml owns and `path_exempts_field` already had to de-duplicate once. Doctor reports cortex lint's own counts (the r2 wording said "calls `lint_frontmatter`"; superseded by the r3 entry below, which routes through `cortex::lint` so the exclude/include filter applies); the entity exemption is one cortex.yml line (Phase 14). Criterion compares doctor to `sb cortex lint` output and requires the config edit to move both with no rebuild. Folded in.
- 2026-09-05 (panel r2, both seats) **Guard sits before `create_dir_all`; runbook uses `mv -T` with a destination preflight.** Folded in.
- 2026-09-05 (panel r2, both seats) **`WorkerGuard` is owned by `main`, `lossy(false)` (superseded by the r3 entry below: stays lossy, drops counted), serve smoke is `sb oracle serve </dev/null`.** `oracle::serve` cannot hold a guard created in `logger::init_for`; `sb oracle call` never touches the tracing writer. Folded in.
- 2026-09-05 (panel r2, staff) **Phase 4 count is 10 calls with `(`; 11 without** (the `session.rs:28` comment). Criterion tightened. Folded in.
- 2026-09-05 (panel r2, staff) **Phase 15 lists every path by name; 88 files, 89 violations.** Folded in.
- 2026-09-05 (panel r2, staff) **`classify.rs:60` degrades silently on the guard error**; a `log::warn!` is added in Phase 9. Doctor maps the typed guard error to Error, other open failures stay Warn. Folded in.
- 2026-09-05 (panel r2, architect) **Basename resolver can false-PASS on a duplicate basename within one crate.** Accepted as a known limit: `find <crate>` scopes it per crate, the reverse check still catches undocumented files, and the lint's job is stale-name detection, not identity. Recorded, not fixed.
- 2026-09-05 (panel r3, staff) **Doctor enters through `cortex::lint`, not `scan_vault` + `lint_frontmatter`.** The r2 fold would have linted the ignore-only set (3,509 files) where the CLI lints the exclude/include-filtered set (3,429). `cortex::lint` with `rule=["frontmatter"]` is the CLI's own path; the only addition is `Report::count_by_rule_prefix`. `lint_frontmatter` was already `pub`; the doc's "was pub(crate)" was wrong. Folded in.
- 2026-09-05 (panel r3, both seats) **`non_blocking` stays lossy; drops are counted and logged.** `lossy(false)` would let a stalled disk backpressure MCP request handling through the log channel. Default lossy plus `ErrorCounter::dropped_lines()` in the shutdown line keeps the request path free and the loss visible. `WorkerGuard::drop` flush is bounded (100 ms + 1 s) and the doc says so. Folded in.
- 2026-09-05 (panel r3, staff) **Stale text fixed:** Rollout order now places S1/S3 before R1; the Risks row no longer names `sb oracle call` as the smoke; the index-row observation is 2 + 11, not 4. Folded in.
- 2026-09-05 (panel r1, staff) **Side findings dispositioned as X1-X5** in the Goals table with their coupling, so they are requested-scope decisions Scott can strike, not silent inclusions.

## Alternatives Considered

### Alternative 1: Derive the bootstrap PATTERNS table with `include_dir!`
- **Description:** replace the hand list with a directory macro.
- **Why not chosen:** already rejected in code (`bootstrap.rs:45-48`: an explicit list makes adding a pattern a reviewable change) and already guarded by a test. Nothing to fix.

### Alternative 2: Auto-migrate the oracle DB at `SearchIndex::open`
- **Description:** if the legacy path exists and the new one does not, move at first open.
- **Cons:** cortex daemon and `sb oracle serve` can open concurrently with no lock; `otto deploy` runs bootstrap with daemons live. A lost race creates an empty DB and a full re-embed.
- **Why not chosen:** a fail-closed guard at the same chokepoint (refuse to open, never move) gives the safety without the race; the one-time move stays an atomic directory rename by the operator.

### Alternative 7: `tracing_appender::rolling::RollingFileAppender`
- **Description:** use tracing-appender's own rotator for `oracle serve`.
- **Cons:** rolls by time (hourly/daily/never), not by size; a second rotation policy beside `vault::logging`'s 50 MiB x 5.
- **Why not chosen:** siblings behave identically. `FileRotate` stays the one rotator; only `non_blocking` is taken from tracing-appender.

### Alternative 3: Install Templater
- **Description:** add the community plugin, restore `workweek.js`, keep the templates as written.
- **Cons:** plugin dependency for two substitutions core already does; settings live in git-ignored `data.json`; one arithmetic field.
- **Why not chosen:** core syntax removes the dependency; the arithmetic becomes a number.

### Alternative 4: Strip the 104 transcripts with `bin/strip-transcripts`
- **Description:** run the existing one-shot tool.
- **Why not chosen:** see Resolved Decisions (S7). It would touch exactly one note and destroy its only transcript.

### Alternative 5: Generate `frontmatter.md` too
- **Description:** render all five schema docs.
- **Why not chosen:** the field tables (universal, source, creator, daily, book, meeting) exist only in that doc; generating them means inventing a field registry that has no consumer. Parked with a revisit condition: if a field registry lands for another reason, render this file from it.

### Alternative 6: Make the doctor thresholds configurable
- **Description:** a `doctor:` section in `cli.yml`.
- **Why not chosen:** doctor has no config today and its two existing thresholds are consts. Config drives behavior when behavior varies per host; a stale-inbox threshold does not.

## Technical Considerations

### Dependencies
- One new direct dependency: `tracing-appender` in `sb` (Phase 6, `non_blocking` only; pulls `crossbeam-channel` into `Cargo.lock`). `walkdir` is already a vault dep; `file-rotate` already in vault; `rkvr` is in borg; `glob` is already in cortex.
- `sb/Cargo.toml` already depends on `borg` (for `rkvr`) and `vault`.

### Performance
- `dir_size` over `~/.local/share/sb/borg/stages` walks 23,963 trace dirs (234 MB). Measured cost is a doctor concern only; if it exceeds a second, cap depth or cache. Record the measurement in implementation notes.
- Phase 7's `.claude` ignore shrinks the index by 2 rows.

### Security
- `--prune-legacy-config` deletes under `~/.config`. Fail closed on any unknown file; recoverable delete via rkvr; dry-run default.
- R1 moves the only copy of the embeddings. WAL and SHM move with it; the `pgrep` gate blocks the move while any reader is open.

### Testing Strategy
- Every code phase carries unit tests named in its criteria, and at least one break-the-test check recorded in implementation notes (Phases 2, 11).
- `otto ci` after every phase; Phase 13 adds the same gate in GitHub Actions.
- Host-state criteria are run by hand in the runbook with the verify column.

### Rollout Plan
- Phases 0-13 land on `main` one commit each; `bump && otto deploy` after Phase 0, after Phase 8, and after Phase 13. Phase 9 is built and merged, then the R1 runbook step moves the DB with cortex stopped, then Phase 9's binary is deployed (the runbook row says exactly this). If the order slips and Phase 9 is deployed first, its guard makes the daemon fail loudly rather than corrupt anything. Phase 14 is a dotfiles commit; Phase 15 a vault commit by Scott.
- Ship order forced: R7 -> R3 -> F10; S5 -> F10; F1 -> `git rm inbox/.md`; F4 (dotfiles) -> F2 apply; S1 and S3 -> R1 move (the directory rename must carry only the four live files) -> Phase 9 deploy; Phase 12 code -> F3 render; Phase 14 -> Phase 15.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Phase 9 binary deployed before the R1 move | Med | Low (was High) | `SearchIndex::open` refuses to create the new DB while the legacy one exists; cortex fails loudly on restart; the move clears it. Criterion "indexed count equals the recorded pre-move count" |
| `--prune-legacy-config` deletes something unmigrated | Low | Med | Fail closed on strangers; rkvr recoverable delete; dry-run default |
| `oracle serve` log drops lines under a burst (lossy channel) or at shutdown (bounded flush) | Low | Low | Drop count logged at shutdown; `WorkerGuard` owned by `main` and dropped before `process::exit`; rotation test writes past the limit; smoke `sb oracle serve </dev/null` asserts the shutdown line |
| Extensionless `[[domains]]` vs `[[domains.base]]` resolution differs in Obsidian | Med | Low | Runbook F8 verifies all six links in the app |
| `cargo test` in CI needs network for candle | Low | Med | Default path is offline by inspection; first run proves it |
| F2 apply rewrites 87 journal files: Syncthing churn on other devices | Low | Low | One-time; commit immediately |
| Deleting `cortex.yml` `schema:` changes lint from 2016 Errors to 89 real ones, exposing F2 | Certain | Positive | F2 is the next step in the same phase pair |

## Open Questions

None. Four panel rounds (architect + staff engineer, 2026-09-05, run dir `/tmp/review-panel/ZwHkq29D`): every finding is folded or pushed back with evidence in Resolved Decisions; the final round returned no blockers from either seat. The one decision that is Scott's, not the panel's: whether to keep rows X1-X5 (verification side findings) in scope. They are in the doc as included; strike any row to remove it.

## References

- Discovery brief and research brief: session scratchpad `discovery-brief.md`, `research-brief.md` (2026-09-05).
- `docs/design/2026-06-09-codebase-review-remediation.md` (the precedent remediation doc).
- `docs/design/2026-08-30-video-distill-token-budget.md` (R7's parent design).
- `docs/design/2026-05-20-shakedown-v0.8.5-cleanup.md:144` and `2026-05-24-install-pipeline.md:72` (prior mentions of `--prune-legacy-config`).
- `docs/design/2026-07-07-distillation-output-restore.md` (transcript-in-staging contract; S7).
- `docs/design/2026-07-05-cortex-daemon-oscillation-loop.md` (the day `cortex.service.bak` was made).

### Found by the new CI workflow on its first runs, 2026-09-06

R4 existed to make CI check what `otto ci` checks. Its first three runs each
failed on a different pre-existing portability defect, none of them caused by
this work. That is the workflow doing its job on day one.

- **The repo's rustfmt config lived in `$HOME`.** No `rustfmt.toml` existed at
  the repo root, so rustfmt walked up to `~/.rustfmt.toml` (`max_width = 120`).
  `cargo fmt --all --check` therefore passed only on a machine carrying that
  file; CI checks out to `/__w/...`, fell back to the 100-column default, and
  demanded rewraps in untouched files. Fixed: `rustfmt.toml` committed at the
  root with the two values that home config set.
- **Eleven cortex tests read the developer's real `~/.config/sb/`.**
  `startup::*`, `daemon::*` and `sweep::*` fail in a clean container with
  `missing canonical-tags vocabulary at /github/home/.config/sb/canonical-tags.yml`,
  and the first failure poisons the suite's shared env lock, cascading into the
  other ten. Worked around in CI by provisioning `canonical-tags.yml` and
  `tag-mapping.yml` from the repo's `config/` before the test step. **Not
  fixed:** those tests should use a tempdir and `XDG_CONFIG_HOME` like
  `cortex/src/sweep/tests.rs:160-190` already does, rather than depending on
  machine state. That is a follow-up, not part of this doc.
- **The CI toolchain pin was stale** relative to the box the repo is developed
  on (1.96.0 vs 1.98.0). Harmless for `release.yml`, which only builds, but this
  workflow runs `clippy -D warnings`, and lint sets move between releases.
  Fixed: `RUST_VERSION: 1.98.0` in `ci.yml` only.

### Verified complete, 2026-09-06, desk

Runbook run and acceptance criteria measured after `v0.14.11` deployed:

- `otto ci` green; CI green on `bb043c7`; tag `v0.14.11` on `origin/main`, not orphaned.
- `sb doctor` exit 0. Its only Warn is `oldest inbox note ... is 1503h old`, which is R5 firing on a note awaiting triage: the check working, as the criterion says. Output carries `maxTokens`, four `[data dir]` lines, and `frontmatter gaps (cortex lint policy): domain=1, origin=13, tags=290`, equal to `sb cortex lint`'s row counts for the same three fields by an independent path.
- `sb cortex lint | grep -c 'not valid'` = 0 (was 2016).
- Vault: `origin: human` 0 (was 87); `domain-ai` in `home.md` 0; `inbox/.md` gone; vault `.github/workflows/ci.yml` gone; Templater syntax in `system/templates` 0; `sb cortex schema --check` exit 0.
- Host: legacy `~/.local/share/oracle` gone; `~/.local/share/sb/oracle/oracle.db` present; indexed count 3456, identical to the pre-move reading, so the move lost nothing; systemd cortex files 1; patterns 26 (and still 26 after the deploy's `bootstrap --force`, so the facet orphans did not return); borg unit renders `--log-level info`.
- **Phase 15's re-stamp criterion now passes for real.** After cortex restarted on the new config it parses 3498 notes rather than 3509 -- exactly the eleven templates fewer -- with no `system/templates` lines in `cortex.log` since the restart and no `cortex-` stamps in `daily.md`. The earlier reading was taken against a daemon that had never reloaded its config and proved nothing; this one does.

Not done, and why:

- **S4** (`sb bootstrap --prune-legacy-config --apply`). The verb works and its dry run lists exactly `borg`, `cortex`, `second-brain`, but the apply deletes through `borg::rkvr::remove`, and rkvr cannot create its archive directory under the agent sandbox (`Read-only file system`). It fails closed on the first directory, so nothing was deleted. Needs to be run by hand.
- **S7**. The YouTube source is gone, so the transcript cannot be regenerated. Left in place.
