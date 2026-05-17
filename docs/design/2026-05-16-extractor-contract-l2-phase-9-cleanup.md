# Design Document: L2 Extractor Contract - Phase 9 Completion

**Author:** Scott Idler
**Date:** 2026-05-16
**Status:** Implemented
**Review Passes Completed:** 2/5

**Amends:** [2026-05-16-extractor-contract-and-l2-summaries.md](2026-05-16-extractor-contract-and-l2-summaries.md) (the "L2 doc"). That doc currently carries `Status: Implemented`; this addendum reopens it on three deferred items surfaced by the post-implementation architect audit plus a verbatim-data-loss defect surfaced by the design-review architect pass. After Phase 9 lands, the L2 doc returns to `Status: Implemented` and Phase 9 is recorded as the completion step.

**Revision history:**

- **Rev 1 (initial draft):** Three sub-phases (9a thread, 9b github, 9c idea/image/voice/vocab) routed through `IdeaDistiller` / `PassthroughDistiller`.
- **Rev 2 (this doc):** Architect Round 1 design review found three blocking issues: (1) `IdeaDistiller`/`PassthroughDistiller` both hardcode `SUMMARY_CHAR_LIMIT = 280`, which would silently truncate Groq audio transcripts and Vision+OCR image text; (2) `TraceMeta` has no `source` field (Rev 1's 9a/9b pseudocode would not compile); (3) `RepoResponse`/`ReadmeResponse` are `Deserialize`-only and `fetch_readme` discards the wrapper, so Rev 1's "serialize-the-parsed-structs" envelope plan is unbuildable. Architect Round 2 (focused on 9c options) confirmed that the (A) "render raw transcript" and (B) "build real Image/VoiceNote distillers" approaches compose rather than fork: real distillers add structured summary/claims, but the raw transcript must still land in the published note or six-month-later Obsidian full-text search regresses. Rev 2 splits the work into five mergeable sub-phases that incorporate all four findings.

## Summary

Close four completeness gaps in the L2 Distilled rollout. Three were surfaced by the post-implementation architect audit; the fourth by the design-review architect pass on Rev 1 of this doc:

1. **Phase 6 thread documentation hygiene + Stage-1 transcript persistence.** Thread distiller's docstring still describes it as shadow-mode-only despite the post-Phase-6 cutover. Stage-0 `fetched.html` lands via the article-fetch chain; Stage-1 `transcript.md` does not.
2. **Phase 4 GitHub Stage 0/1 artifact persistence.** `distill_for_publish_repo` writes only `distilled.yml`, skipping the Stage-0 GitHub-API JSON response and Stage-1 rendered transcript.
3. **Phase 3 Idea/Image/VoiceNote/Vocabulary pipeline cutover.** Four non-URL processors still build a freeform `summary: String` and bypass `distillers::Dispatcher`. They never carry `distilled: true` frontmatter and never produce a `distilled.yml`.
4. **Distiller verbatim-data-loss defect (Architect Round 1).** `IdeaDistiller` (`distillers/src/idea.rs:13`) and `PassthroughDistiller` (`distillers/src/passthrough.rs:13`) both hardcode `SUMMARY_CHAR_LIMIT = 280`. A 13K-token Groq audio transcript routed through `PassthroughDistiller` today would have its `distilled.summary` truncated to 280 characters and the rest silently dropped at render time. The global `validate.rs` cap is 2000 chars (`MAX_SUMMARY_CHARS`), so the 280 is a distiller-specific tweet-length design choice that is structurally wrong for substantive audio and image content.

After Phase 9, every L2-in-scope IngestKind (`ArticleUrl`, `GitHubUrl`, `YoutubeUrl`, `ThreadUrl`, `Image`, `VoiceNote`, `Idea`, `VocabularyEn`, `VocabularyEs`) flows through a real distiller, produces a `distilled.yml`, and renders into a structured note body. The Distilled contract gains an optional `transcript: Option<String>` field so non-URL kinds can preserve their raw extracted text alongside the LLM-distilled summary and claims — the published note becomes both a search target (via `## Summary` / `## Claims`) and a verbatim archive (via `## Transcript`).

## Problem Statement

### Background

The L2 doc shipped in eight phases. Architect Round-1 (post-implementation audit) flagged three places where the doc's bullets and the merged code diverged. Architect Round-2 (design review of Rev 1 of this doc) flagged a fourth: routing Vision+OCR image content and Groq audio transcripts through the existing `PassthroughDistiller` would invoke its 280-char truncation. Each finding is verified against the codebase; together they shape Rev 2.

### Problem

#### 1. Thread path documentation and Stage-1 artifact gap

`borg/src/stages/distill.rs:430-486` is the live `distill_for_publish_thread` function. Its rustdoc opens with:

> Shadow-mode: run the thread distiller against the markdown rendered by the standard Stage-0 fetcher chain ... Fires-and-forgets - never blocks or affects the legacy path.

That is no longer true. `pipeline.rs:516-524` invokes `distill_for_publish_thread` directly inside the URL-pipeline cutover block and the returned `Distilled` is the source of truth for the published note. Shadow mode was retired during the post-Phase-6 cutover; the docstring did not move.

Separately, the L2 doc's Open Questions list still carries `[ ] Thread Stage 0 implementation status` even though the audit happened during Phase 6 (the rendered markdown was empirically sufficient for `distill-thread`).

For replay forensics: the article fetch chain already persists `fetched.html` + `fetched.yml` via `persist_fetched_if_staging`. The rendered thread markdown that gets fed to the distiller as `article_md` is never persisted as `transcript.md`. A future `borg replay --from-stage 2` cannot replay a thread distill without re-fetching.

#### 2. GitHub Stage 0/1 artifact persistence gap

`distill_for_publish_repo` (`borg/src/stages/distill.rs:262-324`) calls `GitHubFetcher::fetch_repo` and writes only `distilled.yml`. Neither the github-api JSON response nor the rendered repo transcript reaches `ArtifactStore::write_fetched` / `write_transcript`. A `borg replay` against a github trace sees stale article-path bytes that the repo distiller never consumed.

Implementation constraint surfaced by Architect Round 1: `RepoResponse` and `ReadmeResponse` (`borg/src/github.rs:106, 122`) derive only `Deserialize`. `fetch_readme` (line 205-223) discards the `ReadmeResponse` wrapper and returns `Result<String>` after base64 decoding. The "serialize the parsed structs into a JSON envelope" pattern proposed in Rev 1 is unbuildable. Rev 2 captures `response.bytes().await` *before* deserialization instead.

#### 3. Idea/Image/VoiceNote/Vocabulary pipeline cutover skipped

L2 doc's Rollout Plan Step 2 made Phase 3 responsible for flipping these branches onto the Distilled contract. Phase 3 shipped the article distiller; the four non-URL processors never followed. Today:

- `process_image_inner` (`pipeline.rs:1098-1288`) builds `summary: String` by concatenating `## Description` (vision API output) and `## Extracted Text` (OCR/vision merge). No `distilled: true`, no `## Summary` / `## Claims` / `## Links` structured body.
- `process_audio_inner` (`pipeline.rs:1343-1512`) builds `summary: String` as `## Transcript\n\n{transcription}`. Identical gap.
- `process_text_inner` general branch (`pipeline.rs:1850-1948`) builds `summary: text`. Identical gap.
- `process_vocab` (`pipeline.rs:1961-`) builds `summary: String` from definition prose. Identical gap.

Oracle's `index_vault` parses `## Summary` / `## Claims` from body sections to populate the FTS5 columns. Notes without these headings fall through to `detail::extract_summary` (legacy body-summary fallback). Non-URL notes are therefore invisible to the structured summary/claim columns until cutover lands, and carry no `distilled: true` skip-marker, so `cortex summarize --backfill` cannot distinguish them from genuinely-unprocessed legacy notes.

#### 4. Distiller verbatim-data-loss defect (Architect Round 1)

`IdeaDistiller` (`distillers/src/idea.rs:13`) and `PassthroughDistiller` (`distillers/src/passthrough.rs:13`) both contain:

```rust
const SUMMARY_CHAR_LIMIT: usize = 280;
```

Their `distill` impls (lines 38-42) truncate the transcript to 280 chars and assign the result to `distilled.summary`. Because `distillers::render` reads only `distilled.summary` for the `## Summary` body section (with empty `## Claims` for these no-LLM kinds), all transcript content beyond 280 chars is silently dropped before the note is written. The legacy non-URL paths today preserve the full content under `## Description` / `## Extracted Text` / `## Transcript` headings — Rev 1 of this doc would have **regressed** that by routing through these distillers as-is.

The global validation cap (`MAX_SUMMARY_CHARS = 2000` in `distillers/src/validate.rs:11`) is intentional schema protection. The 280-char per-distiller cap is a tweet-length design choice appropriate for trivial Idea inputs (a one-line thought) but structurally wrong for substantive audio and image content.

Architect Round 2 verdict, which Rev 2 adopts: (A) preserving the raw transcript and (B) building real LLM-driven distillers for Image/VoiceNote are not alternatives — they compose. The LLM distiller produces structured summary + claims for search and decay signals; the raw transcript is required so six-month-later Obsidian full-text search still finds the exact phrasing the user remembers. URL kinds don't need this because the source URL is recoverable; non-URL notes are the only persistent source.

### Goals

- Update `distill_for_publish_thread`'s docstring and mark L2 Open Question #1 Resolved.
- Persist `transcript.md` for thread URLs at the point the distiller is invoked.
- Persist `fetched.html` + `fetched.yml` (github-api JSON envelope, captured from raw `response.bytes()`) and `transcript.md` + `transcript.yml` (rendered repo transcript) inside `distill_for_publish_repo`.
- Delete `SUMMARY_CHAR_LIMIT = 280` from `IdeaDistiller` and `PassthroughDistiller`. Rely on the global 2000-char `MAX_SUMMARY_CHARS` for schema protection.
- Extend the `Distilled` contract with a `transcript: Option<String>` field. Render it as a `## Transcript` body section when present. URL distillers leave it `None`; non-URL distillers fill it with the raw extracted text.
- Build a real `ImageDistiller<F>` with a `distill-image` Fabric pattern (single-call; image transcripts are small enough that map-reduce is unnecessary).
- Build a real `VoiceNoteDistiller<F>` with map-reduce orchestration ported from `VideoDistiller<F>` (`distillers/src/video.rs:125-281`), plus three patterns: `distill-voicenote`, `distill-voicenote-chunk`, `distill-voicenote-reduce`. Audio transcripts from Groq routinely run tens of thousands of characters with no inherent timestamps.
- Cut over `process_image_inner`, `process_audio_inner`, the Idea-classified branch of `process_text_inner`, and `process_vocab` to call `distillers::Dispatcher`, render via `distillers::render`, and emit `distilled: true` frontmatter plus the structured body sections — identical in shape to URL-kind notes.
- Update the L2 doc: change `Status: Implemented` -> `Status: Implemented (Phase 9 deferred items)` while Phase 9 is in flight, then back to `Status: Implemented` once Phase 9 merges.

### Non-Goals

- New IngestKinds beyond what the L2 doc enumerates. Code and Document still bypass the distiller after Phase 9 (see Open Questions).
- Native thread JSON fetchers (X/Reddit/HN APIs). Phase 6's audit decision stands.
- A vocabulary-specific structured `KindPayload::Vocab` payload. Vocabulary uses `IdeaDistiller` with the raw definition preserved via the new `transcript` field.
- Cortex backfill changes. Phase 7's `cortex summarize --backfill` continues to walk only URL-kind notes; backfilling pre-Phase-9 non-URL notes is recorded as an Open Question.
- Changing the FTS5 schema. The `transcript` field renders into the body and is indexed via the existing FTS5 `body` column. No new column.

## Proposed Solution

### Overview

Phase 9 is five independently mergeable sub-phases. Each closes one or more findings.

```
 9a Thread cleanup (~40 LOC)
   ├── docstring fix on distill_for_publish_thread
   ├── Stage-1 write_transcript call (using correct TraceMeta fields)
   └── L2 Open Question #1 -> Resolved

 9b GitHub artifacts (~100 LOC)
   ├── GitHubFetcher::fetch_repo captures raw response bytes before deserialize
   ├── distill_for_publish_repo writes fetched.html + transcript.md
   └── extractor="github-api" / "github-render" disambiguates from article path

 9c-hotfix Verbatim preservation + Idea/Vocab cutover (~120 LOC)
   ├── DELETE SUMMARY_CHAR_LIMIT=280 from idea.rs and passthrough.rs
   ├── ADD Distilled.transcript: Option<String> field (vault::distilled)
   ├── EXTEND distillers::render to emit ## Transcript when Some
   ├── Idea/Vocab distillers populate transcript with raw input
   ├── Cut over process_text_inner (general) and process_vocab to distillers
   └── DistillKind::Vocabulary added; IngestKind translation wired

 9c-image Image distiller (~250 LOC)
   ├── borg/patterns/distill-image.md (single-call Fabric pattern)
   ├── distillers/src/image.rs - ImageDistiller<F>
   ├── Dispatcher gains pub image: ImageDistiller<F>
   ├── DistillKind::Image now routes to ImageDistiller (not Passthrough)
   ├── ImageDistiller fills Distilled.transcript with vision+OCR concat
   └── Cut over process_image_inner

 9c-voicenote VoiceNote distiller with map-reduce (~400 LOC)
   ├── borg/patterns/distill-voicenote.md (short path)
   ├── borg/patterns/distill-voicenote-chunk.md (map path)
   ├── borg/patterns/distill-voicenote-reduce.md (reduce path)
   ├── distillers/src/voicenote.rs - VoiceNoteDistiller<F> with distill_short/distill_long
   ├── Dispatcher gains pub voicenote: VoiceNoteDistiller<F>
   ├── DistillKind::VoiceNote now routes to VoiceNoteDistiller (not Passthrough)
   ├── VoiceNoteDistiller fills Distilled.transcript with Groq output
   └── Cut over process_audio_inner

 9d L2 doc resync
   └── add Phase 9 to L2 Implementation Plan; restore Status: Implemented
```

PassthroughDistiller remains in the codebase but its consumers are reduced to: nothing wired today. It can be removed in a follow-on or kept as a stub for genuinely trivial future kinds.

### Architecture

Two architectural invariants preserved:

- **One-way data flow** (vault file is canonical, oracle indexes from filesystem only). Phase 9 writes only to the staging dir and to the published markdown file. No SQLite writes are added.
- **Distilled is the cross-stage contract.** Every IngestKind that ships through Phase 9 produces a Distilled, persists it as `distilled.yml`, and renders it via `distillers::render`. The contract gains one optional field (`transcript`) but no breaking changes.

The contract extension is the key Rev-2 architectural decision. It is justified because non-URL kinds have no recoverable source outside the vault note itself. The Architect's Round-2 framing: "Distillation must augment the source material, not erase it."

### Data Model

#### `Distilled.transcript` field

```rust
// vault/src/distilled.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Distilled {
    pub summary: String,
    pub claims: Vec<Claim>,
    pub tags: Vec<String>,
    pub links: Vec<Link>,
    pub kind_specific: Option<KindPayload>,
    pub meta: DistilledMeta,

    /// Raw extracted text the distiller received as input. Preserved for
    /// kinds whose published note is the only persistent source (Image,
    /// VoiceNote, Idea, Vocabulary). URL kinds leave this `None` because
    /// the source URL is the recoverable archive.
    ///
    /// Rendered by `distillers::render` as a `## Transcript` body section
    /// when `Some`. Indexed by oracle's `index_vault` via the existing
    /// FTS5 `body` column (no new column).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}
```

Serialization conventions:
- `#[serde(default)]` so old `distilled.yml` files (without the field) deserialize cleanly as `None`.
- `skip_serializing_if` keeps URL-kind YAML unchanged (no `transcript: null` noise).
- Schema version bumps via `meta.extractor` suffix per L2 doc convention. URL-kind extractor ids (`distill-article-v1`, `distill-video-v1`, etc.) do not bump; only non-URL distillers gain new ids (`distill-image-v1`, `distill-voicenote-v1`, `distill-idea-v2` after the 280-cap removal).

#### Render output

The renderer emits `## Transcript` after `## Summary` / `## Claims` / `## Links` when `transcript.is_some()`. Heading is a single hardcoded string; per-kind labels (`## Extracted Text` for images, `## Spoken Transcript` for audio) are a polish consideration deferred to a follow-on if needed. One heading keeps `index_vault`'s body-section parser simple.

#### No FTS5 schema change

`transcript` is rendered into the published note body. The existing FTS5 `body` column already indexes the full body text. Oracle's `index_vault` does not need new parsing; it already indexes whatever `## Transcript` content lands in the body.

### API Design

#### 9a — Thread Stage-1 persistence

```rust
// borg/src/stages/distill.rs

pub async fn distill_for_publish_thread(
    fabric: &FabricConfig,
    staging: &StagingConfig,
    trace_id: &str,
    url: &str,
    thread_md: &str,
) -> Distilled {
    log::debug!(
        "distill_for_publish_thread: trace={trace_id} url={url} transcript_len={}",
        thread_md.len()
    );

    if staging.enabled {
        let store = FsArtifactStore::from_config(staging);
        // TraceMeta has no `source` field. The `pattern` field is the
        // closest semantic match: it records which Stage-1 shim produced
        // this transcript. URL of origin is recoverable from fetched.yml
        // (which the article-fetch chain already wrote).
        let meta = TraceMeta {
            extractor: "thread-markdown-shim".to_string(),
            ..TraceMeta::default()
        };
        if let Err(e) = store.write_transcript(trace_id, thread_md, &meta) {
            log::warn!("[{trace_id}] distill_for_publish_thread: persist transcript.md failed: {e:#}");
        }
    }

    // ... existing distillation path unchanged ...
}
```

The Rev-1 pseudocode `TraceMeta { extractor: ..., source: url }` was wrong; `TraceMeta` (`borg/src/types.rs:136-146`) has `extractor`, `fallbacks_attempted`, `token_count`, `pattern`, and (verify in `types.rs`) several optional fields, but no `source`. Origin URL is reconstructable from the trace's `fetched.yml` written upstream by `persist_fetched_if_staging`.

#### 9b — GitHub Stage 0/1 persistence

Approach: capture raw response bytes before deserialization. `RepoResponse` and `ReadmeResponse` stay `Deserialize`-only; no `Serialize` derive is added. The envelope is built from the raw bodies the API returned.

```rust
// borg/src/github.rs

pub struct RepoFetch {
    pub transcript: String,
    pub metadata: RepoMetadata,
    /// Raw GitHub-API JSON envelope: a UTF-8 JSON object with two top-level
    /// keys, `repo` and `readme`, whose values are the *unparsed* response
    /// bodies. Persisted as `fetched.html` by `distill_for_publish_repo`.
    /// Replay tooling that wants the structured shape parses this envelope
    /// with `serde_json::Value` (no internal struct dep required).
    pub raw_json: Vec<u8>,
}

impl GitHubFetcher {
    pub async fn fetch_repo(&self, owner: &str, repo: &str) -> Result<RepoFetch> {
        let repo_bytes = self.fetch_repo_meta_bytes(owner, repo).await?;
        let repo_meta: RepoResponse = serde_json::from_slice(&repo_bytes)
            .context("github: /repos parse failed")?;

        let readme_bytes = self.fetch_readme_bytes(owner, repo).await.ok();
        let readme_md = match &readme_bytes {
            Some(b) => decode_readme_from_bytes(b).unwrap_or_default(),
            None => String::new(),
        };

        // Build the envelope. Both halves are inlined as JSON values, not
        // re-serialized — preserves the API's exact byte sequence (whitespace,
        // field ordering, unknown fields) for forensic replay.
        let envelope = serde_json::json!({
            "repo": serde_json::from_slice::<serde_json::Value>(&repo_bytes).unwrap_or(serde_json::Value::Null),
            "readme": readme_bytes.as_deref()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
                .unwrap_or(serde_json::Value::Null),
        });
        let raw_json = serde_json::to_vec(&envelope)
            .context("github: envelope serialize failed")?;

        let metadata = RepoMetadata { /* ... from repo_meta ... */ };
        let transcript = render_transcript(&readme_md, &metadata);
        Ok(RepoFetch { transcript, metadata, raw_json })
    }

    async fn fetch_repo_meta_bytes(&self, owner: &str, repo: &str) -> Result<Vec<u8>> {
        let url = format!("{API_BASE}/repos/{owner}/{repo}");
        let req = self.client.get(&url).header("Accept", "application/vnd.github+json");
        let req = if let Some(t) = &self.token { req.header("Authorization", format!("Bearer {t}")) } else { req };
        let response = req.send().await.context("github: /repos request failed")?;
        if !response.status().is_success() {
            bail!("github /repos/{owner}/{repo} returned HTTP {}", response.status().as_u16());
        }
        Ok(response.bytes().await.context("github: /repos bytes failed")?.to_vec())
    }

    // fetch_readme_bytes is analogous.
}
```

Inside `distill_for_publish_repo`:

```rust
let fetch_result: RepoFetch = match GitHubFetcher::new().fetch_repo(&owner, &repo).await {
    Ok(r) => r,
    Err(e) => { /* existing fallback */ }
};

if staging.enabled {
    let store = FsArtifactStore::from_config(staging);
    let fetched_meta = FetchMeta {
        source: url.to_string(),
        extractor: "github-api".to_string(),
        status: 200,
        content_type: Some("application/json".to_string()),
        bytes: fetch_result.raw_json.len() as u64,
        sha256: sha256_hex(&fetch_result.raw_json),
        fallbacks_attempted: Vec::new(),
    };
    if let Err(e) = store.write_fetched(trace_id, &fetch_result.raw_json, &fetched_meta) {
        log::warn!("[{trace_id}] distill_for_publish_repo: persist fetched.html failed: {e:#}");
    }
    let trace_meta = TraceMeta {
        extractor: "github-render".to_string(),
        ..TraceMeta::default()
    };
    if let Err(e) = store.write_transcript(trace_id, &fetch_result.transcript, &trace_meta) {
        log::warn!("[{trace_id}] distill_for_publish_repo: persist transcript.md failed: {e:#}");
    }
}
```

#### 9c-hotfix — Verbatim preservation + Idea/Vocab cutover

Three coupled changes:

1. **Delete the 280-char caps.** `distillers/src/idea.rs:13` and `distillers/src/passthrough.rs:13`: delete `const SUMMARY_CHAR_LIMIT`. Delete the truncation branches at lines 38-42 of each file. Both distillers now copy the full trimmed transcript into `distilled.summary` (subject to the global 2000-char `MAX_SUMMARY_CHARS` enforced by `validate::enforce_bounds`). Bump `IdeaDistiller`'s `ID` to `distill-idea-v2`; `PassthroughDistiller`'s `ID` stays at `distill-passthrough-v1` for now since 9c-image and 9c-voicenote will replace its consumers.

2. **Add `Distilled.transcript`.** `vault/src/distilled.rs`: append the field per the Data Model section above. `IdeaDistiller::distill` and `PassthroughDistiller::distill` populate `transcript: Some(full_input_text)`. URL distillers (Article, Repo, Video, Thread) continue to leave it `None` — they are explicitly modified to construct `Distilled { transcript: None, ..fields }`.

3. **Extend `distillers::render`.** `distillers/src/render.rs` (or wherever the `render` function lives): after emitting `## Links`, if `distilled.transcript.is_some()`, emit:

   ```markdown
   ## Transcript

   {transcript_text}
   ```

   Snapshot tests assert exact byte equivalence with fixtures. Heading text is `## Transcript` for all kinds in Rev 2.

Pipeline cutover for `process_text_inner` general branch and `process_vocab`:

```rust
// borg/src/stages/distill.rs

pub async fn distill_for_publish_idea(
    fabric: &FabricConfig,
    staging: &StagingConfig,
    trace_id: &str,
    transcript: &str,
    title_hint: Option<&str>,
) -> Distilled {
    log::debug!(
        "distill_for_publish_idea: trace={trace_id} transcript_len={} title_hint={title_hint:?}",
        transcript.len()
    );
    let stage = DistillStage::from_fabric_config(fabric);
    let started = std::time::Instant::now();
    let distilled = match stage.distill(IngestKind::Idea, transcript, None, title_hint).await {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[{trace_id}] distill_for_publish_idea: dispatch error: {e:#}; using fallback");
            // IdeaDistiller emits distill-idea-v2 on success after 9c-hotfix.
            distillers::fallback_distilled("distill-idea-v2", "dispatch-error", transcript, None)
        }
    };
    /* persist + log identical to existing distill_for_publish_repo helper */
    distilled
}

// distill_for_publish_vocab is analogous; accepts IngestKind so VocabularyEn
// vs VocabularyEs flows through to DistillStage.
```

`DistillKind` gains a `Vocabulary` variant routed to `IdeaDistiller`. `IngestKind::VocabularyEn | VocabularyEs => DistillKind::Vocabulary` at the call site.

Call-site shape in `pipeline.rs::process_text_inner` general branch (mirroring `pipeline.rs:549-617`):

```rust
let distilled = crate::stages::distill::distill_for_publish_idea(
    &config.fabric, &config.staging, trace_id, text, Some(&title),
).await;

let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
all_tags.extend(distilled.tags.iter().map(|t| hygiene::sanitize_tag(t)));
if use_fabric && let Ok(fabric_tags) = fabric::generate_tags(&distilled.summary, &config.fabric).await {
    all_tags.extend(fabric_tags.into_iter().map(|t| hygiene::sanitize_tag(&t)));
}
finalize_tags(&mut all_tags, config).await;

let rendered = distillers::render(&distilled);
let note = NoteContent {
    title: title.clone(),
    tags: all_tags.clone(),
    summary: distilled.summary.clone(),
    distilled_body: Some(rendered.body_markdown),
    frontmatter_additions: rendered.frontmatter_additions,
    /* ... */
};
```

Why `summary: distilled.summary.clone()` rather than `String::new()`: URL kinds (`pipeline.rs:617`) keep the field populated because downstream `IngestResult` callers and ledger entries surface a one-line preview from it. Non-URL helpers follow the same convention.

#### 9c-image — `ImageDistiller<F>`

New file: `distillers/src/image.rs`. Mirrors the existing structure of `distillers/src/repo.rs` (a Fabric-backed distiller with a single-call shape).

```rust
// distillers/src/image.rs

const PATTERN: &str = "distill-image";
const ID: &str = "distill-image-v1";

#[derive(Debug, Clone, Default)]
pub struct ImageConfig {
    /* mirror RepoConfig: model selection, max-output-tokens, etc. */
}

#[derive(Debug, Clone)]
pub struct ImageDistiller<F: FabricCaller + Clone> {
    pub fabric: F,
    pub config: ImageConfig,
}

#[async_trait]
impl<F: FabricCaller + Clone + Send + Sync> DistillExtractor for ImageDistiller<F> {
    fn id(&self) -> &'static str { ID }

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled> {
        log::debug!(
            "ImageDistiller::distill: transcript_len={} title_hint={:?}",
            inputs.transcript.len(), inputs.title_hint
        );

        // Single Fabric call. Image transcripts (vision description + OCR
        // concat) almost never exceed an LLM's context window — Architect
        // Round-2 confirmed a branching path is unnecessary.
        let raw = self.fabric.call(PATTERN, inputs.transcript, &self.config.into_call_config()).await?;
        let mut distilled = parse_distill_yaml(&raw)
            .unwrap_or_else(|e| {
                log::warn!("ImageDistiller: yaml parse failed: {e}; using fallback");
                fallback_distilled(ID, "yaml-parse-error", inputs.transcript, inputs.title_hint)
            });

        // Preserve the raw input as the transcript so the published note
        // is a verbatim archive even after LLM distillation.
        distilled.transcript = Some(inputs.transcript.to_string());

        validate::enforce_bounds(&mut distilled);
        Ok(distilled)
    }
}
```

New file: `borg/patterns/distill-image.md`. Fabric pattern instructs the LLM to:
- Synthesize a coherent summary from the input (which is a concatenation of `## Description` from Vision API and `## Extracted Text` from OCR).
- Extract claims as bulleted statements (anchors `None`; no timestamps in images).
- Extract any URLs found in the OCR text into `links`.
- Apply canonical tags.
- Output strict YAML matching the existing `parse_distill_yaml` shape.

Dispatcher (`distillers/src/dispatcher.rs`) gains:

```rust
pub struct Dispatcher<F: FabricCaller + Clone> {
    /* ... existing fields ... */
    pub image: ImageDistiller<F>,
    pub voicenote: VoiceNoteDistiller<F>,
}

// In Dispatcher::distill:
DistillKind::Image => self.image.distill(inputs).await,
DistillKind::VoiceNote => self.voicenote.distill(inputs).await,
```

The `DistillKind::Image | DistillKind::VoiceNote => self.passthrough.distill(inputs).await` arm is deleted.

Pipeline cutover for `process_image_inner` mirrors the `process_text_inner` shape above with `distill_for_publish_image` and `IngestKind::Image`.

#### 9c-voicenote — `VoiceNoteDistiller<F>` with map-reduce

New file: `distillers/src/voicenote.rs`. Verbatim ports the structural template of `distillers/src/video.rs:125-281` (verified during design):
- `const PATTERN_SHORT: &str = "distill-voicenote";`
- `const PATTERN_CHUNK: &str = "distill-voicenote-chunk";`
- `const PATTERN_REDUCE: &str = "distill-voicenote-reduce";`
- `const SHORT_TRANSCRIPT_TOKEN_THRESHOLD: usize = 8000;` (mirror video's threshold)
- `async fn distill_short(&self, transcript: &str) -> Result<Distilled>` — single Fabric call against `distill-voicenote`.
- `async fn distill_long(&self, transcript: &str) -> Result<Distilled>` — chunk via `video::chunk_transcript` (or a copy of its logic), parallel `distill-voicenote-chunk` calls bounded by `borg.fabric.max-concurrent`, then a single `distill-voicenote-reduce` call against the concatenated chunk summaries.

**Key structural difference vs. video:**
- Audio chunks have no native timestamps. YouTube provides VTT cues that the video distiller stitches into `Claim.anchor`. Groq's default response is plain text; Groq's verbose-json format does provide segment-level timestamps but the current `process_audio_inner` consumes only `.text`. Phase 9 keeps the current Groq usage; voicenote claims have `anchor: None`. A follow-on phase can switch Groq to verbose-json and thread timestamps through, but that's out of scope.
- The chunk-reduce stage drops anchors structurally (no merging by timestamp). `parse_reduce_yaml`-equivalent for voicenote produces a flat claim list.

```rust
// distillers/src/voicenote.rs

impl<F: FabricCaller + Clone + Send + Sync> VoiceNoteDistiller<F> {
    pub async fn distill_inner(&self, transcript: &str) -> Result<Distilled> {
        let token_estimate = transcript.chars().count() / 4;  // same heuristic as video
        let mut distilled = if token_estimate <= SHORT_TRANSCRIPT_TOKEN_THRESHOLD {
            self.distill_short(transcript).await?
        } else {
            self.distill_long(transcript).await?
        };
        // Preserve the full Groq transcript regardless of which path ran.
        distilled.transcript = Some(transcript.to_string());
        Ok(distilled)
    }

    async fn distill_short(&self, transcript: &str) -> Result<Distilled> {
        let raw = self.fabric.call(PATTERN_SHORT, transcript, &self.config.into_call_config()).await?;
        parse_distill_yaml(&raw)
            .or_else(|_| Ok(fallback_distilled(ID, "yaml-parse-error", transcript, None)))
    }

    async fn distill_long(&self, transcript: &str) -> Result<Distilled> {
        let chunks = chunk_transcript(transcript, CHUNK_TARGET_TOKENS);
        let chunk_summaries = parallel_distill_chunks(&self.fabric, &chunks, &self.config).await;
        let merged = chunk_summaries.into_iter().filter_map(|r| r.ok()).collect::<Vec<_>>();
        if merged.is_empty() {
            log::warn!("VoiceNoteDistiller: all chunks failed; using map-reduce fallback");
            return Ok(fallback_distilled(ID, "all-chunks-failed", transcript, None));
        }
        let reduce_input = format_chunks_for_reduce(&merged);
        let reduce_raw = self.fabric.call(PATTERN_REDUCE, &reduce_input, &self.config.into_call_config()).await?;
        parse_reduce_yaml(&reduce_raw)
            .or_else(|_| Ok(concat_fallback_from_chunks(merged)))
    }
}
```

Three new pattern files: `borg/patterns/distill-voicenote.md`, `distill-voicenote-chunk.md`, `distill-voicenote-reduce.md`. Authored against the same prompt structure as the `distill-video-*` patterns, with the timestamp-extraction instructions stripped out.

Pipeline cutover for `process_audio_inner`:

```rust
let transcript_text = transcription.as_ref().map(|t| t.text.clone()).unwrap_or_default();

let distilled = crate::stages::distill::distill_for_publish_voicenote(
    &config.fabric, &config.staging, trace_id, &transcript_text, Some(&title),
).await;

/* same tag-merge + render + NoteContent pattern as 9c-hotfix */
```

The legacy `summary = "## Transcript\n\n{transcription}"` and `summary = ""` branches are deleted — the full transcript now lands in `distilled.transcript` and renders as `## Transcript` via the renderer.

### Stage placement

- 9a: write at the entry of `distill_for_publish_thread` before the dispatch call.
- 9b: write immediately after `GitHubFetcher::fetch_repo` returns, inside `distill_for_publish_repo`, before the `DistillStage::distill` call.
- 9c-hotfix: changes are inside the distillers crate (vault::distilled, idea.rs, passthrough.rs, render.rs) and at the pipeline call sites for text-general and vocab.
- 9c-image and 9c-voicenote: new distillers consume the entry path; cutover happens at the pipeline call site for image and audio.

### Validation and failure modes

All Phase 9 writes (`write_transcript`, `write_fetched`, `write_distilled_yml`) are best-effort with `WARN` on failure — never block ingestion. This matches the existing Phase 6 pattern in `distill_for_publish_video`.

For 9c-image and 9c-voicenote the Fabric-call failures fall through to `fallback_distilled(ID, "dispatch-error", transcript, title_hint)`. The fallback path still populates `transcript: Some(transcript.to_string())` so the published note retains the raw text even when the LLM call dies.

`enforce_bounds` continues to cap `summary` at 2000 chars and `claims` at 10 items globally. The new `transcript` field is uncapped at the schema level — long Groq transcripts can be hundreds of KB. The published markdown file's size is the operational bound. Snapshot tests assert that a 50K-char synthetic transcript renders correctly without truncation.

### Long-transcript handling

- Image: single-call. The vision+OCR concatenation is bounded by `process_image_inner`'s existing extraction (~few KB max).
- VoiceNote: map-reduce path ports the video distiller's structure. Threshold `SHORT_TRANSCRIPT_TOKEN_THRESHOLD = 8000` (mirror video).
- Idea/Vocab: no LLM call; the full input flows straight to `distilled.transcript` and `summary` (subject to the global 2000-char cap on the latter).

### Backfill plan

Phase 7's `cortex summarize --backfill` walks notes with URL sources today. Phase 9 lands `distilled: true` on newly ingested Image/Audio/Text-Idea/Vocabulary notes. Legacy non-URL notes ingested before Phase 9 will have no `distilled` flag; they remain on the legacy body-summary fallback in `index_vault`.

Out of scope: a `--kind <image|voicenote|idea|vocab>` flag on `cortex summarize --backfill` for retro-distillation. Recorded as Open Question.

### Implementation Plan

#### Phase 9a — Thread docstring + Stage-1 transcript persistence

**Model:** sonnet
**Est:** ~40 LOC
**Depends on:** none

- Rewrite the rustdoc on `distill_for_publish_thread` (`borg/src/stages/distill.rs:430-486`) to drop "shadow-mode-only" framing.
- Add `store.write_transcript(trace_id, thread_md, &meta)` gated on `staging.enabled`. Use `TraceMeta { extractor: "thread-markdown-shim", ..TraceMeta::default() }` — no `source` field exists on `TraceMeta`.
- Update L2 doc Open Question #1 to Resolved.
- Test: integration test that a thread URL ingestion produces `transcript.md` + `transcript.yml` in the staging dir with `extractor: thread-markdown-shim`.

#### Phase 9b — GitHub Stage 0/1 artifact persistence

**Model:** sonnet
**Est:** ~100 LOC
**Depends on:** none

- Refactor `GitHubFetcher::fetch_repo` (`borg/src/github.rs:170-189`) to capture raw response bytes via `response.bytes().await` before deserializing. Split `fetch_repo_meta` / `fetch_readme` into `_bytes` variants. Build the JSON envelope from raw bytes via `serde_json::Value` round-trip. Do NOT add `Serialize` to `RepoResponse` / `ReadmeResponse`.
- Extend `RepoFetch` with `pub raw_json: Vec<u8>`.
- Inside `distill_for_publish_repo`, after the fetch succeeds and gated on `staging.enabled`: build `FetchMeta { extractor: "github-api", content_type: Some("application/json"), ... }` and call `store.write_fetched`; build `TraceMeta { extractor: "github-render", ..default() }` and call `store.write_transcript`. WARN on each failure; never block.
- Test: integration test that a github URL ingestion produces `fetched.html` (with `{"repo": ..., "readme": ...}` JSON), `fetched.yml` with `extractor: github-api`, `transcript.md`, `transcript.yml`, and `distilled.yml`.

#### Phase 9c-hotfix — Verbatim preservation + Idea/Vocab cutover

**Model:** opus
**Est:** ~120 LOC
**Depends on:** none (composes cleanly with 9a/9b)

- `vault/src/distilled.rs`: add `pub transcript: Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Update existing distillers (Article/Repo/Video/Thread) to explicitly construct with `transcript: None` so unintended omission is caught at compile time.
- `distillers/src/idea.rs`: delete `SUMMARY_CHAR_LIMIT` const and the truncation branch. Set `distilled.transcript = Some(trimmed.to_string())`. Bump `ID` to `distill-idea-v2`.
- `distillers/src/passthrough.rs`: delete `SUMMARY_CHAR_LIMIT` const and the truncation branch. Set `distilled.transcript = Some(trimmed.to_string())`. ID stays `distill-passthrough-v1` (consumers retire in 9c-image and 9c-voicenote).
- `distillers/src/render.rs` (or the render entry point): after the existing `## Links` emission, if `distilled.transcript.is_some()`, emit `## Transcript\n\n{text}\n`.
- `distillers/src/dispatcher.rs`: add `DistillKind::Vocabulary` variant. Add `Self::Vocabulary => "vocabulary"` to `as_str`. Add `DistillKind::Vocabulary => self.idea.distill(inputs).await` to the dispatch match. Update crate-level docstring to drop "Vocabulary remains outside the contract" caveat.
- Borg: extend `IngestKind` -> `DistillKind` translation with `VocabularyEn | VocabularyEs => DistillKind::Vocabulary`.
- Add `borg/src/stages/distill.rs` helpers `distill_for_publish_idea` and `distill_for_publish_vocab`, mirroring `distill_for_publish_repo`'s logging/persistence shape. Fallback extractor ID `distill-idea-v2` for both.
- Cut over `pipeline.rs::process_text_inner` general branch (after the `ContainsUrl`/`Define`/`Clarify`/code-snippet redirects) to `distill_for_publish_idea`.
- Cut over `pipeline.rs::process_vocab` to `distill_for_publish_vocab` (passing through `IngestKind::VocabularyEn` or `VocabularyEs`).
- Tests:
  - Unit: `IdeaDistiller` against a 5000-char input produces `distilled.summary` length ≤ 2000 (global cap) and `distilled.transcript` length = 5000.
  - Unit: `distillers::render` of `Distilled { transcript: Some(_), .. }` emits `## Transcript` block exactly once.
  - Snapshot: rendered output for an Idea note byte-equivalent to a fixture.
  - Integration: ingest a 1000-char text note; assert vault note carries `distilled: true` frontmatter, `## Summary` body section (≤2000 chars), and `## Transcript` body section with all 1000 chars preserved.

#### Phase 9c-image — `ImageDistiller<F>` + cutover

**Model:** opus
**Est:** ~250 LOC
**Depends on:** 9c-hotfix (needs `Distilled.transcript`)

- Author `borg/patterns/distill-image.md`. Input shape: concatenated `## Description` + `## Extracted Text` block. Output shape: same YAML schema as existing distill patterns. Instruct LLM to synthesize summary + extract URLs as `links` + apply canonical tags.
- New `distillers/src/image.rs`. Mirror `distillers/src/repo.rs`'s structure (single-call distiller with `ImageConfig`, `ImageDistiller<F>`, `impl DistillExtractor`). Populate `distilled.transcript = Some(inputs.transcript.to_string())`.
- `distillers/src/dispatcher.rs`: replace `DistillKind::Image | DistillKind::VoiceNote => self.passthrough.distill(inputs).await` with `DistillKind::Image => self.image.distill(inputs).await,` (VoiceNote remains routed to passthrough until 9c-voicenote lands; this is the transition state).
- Add `pub image: ImageDistiller<F>` to `Dispatcher` struct and constructor.
- Add `borg/src/stages/distill.rs::distill_for_publish_image` helper.
- Cut over `pipeline.rs::process_image_inner` to call the helper, render via `distillers::render`, populate `NoteContent { summary: distilled.summary.clone(), distilled_body: Some(rendered.body_markdown), frontmatter_additions: rendered.frontmatter_additions, .. }`. Delete the legacy `summary = format!("## Description\n\n...\n\n## Extracted Text\n\n...")` concatenation.
- Tag merge order in `process_image_inner` after cutover: user-supplied tags → hard-coded `"image"` → `distilled.tags` → `vision.suggested_tags` → fabric-tags driven by `distilled.summary` → `finalize_tags`.
- Tests:
  - Unit: `ImageDistiller` with `FakeFabric` returning valid YAML produces correctly-shaped `Distilled` including transcript.
  - Unit: malformed Fabric output falls back, transcript still preserved.
  - Integration: ingest a fixture image; vault note has `distilled: true`, `distill-image-v1` extractor id, structured body, and `## Transcript` section.

#### Phase 9c-voicenote — `VoiceNoteDistiller<F>` with map-reduce + cutover

**Model:** opus
**Est:** ~400 LOC
**Depends on:** 9c-hotfix (needs `Distilled.transcript`); 9c-image is parallelizable but conventionally lands after for risk shaping

- Author three patterns: `borg/patterns/distill-voicenote.md` (short path; for transcripts < ~8K tokens), `borg/patterns/distill-voicenote-chunk.md` (map step), `borg/patterns/distill-voicenote-reduce.md` (reduce step). Use `borg/patterns/distill-video*.md` as the structural template; strip timestamp-handling instructions.
- New `distillers/src/voicenote.rs`. Mirror `distillers/src/video.rs:125-281`:
  - `SHORT_TRANSCRIPT_TOKEN_THRESHOLD = 8000`
  - `distill_short` and `distill_long` methods.
  - `parallel_distill_chunks` bounded by `borg.fabric.max-concurrent`.
  - `parse_reduce_yaml` analog for voicenote shape.
- Reuse `chunk_transcript` from `distillers/src/video.rs:439` if exported, or copy it. (Refactor to a shared `text-chunking` module is a follow-on, out of scope here.)
- `distillers/src/dispatcher.rs`: replace `DistillKind::VoiceNote => self.passthrough.distill(inputs).await` with `DistillKind::VoiceNote => self.voicenote.distill(inputs).await`. PassthroughDistiller now has zero consumers but is kept in the crate for potential future trivial-input kinds.
- Add `pub voicenote: VoiceNoteDistiller<F>` to `Dispatcher`.
- Add `borg/src/stages/distill.rs::distill_for_publish_voicenote` helper.
- Cut over `pipeline.rs::process_audio_inner` to call the helper. Delete the legacy `summary = format!("## Transcript\n\n{transcript_text}")` block.
- Tests:
  - Unit: `VoiceNoteDistiller` short path with `FakeFabric` returning valid YAML.
  - Unit: `VoiceNoteDistiller` long path with synthetic 12K-token transcript; assert chunks dispatched, reduce called, transcript preserved verbatim.
  - Unit: all-chunks-failed fallback path preserves transcript.
  - Integration: ingest a fixture short audio (< 8K tokens) and a fixture long audio (> 8K tokens); both produce correct vault notes with `## Transcript` matching the input.

#### Phase 9d — L2 doc resync

**Model:** sonnet
**Est:** ~10 LOC of docs
**Depends on:** 9a–9c-voicenote landed

- Append `### Phase 9 - Deferred-items cleanup + verbatim preservation` to the L2 doc's Implementation Plan, with a pointer to this doc and a one-sentence summary per sub-phase.
- Update L2 Rollout Plan Step 2: replace "Phase 3 takes on the Idea/Image/VoiceNote pipeline flip" future-tense framing with "Final cutover landed in Phase 9 (see 2026-05-16-extractor-contract-l2-phase-9-cleanup.md)."
- Restore `Status: Implemented` on the L2 doc.

## Alternatives Considered

### Alternative 1: Route non-URL kinds through PassthroughDistiller as-is (Rev 1's plan)

- **Description:** Reuse `PassthroughDistiller` and `IdeaDistiller` for Image/VoiceNote/Idea/Vocab without changes. No new patterns, no new distillers, no contract extension.
- **Pros:** Smallest change. Three sub-phases instead of five.
- **Cons:** Architect Round 1 verified that both distillers truncate to 280 chars at `distillers/src/{idea,passthrough}.rs:13`. Multi-paragraph Groq transcripts and Vision+OCR text would be silently destroyed at render time. This is the defect Rev 2 exists to fix.
- **Why not chosen:** Data loss regression. Rev 1's plan was unshippable.

### Alternative 2: Render raw transcript only, no LLM distillation (Architect option A)

- **Description:** Extend `Distilled.transcript` and render it, but skip the LLM-driven Image/VoiceNote distillers. Image/Audio synthesize a degenerate Distilled (transcript filled, summary = transcript truncated to 2000 chars).
- **Pros:** Smaller scope — three sub-phases (9a, 9b, transcript-only 9c). No new Fabric patterns.
- **Cons:** No structured `## Claims`, no LLM-distilled `## Summary`, no canonical tag extraction. Image/Audio notes become second-class for FTS5 claim search and decay signals (Doc 3) compared to URL kinds.
- **Why not chosen:** Architect Round 2 verdict: (A) and (B) compose. Rev 2 ships both because the cost delta is small and the structured artifact is required for parity with URL kinds.

### Alternative 3: Skip transcript field; rely on staging artifacts for replay

- **Description:** Don't add `Distilled.transcript`. The full Groq output and OCR text already land in `transcript.md` in the staging directory (which Phase 9a+9b extend to cover thread and github). Search for the verbatim phrase would query the staging store.
- **Pros:** Smaller contract footprint. No body-shape change for non-URL kinds.
- **Cons:** Staging directory is gitignored, not synced to the vault, not searched by Obsidian, not searched by oracle's FTS5. The user's "six months later, what did I say in that voice note?" workflow goes through Obsidian over the vault. Staging artifacts are forensic, not user-facing.
- **Why not chosen:** Architect Round 2 explicitly identified this as the failure mode: "If you use Fabric to summarize a VoiceNote, it will extract claims and generate a clean `## Summary`. But if the user searches Obsidian 6 months later for the exact phrase someone said in that audio, they won't find it."

### Alternative 4: Per-kind heading labels (`## Extracted Text` for images, `## Spoken Transcript` for audio)

- **Description:** The renderer reads `kind_specific` (or a new dedicated field) to pick the heading label per kind.
- **Pros:** Marginally better human readability — "transcript" reads awkwardly for an image.
- **Cons:** Requires the renderer to branch on kind, complicates `index_vault`'s body-section parser, and adds polish work that doesn't change behavior.
- **Why not chosen:** Deferred. A single `## Transcript` heading is sufficient for Rev 2; per-kind labels can land in a follow-on if the awkwardness bites.

### Alternative 5: Roll all of Phase 9 into a single commit

- **Description:** Merge 9a/9b/9c-hotfix/9c-image/9c-voicenote/9d as one PR.
- **Pros:** "Ain't leaving any shit deferred" in one merge.
- **Cons:** ~900 LOC across multiple crates. Code review, bisection, and operational rollback all get harder. The 9c-hotfix data-loss fix should land independently and early.
- **Why not chosen:** Sub-phases ship independently. 9c-hotfix in particular should land before 9c-image and 9c-voicenote (it owns the contract extension they depend on).

## Technical Considerations

### Dependencies

None added. All work inside existing crates against existing APIs. `serde_json::Value` round-trip in 9b uses an existing workspace dep.

### Performance

- 9a: one extra file write per thread ingestion (transcript.md ~10-50 KB). Negligible.
- 9b: two extra file writes per github ingestion. Negligible.
- 9c-hotfix: one extra optional field in Distilled YAML. Negligible.
- 9c-image: adds one Fabric call per image ingestion. Vision+OCR transcripts are small (~few KB), so this is a single low-latency call.
- 9c-voicenote: short-path audio adds one Fabric call (~5-10s); long-path audio adds map-reduce overhead identical to YouTube videos today (a 60-minute meeting ≈ 13K tokens ≈ 2 chunks ≈ 3 Fabric calls). At Sonnet pricing, a long voicenote costs ~$0.05.

Operationally: voicenote distillation budgeted at ~5-15 ingestions/week, so monthly Fabric spend increases by < $5. Within tolerance per the L2 doc's cost table.

### Security

No new network surface. GitHub API calls already use `GITHUB_TOKEN` if present. The github JSON envelope written to `fetched.html` may contain repo description/topics with user-controlled text — but those bytes already render into the published note transcript today; Phase 9b only moves the bytes to a different staging slot.

### Testing Strategy

Per sub-phase, listed in Implementation Plan. Three cross-cutting tests after all sub-phases land:

- **End-to-end smoke**: ingest one of each kind (article, github, youtube, thread, image, audio, idea, vocab-en) via the borg CLI; every trace dir contains `distilled.yml`; non-URL traces additionally contain a vault note with `## Transcript` matching the input verbatim.
- **Regression hunt**: `grep -rn "SUMMARY_CHAR_LIMIT" distillers/` returns no hits. `git grep -n "shadow-mode-only" borg/` returns no hits in `distill.rs`. `git grep -n 'TraceMeta { source:' borg/` returns no hits.
- **Verbatim preservation**: ingest a synthetic 50K-char text note; verify `distilled.yml`'s `transcript` field length = 50K and the published vault note body length ≥ 50K.

### Rollout Plan

Each sub-phase independently mergeable:

1. **9a lands** (~40 LOC): docstring + transcript persistence + correct `TraceMeta` usage. No behavior change for distilled output. L2 Open Question #1 resolved.
2. **9b lands** (~100 LOC): `RepoFetch.raw_json` via raw-bytes capture; `distill_for_publish_repo` writes fetched + transcript artifacts. No behavior change for distilled output.
3. **9c-hotfix lands** (~120 LOC): The data-loss-fix priority. `SUMMARY_CHAR_LIMIT` deleted; `Distilled.transcript` added; renderer extended; `IdeaDistiller`/`PassthroughDistiller` consumers fixed; Idea/Vocab cut over. Behavior change: newly ingested text-idea and vocab notes carry `distilled: true` + structured body + `## Transcript`. `IdeaDistiller` ID bumped to `distill-idea-v2`.
4. **9c-image lands** (~250 LOC): real Image distiller + Fabric pattern + cutover. Behavior change: image notes carry structured summary/claims + `## Transcript` (verbatim vision+OCR text).
5. **9c-voicenote lands** (~400 LOC): real VoiceNote distiller with map-reduce + three patterns + cutover. Behavior change: audio notes carry structured summary/claims + `## Transcript` (verbatim Groq output).
6. **9d lands**: L2 doc resync.

After every merge: `cargo install --path borg && systemctl --user restart borg` (per the L2 doc's daemon-restart contract; oracle does not need restart).

Existing legacy non-URL notes are unchanged. The body-summary fallback in `index_vault` continues to index them.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `Distilled.transcript` adds noise to URL-kind YAML files | Low | Low | `#[serde(skip_serializing_if = "Option::is_none")]` keeps URL-kind `distilled.yml` unchanged. |
| `## Transcript` heading in vault notes confuses users who expect summary-only | Low | Low | Heading is below `## Summary` / `## Claims` / `## Links`. Standard markdown structure. Optional polish: per-kind heading labels (Alternative 4). |
| ImageDistiller's single Fabric call too slow for screenshot-heavy ingestion bursts | Low | Med | `borg.fabric.max-concurrent` already caps parallelism. Image ingestions are ~daily at most. |
| VoiceNote map-reduce chunk boundaries split mid-claim, producing duplicate/incomplete claims | Med | Med | `chunk_transcript` (ported from video) splits at sentence boundaries. Reduce step deduplicates structurally. Same risk as YouTube videos today; no new mitigation needed. |
| Groq transcription quality varies; bad transcripts produce bad distillations | Med | Low | Existing fallback path (transcript present but distill fails) preserves the full transcript under `## Transcript`. The note remains searchable verbatim even when LLM distillation fails. |
| `transcript: None` slipped accidentally on a new URL distiller addition | Low | Low | URL distillers (Article/Repo/Video/Thread) explicitly construct `Distilled { transcript: None, ..fields }` so a future contributor sees the field at the construction site. |
| 9c-image breaks Obsidian image-asset embedding (the existing `Image { asset_path }` ContentType wraps in `![[asset]]`) | Low | Med | Cutover preserves `ContentType::Image { asset_path }`. The image embed stays in `NoteContent.asset_path`; only the body-summary text moves to `distilled_body`. |
| Long-audio Fabric cost spikes during a one-off ingestion burst | Low | Low | Voicenote ingestion volume is low (~5-15/week). At ~$0.05/long-call, even a 100-voicenote burst is $5. |
| Idea/Vocab regress for users who relied on the 280-char tweet-length summary | Low | Low | The 280 cap was an implementation detail, not a documented behavior. Removal is a fix, not a regression. New behavior: full text in `## Transcript`, ≤2000-char summary in `## Summary`. |
| `index_vault` body-section parser misattributes content under user-edited `## Transcript` heading | Low | Low | Existing risk per L2 doc: parser is anchored on exact heading text. User edits to `## Transcript` flow into the index via the next reindex pass; that is the intended workflow. |
| Map-reduce reduce step produces a `Distilled` with `transcript: None` (because `parse_reduce_yaml` is concerned with summary/claims only) | Med | Med | `distill_inner` in `VoiceNoteDistiller` explicitly sets `distilled.transcript = Some(transcript.to_string())` *after* the distill_short/distill_long call returns — guarantees verbatim preservation regardless of which path ran. Unit test asserts this. |
| Pattern drift: `distill-image` and `distill-voicenote` produce empty `claims: []` consistently | Med | Med | Empty-claims canary already exists per L2 doc Validation item 8. WARN log per occurrence; surfaceable via `grep WARN`. Quality iterated on pattern files without blocking the cutover. |

## Open Questions

- [ ] **Per-kind transcript heading labels.** Single `## Transcript` heading for Rev 2; revisit if "transcript" reads awkwardly for image OCR or short Idea text. Polish issue, not contract issue.
- [ ] **Native thread JSON fetchers (potential Phase 10).** If `distill-thread` quality regresses, implement native X/Reddit/HN API fetchers. Lever to pull only if telemetry justifies it.
- [ ] **Code and Document kinds.** `process_code_snippet` and `process_document_file_inner` bypass the distiller. Not L2 in-scope per the L2 doc's Non-Goals. A follow-on phase would promote them to L2 kinds with their own distillers (Code: passthrough-shaped; Document: extract-then-distill via Fabric).
- [ ] **Vocabulary-specific structured payload.** Phase 9c-hotfix routes Vocabulary through `IdeaDistiller` (degenerate). A future enhancement could give Vocabulary its own structured `KindPayload::Vocab { word, definition, examples, etymology }` if query patterns surface that need the fields.
- [ ] **Backfill for legacy non-URL notes.** `cortex summarize --backfill` walks URL-source notes only. If a use case emerges, grow `--kind <image|voicenote|idea|vocab>` and re-distill the pre-Phase-9 cohort. Small follow-on.
- [ ] **Groq verbose-json for voicenote timestamps.** Current `process_audio_inner` consumes Groq's `.text` field. Switching to verbose-json would unlock per-segment timestamps as `Claim.anchor`s, matching the video distiller's anchor model. Out of scope here; tracked as a future enhancement.
- [ ] **PassthroughDistiller retirement.** After 9c-image and 9c-voicenote land, `PassthroughDistiller` has zero consumers. Keep as a stub (in case a future trivial-input kind emerges) or delete in a 9e cleanup? Defer the call.

## References

- [2026-05-16-extractor-contract-and-l2-summaries.md](2026-05-16-extractor-contract-and-l2-summaries.md) - parent L2 design doc
- [2026-04-19-staged-ingestion-pipeline.md](2026-04-19-staged-ingestion-pipeline.md) - Stage 0/1/2 artifact contract
- Architect Round 1 (post-implementation audit, 2026-05-16) - identified the three deferred items
- Architect Round 2 (design review of Rev 1, 2026-05-16) - identified the 280-char truncation defect and the (A)+(B) composition verdict
- `borg/src/stages/distill.rs` - `distill_for_publish_thread` (9a), `distill_for_publish_repo` (9b), new helpers for 9c-*
- `borg/src/pipeline.rs` - `process_image_inner` / `process_audio_inner` / `process_text_inner` / `process_vocab` (9c)
- `borg/src/github.rs` - `GitHubFetcher::fetch_repo` (9b raw-bytes refactor)
- `borg/src/stages/artifact.rs` - `write_fetched` / `write_transcript` primitives reused by 9a/9b
- `vault/src/distilled.rs` - `Distilled.transcript` field (9c-hotfix)
- `distillers/src/dispatcher.rs` - `DistillKind` gains `Vocabulary` (9c-hotfix); routing updates for Image/VoiceNote (9c-image, 9c-voicenote)
- `distillers/src/idea.rs` / `passthrough.rs` - 280-char cap deletion (9c-hotfix)
- `distillers/src/render.rs` - `## Transcript` emission (9c-hotfix)
- `distillers/src/video.rs:125-281` - map-reduce template ported by `VoiceNoteDistiller` (9c-voicenote)
- `distillers/src/validate.rs` - `MAX_SUMMARY_CHARS = 2000` (global cap retained as schema protection)
