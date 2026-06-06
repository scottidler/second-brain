# distillers — Stage-2 Structured Extraction

> Read before touching `distillers/`. Parent: `../CLAUDE.md`. The contract type lives in `../vault/AGENTS.md` (`vault::distilled`).

## Purpose

Per-kind Stage-2 processors that take a Stage-1 transcript (markitdown / VTT / thread render / OCR / prose) plus Stage-0 envelope metadata and emit a typed `vault::distilled::Distilled`. A dispatcher routes by `DistillKind`; Fabric-backed kinds shell out to Claude; no-LLM kinds (Idea, Vocabulary) preserve input verbatim. Stage-3 (publish, in borg) renders `Distilled` to vault markdown + frontmatter — the vault file is canonical.

## Entry Points

- `dispatcher.rs`: `Dispatcher::new(fabric_caller, article_config)`, `Dispatch::distill()` (async, routes by `DistillKind`).
- `render.rs`: `render(distilled) -> RenderedDistilled { body_markdown, frontmatter_additions }` (pure).
- `lib.rs`: `DistillExtractor::distill()` (trait each per-kind extractor implements).

## The Distilled Contract

`vault::distilled::Distilled`:
- **summary** — 2–4 sentence prose (feeds FTS5, embeddings, display).
- **claims** — `Vec<Claim>` with optional anchors (video timestamp / article heading / tweet id).
- **tags** — canonical, post-filtered against `canonical-tags.yml`, max 7.
- **links** — outbound URLs discovered in source (distinct from the source URL).
- **kind_specific** — `Option<KindPayload>`: `RepoPayload` (stars/language/last_commit/topics/install), `VideoPayload` (channel/duration/published_at), `ThreadPayload` (author/post_count/platform).
- **meta** — `DistilledMeta` (extractor id, model, tokens, produced_at, validation).
- **transcript** — `Option<String>`; set only for non-URL kinds (Image/VoiceNote/Idea/Vocabulary) so content stays searchable; URL kinds leave it `None` (the URL is the archive).

## Contracts & Invariants

- **`Dispatch` is object-safe** (`dyn Dispatch`) so test setups can hold `&dyn Dispatch`.
- **Extractor ids are stable + versioned** (e.g. `distill-idea-v2`) for forensics/replay.
- **Bounds enforced post-distill:** `validate::enforce_bounds` caps `MAX_SUMMARY_CHARS` (2000); records truncations in validation meta.
- **Transcript only for non-URL kinds** — keeps diffs quiet for URL kinds.
- **Fabric calls respect `max_chars` + `timeout_secs`**, falling back to passthrough on timeout/error.

## Rendering

`render()` produces:
- **body_markdown** — Summary / Claims (with anchors) / Links / Transcript (if present) sections.
- **frontmatter_additions** — `BTreeMap` (alphabetical → stable diffs): `distilled: true`, `distilled-extractor`, `cortex-*` payload keys.

## Patterns

- **Add a distiller kind:** add a `DistillKind` variant + a `<Kind>Distiller<F>` implementing `DistillExtractor`, route it in `dispatcher.rs`, and (if it carries metadata) extend the matching `KindPayload` + `render()`. Inject Fabric via the `FabricCaller` trait (`FabricShell` prod, `FakeFabric` tests).

## Anti-patterns

- Don't serialize `Distilled` straight to the vault — go through `render()`.
- Don't drop validation meta (`fallback_reason`, bounds truncations) — it's load-bearing for forensics.
- Don't set `transcript` for URL kinds (Article/Repo/Video/Thread).

## Module Map

- `lib.rs` (`DistillExtractor`, `DistillInputs`), `dispatcher.rs` (`Dispatcher<F>`, `Dispatch`, `DistillKind`), `render.rs`.
- Per-kind: `article.rs`, `repo.rs`, `video.rs`, `thread.rs`, `image.rs`, `voicenote.rs`, `idea.rs`, `passthrough.rs`.
- Support: `fabric.rs` (`FabricCaller`/`FabricShell`/`FakeFabric`), `text.rs`, `validate.rs`.
