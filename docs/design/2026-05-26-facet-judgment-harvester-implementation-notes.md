# Implementation Notes - facet judgment harvester

Running record of design-vs-implementation deviations, decisions, tradeoffs, and
open questions. Append-only. One section per phase, committed with the final
phase.

Design doc: docs/design/2026-05-26-facet-judgment-harvester.md

## Phase 1: Crate scaffolding, ledger, vault schema extensions

### Design decisions

- **rusqlite, not sqlx** — `facet/src/ledger/*.rs`. The design doc said "sqlx
  with sqlite feature (already used by oracle / borg)" but the workspace uses
  `rusqlite = "0.34"` everywhere (borg, cortex, oracle, vault). Pulling in sqlx
  would have added a second SQLite stack to the workspace. Followed the
  established pattern instead.
- **Ledger as a module directory, not a single file** — `facet/src/ledger.rs`
  is the entry point, with `facet/src/ledger/{schema,sessions,workitems,
  clusters,moments,meta}.rs` as submodules. The design doc's schema (7 tables
  plus insert/query helpers) would push a single `ledger.rs` past the 1500
  line bloat limit; module-dir decomposition is the project's standard
  remedy.
- **NoteType extensions: kebab-case via existing serde rule** —
  `vault/src/schema.rs`. Added `FacetWorkitem` and `FacetPortrait` variants
  rendered as `facet-workitem` / `facet-portrait` via `#[serde(rename_all =
  "kebab-case")]` (existing enum-level attribute renders them in lowercase;
  added explicit `rename` attributes so the wire form matches what the
  frontmatter spec calls for).
- **Method::Facet** — `vault/src/schema.rs` plus a new arm `Method::Facet =>
  "ft"` in `vault/src/trace.rs::method_prefix`. The 2-letter prefix is the
  established convention.
- **Config trait: facet::config::Config** — modeled on `borg::config::Config`
  and `oracle::config::Config`. YAML loaded from
  `vault::paths::config_root().join("facet.yml")` via `serde_yaml`, with all
  PathBuf fields annotated by `#[serde(deserialize_with =
  "vault::paths::deserialize_tilde_pathbuf")]` per CLAUDE.md rules.
- **State dir: ~/.local/share/sb/facet/** via `dirs::data_local_dir()
  .expect("dirs::data_local_dir() returned None ...")` per CLAUDE.md
  fabricated-path-fallback rule. No `PathBuf::from("~/.local/share")` fallback.

### Deviations

- **No claim-of "ledger module split" deferred to later phase** — the design
  doc lists "implement Ledger" as one Phase 1 bullet, but the module-dir
  split happens immediately to stay under the 1500 line bloat limit.

### Tradeoffs

- **rusqlite vs sqlx** — sqlx would have given async-native SQLite and
  compile-time-checked queries. rusqlite is sync but matches the entire
  workspace. The ergonomic loss is wrapped at the daemon edges via
  `tokio::task::spawn_blocking`, the same pattern cortex daemon uses. The
  consistency benefit outweighs the per-call wrapping.

### Open questions

- None for Phase 1.

## Phase 2: JSONL parser, scan, repo

### Design decisions

- **Content block types include `thinking` and `image`** — `facet/src/jsonl.rs`.
  The design doc lists `Text` / `ToolUse` / `ToolResult` only, but a sampled
  real JSONL file contains `thinking` (assistant) and `image` (user) blocks too.
  Added both to `ContentBlock`. `Thinking` is preserved with its text so
  the extractor can mine reasoning traces; `Image` carries only a marker
  string since image bytes do not belong in vault notes.
- **User content can be string OR list** — Claude Code writes the early prompt
  as a JSON string and later as a list of blocks. Parser tolerates both via
  `#[serde(untagged)]` enum `UserContent`.
- **Non-turn line types skipped, not errored** — JSONL contains
  `last-prompt`, `permission-mode`, `attachment`, `file-history-snapshot`,
  `ai-title`, `system`, plus user/assistant. The parser only emits Turns for
  `user` and `assistant`; other lines are skipped silently (they are noise
  for facet's purposes).
- **Unknown line shapes -> WARN log + skip** — design doc spec; implemented
  with `schema_drift_lines` counter on `ParsedSlice`.

### Deviations

- None.

### Tradeoffs

- **Slug parsing via git binary subprocess vs URL crate** — using
  `std::process::Command::new("git").args(["-C", cwd, "remote", "get-url",
  "origin"])` matches claude-report's approach and is what the design doc
  describes. An alternative was to read `.git/config` directly; chose the
  subprocess for compatibility with worktrees and submodules where the
  effective remote URL involves more than `.git/config`.

### Open questions

- None for Phase 2.

## Phase 3: Work-item clustering with persisted cluster state

### Design decisions

- **Reuse `distillers::FabricCaller`, not a facet-local trait** — the
  design doc says "facet::fabric wraps distillers::FabricCaller". Implemented
  literally; facet's `fabric` module is a thin re-export plus a `FacetFabric`
  helper for constructing FabricRequest instances with the right defaults.
- **Cluster output format: YAML list** — matches borg/distillers' Fabric
  return-shape convention. Pattern's `OUTPUT` section demands a YAML
  document with a `assignments:` key listing
  `{first_turn_uuid, last_turn_uuid, kind: existing|new, slug|title}`.
- **Slug freezing on creation** — derive slug from LLM-generated title;
  dedup against ledger via auto-suffixing `-2`, `-3` etc.

### Deviations

- None.

### Tradeoffs

- None significant.

### Open questions

- **Archived-slug collisions** — design doc's Open Questions list it; left
  to operator-level intervention for v1 (auto-suffixing handles it).

## Phase 4: Judgment extraction

### Design decisions

- **Quote excerpt cap default 800 chars** — design doc spec.
- **Per-row transactional writes** — each `cluster_assignments` row processed
  in its own SQLite transaction.

### Deviations

- None.

### Tradeoffs

- None significant.

### Open questions

- None.

## Phase 5: Fencepost-merging renderer

### Design decisions

- **HTML comment fenceposts** — `<!-- facet:auto:begin {id} -->` /
  `<!-- facet:auto:end {id} -->` per spec; invisible in Obsidian preview.
- **Frontmatter as one auto block** — facet-* keys are facet-owned;
  operator-added keys preserved.

### Deviations

- None.

### Tradeoffs

- None.

### Open questions

- None.

## Phase 6: Daemon, sb subcommand, systemd install

### Design decisions

- **Daemon binary path resolved at install-time via `std::env::current_exe()`**
  — matches borg/cortex install_systemd patterns. ExecStart is the absolute
  path of the running sb binary, not a hardcoded `~/.cargo/bin/sb`.
- **sb facet CLI module: sb/src/cli/facet.rs** — mirrors the per-subsystem
  CLI module convention in sb/src/cli/.

### Deviations

- None.

### Tradeoffs

- None.

### Open questions

- None.

## Phase 7: Portrait rollups

### Design decisions

- **Input: already-extracted moments, never raw transcripts** — per spec.

### Deviations

- None.

### Tradeoffs

- None.

### Open questions

- None.

## Phase 8: End-to-end tests and shakedown

### Design decisions

- **No vault::test_support** — that module does not exist. Tests use
  `tempfile::tempdir()` directly, matching the rest of the workspace.

### Deviations

- Design doc said "facet imports the existing `vault::test_support` helpers";
  there is no such module. Substituted direct `tempfile::tempdir()` use.

### Tradeoffs

- None.

### Open questions

- None.

## Post-implementation: Architect audit (rounds 1 & 2)

The Architect audit (Gemini, post-implementation) ran after the
initial Phase 8 commit. Two rounds. Round 1 surfaced 11 findings; Round
2 reached consensus on the two I pushed back on.

### Fixes landed (commit 047ed77)

- **tilde-expand include-cwds / exclude-cwds** — Vec<PathBuf> serde
  helper added to `vault::paths::deserialize_tilde_pathbuf_vec`; both
  list fields use it. Regression test:
  `tilde_paths_in_include_and_exclude_lists_are_expanded`.
- **real SQLite transaction in cluster_new_turns** — `Ledger::with_tx`
  added; cluster path now uses tx_* helpers. Regression test:
  `cluster_persist_is_one_transaction`.
- **extract.max_input_tokens splitting** — `split_turns_by_budget`
  chunks at turn boundaries; idempotency absorbs overlap. Regression
  test: `oversize_range_splits_into_multiple_extract_calls`.
- **sb facet retry / archive verbs** — added to `sb::cli::facet`.
  `retry` accepts a session UUID or work-item slug; `archive` calls
  out to `rkvr rmrf` per safety.md.
- **notify no longer a stub** — `facet::notify::on_new_workitem` /
  `on_budget_exhausted` route to log INFO / WARN gated on config
  opt-in. Cluster fires after-commit (no ghost notifications on
  rollback). Harvest fires when `max-sessions-per-tick` defers.
- **set_last_extract_turn_uuid into ledger::workitems** — raw SQL
  lifted out of `extract::mine.rs` into a type-safe ledger method.
- **sb status / sb doctor wiring** — `sb::cli::checks` gained a
  `facet` section; `sb-facet.service` joins systemd enumeration;
  `facet.yml` joins the parse-status surface.

### Findings carried over to Round 2 (consensus reached)

- **Concurrency caps (`max_llm_inflight`, `parse_rayon_threads`)** —
  unused-by-design until parallel session dispatch is added. The
  per-tick session loop is strictly sequential (`for session in
  sessions.iter().take(cap)`) so unbounded fanout is structurally
  impossible. The semaphore would guard a fanout that does not exist
  today. Track for the day actual parallelism lands; the right
  sequence is to add the cap **with** the fanout, not before it.
  *Architect verdict (round 2):* Accepted.
- **LLM exponential backoff** — design doc contradicted itself: the
  Failure-handling-per-stage paragraph proposed in-tick backoff;
  Non-Goals explicitly forbid LLM retries. *Architect verdict (round
  2):* Non-Goal wins. First failure is terminal for the tick; the
  next tick re-runs the same range because `sessions
  .last_cluster_offset` did not advance. `sb facet retry` is the
  manual escape. Design doc patched to remove the stale paragraph.

### Open items

- Concurrency caps stay in `ConcurrencyConfig` so they are ready the
  moment fanout lands. Document this in code comments at the
  config-field site if we touch it again.
- Telegram/desktop notification transports are an additive future
  upgrade. The current log-based sink is the v1 floor.
