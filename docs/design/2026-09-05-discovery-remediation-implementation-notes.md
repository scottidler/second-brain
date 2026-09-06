# Implementation Notes: Discovery Remediation

Design doc: `docs/design/2026-09-05-discovery-remediation.md`

## Phase 0: Ship the in-flight --maxTokens work (R7)

### Design decisions
- Committed the design doc first, standalone, before touching any tracked
  file — `docs(design): discovery remediation` (`3c6de89`) — per the doc's
  explicit ordering instruction.
- Folded two clippy fixes into the Phase 0 commit rather than a separate one:
  `chunks_exact` -> `as_chunks::<4>()` in `vault/src/search/vector.rs:162,325,388`,
  and `#[allow(clippy::result_unit_err)]` on `borg::notify::Telegram::processing`
  and `borg::notify::Signal::processing` (`borg/src/notify.rs:114,432`). Both
  lints are newly enforced by the local `rustc`/`clippy` 1.98.0 toolchain
  (`.github/workflows/release.yml` pins CI to 1.96.0) and were failing `otto
  ci` on files untouched by the 9-file `--maxTokens` diff. Fixing them at the
  correct minimal seam (mechanical rewrite; `#[allow]` with a rationale
  comment rather than redesigning the public `Result<(), ()>` API) was
  required to get `otto ci` green at all, so it rode in this commit instead of
  blocking the phase.
- Appended the exact paragraph specified in the doc's Phase 0 bullet to
  `docs/design/2026-08-30-video-distill-token-budget.md` Resolved Decisions,
  verbatim, and touched nothing else in that file.

### Deviations
- **Ordered deferral, not a spec gap:** `bump && otto deploy` was explicitly
  withheld per the team lead's instruction. Those are held for a finalization
  checkpoint after all 16 phases land, not run per-phase. Consequently the
  third success criterion (`sb doctor | grep -c 'maxTokens'` >= 1 on the
  *deployed* binary) is DEFERRED-TO-DEPLOY: it cannot be true until that
  finalization deploy happens.
- Two files outside the doc's named 9 (`borg/src/notify.rs`,
  `vault/src/search/vector.rs`) are in this commit. Both changes are
  toolchain-drift clippy fixes, not behavior changes, and were necessary for
  `otto ci` to pass under the locally installed 1.98.0 toolchain regardless
  of this phase's diff (verified: the two lints fire on baseline `f97718f`
  code untouched by the 9-file diff). No other phase in this doc claims these
  two files.

### Tradeoffs
- `#[allow(clippy::result_unit_err)]` vs. introducing a real error enum for
  `notify::Telegram`/`notify::Signal::processing`: chose the allow. A typed
  error would ripple into every caller of both `processing` fns across
  `borg/src/pipeline*`, which is out of Phase 0's scope and belongs to
  whichever phase (if any) later touches notify's error contract.

### Open questions
- None.

## Phase 1: Delete the passthrough stub (S5)

### Design decisions
- `git rm distillers/src/passthrough.rs distillers/src/passthrough/tests.rs`
  and removed both the `pub mod passthrough;` and
  `pub use passthrough::PassthroughDistiller;` lines from
  `distillers/src/lib.rs:17,37`.
- Rewrote the dead-code comment at `distillers/src/dispatcher.rs:169-173`
  (the `PassthroughDistiller` mention above the `VoiceNote` match arm) to
  drop the now-nonexistent-type reference, keeping only the still-true
  routing note about `VoiceNote`'s own Fabric-backed distiller.
- Reworded the matching comment at
  `borg/src/stages/distill/tests.rs:136-138` (above
  `distill_stage_handles_image_through_image_distiller`) the same way: it
  named `PassthroughDistiller` as what Image *used to* route through; the
  comment now just states the current routing and fallback behavior.
- Retagged
  `config/eval/distill-fixtures/idea/linker-edge-from-capture-note/distilled.yml:12`
  `meta.extractor` from `distill-passthrough-v1` to `distill-idea-v2` since
  the fixture's `IdeaDistiller` is the live extractor for that path.
- Left `borg::stages::extract::PassthroughExtractor`
  (`borg/src/stages/extract.rs:25`) untouched — a distinct, live Stage-1
  extractor, out of this phase's scope per the doc's explicit instruction.

### Deviations
- None.

### Tradeoffs
- None — this phase is a pure deletion with no design choice beyond what
  the doc specified.

### Open questions
- None.

## Phase 2: AGENTS.md module-map lint (R3)

### Design decisions
- `bin/agents-map` (bash, executable, same `FAIL:`/loop/`exit 1` shape as the
  `bloat` task): forward check extracts backticked ``` `token.rs` ``` tokens
  from every `AGENTS.md` (`grep -oE '`[A-Za-z0-9_./-]+\.rs`'`, trailing
  backtick required so refs like `` `borg/src/service.rs:240` `` — which end
  in a line number before the closing backtick — never match); each token
  resolves by basename via `find <crate> -name <basename> -not -path
  '*/target/*'`, where `<crate>` is the AGENTS.md path's first path
  component. Basename (not path-prefix) resolution is load-bearing: nested
  AGENTS.md files name parent-dir modules (`borg/src/pipeline/AGENTS.md`
  names `pipeline.rs`, which actually lives at `borg/src/pipeline.rs`, a
  sibling of the `pipeline/` dir, not inside it) and `sb/AGENTS.md` names
  `cli/*.rs` files by bare name.
- Reverse check runs only when `agents_md` path equals `<crate>/AGENTS.md`
  exactly (crate-root, not nested): for each `<crate>/src/*.rs` at
  `-maxdepth 1`, excluding `lib.rs`/`main.rs`/`tests.rs`/`testutil.rs`, a
  plain `grep -qF <basename>` against the AGENTS.md file must hit; `FAIL:
  <agents.md>: <file> undocumented` otherwise. Plain substring match (not
  backtick-anchored) per the doc's literal wording ("must appear ...
  somewhere in that file"); verified no false-positive substring collisions
  across the current six crate-root files.
- `.otto.yml`: new `agents-map` task (`bin/agents-map`, one line), wired into
  `ci: before` as `[lint, bloat, agents-map, check, test]` (after `bloat`,
  before `check`, per the doc).

### Deviations
- **Expected drift, not a defect, from the doc's `f97718f` measurement.**
  Phase 1 (`4eaf9d5`) deleted `distillers/src/passthrough.rs` after the doc's
  numbers were taken, so the forward check now reports **two** misses instead
  of the doc's one: `cortex/AGENTS.md: hygiene.rs not found` (the doc's
  original catch) and `distillers/AGENTS.md: passthrough.rs not found` (new,
  caused by Phase 1). Phase 3 already plans to remove that row, so no action
  taken here. The reverse check independently found exactly 20 undocumented
  files, matching the doc's count precisely (9 in `borg/AGENTS.md`, 6 in
  `cortex/AGENTS.md`, 2 in `distillers/AGENTS.md`, 1 in `oracle/AGENTS.md`, 2
  in `vault/AGENTS.md`) — see the exact FAIL lines in the phase report.
- Same effect, correct seam: the doc's Phase 2 bullet writes the reverse
  check as "appear by basename somewhere in that file" without specifying a
  match mechanism; implemented as a literal substring `grep -F`, which is the
  simplest faithful reading and matches every basename actually present with
  no observed false hit or false miss against the current tree.

### Tradeoffs
- Substring `grep -F` vs. requiring backtick-wrapped tokens for the reverse
  check: chose substring, per the doc's literal "appear ... somewhere" wording
  (not "appear backticked"). A backtick-anchored variant would be stricter
  but is not what Phase 2 specified; Phase 3 is free to backtick every row it
  adds regardless of which reading is enforced.

### Open questions
- None: the design doc's own success criteria for this phase are "exit 1
  here, exit 0 after Phase 3", which is exactly what was observed.

## Phase 3: Repo doc drift (F10)

### Design decisions
- Root `CLAUDE.md:16` distillers list corrected to `(article, repo, video,
  thread, image, voicenote, session, idea)`, matching the eight files actually
  present in `distillers/src/` post-Phase-1.
- Root `CLAUDE.md:25` corrected to `borg::service::install_systemd`
  (`borg/src/service.rs:240`), plus the pure renderer `render_systemd_unit`
  at `:186` — verified both line numbers against source.
- Root `CLAUDE.md:52` and `vault/AGENTS.md:17,23` Distilled contract corrected
  to `{ summary, tldr, slug, enumeration, key_ideas, claims, tags, links,
  kind_specific, meta, transcript }`, matching `vault::distilled::Distilled`'s
  actual field order in `vault/src/distilled.rs`.
- Root `CLAUDE.md:55` L2 patterns line rewritten to the doc's prescribed
  prose ("chunk/reduce triples for article, video, thread, session,
  voicenote; `distill-repo`, `distill-image`; nine support patterns; 26
  files") — verified by listing `borg/patterns/` (26 files: 5 kinds x 3
  chunk/main/reduce = 15, + `distill-repo` + `distill-image` = 17, + 9
  support patterns = 26).
- `borg/AGENTS.md` Entry Points: added `GET /trace/{trace_id}`
  (`routes.rs:211`, `trace_state`) and named the auth gate
  (`routes::require_auth`, `routes.rs:49`) as a `route_layer` wired in
  `build_router` (`lib.rs:104-127`) over `/ingest`, `/ingest/file`, `/note`,
  and `/trace/{trace_id}`; `/health`/`/health/audit` stay open — verified
  against the actual router-assembly code, not just the doc's claim.
- Module-map rows added/removed across five AGENTS.md files, one line each,
  written from each module's own doc comment (read every file's header
  before writing its purpose, per instruction — none guessed from filename
  alone):
  - `vault/AGENTS.md`: added `text.rs`, `tombstone.rs`.
  - `borg/AGENTS.md`: added `backoff.rs`, `byline.rs`, `dedupe.rs`,
    `dispatch.rs`, `eval.rs` (+`eval/`), `harvest.rs` (+`harvest/`),
    `readability.rs`, `service.rs`, `thread.rs`.
  - `cortex/AGENTS.md`: removed `hygiene.rs` (file does not exist in
    `cortex/src/`; the doc's own S5-adjacent stale-row target); added
    `association.rs`, `bridge.rs`, `entities.rs`, `graph.rs`, `hub.rs`
    (+`hub/render.rs`, `hub/asymmetry.rs`), `memgraph.rs` under a new
    "Knowledge graph (graph-augmented-memory)" grouping line (the six
    modules share one design lineage, so grouped rather than scattered
    across the existing groups).
  - `oracle/AGENTS.md`: added `eval.rs` (+`eval/`).
  - `distillers/AGENTS.md`: removed `passthrough.rs` (deleted by Phase 1);
    added `session.rs`, `parse.rs`.
  - `sb/AGENTS.md`: no change, per the doc ("none missing").

### Deviations
- None from the doc's Phase 3 bullets. The doc's own note that `f97718f`
  line numbers "confirmed" the routes.rs `:211`/`:49` numbers were re-checked
  against current `HEAD` (98e9543) content, not assumed stale.

### Tradeoffs
- Grouped the six new cortex knowledge-graph modules under one new bullet
  rather than folding them individually into the existing
  Classification/Quality/Lifecycle groups: they share a single design
  lineage (`graph-augmented-memory`, `entity-hub-two-vector-synthesis`,
  `MemGraphRAG`) that none of the existing group labels name, and a future
  reader scanning the module map benefits more from that grouping than from
  matching the shortest possible diff.
- `hub/render.rs` and `hub/asymmetry.rs` documented inline within the
  `hub.rs` row (`hub.rs (+hub/render.rs, hub/asymmetry.rs)`) rather than as
  separate top-level rows: the Phase 2 reverse check only requires top-level
  `<crate>/src/*.rs` files to be documented (nested files are out of its
  scope per the doc), so this satisfies the lint while still surfacing both
  files for a human reader, matching the doc's own bracketed instruction.

### Open questions
- None.
