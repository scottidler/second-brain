# Design Document: Artifact Generation Layer (`sb cortex render`)

**Author:** Scott A. Idler
**Date:** 2026-06-08
**Status:** In Review
**Review Passes Completed:** 5/5 + Architect review (round 1, all findings folded in) + Staff Engineer review (round 1, all verified findings folded in)

## Summary

Add a generation/synthesis layer to second-brain that turns knowledge already in
the vault into shareable artifacts: **slide decks** (Marp), **architecture
diagrams** (mermaid / D2), and **AI image prompts** (plus an optional, offloaded
pixel backend). It is exposed as a new cortex verb, `sb cortex render`, and reuses
the existing Fabric-pattern → renderer → vault-attachment machinery. This closes
the one capability gap where NotebookLM's "Studio" beats our stack, while keeping
the corpus, ownership, and retrieval entirely on our side.

## Problem Statement

### Background

second-brain is a knowledge *substrate*: `borg` ingests, `cortex` organizes,
`oracle` queries. Evaluated against Google NotebookLM, our substrate wins on
ingestion, governance, data ownership, and controllable retrieval. NotebookLM's
only decisive advantage is its **Studio** output layer - one-click generation of
audio/video overviews, slide decks, infographics, and reports *from* a source
set - plus agentic Deep Research.

We do not want to adopt NotebookLM as the system of record (static source
snapshots, cloud lock-in, ToS-bound automation). We *do* want the cheap, high-ROI
slice of its generation surface, built natively so the outputs land back in the
vault and become first-class, oracle-indexed notes.

### Problem

There is no way to take a note (or a small set of notes) and produce a
presentation deck, an architecture diagram, or an image from it without leaving
the vault and doing it by hand. The knowledge is captured and organized but
cannot be *re-expressed* in the formats people actually share.

### Goals

- Generate a **Marp slide deck** (PPTX/PDF/HTML/PNG) from a note's content.
- Generate an **architecture diagram** (sequence, flow, etc.) as mermaid (inlined,
  Obsidian-native) or D2 (rendered SVG attachment).
- Generate a **high-quality image prompt** from a note concept, with an optional,
  config-gated backend that turns the prompt into an image via an offloaded
  (non-GPU-on-`desk`) renderer.
- Land every artifact in the vault under the existing `system/attachments/`
  convention, embedded into the source note and recorded in frontmatter.
- Persist the *source* of each artifact (the Marp markdown, the D2/mermaid source,
  the image prompt) for reproducibility and re-render.
- Reuse existing primitives: `vault::fabric::run_pattern`, the asset-storage
  helper, the cortex verb/module pattern, and `sb doctor` external-tool gating.

### Non-Goals

- **Audio / video overviews.** TTS + video compositing + GPU; NotebookLM's genuine
  moat. Handed off to NotebookLM via the installed `nlm` CLI / MCP, not built.
- **Agentic Deep Research.** A separate capability, out of scope here.
- **Local GPU image diffusion on `desk.lan`.** `desk` has no GPU; pixel generation
  is always offloaded (hosted open-weights API) or skipped.
- **A new top-level `sb` namespace.** This rides under `cortex`, beside `intel`
  and `summarize`, which already do LLM-bound generation over existing notes.
- **Replacing the ingestion-time `ImageDistiller`.** That stays; this is a
  distinct, on-demand lifecycle.
- **Origin gating.** `render` is an explicit, user-invoked, single-note action, so
  - unlike vault-wide retroactive pipelines (which filter to `origin: assisted`) -
  it does **not** filter by origin. Rendering an *authored* note (e.g. a design
  doc → deck) is a primary use case, not an edge case.

## Proposed Solution

### Overview

A new cortex module `cortex::render` with one entry per artifact kind. The CLI
verb `sb cortex render --kind <kind> <note>` resolves the note, runs the matching
Fabric pattern to emit *artifact source*, invokes the matching renderer, stores
the output (and the source) as a vault attachment, and rewrites the source note
to embed the artifact and record it in frontmatter.

```
note (vault) ──► fabric pattern (emit source) ──► renderer ──► attachment + embed
   │                  gen-slides.md                  marp           system/attachments/...
   │                  gen-diagram.md                 d2 / inline    + ![[...]] / fenced block
   │                  gen-image-prompt.md            (backend)      + frontmatter renders:
   └── oracle re-indexes the updated note
```

The shape is identical to the Stage-2 distillers (Fabric emits a structured
artifact; the system renders/publishes it), but the lifecycle is **on-demand
against an already-published note**, not ingestion-time against freshly-fetched
content - so it lives in cortex, not the distiller dispatch.

### Architecture

**Crates touched**

- `vault` - gains `vault::assets` (the asset-storage helper, *moved here* from
  `borg::assets` so both borg and cortex can call it). vault stays LLM-free;
  `run_pattern` already lives in `vault::fabric`.
- `cortex` - new `render` module (`render.rs` + `render/{slides,diagram,image}.rs`).
  cortex already depends on `vault` and `distillers`.
- `borg` - `assets.rs` becomes a thin re-export of `vault::assets` (or its call
  sites switch to `vault::assets::store_asset`); the `rkvr::remove` wrapper is
  likewise relocated to `vault::rkvr` and re-exported. No behavioral change.
- `sb` - new `Command::Render(RenderArgs)` variant in the cortex CLI enum; formats
  and prints the typed `RenderReport` (libraries never print).

**Renderers**

| Kind     | Emit pattern          | Renderer                         | Output                          |
|----------|-----------------------|----------------------------------|---------------------------------|
| slides   | `gen-slides.md`       | `marp` (external bin)            | `.pptx` / `.pdf` / `.png` attachment |
| diagram  | `gen-diagram.md`      | mermaid: **none** (inline fenced block); D2: `d2` (external bin) | fenced ```mermaid``` in note, or `.svg` attachment |
| image    | `gen-image-prompt.md` | none (prompt) + optional backend | prompt in note; optional `.png` attachment |

mermaid needs no external binary because Obsidian renders ` ```mermaid ` blocks
natively. D2 renders SVG without a headless browser (Go binary). Marp ships a
standalone binary (no Node) and is headless-friendly - we depend on that binary,
not the npm package, per the self-contained-install convention.

> If a mermaid diagram ever needs to be a rendered *image* (e.g. embedded into a
> Marp deck, or for a non-Obsidian consumer), the user's own `mermaid-rs` CLI is
> the renderer - no new dependency. v1 keeps mermaid inline-only.

**Image pixel backend (pluggable, config-selected)**

The prompt half is always produced (local, CPU, cheap). The pixel half is an
optional backend selected in config - this is the "select the active
methodology" carve-out from `general.md`, mirroring oracle's configurable
retrieval pipeline. v1 backends:

- `none` (default) - emit the prompt only.
- `http` - POST the prompt to a hosted open-weights endpoint (FLUX.2 / SD 3.5 via
  fal/Replicate-style API); credentials via env var, never config.

### Data Model

```rust
// cortex/src/render.rs
pub enum RenderKind { Slides, Diagram, Image }

pub struct RenderRequest {
    pub note: PathBuf,          // resolved, tilde-expanded vault path
    pub kind: RenderKind,
    pub format: Option<String>, // slides: pptx|pdf|png ; diagram: mermaid|d2 ; image: png
}

pub struct RenderedArtifact {
    pub kind: RenderKind,
    pub source_rel: String,     // vault-relative path to saved artifact SOURCE
    pub output_rel: Option<String>, // vault-relative path to rendered OUTPUT (None for inline mermaid)
    pub embed: String,          // markdown to splice into the note body
}

pub struct RenderReport {
    pub note: PathBuf,
    pub artifacts: Vec<RenderedArtifact>,
    pub fabric_meta: Vec<vault::distilled::DistilledMeta>, // extractor id + model + token estimate; CONSTRUCTED BY render (run_pattern returns only stdout, so render synthesizes DistilledMeta the same way distillers do - it is not returned for free)
}
```

**Vault layout** (extends the existing `system/attachments/images/YYYY-MM/`
convention):

```
system/attachments/
  slides/2026-06/<slug>-<hash>.pptx
  slides/2026-06/<slug>-<hash>-deck.md      # source (Marp markdown)
  diagrams/2026-06/<slug>-<hash>.svg        # D2 only
  diagrams/2026-06/<slug>-<hash>.d2         # source
  images/2026-06/<slug>-<hash>.png          # optional backend output
  images/2026-06/<slug>-<hash>-prompt.txt   # source prompt
```

Source filenames carry a single dot (`-deck.md`, `-prompt.txt`), never a double
extension (`.marp.md`, `.prompt.txt`). `store_asset` derives the stem from
everything before the *last* `.` and lowercases only the final extension
(`borg/src/assets.rs`), so a `foo.marp.md` input lands as `foo-marp-<hash>.md`, not
`foo-<hash>.marp.md`. Keeping one dot per source filename preserves the
"`store_asset` behavior unchanged" invariant asserted in Phase 1.

**Frontmatter** on the source note (additive):

```yaml
renders:
  - system/attachments/slides/2026-06/foo-ab12cd34.pptx
  - system/attachments/diagrams/2026-06/foo-ef56ab78.svg
```

`renders:` mirrors the existing `slides:` list semantics so dashboards/oracle can
discover generated artifacts.

Because `renders:` is a YAML **sequence**, the frontmatter update must go through
`vault::frontmatter` - parse the note, set `Frontmatter.extra["renders"]` as a
`serde_yaml::Value::Sequence`, and serialize via `Frontmatter::to_yaml`. It must
**not** use `cortex::scope::insert_frontmatter_fields`, which serializes non-scalar
values with `Debug` formatting (`format!("{other:?}")`) and would corrupt the
sequence. Note this is a whole-frontmatter canonical rewrite (known fields in
canonical order, extras alphabetized), not a surgical line edit - consistent with
`cortex::summarize`, but comments/ordering/duplicate keys are not preserved, so the
update must be intentional: parse the existing `renders` as a sequence of strings,
preserve unrelated entries, remove/dedupe the render-owned stale entry for the
current kind, and write back one canonical sequence; if the existing `renders` is
malformed or duplicated at the YAML-key level, WARN and canonicalize to a single
`renders:` key rather than preserving the ambiguity. The body sentinel update and
the `renders:` update are composed into **one buffer** and committed by the single
temp-write + rename in step 5 of the Durable Ordering sequence (GC stays after the
rename).

**Note-path input:** the positional accepts a vault-relative path
(`notes/ai/foo.md`) or an absolute path; both resolve against the vault root via
`vault::paths`, tilde-expanded at the boundary. `<slug>` in attachment filenames
is the note's filename stem. After resolution the path is canonicalized and **must
live under the vault root**; a path that resolves outside the vault is rejected
with a typed error (render never reads or writes outside the vault). This guard is
render's responsibility - `vault::paths::resolve_vault_root` resolves the vault
*root*, not the note positional, so it does not contain a traversal escape on its
own.

**Note mutation (idempotent via sentinels):** embeds are spliced into a single
managed `## Artifacts` section appended to the note body (created on first
render). Each generated block is wrapped in stable HTML-comment **sentinels**
keyed by kind:

```markdown
## Artifacts

<!-- sb:render kind=slides -->
[foo deck (PPTX)](system/attachments/slides/2026-06/foo-ab12cd34.pptx)
<!-- /sb:render kind=slides -->

<!-- sb:render kind=diagram -->
(mermaid fenced block here)
<!-- /sb:render kind=diagram -->
```

Re-rendering a kind locates the existing block **by its sentinel** (stable per
kind), not by the artifact filename, and replaces the block in place. This is the
load-bearing correction: `store_asset` derives filenames from a SHA-256 of the
*content bytes* (`<stem>-<hash>.<ext>`), and Fabric output is non-deterministic, so
every re-render yields a *new* basename; keying idempotency on the basename would
miss every time and append duplicates. Keying on the sentinel makes the
content-hash irrelevant to replacement (its dedup value is retained) and cleanly
distinguishes Cortex-generated blocks from any mermaid a user hand-adds elsewhere:
Cortex only ever rewrites content *between its own sentinels*, never the rest of
the section.

**Asset GC on replace:** before writing the new block, the old block's artifact
path is parsed out of the sentinel block and the superseded asset(s) (output +
saved source) are removed via `vault::rkvr::remove` (archived, recoverable, per
the rkvr safety rule), falling back to a logged std removal if rkvr is absent.
Note `borg`'s existing rkvr wrapper (`borg/src/rkvr.rs::remove`) is `pub(crate)`
and unreachable from cortex, which has no `borg` dependency; the wrapper is
therefore relocated to `vault::rkvr` (re-exported from `borg`) in Phase 1,
symmetrically with `store_asset`. Without GC, iterative re-render (the expected
workflow: render, read, tweak, re-render) would strew orphaned, content-hashed
binaries across the Syncthing'd vault. The stale `renders:` frontmatter line for
that kind is replaced in the same write.

A render failure leaves the note, the `## Artifacts` section, the `renders:`
frontmatter, and the prior assets untouched (the newly-saved artifact *source* is
still written for inspection).

**Durable ordering (the transaction boundary).** The assertion above is backed by
a fixed step order in which the *irreversible* GC is always the **last** step, so a
crash at any point leaves a recoverable state, never a note that points at a
removed asset. Per render kind:

1. Run the Fabric pattern; `extract_block` the source. (Pure; nothing written.)
2. Write the artifact *source* via `store_asset` (content-hashed, new basename).
3. Render to a renderer-owned temp path. On renderer failure, **stop here**: the
   source is retained for inspection; note + prior assets untouched.
4. Store the rendered *output* via `store_asset`.
5. **Atomically** rewrite the source note (temp-write + `rename`, mirroring
   `cortex::summarize`'s note rewrite), updating both the `## Artifacts` sentinel
   block and the `renders:` frontmatter line in that single atomic rename.
6. **Only after** the note rename succeeds, GC the *superseded* prior asset(s) via
   `vault::rkvr::remove`.

This ordering is load-bearing because `store_asset` writes directly and
non-atomically (`borg/src/assets.rs`), and the GC spans a separate write from the
note rewrite. Invariant at every step: the note body/frontmatter reference only
assets that exist on disk. A crash before step 5 leaves the new source/output as
orphans (reclaimed by the next successful render of that kind, or recoverable via
rkvr); a crash between 5 and 6 leaves the *old* asset as an orphan (likewise
reclaimable) - but the note is never left pointing at a removed asset.

### API Design

```rust
// cortex - library, returns typed data, never prints
pub fn run(vault_root: &Path, config: &RenderConfig, req: RenderRequest)
    -> Result<RenderReport>;

// vault::assets - moved from borg::assets, unchanged signature
pub fn store_asset(vault_root: &Path, data: &[u8], filename: &str, subdirectory: &str)
    -> Result<(PathBuf, String)>; // (absolute, vault-relative)
```

```
# CLI (clap): note is the positional; --kind is a REPEATED flag (ArgAction::Append),
# case-insensitive (cli.md). Repeated form is used instead of a space-separated
# variadic because a greedy variadic --kind would swallow the trailing note positional.
sb cortex render notes/ai/foo.md  --kind slides
sb cortex render notes/tech/bar.md --kind diagram --format d2
sb cortex render notes/ai/baz.md  --kind image
sb cortex render notes/ai/foo.md  --kind slides --kind diagram   # multiple kinds, one note
```

**Config** (`~/.config/sb/cortex.yml`, kebab-case keys):

```yaml
render:
  model: ""                 # TEXT model for source emission; "" inherits the cortex default
  slides:
    theme: default          # marp theme name
    default-format: pptx
    model: ""               # optional per-kind override (else render.model -> cortex default)
  diagram:
    default-engine: mermaid # mermaid | d2
    d2-layout: dagre        # dagre | elk
    model: ""               # optional per-kind override
  image:
    backend: none           # none | http  (active-methodology selection)
    model: ""               # TEXT model for PROMPT emission (distinct from the pixel model below)
    http:
      endpoint: ""          # base URL; API key via env var only
      model: flux.2         # PIXEL/image model at the http backend
```

**Model resolution** is explicit and layered: a kind's `model` override, else
`render.model`, else the Cortex global default fed to `vault::fabric::run_pattern`.
The image kind separates the *text* model (`image.model`, emits the prompt) from
the *pixel* model (`image.http.model`, renders the image) so they never conflate.

### Implementation Plan

#### Phase 1: Shared asset helper relocation
**Model:** sonnet
- Move `borg::assets::store_asset` (and its tests) to a new `vault::assets`
  module; keep filename sanitization + content-hash behavior byte-identical.
- Relocate `borg`'s `pub(crate)` rkvr wrapper (`borg/src/rkvr.rs::remove`) to
  `vault::rkvr` (re-export from `borg`) so cortex can reach it; behavior identical.
- Repoint **only** `borg`'s three `store_asset` call sites
  (`borg/src/pipeline/handlers.rs:475`, `:741`, `:960`) to
  `vault::assets::store_asset`; leave `borg::assets` as a `pub use` re-export so
  nothing else breaks. (Verify the line numbers at implementation time with
  `rg -n 'store_asset' borg/src` - the call sites live in `pipeline/handlers.rs`,
  not the older `pipeline.rs` layout.)
- **Do NOT touch the slides publisher.** `borg/src/slides/publish.rs` does *not*
  use `store_asset` today; it has its own sequential-naming contract
  (`pick_filename` -> `<slug>-slide-NNN.jpg`) and copies on-disk FFMPEG outputs via
  `atomic_copy`. Forcing it onto the content-hashed, in-memory `store_asset` would
  break the sequential naming and is explicitly out of scope - the earlier draft
  overstated this consolidation.
- `cargo test --workspace` green; genuinely no behavior change (3 call sites
  repointed, publisher untouched).

#### Phase 2: cortex::render scaffold + slides
**Model:** opus
- Add `RenderKind`, `RenderRequest`, `RenderedArtifact`, `RenderReport`,
  `RenderConfig` to `cortex/src/render.rs`; wire `render/slides.rs`.
- Author `gen-slides.md` Fabric pattern in `borg/patterns/`; emit Marp markdown
  from the note's distilled `## Summary` + `## Claims` when present, falling back
  to the body, truncated to `run_pattern`'s `max_chars` with a WARN on overflow.
- **Extract** the artifact source from the Fabric output rather than naively
  stripping fences: LLMs routinely emit a conversational preamble/postamble
  ("Here is the deck you requested:" ... fenced block ... "Let me know if..."). A
  shared `extract_block(output, lang)` helper pulls the first fenced block of the
  expected language (or, for Marp, the content between the outermost fences),
  discarding everything outside it; a naive `strip_prefix`/`strip_suffix` would
  leave the prose in the `.marp.md`/`.d2` source and break the headless renderer.
  Reused by diagram/image emit.
- slides flow: `run_pattern("gen-slides", ...)` -> `extract_block` -> write
  `.marp.md` source via `store_asset` -> render with marp (`-o <out>.pptx` /
  `.pdf`; all-slides PNG uses `--images png`, since `-o out.png` is
  first-slide-only) -> store output -> build embed (link to PPTX/PDF; `![[...]]`
  for PNG) -> upsert the `<!-- sb:render kind=slides -->` block in `## Artifacts`
  (GC the prior slides asset+source via `vault::rkvr::remove` if present) ->
  replace the `slides` `renders:` line.
- Implement the shared note-mutation helper here (sentinel upsert + GC +
  `renders:` replace); Phases 3-4 reuse it.
- Function-level DEBUG logging on entry/exit per `logging.md`, plus one INFO
  summary per render carrying note path, kind, pattern, resolved model, renderer
  binary, elapsed, and output path (or "inline"); on failure, the same line names
  the failure stage (pattern / extract / render / store / note-write / gc) so a
  render is diagnosable from one log line without a rerun.

#### Phase 3: Diagram rendering (mermaid + D2)
**Model:** opus
- `render/diagram.rs`; author `gen-diagram.md` (emits mermaid OR D2 source per
  `--format`/config; pattern instructs the model which dialect).
- mermaid path: `extract_block(output, "mermaid")`, then a **syntactic smoke
  check** (non-empty extracted block whose first non-blank line is a recognized
  diagram keyword - `graph` / `flowchart` / `sequenceDiagram` / `classDiagram` /
  `stateDiagram` / `erDiagram` / etc.), inline the fenced block inside the
  `<!-- sb:render kind=diagram -->` sentinel block (no attachment, no external
  bin). There is no mermaid parser dependency and v1 adds none - Obsidian is the
  renderer of record, so the check is a guard against obvious garbage, not a full
  parse.
- D2 path: `extract_block(output, "d2")` -> write `.d2` source via `store_asset`
  -> `d2 <src> <out>.svg` -> store SVG -> `![[...]]` embed inside the sentinel.
- Both paths go through the shared note-mutation helper (GC prior diagram asset on
  re-render; replace the `diagram` `renders:` line).

#### Phase 4: Image prompt + offloaded backend
**Model:** opus
- `render/image.rs`; author `gen-image-prompt.md` (concept → structured prompt).
- Always write `.prompt.txt` source and splice the prompt into the note.
- `backend: http` path: submit the prompt to the configured endpoint (sync or
  create-and-poll, per provider; API key from env), store the returned PNG,
  `![[...]]` embed. `backend: none` stops after the prompt. Because this path can
  hang or burn money, the contract is bounded, not deferred to "implementation
  detail":
  - **Auth:** API key read from a fixed env var (`SB_IMAGE_API_KEY`), never config.
  - **Timeout:** a bounded overall wall-clock (config `image.http.timeout-secs`,
    default 120); create-and-poll caps total poll attempts and never loops
    unbounded.
  - **Retry:** single attempt; on failure return a typed error (no silent retry,
    no cost amplification). The `.prompt.txt` source is still written.
  - **Response size:** reject a response body over a hard cap
    (`image.http.max-bytes`, default 25 MB) before storing.
  - **Observability:** log endpoint, pixel model, elapsed, returned bytes, and any
    provider-reported cost on the render's INFO summary line.

#### Phase 5: CLI wiring, doctor gating, tests
**Model:** sonnet
- Add `Command::Render(RenderArgs)` to `sb/src/cli/cortex.rs`; format/print the
  `RenderReport`.
- Add `marp_cli_findings()` and `d2_cli_findings()` to
  `sb/src/cli/checks.rs::external_binaries_findings()` with install hints (marp:
  the standalone binary from `marp-team/marp-cli` releases, not the npm package;
  d2: `curl -fsSL https://d2lang.com/install.sh | sh`); both reported as optional
  (warn, not error) since render is opt-in.
- Register the three `gen-*.md` patterns in the `PATTERNS` const in
  `sb/src/cli/bootstrap.rs` (patterns are compiled in via `include_str!`, **not**
  discovered dynamically) and bump the count assertion in
  `sb/src/cli/bootstrap/tests.rs` from 17 to 20. This registration - not the
  `otto deploy` patterns-sync step, which only copies what `borg/patterns/` already
  contains - is the load-bearing wiring; omitting it ships a feature that compiles
  but fails at runtime because Fabric cannot resolve `gen-slides` / `gen-diagram` /
  `gen-image-prompt`.
- Unit tests per renderer (source emission, attachment pathing, embed string);
  integration test that `render --kind diagram --format mermaid` inlines a valid
  fenced block; `cargo test --workspace` + `otto ci` green.

## Alternatives Considered

### Alternative 1: Implement as a new `DistillKind::Render` in the distiller dispatch
- **Description:** Add a `RenderDistiller` and route it through
  `distillers/src/dispatcher.rs`.
- **Pros:** Reuses the `DistillExtractor` trait and `Distilled` contract directly.
- **Cons:** Distillers run **ingestion-time**, on freshly-fetched content inside
  borg's Stage-2 pipeline. Generation is **on-demand** against an
  already-published note. Overloading the dispatch couples two unrelated
  lifecycles and forces a `Distilled` shape onto artifacts that aren't summaries.
- **Why not chosen:** Lifecycle mismatch. cortex already hosts on-demand,
  note-operating, LLM-bound verbs (`intel`, `summarize`); render belongs there.

### Alternative 2: New top-level `sb studio` namespace
- **Description:** A fourth subsystem beside borg/cortex/oracle.
- **Pros:** Conceptually clean "Studio" analog to NotebookLM.
- **Cons:** Changes the composition root and the documented binary surface
  (`borg`/`cortex`/`oracle`/`status`/`doctor`/`bootstrap`); a new crate for one
  module is overweight when cortex already fits.
- **Why not chosen:** Cost outweighs the conceptual tidiness; revisit only if the
  generation surface grows large enough to warrant its own crate.

### Alternative 3: Adopt NotebookLM wholesale for generation
- **Description:** Push curated source sets into NotebookLM via `nlm` and use its
  Studio for everything.
- **Pros:** Zero build for slides/diagrams; gets audio/video for free.
- **Cons:** Cloud lock-in, static-snapshot sources, ToS-bound automation, outputs
  don't return to the vault as first-class notes.
- **Why not chosen:** Slides/diagrams/image-prompts are cheap to build natively
  and keep ownership. We *do* keep `nlm` for the genuinely hard artifacts
  (audio/video) and Deep Research - a complement, not the system of record.

## Technical Considerations

### Dependencies

- **Internal:** `vault::fabric`, `vault::assets` (new), `vault::paths`,
  `vault::distilled::DistilledMeta`. cortex → vault/distillers only (verified:
  cortex has no `borg` dependency).
- **External (runtime, optional):** `marp` (slide rendering), `d2` (SVG diagram
  rendering). mermaid requires nothing. Image `http` backend requires network +
  an API key env var.

### Performance

- Each render is one Fabric subprocess (LLM-bound, governed by the existing
  `run_pattern` timeout) plus one fast local renderer invocation. No batch
  fan-out in v1; if batch render lands later it uses `rayon::par_iter` for the
  renderer step and serializes the LLM calls, per cortex conventions, and obeys
  the no-unbounded-fanout rule on the daemon.

### Security

- API keys for the image backend come from environment variables, never config
  (consistent with existing secret handling). Endpoint URL is config.
- Marp/D2 consume model-emitted source; both are sandboxed renderers (no shell
  eval). Marp HTML/PDF generation runs headless with no remote asset fetch.

### Testing Strategy

- Unit: `extract_block` (preamble/postamble discarded, correct language picked,
  no-fence fallback); sentinel upsert (insert-then-replace yields exactly one block
  per kind); `renders:` line replace-not-append; embed-string construction.
- Unit: GC-on-replace removes the prior asset+source and routes through
  `vault::rkvr::remove` (assert the std fallback path when rkvr is absent).
- Unit: **idempotency** - rendering the same kind twice (with distinct,
  hash-differing fake outputs) leaves one sentinel block and one `renders:` line,
  and the first asset is GC'd.
- Unit: a hand-added mermaid block *outside* any sentinel survives a diagram
  re-render untouched.
- Integration: end-to-end `render --kind diagram --format mermaid` produces a note
  with a valid sentinel-wrapped fenced block (no external bin needed in CI);
  marp/d2 paths gated behind a feature/skip when the binary is absent.
- `sb doctor` reports marp/d2 presence.

### Rollout Plan

- Ships in one release across all five phases back-to-back (no phase gating, no
  soak). `otto deploy` syncs the new patterns; `sb doctor` surfaces missing
  renderers as warnings. Feature is inert until a user runs `sb cortex render`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Model emits invalid Marp/D2/mermaid source | Med | Med | Strip wrapping code fences; validate before render; on render failure, keep the saved source + return a typed error (note unchanged) |
| Note too long for Fabric `max_chars` | Med | Low | Emit from distilled summary+claims first, fall back to body, truncate + WARN |
| Stub / entity / empty note rendered | Low | Low | Fail cleanly with "insufficient content"; no attachment, no note mutation |
| Orphaned content-hashed assets bloat the Syncthing'd vault on iterative re-render | Med | Med | Sentinel-keyed replacement + GC of the superseded asset via `vault::rkvr::remove` on every re-render (recoverable); steady-state assets per note bounded by kinds, not by render count |
| Manual edits corrupt a `<!-- sb:render -->` sentinel block | Low | Med | If the closing sentinel for a kind is missing/malformed, treat the kind as absent and append a fresh block (never delete unmarked content); WARN so the operator notices |
| `marp`/`d2` absent on host | Med | Low | Optional-tool doctor warnings + install hints; mermaid path needs no binary |
| Image backend cost/latency (hosted GPU) | Med | Low | `backend: none` default; pixel-gen is strictly opt-in |
| Frontmatter `renders:` collides with future schema | Low | Med | Additive-only key; mirrors existing `slides:` precedent |
| `store_asset` relocation breaks borg call sites | Low | High | Re-export shim + `cargo test --workspace` gate in Phase 1 |

## Resolved Decisions

- [x] **Single-note input in v1.** The unit is one note. Multi-note "source set"
      rendering (NotebookLM-style) is a clean forward extension - `RenderRequest.note`
      becomes a `Vec` and the emit step concatenates summaries - but is not v1.
- [x] **Default diagram engine: mermaid.** Obsidian-native, zero external infra,
      highest LLM reliability. D2 is opt-in via `--format d2` / config for
      architecture diagrams that need better layout.
- [x] **Mutate the source note** (managed `## Artifacts` section + `renders:`
      frontmatter), not a companion note. Keeps the artifact with the knowledge
      that produced it and lets oracle index both together.
- [x] **`--format` requires exactly one `--kind`.** `RenderRequest.format` is a
      single `Option<String>`, so with multiple `--kind` values its target is
      ambiguous (slides' `pptx` vs diagram's `d2`). The CLI rejects `--format`
      combined with more than one `--kind` (typed error); each kind otherwise uses
      its configured `default-format` / `default-engine`.

## Open Questions

- [ ] None blocking. (Image `http` backend provider specifics - fal vs Replicate
      request contract - are an implementation detail of Phase 4, settled when the
      endpoint is chosen.)

## References

- `CLAUDE.md` - workspace architecture, L2 Distilled contract, attachments/one-way
  data flow invariants
- `distillers/AGENTS.md`, `distillers/src/dispatcher.rs`, `distillers/src/article.rs`
- `vault/src/fabric.rs` (`run_pattern`), `borg/src/assets.rs` (`store_asset`),
  `borg/src/slides/publish.rs` (publish precedent)
- `sb/src/cli/cortex.rs` (verb wiring), `sb/src/cli/checks.rs` (doctor gating)
- `docs/design/2026-06-06-configurable-retrieval-pipeline.md` (config
  active-methodology precedent for the image backend)
- `~/repos/tmc/nlm` - NotebookLM CLI/MCP for the deferred audio/video + Deep
  Research capabilities
