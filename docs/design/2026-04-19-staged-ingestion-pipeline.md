# Design Document: Staged, Replayable Ingestion Pipeline

**Author:** Scott Idler
**Date:** 2026-04-19
**Status:** Implemented (phases 1-7; phase 8 decomposition deferred)
**Review Passes Completed:** 5/5

**Supersedes:** [2026-03-30-llm-block-detection.md](2026-03-30-llm-block-detection.md) (never shipped; its block-detection + blocklist concepts are absorbed here as stage-boundary gates)

## Summary

Transform borg from a single-shot ingestion pipeline into a staged pipeline with persisted intermediate artifacts keyed by `trace_id`, first-class replay, per-stage quality gates, and typed per-input pipelines. Staging lives outside the vault; only the finished note is published. The goal is to make ingestion debuggable, recoverable, and extensible as the second-brain investment grows.

## Problem Statement

### Background

Borg today captures input from Telegram / Discord / ntfy / HTTP / clipboard / CLI, fetches any referenced URL (Fabric `-u` or Jina, falling back to `markitdown`), summarizes with Fabric, classifies into the vault schema (domain, type, tags), and writes a finished note into the Obsidian vault. The pipeline is a single in-memory flow: input → note. No intermediate artifact is persisted.

A prior design doc ([2026-03-30-llm-block-detection.md](2026-03-30-llm-block-detection.md)) identified that quality gates run too late (on the Fabric summary, after the LLM has paraphrased garbage into coherent prose) and proposed a Haiku classifier + domain blocklist. That design was "In Review" and never implemented. `borg/src/classify.rs` and `borg/src/blocklist.rs` do not exist; only the pattern-based `detect_blocked_content` in `quality.rs` ships.

### Problem

An audit of the vault on 2026-04-19 found **28 notes containing blocked-fetch content**, 16 of them from `xda-developers.com`. XDA returns `HTTP 451 / SecurityCompromiseError / "Anonymous access to domain blocked until Mon Apr 20 2026"` because Jina's IP range has been flagged for suspected DDoS abuse. The block message is plain text with a 200-ish status from Fabric's perspective; Fabric cheerfully summarizes it into *"The provided input contains an error message indicating that access to the XDA Developers website is blocked due to suspected DDoS attacks"* and borg writes that as a note. Every structural quality check (word count, summary section, frontmatter completeness) passes.

Underneath the XDA specifics are four structural problems:

1. **No intermediate artifacts.** When ingestion goes wrong, there is nothing to inspect after the fact except the broken note. The original URL, raw HTML, extracted markdown, and LLM summary are all discarded.
2. **No replay.** Fixing a failure (new extractor, new block-detection pattern, new Fabric pattern, new tag vocabulary) cannot be retroactively applied to already-ingested content. The user either tolerates bad notes or deletes + re-asks the source.
3. **One-pipeline-fits-all.** Every input gets the same shape: fetch → Fabric → classify. Vocabulary entries, raw ideas, GitHub README dumps, and YouTube transcripts do not all need the same LLM pass. Vocabulary especially doesn't want Fabric at all - it wants a structured form.
4. **No real-time alerting.** The 28 bad notes accumulated silently over weeks. Users discover ingestion failures by browsing the vault and finding broken content, not by being notified when a stage gate rejects input.

### Goals

- **Persisted stage artifacts** outside the vault, keyed by `trace_id`, for every ingestion attempt (successful or failed).
- **Replay** as a first-class CLI feature: re-run any suffix of the pipeline for a single trace, a time range, or a filter expression.
- **Stage-boundary quality gates** - block detection runs on raw fetched content *before* Fabric, failed-fetch paraphrase detection runs *after* Fabric. The user sees problems at the boundary where they occur.
- **Typed input variants.** Each input kind (article URL, GitHub URL, YouTube URL, thread URL, image, voice note, idea, vocabulary) has a pipeline shape appropriate to its data, not a single one-size-fits-all flow.
- **Real-time alerting** on gate rejections, with aggregation to avoid notification spam.
- **30–90 day retention** on staging artifacts; vault stays clean forever.
- **Recovery path for existing bad notes** - the 28 existing blocked-content notes must be reingestable once the new pipeline lands.

### Non-Goals

- Storing raw / transcripts / summaries inside the Obsidian vault. Staging is outside the vault, emphatically.
- Indefinite retention of stage artifacts. Raw HTML and images are large; they age off.
- Making staging artifacts searchable via Oracle / Obsidian. They are operational data, not knowledge.
- Replacing the existing router or notification daemon wiring. Those are untouched.
- Solving the 3073-line `borg/src/pipeline.rs` decomposition as part of this doc. That's adjacent; it will happen incidentally as stages move into their own modules, but is not the goal.
- Bypassing or circumventing block pages via proxies / CAPTCHA solving. A browser-UA HTTP fallback is in scope (servers that block bots but not browsers); adversarial bypass is not.
- Bespoke storage formats. YAML sidecar metadata + bytes on disk is the default.

## Proposed Solution

### Overview

Borg is reorganized around four stages. Each stage has one input and one output, both persisted. The existing capture→fetch→Fabric→classify flow becomes a fixed traversal of stages 0→3, and replay is "re-run any suffix for a given `trace_id`."

```
 ┌───────────────┐   ┌────────────────┐   ┌──────────────┐   ┌───────────┐
 │ STAGE 0       │   │ STAGE 1        │   │ STAGE 2      │   │ STAGE 3   │
 │ raw/          │──▶│ transcripts/   │──▶│ summaries/   │──▶│ vault     │
 │ capture       │   │ extract        │   │ summarize    │   │ notes/    │
 └───────────────┘   └────────────────┘   └──────────────┘   └───────────┘
        │                   │                    │                  │
     gate-0              gate-1               gate-2              (current
     domain              block-page           failed-fetch        quality
     blocklist           detection            paraphrase          checks
                         HTTP 451             detection           remain)
                         browser-UA
                         fallback
```

**Staging location (proposed default; alternatives in Storage Organization Options below):** `~/.local/share/borg/stages/` with a single directory per stage. The vault path is unchanged.

Every stage artifact lives under a single per-trace directory keyed by `trace_id` (e.g. `stages/tg-26a031/`). The `trace_id` is generated once at capture (`vault::trace::generate(Method)`; existing prefixes: `tg-` Telegram, `ds-` Discord, `nt-` ntfy, `ht-` HTTP, `cb-` clipboard, `cl-` CLI) and becomes the canonical join key across all four stages:

- Stage 0 writes `stages/<trace_id>/envelope.yml`, `body.*`, `attachments/*`, `fetched.*`
- Stage 1 writes `stages/<trace_id>/transcript.md` + `transcript.yml`
- Stage 2 writes `stages/<trace_id>/distilled.yml` (the structured `Distilled` contract from [Doc 1](2026-05-16-extractor-contract-and-l2-summaries.md)). Pre-Doc-1 traces wrote `summary.md` + `summary.yml`; both shapes are read by `borg replay`.
- Stage 3 writes `vault/notes/<slug>.md` with `trace: <trace_id>` in the frontmatter

Given a `trace_id`, every artifact produced by that ingestion is reachable by one `ls stages/<trace_id>/` (staging) plus a grep of `vault/notes/` for `trace: <trace_id>` (vault). Replay, inspection, and deletion all use this single ID. The vault frontmatter field name is `trace:` (short form); the CLI flag, sidecar keys, and code use `trace_id`.

**Dedup on re-ingestion of the same URL:** every fresh capture gets a fresh `trace_id`. Borg does *not* short-circuit on "URL already ingested" - the user may deliberately re-capture a URL to trigger a fresh replay. The vault-side collision handling (same filename stem) falls back to the existing reingest-domain-preservation behavior: the new note atomically overwrites the old one while preserving cortex-owned frontmatter fields (tags, quality issues, curation state). Stage artifacts from the prior trace are not touched by a new trace; they age off normally under retention.

### Architecture

Stage responsibilities:

**Stage 0 - Capture → `raw/`**
- Persists the **full raw capture event byte-for-byte**, not a destructured projection of it. Stage 0 is a faithful record of what landed at the router, preserving everything the user sent so that later reinterpretation is possible.
- For a Telegram message, that is the complete message payload: text body (caption + any trailing prose), attachment bytes (image/audio/file), envelope metadata (chat_id, message_id, reply_to, timestamps). A message containing `"vocab:en perro"` with an attached photo saves both the caption text and the photo bytes; neither is discarded.
- For a URL message, Stage 0 captures (a) the original message body exactly as received (prose + URL), and (b) the fetched URL response body via Jina / Fabric `-u` / browser-UA / markitdown chain. Stage 0 *does* perform the network fetch - this is where Jina, Fabric `-u`, and browser-UA live. The captured bytes are what Stage 1 reads from disk.
- Writes a raw-payload tree under `raw/<trace_id>/`: `envelope.yml` (transport metadata), `body.txt` or `body.bin` (message body), `attachments/*` (any files), and for URL inputs `fetched.html` + `fetched.yml` (which extractor succeeded, status, headers).
- Gate-0: **domain blocklist** (from the superseded doc). **URL-only gate** - runs solely on URL-bearing captures; non-URL kinds (image, voice, idea, vocabulary, text-only ideas) pass through unconditionally. Before a URL fetch is attempted, check the blocklist; fast-fail on known-blocking domains with a clear error.

**Stage 1 - Extract → `transcripts/`**
- Produces text **from the bytes Stage 0 saved** - never from the network. This is the offline-replay guarantee: given a populated `raw/<trace_id>/`, Stage 1 must produce the identical `transcripts/<trace_id>.md` without any external call. The current live-URL extractors (`jina::fetch_article_markdown`, `fabric::fetch_article`) are Stage-0 tools (they fetch); they are not Stage-1 tools.
- **Stage-1 primary extractor is `markitdown-cli` fed from disk** (`markitdown < raw/<trace_id>/fetched.html`). Markitdown handles HTML, PDF, docx, ipynb, and other formats we already use it for. For formats markitdown doesn't cover natively, per-kind extractors (current shape - see the Per-IngestKind table below for the full pipeline):
  - `ArticleUrl` / `ThreadUrl`: markitdown on `fetched.html`. Thread URLs use the rendered markdown directly (no native JSON API in scope per Phase 6's audit decision).
  - `GitHubUrl`: borg's `GitHubFetcher::render_transcript` (README + metadata block) - distinct from markitdown because the raw GitHub-API envelope is JSON, not HTML.
  - `YoutubeUrl`: VTT segments parsed by `parse_vtt_segments` produce a timestamped transcript.
  - `Image`: Vision API (Groq) description concat with Tesseract OCR text.
  - `VoiceNote`: Groq Whisper transcription (plain text, no native anchors).
  - `Idea` / `VocabularyEN` / `VocabularyES`: `body.txt` passthrough. Phase 9 added: the trimmed input becomes both `Distilled.summary` and `Distilled.transcript` (verbatim preservation, no LLM call) - see [extractor-contract-l2-phase-9-cleanup.md](2026-05-16-extractor-contract-l2-phase-9-cleanup.md).
- Writes `transcripts/<trace_id>.md` (or `.yml` for structured forms) + metadata sidecar.
- Gate-1: **block-page detection on the raw Stage-0 artifact.** Pattern match the saved `fetched.html` / `body.txt` for `"anonymous access to domain"`, `"blocked until"`, `"SecurityCompromiseError"`, HTTP 451 from Jina's response headers, etc. Reject BEFORE Stage 2 summarization runs. Record the domain to the blocklist.

**Stage 2 - Distill → `distilled.yml`**
- Per-IngestKind dispatcher produces the structured [`Distilled`](2026-05-16-extractor-contract-and-l2-summaries.md) contract. URL kinds (`ArticleUrl`, `GitHubUrl`, `YoutubeUrl`, `ThreadUrl`) call Fabric (`distill-article` / `distill-repo` / `distill-video` / `distill-thread`; long videos and voicenotes use map-reduce chunk+reduce patterns). Non-URL kinds (`Image` via Fabric `distill-image`; `VoiceNote` via Fabric `distill-voicenote`; `Idea`, `VocabularyEN`, `VocabularyES` via `IdeaDistiller` with no Fabric call) all populate `Distilled.transcript = Some(input)` so the published note carries the verbatim source below the LLM-distilled summary.
- Writes `distilled.yml` to the per-trace staging directory.
- Gate-2: **failed-fetch paraphrase detection.** Pattern match for `"only an error message"`, `"no actual content"`, `"error message indicating"`. This is the backstop for block pages that slipped past Gate-1.

**Stage 3 - Publish → `vault/notes/`**
- Assembles frontmatter + body from the summary + metadata. Writes the final markdown note into the Obsidian vault. This is what exists today; it is unchanged as far as vault consumers (Obsidian, cortex, oracle) are concerned.
- Gate-3: the existing structural quality checks (word count, summary section, outbound links) stay where they are.

Each gate's rejection does three things: (1) aborts the pipeline for that trace, (2) records a rejection record to `rejections/<trace_id>.yml` with stage, reason, and raw artifact pointers, (3) fires an alert (see Alerting).

**Blocklist side-effects** are gate-specific, not automatic on every rejection:

- **Gate-0** rejection does *not* update the blocklist (the domain was already on it; the gate just enforced).
- **Gate-1** (block-page detection / HTTP 451) *adds the domain to the blocklist* with a `retriable-after` timestamp parsed from the block message where possible, else `now + 7d`. The `blocklist-updated: true` field in the rejection record flags this.
- **Gate-2** (paraphrase detection) does *not* update the blocklist - by this stage Fabric has already masked the domain signature; there is no reliable raw evidence to justify a domain-wide block. It alerts only.
- **Gate-3** (existing structural quality) does not update the blocklist.

A domain stays on the blocklist until: (a) the `retriable-after` timestamp passes, at which point it becomes auto-retriable; or (b) the user runs `borg blocklist remove <domain>` explicitly.

### Typed Input Variants

`IngestKind` is a **processing hint** attached to the Stage-0 sidecar, not a destructive selector. Stage 0 always persists the full raw capture event (envelope + body + attachments + any fetched URL bytes). `IngestKind` tells Stages 1-2 which extractor + summarizer to run against that preserved raw. If classification was wrong, the user can re-run with a different `IngestKind` (`borg replay <trace_id> --kind <new-kind>`) without losing any source data.

A message with a photo AND a `vocab:en perro` caption saves both; the dispatcher picks one primary `IngestKind` (first-match per rules below) but the raw attachments remain so a future replay can reclassify. A long paragraph wrapping a URL saves the full paragraph *and* the URL's fetched HTML; the transcript joins both so no prose is lost.

All rows assume Stage 0 has already saved the **full capture event** (envelope + body + attachments + any fetched bytes). The "Stage 1" column describes which extractor runs against the Stage-0 bytes; it is a pure bytes→text transform with no network access.

| IngestKind | Stage 0 fetch (online) | Stage 1 extract (offline, from disk) | Stage 2 distill | Stage 3 publish |
|---|---|---|---|---|
| `ArticleUrl` | Jina / Fabric-u / UA-fallback → `fetched.html` | `markitdown < fetched.html` → `transcript.md` | Fabric `distill-article` → `Distilled` | article note |
| `GitHubUrl` | GitHub REST API (raw `{"repo": ..., "readme": ...}` envelope) → `fetched.html` | `render_transcript` (README + metadata block) → `transcript.md` (`extractor: github-render`) | Fabric `distill-repo` → `Distilled` (+ `KindPayload::Repo`) | repo note |
| `YoutubeUrl` | yt-dlp metadata + VTT subtitles → staged | `parse_vtt_segments` → timestamped `transcript.md` | Fabric `distill-video` (short) or `distill-video-chunk` + `distill-video-reduce` (long, map-reduce) → `Distilled` (+ `KindPayload::Video`) | video note |
| `ThreadUrl` (X/Reddit/HN) | Jina / Fabric-u / UA-fallback chain (no native API) → `fetched.html` | rendered markdown → `transcript.md` (`extractor: thread-markdown-shim`) | Fabric `distill-thread` → `Distilled` (+ `KindPayload::Thread`) | thread note |
| `Image` | n/a (bytes in `attachments/`) | Vision API (Groq) + Tesseract OCR concat → `## Description` + `## Extracted Text` transcript | Fabric `distill-image` → `Distilled` with `transcript = Some(vision+OCR concat)` | image note |
| `VoiceNote` | n/a (bytes in `attachments/`) | Groq Whisper transcription → plain text | Fabric `distill-voicenote` (short) or `distill-voicenote-chunk` + `distill-voicenote-reduce` (long, map-reduce) → `Distilled` with `transcript = Some(Groq output)` | voice note |
| `Idea` | n/a | `body.txt` passthrough | `IdeaDistiller` (no Fabric) → `Distilled` with `transcript = Some(input)` | idea note |
| `VocabularyEN` / `VocabularyES` | n/a | vocab body (Define / Clarify prose) | `IdeaDistiller` (no Fabric, degenerate) → `Distilled` with `transcript = Some(definition)` | vocab note |

As of Phase 9 every IngestKind flows through the `Distilled` contract (`vault::distilled::Distilled` — see [2026-05-16-extractor-contract-and-l2-summaries.md](2026-05-16-extractor-contract-and-l2-summaries.md) Phases 1-8 and [2026-05-16-extractor-contract-l2-phase-9-cleanup.md](2026-05-16-extractor-contract-l2-phase-9-cleanup.md) for the non-URL distillers + verbatim preservation contract). Non-URL kinds populate `Distilled.transcript = Some(...)` so the published note carries the raw extracted text below the LLM-distilled `## Summary` / `## Claims` / `## Links`; URL kinds leave `transcript: None` because the origin URL is the recoverable archive.

**`IngestKind` detection rules, evaluated top-down (first match wins). Classification is non-destructive: Stage 0 always keeps the full raw event; only the extractor + summarizer shape is selected by the match:**

1. **Explicit prefix on text payload.** If the message body begins with `vocab:en `, `vocab:es `, or `idea:`, the primary `IngestKind` is `VocabularyEN`, `VocabularyES`, or `Idea`. Attachments and URLs in the same message are still saved to `raw/<trace_id>/attachments/` and `raw/<trace_id>/fetched.*`; they are simply not the primary processing path.
2. **Content-type of the payload.** If the capture delivered binary bytes with a `Content-Type` of `image/*`, primary is `Image`; `audio/*` → `VoiceNote`. A caption (body text) alongside the binary is still saved to `raw/<trace_id>/body.txt` and fed to Stage 1's extractor (so the note includes the caption as provenance).
3. **URL domain + path match.** If the message body contains at least one URL: the first URL selects `GitHubUrl` / `YoutubeUrl` / `ThreadUrl` / `ArticleUrl` by domain as before. Additional URLs in the same message are saved but do not spawn additional traces automatically (the user can explicitly re-ingest them).
4. **Default.** Short plain text with no URL and no prefix falls through to `Idea`.

Multi-URL messages: Stage 0 saves the complete message body verbatim; Stage 1 extracts the primary URL. A future enhancement can fan out multiple URLs into sibling traces linked by `parent-trace:`; out of scope here.

### Replay UX

Replay is a new top-level CLI command:

```
borg replay <trace_id>                 # re-run all stages from raw
borg replay <trace_id> --from-stage 2  # keep stages 0,1; re-run 2,3
borg replay --since 7d                 # replay every trace in last 7d
borg replay --where 'domain=tech'      # replay every trace matching predicate
borg replay --rejected --since 24h     # replay everything gate-rejected
borg replay --kind ArticleUrl --since 7d --from-stage 1  # re-extract all articles
borg replay --dry-run <trace_id>       # show what would happen; no writes
borg replay --bootstrap-from-vault --note <note-path>   # seed stage-0 from a single vault note's frontmatter and replay
# bulk reingest of vault-quality-flagged notes: use cortex pipe (see Predicate grammar)
cortex lint --json | jq -r '.[] | select(.issues | includes("failed-fetch")) | .trace' | xargs -n1 borg replay --bootstrap-from-vault --note
```

`--from-stage N` reuses artifacts `< N` and regenerates `>= N`. Classification against the new vault schema (`--from-stage 3`) is cheap and requires no external calls; re-extraction (`--from-stage 1`) requires only the raw artifact; re-summarization (`--from-stage 2`) requires only the transcript.

`--bootstrap-from-vault` is the migration path for notes that predate staging: borg reads the vault note's frontmatter (`source:`, `trace:`, `method:`), synthesizes a stage-0 metadata sidecar, re-fetches the URL to populate `raw/<trace_id>.<ext>`, and runs the full pipeline from stage 1. No raw artifact is required going in; it is produced as a side effect. Without this flag, replay requires an existing raw artifact and fails cleanly if it has aged off.

**Predicate grammar (minimal):** `--where` evaluates against Stage-0 sidecar metadata only - it does not scan the vault. Supported ops: `=`, `!=`, `includes` (list membership), `>`, `<` (on timestamps). Supported keys: `domain`, `kind`, `method`, `gate`, `received-at`. A single key-op-value clause, or a conjunction joined by `AND`. Example: `--where 'kind=ArticleUrl AND domain=xda-developers.com'`. Full grammar (parentheses, OR, regex) deferred to Phase 6 if needed.

**Cross-domain queries (vault + staging) are not supported natively**, because Stage sidecars do not carry vault-derived fields like `cortex-quality-issues`. Use the cortex pipe pattern instead:

```
cortex lint --json | jq -r '.[] | select(.issues | includes("failed-fetch")) | .trace' \
  | xargs -n1 borg replay --bootstrap-from-vault --note
```

This keeps `cortex lint` as the single source of truth for vault-quality state, avoids duplicating that logic into a Stage 0 sidecar, and defers the SQLite index question until query volume actually demands it. A convenience subcommand `borg migrate reingest-failed` (declared later) wraps this exact pipe.

The existing `borg --force <url>` flag becomes sugar for `borg replay --from-stage 0` against the most recent trace for that URL.

### Data Model

**Envelope metadata** (`<trace_id>/envelope.yml`, written at Stage 0):

```yaml
trace: tg-26a031
kind: article-url
method: telegram
received-at: "2026-04-19T14:03:22Z"
origin-message-id: "123456"
# transport-specific envelope fields follow (chat_id, from_user, reply_to, etc.)
```

**Fetch metadata** (`<trace_id>/fetched.yml`, written at Stage 0 when a URL was fetched):

```yaml
source: "https://www.xda-developers.com/7-docker-containers/"
extractor: jina          # or: fabric-u | markitdown | browser-ua
status: 200
content-type: "text/html"
bytes: 299041
sha256: "e3b0c442..."
fallbacks-attempted: []
```

**Per-trace artifact path convention (Option B default):**

```
~/.local/share/borg/stages/
  tg-26a031/
    envelope.yml
    body.txt                   # message body (caption/prose/idea/vocab input)
    attachments/
      photo.jpg                # when the capture included binary
    fetched.html               # when a URL was fetched
    fetched.yml                # fetch metadata
    transcript.md              # Stage 1 output
    transcript.yml             # extractor used, fallbacks, token counts
    distilled.yml              # Stage 2 output (structured Distilled contract)
    rejection.yml              # gate, reason, artifact pointers (when rejected)
```

`rejection.yml` lives inside the per-trace directory (no separate `rejections/` top-level). The retention sweep filters by presence of this file to apply the longer rejected-retention window.

**Rejection record** (`<trace_id>/rejection.yml`):

```yaml
trace: tg-26a031
stage: 1
gate: block-page
reason: "anonymous access to domain blocked until 2026-04-20"
rejected-at: "2026-04-19T14:03:24Z"
raw-artifact: tg-26a031/fetched.html
source: "https://www.xda-developers.com/7-docker-containers/"
domain: xda-developers.com
blocklist-updated: true
retriable-after: "2026-04-20T00:00:00Z"
```

**Domain blocklist** (carried forward from the superseded doc) at `~/.local/share/borg/blocked-domains.yml`, unchanged from its original spec.

**Config additions** (`borg/src/config.rs`):

```rust
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StagingConfig {
    pub enabled: bool,                      // default: false until phase 3 ships; flipped true post-shakedown
    pub root: PathBuf,                      // default: ~/.local/share/borg/stages
    pub retention_days: u32,                // default: 60
    pub rejected_retention_days: u32,       // default: 90
    pub layout: StagingLayout,              // default: PerTrace (Option B)
    pub max_size_gb: u32,                   // default: 20; soft cap for disk-usage alerts
    pub size_alert_threshold_pct: u8,       // default: 80; fire alert when usage exceeds this pct of max_size_gb
    pub double_write: bool,                 // default: true during phases 3-4; false after phase 5 cutover
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StagingLayout {
    #[default]
    PerTrace,       // stages/<trace_id>/{envelope.yml,body.txt,attachments/*,fetched.*,transcript.*,distilled.yml}
    PerStage,       // stages/raw/<trace_id>/*, stages/transcripts/<trace_id>.md, stages/distilled/<trace_id>.yml, ...
}
```

See Storage Organization Options below for the tradeoffs driving the `layout` choice.

### Storage Organization Options

User asked explicitly for organization options to choose from. Three candidates, ordered by the author's current lean:

**Option B - Per-trace directories (recommended default)**

```
stages/
  tg-26a031/
    envelope.yml
    body.txt
    attachments/
      photo.jpg
    fetched.html
    fetched.yml
    transcript.md
    transcript.yml
    distilled.yml
```

- Pros: the full-payload raw model from Fix-1 (envelope + body + attachments + fetched.*) is natively a directory, so per-trace layout matches the data shape. `ls stages/tg-26a031/` tells the whole story. `rm -rf stages/tg-26a031/` deletes one trace cleanly. Directory mtime on POSIX updates when any file inside is written, so retention sweep is `find stages/ -mindepth 1 -maxdepth 1 -type d -mtime +60 -exec rm -rf {} \;` - one flat directory-level scan, no per-file mtime check.
- Cons: `ls stages/` grows proportional to trace count (10k+ directory entries eventually). Manageable on any modern filesystem; on ext4 / btrfs / zfs this is a non-issue well into six figures.

**Option A - Per-stage directories**

```
stages/
  raw/
    tg-26a031/     (same contents as the Option B root per-trace dir)
  transcripts/     tg-26a031.md      tg-26a031.yml
  summaries/       tg-26a031.md      tg-26a031.yml
  rejections/      tg-26a031.yml
```

- Pros: stage-level visibility (`ls transcripts/ | wc -l`) is trivial. Easy to back up or exclude specific stages from sync.
- Cons: reconstructing one trace requires walking four locations. The raw-as-directory model means Option A still has per-trace subdirectories inside `raw/` anyway, which undercuts the "one flat directory per stage" simplicity that was Option A's main selling point.

**Option C - Flat directory with stage-in-filename**

```
stages/
  tg-26a031.raw.html        (only viable if raw is a single file, which Fix-1 rules out)
  ...
```

- Pros: one directory to walk for any operation. Globbing works.
- Cons: incompatible with the full-payload raw model, since a single capture now includes an envelope, body, attachments, and a fetched response - not a single file. This option is no longer viable after Fix-1.

**Recommendation:** **B**, primary reasons: (1) the full-payload raw model is inherently directory-shaped, so per-trace layout is the natural fit; (2) directory-level `mtime` handles retention in one line with no per-file scan; (3) per-trace ergonomics (`ls stages/<trace_id>/`, `rm -rf stages/<trace_id>/`) are better for inspection and cleanup. Option A remains available via the `StagingLayout::PerStage` config flag for users who prefer stage-level views; Option C is retired.

### Retention

A `borg retention sweep` command (and a daemon task that runs it hourly) deletes stage artifacts older than `retention_days`. Default 60 days. Rejected traces are retained longer (default 90 days) so the user has a longer window to investigate failures. The vault `notes/` directory is never touched by retention.

```
borg retention sweep              # one-shot, respects config
borg retention sweep --dry-run    # report what would be deleted
borg retention status             # show artifact counts + disk usage per stage
```

A note in the vault that references an already-aged-off trace still functions normally (the vault is self-contained). Replay of such a trace fails with a clear error: "trace tg-26a031 not in staging (aged off 2026-02-18); cannot replay without raw artifact."

### Alerting

When a gate rejects content, borg sends an alert via the configured notifier. The first proposal: reuse the existing Telegram notifier (borg already holds a bot token) and send a message of the form:

```
[borg] stage-1 reject: xda-developers.com
trace tg-26a031 - "anonymous access blocked until 2026-04-20"
replay: borg replay tg-26a031 --from-stage 0 (after 2026-04-20)
```

**Aggregation** (to avoid notification spam on domain-wide outages):

- Within a 60-minute window, repeated rejections from the same domain and gate collapse to a count summary on the existing thread: `3x xda-developers.com blocked, first tg-26a031, last tg-26a099`.
- First rejection for a new domain always fires a fresh alert.
- Config knob: `alerts.per-domain-cooldown-minutes` (default 60).

An alternative channel is ntfy (the existing `ntfy.rs` supports it). Telegram is the preferred default because the user lives there already and alerts can land in the same chat as the original trigger. **Open question** - see Open Questions.

### Migration

The 28 existing bad notes retain their `source:` URL and `trace:` ID in frontmatter, so nothing is structurally lost. No raw artifact exists for these old traces - they predate staging - so migration must synthesize stage-0 from vault frontmatter. Migration steps:

1. Land phases 1–3 (artifact store + stage 0 + stage 1 extractor chain with browser-UA fallback). The UA fallback is the primary recovery mechanism - it bypasses Jina's IP-based block directly without waiting for the XDA lock to expire.
2. Land phase 7 (cortex `failed-fetch` quality issue) so notes are identifiable by frontmatter without relying on XDA-specific string matching.
3. Run cortex lint to stamp `cortex-quality-issues: [failed-fetch]` onto affected notes.
4. Run the cortex pipe: `cortex lint --json | jq -r '.[] | select(.issues | includes("failed-fetch")) | .trace' | xargs -n1 borg replay --bootstrap-from-vault --note`. Each affected note is re-fetched (Stage 0) through the new Stage-0 fetcher chain: Fabric-Jina → direct-Jina → browser-UA. For XDA specifically, the browser-UA path succeeds where Jina fails. Stage 1 then runs `markitdown` on the saved bytes offline.
5. If the browser-UA fallback is also blocked for some domain (rare - the failure mode is bot-IP detection, not UA detection), that trace gets recorded to the domain blocklist with `retriable-after` pointing at the block expiration. The user can re-run the migrate step after expiration.
6. Inspect results; promote successful reingests, delete failed ones.

A one-off `borg migrate reingest-failed` subcommand wraps steps 3–4 as a convenience. It is exactly equivalent to the cortex pipe shown above - it shells out to `cortex lint --json`, filters, and invokes `borg replay --bootstrap-from-vault --note` for each match, nothing more.

### Handling of the three specific fixes

These fold naturally into stage boundaries:

1. **Raw-content block detection before Fabric → Gate-1.** Pattern list from the superseded doc (`"anonymous access to domain"`, `"blocked until"`, `"SecurityCompromiseError"`, HTTP 451 from Jina). Runs on the raw artifact in stage 0 output, before stage 1 extraction writes to `transcripts/`.
2. **Browser-UA fetch fallback → Stage 1 extractor chain.** The extractor for `ArticleUrl` becomes: try Fabric-embedded Jina → try direct Jina → try markitdown → **try reqwest with `Mozilla/5.0` UA piped to markitdown-cli**. Sites that block bot IPs but not browser UAs (XDA) are recovered here.
3. **Failed-fetch quality gate → Gate-2** (pattern-based backstop) + **cortex/src/quality.rs enhancement** for notes that somehow land in the vault without going through stage gates (e.g. manually authored). Cortex adds a `failed-fetch` quality issue at `Critical` severity.

### API Design

**New CLI surface in `borg/src/cli.rs`:**

```rust
#[derive(Subcommand)]
pub enum Commands {
    // existing subcommands preserved...

    /// Replay the ingestion pipeline for existing traces
    Replay(ReplayArgs),

    /// Inspect staging artifacts
    Trace(TraceArgs),

    /// Manage staging retention
    Retention(RetentionArgs),

    /// Manage the domain blocklist (list / add / remove / retry-now)
    Blocklist(BlocklistArgs),
}

#[derive(Args)]
pub struct ReplayArgs {
    /// Specific trace ID to replay
    trace_id: Option<String>,

    /// Start replay at this stage (keep artifacts from earlier stages)
    #[arg(long, default_value_t = 0)]
    from_stage: u8,

    /// Replay all traces from the last N duration (e.g. "7d", "24h")
    #[arg(long)]
    since: Option<String>,

    /// Predicate filter (e.g. "domain=xda-developers.com", "kind=ArticleUrl").
    /// Supports `=`, `!=`, `includes`, `>`, `<`, joined by `AND`.
    #[arg(long)]
    r#where: Option<String>,

    /// Only replay rejected traces
    #[arg(long)]
    rejected: bool,

    /// Seed stage 0 from vault-note frontmatter when no raw artifact exists.
    /// Required for migrating pre-staging notes. Applies to each note
    /// targeted by the other selection flags (trace_id, --where, --since).
    #[arg(long)]
    bootstrap_from_vault: bool,

    /// Path to a specific vault note when replaying a single note via
    /// --bootstrap-from-vault. Optional when --where / --since select the set.
    #[arg(long)]
    note: Option<PathBuf>,

    /// Dry-run: print actions without executing
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
pub struct TraceArgs {
    /// Trace ID to inspect
    trace_id: String,

    /// Show full artifact contents, not just pointers
    #[arg(long)]
    full: bool,
}

#[derive(Args)]
pub struct BlocklistArgs {
    #[command(subcommand)]
    action: BlocklistAction,
}

#[derive(Subcommand)]
pub enum BlocklistAction {
    /// List all blocklisted domains with their retriable-after timestamps
    List,
    /// Add a domain to the blocklist with an optional retriable-after time
    Add { domain: String, #[arg(long)] retriable_after: Option<String> },
    /// Remove a domain from the blocklist
    Remove { domain: String },
    /// Force a retry now, ignoring retriable-after
    RetryNow { domain: String },
}
```

**New modules in `borg/src/`:**

- `stages/` (module directory): `mod.rs`, `raw.rs`, `extract.rs`, `summarize.rs`, `publish.rs`, `gate.rs`, `artifact.rs`. This is also the mechanism by which the 3073-line `pipeline.rs` decomposes naturally.
- `replay.rs`: implements `Replay` subcommand logic.
- `retention.rs`: implements `Retention` subcommand + daemon task.
- `blocklist.rs`: domain blocklist (from superseded doc).
- `classify.rs`: block-page detection (Gate-1).
- `inputs.rs`: `IngestKind` enum + dispatcher.

**Ports for testability** (shell/core split):

```rust
trait ArtifactStore {
    fn write_raw_envelope(&self, trace_id: &str, env: &Envelope) -> Result<()>;
    fn write_raw_body(&self, trace_id: &str, bytes: &[u8]) -> Result<()>;
    fn write_raw_attachment(&self, trace_id: &str, name: &str, bytes: &[u8]) -> Result<()>;
    fn write_raw_fetched(&self, trace_id: &str, bytes: &[u8], meta: &FetchMeta) -> Result<()>;
    fn read_raw(&self, trace_id: &str) -> Result<RawCapture>;          // whole-capture view
    fn write_transcript(&self, trace_id: &str, md: &str, meta: &TraceMeta) -> Result<()>;
    fn read_transcript(&self, trace_id: &str) -> Result<(String, TraceMeta)>;
    fn write_summary(&self, trace_id: &str, md: &str, meta: &TraceMeta) -> Result<()>;
    fn read_summary(&self, trace_id: &str) -> Result<(String, TraceMeta)>;
    fn list_traces(&self, filter: &TraceFilter) -> Result<Vec<TraceId>>;
    fn delete_trace(&self, trace_id: &str) -> Result<()>;
}

/// Stage 0 only: online fetch of a URL to bytes + metadata. Never called by Stage 1.
trait Fetcher {
    fn fetch(&self, url: &Url) -> Result<FetchResult>;
}

/// Stage 1: produces a transcript from bytes Stage 0 persisted. Must not reach the network.
trait Extractor {
    fn extract(&self, raw: &RawCapture) -> Result<Transcript>;
}

/// Stage 2: summarizes a transcript via Fabric/LLM. No network fetching of source URLs.
trait Summarizer {
    fn summarize(&self, transcript: &str, pattern: &str) -> Result<String>;
}
```

Production uses:
- `FsArtifactStore` (filesystem-backed, per-trace directories);
- `MultiFetcher` (chain: Jina → Fabric `-u` → markitdown → browser-UA) - **Stage 0 only**;
- `MarkitdownExtractor`, `GroqVisionExtractor`, `WhisperExtractor`, `PassthroughExtractor` (Stage 1, all offline from disk);
- `FabricSummarizer` (Stage 2).

Tests use `MemArtifactStore`, `FakeFetcher`, `FakeExtractor`, `FakeSummarizer`. The offline-replay contract is test-enforced by a `NoNetworkFetcher` stub at Stage 1: any Stage-1 test that attempts a network call fails the test.

### Implementation Plan

#### Phase 1 - Artifact store + trace metadata
**Model:** sonnet

- Add `stages/artifact.rs` with `ArtifactStore` trait + `FsArtifactStore` impl.
- Define `TraceMeta`, `StageKind`, `IngestKind` types in `types.rs`.
- Add `StagingConfig` + `StagingLayout` to `config.rs`.
- Tests against `MemArtifactStore`.
- No pipeline wiring yet; this is plumbing.

#### Phase 2 - Stage 0 (capture + fetch) + blocklist + Gate-0
**Model:** sonnet

- `stages/raw.rs`: writes the full capture event to `stages/<trace_id>/` (`envelope.yml`, `body.txt`, `attachments/*`).
- For URL-bearing captures, runs the Stage-0 fetcher chain (`MultiFetcher`: Jina → Fabric `-u` → browser-UA with `Mozilla/5.0` UA) and writes `fetched.html` + `fetched.yml`. This is where all current live-fetch code (`jina.rs`, `fabric::fetch_article`, a new `useragent.rs` for browser-UA) lives; after this phase none of them is called from Stage 1.
- HTTP 451 handling from any fetcher: immediate reject into blocklist (before Gate-0 on future traces).
- `blocklist.rs`: port spec from the superseded doc.
- Gate-0 wired into capture: reject URLs on domains in blocklist before any fetch is attempted.
- Wire from `routes.rs` and `telegram.rs` so every intake flows through stage 0.

**Fetcher-intercept mechanism (how double-write achieves one-fetch-per-ingestion):**

Use the `Fetcher` trait declared in the Ports section as the single call site, wrapped by an `FsCachingFetcher` decorator that writes bytes to disk as a side effect. The old pipeline stops calling `jina::fetch_article_markdown` / `fabric::fetch_article` directly and receives a `&dyn Fetcher` handle instead; during double-write the concrete type is `FsCachingFetcher<MultiFetcher>`, so every fetch persists to `stages/<trace_id>/fetched.*` before returning to the old pipeline. Stage 1 reads those same bytes from disk offline.

```rust
trait Fetcher {
    async fn fetch(&self, url: &Url) -> Result<FetchResult>;
}

struct MultiFetcher { /* Jina → Fabric -u → useragent chain */ }
impl Fetcher for MultiFetcher { ... }

struct FsCachingFetcher<F: Fetcher> {
    inner: F,
    store: Arc<FsArtifactStore>,
    trace_id: TraceId,
}
impl<F: Fetcher> Fetcher for FsCachingFetcher<F> {
    async fn fetch(&self, url: &Url) -> Result<FetchResult> {
        let result = self.inner.fetch(url).await?;
        self.store.write_raw_fetched(&self.trace_id, &result.bytes, &result.meta)?;
        Ok(result)
    }
}
```

Two invariants:
- **Atomic writes.** `write_raw_fetched` writes to `fetched.html.tmp` then renames to `fetched.html`. Crash mid-fetch never leaves a partial artifact that Stage 1 might read.
- **Cache all responses, including block pages.** `FsCachingFetcher` does not filter by status; a 451 block page lands in `fetched.html` with `status: 451` in `fetched.yml`. Gate-1 reads from disk and produces `rejection.yml` pointing at the saved `fetched.html` as the raw-artifact (matches the rejection-record sample in the Data Model).

Integration test asserts exactly one call to `Fetcher::fetch` per ingestion during double-write using a counting fake fetcher; this closes the risk-table commitment.

#### Phase 3 - Stage 1 (offline extract) + Gate-1
**Model:** opus

- `stages/extract.rs`: implements the `Extractor` trait for each `IngestKind`. All extractors read from `stages/<trace_id>/` on disk; **none perform network I/O**. Test fixture `NoNetworkFetcher` panics on any `Fetcher::fetch` call inside a Stage 1 test so this invariant is enforced.
  - `MarkitdownExtractor` (default for `ArticleUrl` / `GitHubUrl` / `ThreadUrl`): `markitdown < stages/<trace_id>/fetched.html` → `transcript.md`.
  - `YoutubeExtractor`: passthrough on `fetched.txt` (transcript was saved at Stage 0 via Fabric `-y`).
  - `ImageExtractor`: Groq vision / Tesseract on `attachments/*`.
  - `VoiceExtractor`: Whisper on `attachments/*`.
  - `PassthroughExtractor`: `body.txt` copy for `Idea`.
  - `VocabExtractor`: deferred (see NoteType::Vocabulary gate).
- `classify.rs`: block-page pattern detection reading from `stages/<trace_id>/fetched.html` + `fetched.yml` (absorbed from the superseded doc).
- Gate-1 integrated between stage 0 and stage 1 writes.

#### Phase 4 - Stage 2 (summarize) + Gate-2
**Model:** opus

- `stages/summarize.rs`: per-kind summarizer dispatch. Articles → Fabric `summarize` (existing); GitHub → Fabric `repo_summary` (new); YouTube → Fabric `summarize_video` (new); Thread → Fabric `summarize_thread` (new); Vocabulary → skip; Idea → skip or short pattern.
- Author the three new Fabric patterns in `borg/patterns/` and install to `~/.config/borg/patterns/` per the workspace install flow.
- Gate-2: paraphrase detection pattern list.
- Existing Fabric logic in `fabric.rs` becomes a strategy under `stages/summarize.rs`.

#### Phase 5 - Stage 3 (publish) + alerting
**Model:** sonnet

- `stages/publish.rs`: assembles note from summary + metadata, writes to vault. Incorporates existing classify/frontmatter logic.
- Alerting: Telegram notifier integration in `notify.rs`. Per-domain cooldown aggregation.
- Rejection records written at every gate fail; alert fires with replay hint.

#### Phase 6 - Replay + retention + migration
**Model:** opus

- `replay.rs`: implements `borg replay` with all documented flags. Delegates to individual stages via the `ArtifactStore` + stage functions.
- `retention.rs`: sweep command + daemon task. Separate retention for rejected vs successful.
- `borg migrate reingest-failed`: one-off to replay the 28 existing bad notes.

#### Phase 7 - Cortex failed-fetch quality gate
**Model:** sonnet

- `cortex/src/quality.rs`: add `failed-fetch` quality issue with pattern detection (`"only an error message"`, `"no actual content"`, `"error message indicating"`, `"Content inaccessible"`).
- Severity `Critical` → `QualityLevel::Low`. Reported in `cortex lint` output.
- `apply_quality` sets `cortex-quality-issues: [failed-fetch]` so the note is flagged in the vault.

#### Phase 8 - Cleanup and decomposition
**Model:** sonnet

- `pipeline.rs` (currently 3073 lines; `BLOAT_MAX_LINES` temporarily bumped to 3100 in `.otto.yml` to accommodate) decomposes into `stages/` module directory as natural consequence of phases 2–6. Residual glue that remains in `pipeline.rs` should fit under 1500 lines. If not, further split.
- `vault/src/search.rs` (1622 lines) similarly split into a `search/` module directory.
- **Exit criterion:** remove the `BLOAT_MAX_LINES: "3100"` override from `.otto.yml` so the 1500-line default applies, and verify `otto ci` passes green without it.
- Remove dead code from the old one-shot flow; retire the `staging.enabled` and `staging.double-write` flags (no longer needed once the old path is gone).
- Full integration test sweep across all input kinds.

## Alternatives Considered

### Alternative 1: Keep the single-shot pipeline; add more/better gates in place

- **Description:** Rather than restructure, add block-detection + UA-fallback + cortex failed-fetch gate to the existing `pipeline.rs`. This is what the superseded doc proposed.
- **Pros:** Much smaller change. No new concepts (stages, artifacts, replay). Lower migration risk.
- **Cons:** Doesn't enable replay. Doesn't preserve originals. When we discover a new class of failure (happening every few weeks), we either tolerate bad notes or reingest from the source, and the source may no longer be reachable. Doesn't address the one-pipeline-fits-all problem for vocabulary / ideas / etc.
- **Why not chosen:** The 28-note audit is evidence that failures keep outpacing our gates. Replay is what actually closes the loop: we need the ability to re-run ingestion as the tooling improves, not just add more pre-emptive checks.

### Alternative 2: Store staging artifacts inside the vault (in a hidden `.borg/` folder)

- **Description:** Use a dotfolder in the vault so Obsidian ignores it but artifacts are colocated with notes.
- **Pros:** Single sync target (iCloud / Syncthing / Git already watches the vault). One backup covers everything.
- **Cons:** User was emphatic this MUST be outside the vault. Also, vault sync volume explodes (large HTML, images, audio). Obsidian indexing can choke on huge dotfolders even when nominally ignored.
- **Why not chosen:** Explicit user constraint.

### Alternative 3: Per-stage directories (Option A in Storage Organization)

- **Description:** Default to Option A (per-stage directories) rather than Option B (per-trace).
- **Pros:** Stage-level visibility and counts are trivial (`ls transcripts/ | wc -l`). Easier to exclude a specific stage from backup.
- **Cons:** After Fix-1 the raw artifact is a directory (envelope + body + attachments + fetched), so Option A still nests per-trace subdirectories inside `raw/`. The "flat per-stage" simplicity that justified Option A originally no longer holds. Reconstructing one trace requires walking four locations.
- **Why not chosen:** Directory-mtime-driven retention works for both options with a single `find` line, so the earlier retention argument for Option A was wrong. Option B wins on natural fit with the full-payload raw model. Users who still prefer stage-level views can set `staging.layout: per-stage` in config.

### Alternative 4: SQLite metadata index instead of YAML sidecars

- **Description:** A single `stages/index.sqlite` with one row per (trace, stage) carrying metadata. Artifacts still on disk as files.
- **Pros:** Fast queries (`WHERE domain=...`, `WHERE received_at > ...`). Predicate filtering for replay is ~instant.
- **Cons:** Adds a runtime DB dependency. Index can desync from filesystem reality. YAML sidecars are `cat`-friendly and survive filesystem tooling.
- **Why not chosen:** Start with YAML sidecars + filesystem globbing for simplicity. If replay predicate performance becomes a bottleneck at scale (say >50k traces), revisit with SQLite. Until then, `grep` over YAML is fast enough and transparent.

### Alternative 5: LLM block-page classifier (Haiku) as proposed in the superseded doc

- **Description:** Use a few-shot Haiku classifier on raw content to detect block pages, as specified in the superseded 2026-03-30 doc.
- **Pros:** More general than patterns; resilient to novel block page formats.
- **Cons:** Another LLM call per ingestion. Cost + latency + API key dependency. Pattern-based Gate-1 is cheap and already catches the vast majority (XDA's string is literally `"anonymous access to domain"` - a regex catches it).
- **Why not chosen in Phase 1:** Patterns first. If Gate-1 false-negatives cluster around novel block page formats, add the Haiku classifier as a second Gate-1 check (pattern first, classifier on pattern-miss). This is explicitly deferred, not rejected.

## Technical Considerations

### Dependencies

New crates:
- `reqwest` (already in workspace, for browser-UA fetch)
- `chrono` (already in workspace)
- `sha2` (for artifact content hashing in sidecars - used for replay invalidation detection)

No net new external tools. `markitdown` already in the `markdown.rs` fallback chain.

### Performance

- **Capture → write-raw** adds one filesystem write per ingestion. Negligible (<1ms).
- **Stage sidecar writes** add ~3 small YAML writes per ingestion across all stages. Negligible.
- **Replay over a large window** (`--since 30d`) is potentially slow. For 30-day windows with ~500 traces, re-summarization is ~500 Fabric calls - significant. Mitigations: `--dry-run` first; batch / rate-limit built into the replay command (config knob `replay.max-concurrent`, default 4).
- **Retention sweep** is `find -mtime` over flat directories (Option A). O(n) filesystem walk, runs hourly, typically touches <100 files/sweep.

Staging disk usage estimate at 60-day retention:
- Articles: ~300KB/trace average raw HTML → ~15MB/day at 50/day → ~1GB at 60d.
- Images: highly variable; a voice note + photo-heavy day could hit 100MB; 60d → ~6GB.
- Transcripts + summaries: <50KB/trace → negligible.
- **Rough bound: 5–10GB at steady state.** Small compared to the vault itself; tolerable on the dev box but will need revisiting if the user's ingestion volume grows 10x.

### Security

- Staging directory permissions: `0700` (user-only read/write) since stored content can include private messages, clipboard data, OCR'd screenshots of sensitive UI, etc.
- No staging artifact is ever synced to a cloud vault (opt-in only, never automatic). Vault and staging must have separate sync configurations.
- Browser-UA fallback uses the same TLS + redirect policy as existing Jina/markitdown fetches. No credentials are sent. Add explicit `User-Agent: Mozilla/5.0 (X11; Linux x86_64)` only.

### Testing Strategy

- **Unit tests** per stage using `MemArtifactStore`, `FakeFetcher`, `FakeSummarizer`. Each gate has dedicated fixture tests (block-page raw, non-block raw, paraphrased-garbage summary, clean summary).
- **Replay tests** with pre-populated `MemArtifactStore` covering: replay from each stage, replay with missing earlier-stage artifact, replay of rejected traces.
- **Integration tests**: a small fixture library at `borg/tests/fixtures/` with one real captured raw for each `IngestKind`. End-to-end pipeline tests use these fixtures against the full stack with Fabric and LLM calls stubbed.
- **Smoke test in CI**: `borg --version`, `borg replay --help`. Nothing that requires network or API keys.

### Rollout Plan

1. Phases 1–2 land behind `staging.enabled: false` (`StagingConfig::enabled` default). The new code is present but inert.
2. Phase 3 ships; set `staging.enabled: true` and `staging.double-write: true` on the dev machine. **Double-write mode is write-through-with-read-reuse, not parallel independent fetch.** Mechanics: the old single-shot pipeline still owns note publishing, but any URL fetch it performs is intercepted and also written to `raw/<trace_id>/fetched.*` via the new `FsArtifactStore`. Stage 1 / Stage 2 read that saved artifact rather than fetching the URL a second time. Net external-fetch count per ingestion stays at one, not two, even while both pipelines are running. This is critical for IP-sensitive domains like XDA where a second concurrent fetch would trigger the same anti-bot that produced the original bug.
3. After one week of clean double-write on the dev machine (verified via `otto ci` green + manual spot-check of `raw/` artifacts against vault notes), phases 4–5 flip `staging.double-write: false`; the publish path switches to come from `stages/publish` output. The old single-shot path becomes a fallback guarded by `staging.enabled`.
4. Phases 6–7 ship replay + retention + cortex gate.
5. Phase 8 cleanup removes the old pipeline path entirely; the `enabled` and `double-write` flags are retired (replaced by "always on"). Exit criterion for Phase 8: `otto ci` passes with `BLOAT_MAX_LINES` removed from `.otto.yml` (currently bumped to 3100 to accommodate `pipeline.rs`; decomposition into `stages/` drops it back to the 1500 default).

No breaking changes for other workspace crates - `cortex` and `oracle` continue to read the vault unchanged. The only visible change to the vault is that more notes have accurate content (no more blocked-page garbage).

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Staging directory fills disk | Low | High | Retention daemon; `borg retention status` in dashboard; alert when usage exceeds `size-alert-threshold-pct` (default 80%) of `max-size-gb` (default 20GB). |
| Replay corrupts a note by writing a second copy | Med | High | Publish uses `trace_id` as filename stem with collision check; replay of a trace that already has a vault note atomically overwrites (preserving cortex frontmatter per existing reingest-domain-preservation behavior). |
| Alerting spam (domain-wide outage fires N alerts) | High without aggregation | Med | Per-domain aggregation window (default 60m). Global rate limit on Telegram messages. |
| Browser-UA fetch gets borg banned from sites | Low | Med | Use a realistic recent-Firefox UA; respect `robots.txt`; add honest delay between requests. Do not run concurrent UA fetches against the same domain. |
| Staged pipeline has more moving parts → more bugs | Med | Med | Phased rollout with double-write before cutover. Full integration test fixtures per `IngestKind`. |
| Double-write doubles external fetch load on IP-sensitive domains | Med | High (triggers the bug we're fixing) | Double-write is write-through, not parallel fetch: the old pipeline's fetch result is intercepted and written to `stages/<trace_id>/fetched.*`; new stages read from disk. Net fetch count per ingestion stays at 1. Integration test asserts exactly one call to `Fetcher::fetch` per ingestion during double-write. |
| Retention aging off a trace that's still needed for replay | Med | Low | Rejected traces retained 90d (vs 60d for successful). Graceful error message on replay-after-aging, with the user's current recourse (re-ask source) unchanged from today. |
| ~~Vocab pipeline needs a template cortex doesn't know about~~ **Resolved (Phase 9c-hotfix)** | - | - | Vocab routes through `DistillKind::Vocabulary` -> `IdeaDistiller` (degenerate path, no Fabric call); the definition prose becomes both `Distilled.summary` and `Distilled.transcript`. No `NoteType::Vocabulary` schema variant turned out to be required - the existing schema plus the `vocab` content-type discriminator on the published note is sufficient. |
| Migration replay wastes Fabric tokens on irrecoverable URLs | Low | Low | `borg migrate reingest-failed` respects Gate-1 and blocklist; XDA URLs that are still blocked get rejected for free and recorded to the blocklist, not sent to Fabric. |

## Open Questions

- [x] ~~**Storage layout (A/B/C).**~~ **Decided: Option B (per-trace directories)** after the architect review; directory mtime handles retention in one line, and the full-payload raw model is inherently directory-shaped. Option A remains available via `staging.layout: per-stage` config for users who prefer stage-level views.
- [ ] **Alerting channel.** Telegram (reuse existing notifier) vs ntfy vs both. Author leans Telegram as default with ntfy as config option.
- [ ] **Retention defaults.** 60 days for successful, 90 days for rejected - acceptable?
- [x] ~~**Vocabulary schema.**~~ **Resolved (Phase 9c-hotfix):** vocab ships via `DistillKind::Vocabulary` -> `IdeaDistiller` without requiring a new `NoteType::Vocabulary` variant. The existing schema with the `vocab-en` / `vocab-es` tag plus `cortex-thread-platform`-style frontmatter discriminator is sufficient.
- [x] ~~**Idea pipeline.**~~ **Resolved (Phase 9c-hotfix):** ideas go through `IdeaDistiller` - no Fabric call, just trim + link extraction + verbatim preservation in `Distilled.transcript`. Cortex's tagging/classification passes still run downstream on the published note.
- [ ] **Haiku classifier reinstatement.** When do we promote from pattern-only Gate-1 to pattern + Haiku? Suggest: when we see 3 novel block-page formats in the same month that patterns miss.

## References

- **Superseded:** [2026-03-30-llm-block-detection.md](2026-03-30-llm-block-detection.md) - block-detection + blocklist concepts absorbed as Gate-0/Gate-1.
- **Related:** [2026-03-30-reingest-domain-preservation.md](2026-03-30-reingest-domain-preservation.md) - cortex-field preservation on reingest; relevant to replay's publish step.
- **Related:** [2026-03-23-classify-pipeline-fix.md](2026-03-23-classify-pipeline-fix.md) - existing classify step becomes part of Stage 3 publish.
- **Audit:** 2026-04-19 vault audit identified 28 blocked-content notes; 16 from xda-developers.com.
- **Existing code:** `borg/src/pipeline.rs` (3073 lines - decomposition target); `borg/src/quality.rs` (existing pattern gate, becomes Gate-2 backstop); `borg/src/trace.rs` (trace_id generation, extended as join key); `cortex/src/quality.rs` (structural quality; adds `failed-fetch` issue type).
