# borg::pipeline — Ingest Orchestration

> Local node for the pipeline. Parent: `../../AGENTS.md`. Artifact model: `../stages/AGENTS.md`.

## Purpose

Orchestrates ingestion: receives content (URL/image/PDF/audio/text), routes to type-specific handlers, stages through fetch → extract → summarize → distill → publish, and dual-writes the outcome to the receipts DB. Owns permit gating, hard-timeout enforcement, deduplication, and the terminal state transition that closes a trace.

## Entry Point

- `process_content(content: ContentKind, tags, method, force, config, policy: &PublishPolicy, trace_id?, ...) -> IngestResult` (`pipeline.rs`). Returns once the (async) work completes; all work runs on a detached task spawned by the caller.

## Stage Flow

```
Stage 0 (raw)  classify URL + fetch / accept binary   → envelope + fetched.*
Stage 1 (tx)   extract text / transcript / OCR         → transcript.*
Stage 2 (sum)  summarize / distill (LLM)               → summary.*
Gate 2         paraphrase-detect on summary            → rejection.yml if blocked
Stage 3 (pub)  render note + publish to vault          → ledger row + receipts row
```

Each type handler (`process_url`, `process_image`, `process_audio`, `process_document_file`, `process_text`) wraps its logic in a hard timeout and calls `record_terminal_to_receipts()` at the chokepoint.

## Invariants

1. **Permit before work.** `GENERAL_PERMITS` (cap `pipeline.max-concurrent-traces`) is acquired at the top of `process_content`. No async work runs before the permit is held.
2. **HEAVY_PERMITS at the subprocess call site, not at dispatch.** yt-dlp / fabric / ffmpeg / Groq-OCR handlers acquire `HEAVY_PERMITS` immediately before the subprocess — closing the gap where a URL classified as "article" still fans out to ffmpeg.
3. **TraceLeaseGuard before the permit wait.** Each trace writes a renewable lease (`lease_owner_pid` + `lease_until`) onto its shared receipts row before the permit, renews it at permit grant, and clears it on Drop UNLESS `cancel()`led after the terminal write (which NULLs the lease in the same UPDATE). Fail CLOSED: a failed initial lease write aborts the trace to a terminal failure. The watchdog reads liveness from that shared row (absent-or-expired lease = reap-eligible, checked atomically in the promotion UPDATE), so it never falsely reaps a trace a SEPARATE `sb borg harvest` process is still working. See `docs/design/2026-07-24-harvest-watchdog-cross-process-reaping.md`.
4. **Hard timeout bounds every handler** (`tokio::time::timeout(hard_timeout_secs)`). On timeout the future drops, guards release, and the outer dispatch records `Failed { reason: "timeout" }`.
5. **Terminal status is final and atomic.** `record_terminal_to_receipts()` records exactly one of `Completed | Duplicate | Failed`. The UPDATE carries `WHERE status='received'` so concurrent writes can't stomp; SQLite serializes via `busy_timeout`. `Queued` is never terminal.
6. **Inflight guard dedupes concurrent duplicates.** On non-force runs, `InflightGuard::try_acquire(canonical_url)` is held for the rest of the URL handler; a concurrent second attempt returns `Duplicate{original_date:"inflight"}` (a success for receipts). Force runs skip the guard.
7. **A ledger hit reingests in place; it never rejects.** After the inflight guard passes, `process_url_inner` looks up the canonical URL in the ledger; a prior success turns the run into a reingest of that note (old date, preserved fields, attachments). The only duplicate rejection is the in-memory inflight guard. Under `PublishPolicy::Scratch` the lookup is skipped.
8. **Atomic publish.** Stage 3 writes to a temp file, applies frontmatter, then renames — no half-written note ever lands in the vault.
9. **Preserved fields are read list-aware and fail CLOSED.** `publish::read_preserved_fields` returns `Vec<(String, FieldValue)>`, parsing inline `[a, b]`, block `- item`, and quoted forms, and returns `Err` on an unreadable note. Both reingest paths abort rather than publish a note that silently lost its preserved fields; the URL path used to swallow the error and return an empty set.
10. **A reingest keeps the note's path, filename included.** `atomic::url_publish_path` returns the URL's existing note when one is found, so the model's fresh title never renames it; a rename deleted the old file and broke every inbound wikilink (harvest's trace-keyed replace follows the same rule).
11. **`tags` merges, it does not replace.** It is the one key in both `RENDER_NOTE_KEYS` and `CORTEX_PRESERVE_KEYS`: `apply_preserved_fields` unions preserved with fresh, preserved first, capped at `max-per-note`, and the session path does the same through `PriorFrontmatter.tags`. A note already at the cap keeps its tags and the fresh set is dropped with a log line, deliberately, so a refetch never rewrites classification silently. `--retag` is the explicit refresh.
12. **Borg-owned keys preserve on their own list.** `markdown::BORG_PRESERVE_KEYS` (today only `capture-note`) is borg's twin of `CORTEX_PRESERVE_KEYS`; never add a borg field to the cortex list. These values are free text that may span lines: `read_preserved_fields` decodes the key line plus its indented continuation as YAML (an undecodable entry is an `Err`, fail closed), and `apply_preserved_fields` writes it back through `markdown::yaml_scalar`, never a raw `format!`. A value the fresh render already carries wins over the preserved one.

13. **An X status URL is read from fxtwitter only, and fails closed.** `x::status_id` picks the arm in `process_url_inner`; `process_x_post` errors (`fetch-failed`) on any API failure and never falls back to fabric/Jina. Photos are described in `figures.rs` by calling `ocr::ocr_extract` + `ocr::vision_extract` directly (never `process_content`, whose GENERAL permit the URL trace already holds), through `figures::image_section`, the same builder image ingest uses. Each photo is also kept as a `Figure` (bytes, MIME, section capped at `images.max-section-bytes`) in `FetchedPost.figures`, stored and embedded under `## Images` by contract 14 (empty when `images.enabled` is false). A degraded photo (download failed or over `images.max-download-bytes`, vision failed or empty, nothing to say) is omitted or OCR-only and sets `fetch_degraded`; a post with no text whose photos all failed is an `Err`. X Article figures (the post's and the quote's) resolve `atomic` block -> `entityMap` (a list) -> `mediaItems[].mediaId` -> `media_entities[].media_info.original_img_url` in `x::article_figures` (an unresolvable block is skipped and logged, never an error) and go through `figures::collect`, the ONE figure path for every source (X post photos, Article figures, and web-article images): download the first `images.max-candidates` (a URL the prefilter rules out is never downloaded), run `figures::prefilter` (format checks for every source, URL-token and `min-side-px` heuristics for web articles only, sha256 dedupe within the note; an SVG is rasterized by `figures/svg.rs` with the `<image href>` resolver locked down and system fonts loaded, classified and described as the raster, and stored as the original `.svg`), bound the image vision reads (`figures::vision_image`: longer side within `max-side-px`, base64 under the API's 5 MiB), classify (X post photos skip this) with `slides::classify::FIGURE_TAXONOMY_PROMPT` (keep by `images.content-filter`), cap at `max-per-note`, then describe; a download/classify/describe failure drops that figure and degrades, "not a diagram" and the cap do not. Each kept figure renders at its `atomic` block for the distiller and lands in `FetchedPost.figures` in source order. X post photos skip classify and the content heuristics, and share the one `figures: kept=N dropped={...}` log line. With `images.enabled: false` no Article figure is fetched. Web articles (whatever fetched them: readable, fabric, jina; HN is a thread and carries none) go through `article::distill_article`: `figures::describe_article_media` parses every `![alt](url)` (`figures/candidates.rs`) as a `WebArticle` candidate, relative URLs resolved against `page_url`, the post-redirect URL the readable fetcher reports (`readability::Fetched::Article`, also `dom_smoothie`'s base; fabric and Jina expose none, so they pass the requested URL), while `source:`, dedup and staging stay on the requested URL, runs `collect`, and `figures/splice.rs` replaces each kept image (its whole line when it stands alone or is only a linked image) with `### Image N`, `*alt*`, and the section, so the distiller reads the diagram in place; `distill_for_publish_article` takes an `ArticleInput { markdown, quality_source, clean_source }` and `transcript_quality_ok` judges `quality_source`, the PRE-splice markdown, while the transcript kept is the spliced one. The figures go into `FigureSet` and the same plan -> render -> `publish_url_note` order as X. Tests reach fxtwitter, vision and Jina only through `x::bases`, `ocr::bases` and `jina::bases` scopes.
14. **URL notes publish through one writer, `publish::publish_url_note`, in a fixed order** (design: `docs/design/2026-10-22-capture-all-diagrams.md`): refuse a degraded reingest that keeps fewer images than the old note's `images:` (`fetch-failed`, nothing touched; a degraded run keeping at least as many publishes, still degraded) -> size-gate the fully rendered note -> `figures::store` (no-clobber `write_atomic_new`, returns only files it created) -> `archive_then_publish` (on failure only the created files are removed) -> only then archive the old note's orphaned `slides:` / `images:` files (`slides::cleanup::Owned`). `figures::plan` also builds the `## Images` section (`with_images_section` appends it after the distilled body, before the Source footer) and assigns `system/attachments/images/YYYY-MM/<trace-id>-<n>-<sha8>.<ext>` before the note renders and downscales over `images.max-bytes`; the trace id keeps one note's cleanup from touching another's file. Config: the top-level `images:` block (`config::ImagesConfig`).

## PublishPolicy

`PublishPolicy` (`pipeline.rs`) says where a run publishes. `Vault` is ingest, byte-for-byte today's behavior; `Scratch { root }` is `sb borg distill`. One enum, not booleans, because the ledger, lookup, archive, cleanup and vault-write switches always move together. Every production caller except `routes::distill` passes `&PublishPolicy::Vault`. The policy answers each question through a method, not a scattered `match`: `effective_config`, `refusal`, `inflight_key`, `frames_dest`.

| Site | `Vault` | `Scratch { root }` |
|---|---|---|
| non-URL kind (`refusal`, top of `process_content`) | runs | `Failed`, `intake-rejected`, before the lease and Stage 0, no write |
| config every stage reads (`effective_config`, shadowed in `process_content`) | live config | clone with `staging.root = root/stages`, `enabled = true`, `layout = PerTrace` |
| staged `distilled.yml` missing after distill (`scratch_staging_missing`) | not checked | `Failed`, `publish-failed`, before any note is written |
| inflight key (`inflight_key`) | `<canonical-url>` | `distill:<canonical-url>`; a hit is `Failed` `intake-rejected`, never `Duplicate` |
| ledger lookup and append | runs | skipped (the ledger file is `None`), never a reingest |
| note dest, `vault_dirs`, slides, figures (`publish_root`) | vault | `root`; `obsidian_url` is `None` |
| `publish::publish_url_note` (`UrlPublish.policy`) | archive, then publish, with rkvr cleanups | `atomic::publish_unique` and return: no archive, no cleanup |
| `figures::store` rollback | rkvr on failure | skipped; partial output stays for debugging |
| frames work dir (`frames_dest`; `frames::WorkDirGuard`) | removed | copied to `root/frames/` (source video skipped), then removed, on every exit |
| `runs.yml` (`frames::run_records`) | not written | every classified run's best frame OCR'd, in `root/frames/runs.yml` |
| receipts / sidecar | `scratch = 0`, `<vault>/system/intake/` | `scratch = 1`, `root/system/intake/` (written at the door, not here) |

- The frames tree keeps the work dir's own shape: frames are at `root/frames/frames/frame_NNNN.jpg`, beside `frames.yml`, `slides.yml`, `slides/` and `runs.yml`.
- The guard rides in `SlidePayload`, so its drop preserves and removes the work dir on every early return; a failed preserve copy is a WARN, not a failed run.
- Per-run temp paths are keyed by trace (`borg-youtube-frames/<trace_id>`, a `tempfile` dir per whisper call) so a distill and an ingest of one video can run together.
- `Config` is `Clone` only for the one effective-config clone per scratch run.

## Patterns

- **Add a content type:** add a `ContentKind` variant + a `process_<type>` handler that wraps logic in timeout + (for non-force) InflightGuard, calls extractors/distillers, returns `IngestResult`; wire it into the `process_content` match.
- **Add a gate:** a function returning `Result<(), RejectionRecord>`; on failure write the rejection and skip downstream stages.

## Anti-patterns

- Don't add subprocess calls outside the HEAVY_PERMITS sites — the watchdog won't account for their concurrency.
- Don't write to receipts before the terminal chokepoint — state transitions must stay atomic.
- Don't add a vault, ledger, archive or shared-staging write without reading the `PublishPolicy`; the zero-write test (`distill_policy_writes_nothing_to_vault`) has to keep biting.
- Don't leave any handler un-timeout-wrapped — unbounded futures starve the permit pool.
