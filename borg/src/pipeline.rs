use crate::assets;
use crate::config::Config;
use crate::description;
use crate::extraction;
use crate::fabric;
use crate::hygiene;
use crate::jina;
use crate::ledger::{self, LedgerEntry};
use crate::markdown::{self, ContentType, NoteContent};
use crate::ocr;
use crate::receipts;
use crate::retention;
use crate::router;
use crate::trace;
use crate::transcription::TranscriptionClient;
use crate::types::{AudioFormat, ContentKind, IngestMethod, IngestResult, IngestStatus};
use crate::youtube;
use eyre::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;
use vault::canonical::{self, CanonicalTagsFile, TagMapping};
use vault::paths::expand_tilde;
use vault::receipts::FailureStage;
use vault::schema::CORTEX_PRESERVE_KEYS;

pub mod atomic;
mod inflight;
pub mod permits;
use atomic::{apply_cortex_fields, apply_ingested_date, apply_original_date, write_atomic};
use inflight::InflightGuard;

mod handlers;
mod publish;
mod session;
mod tags;
mod text;
pub(crate) use handlers::*;
pub use publish::*;
pub(crate) use session::*;
pub(crate) use tags::*;
pub(crate) use text::*;

/// Cached canonical tag state loaded once at first use.
pub(crate) struct CanonicalState {
    canonical_set: std::collections::HashSet<String>,
    mapping: TagMapping,
    max_per_note: usize,
    reject_concatenated: bool,
}

pub(crate) struct YouTubeResult {
    title: String,
    /// Structured distillation produced by the video distiller. After the
    /// post-Phase-6 cutover this replaces the legacy prose summary; the
    /// caller renders it via `distillers::render` into the published note.
    distilled: vault::distilled::Distilled,
    content_type: ContentType,
    description: String,
    yt_tags: Vec<String>,
    /// Slide manifest + LLM-shaped output when frame-aware ingestion ran
    /// successfully and produced a non-text-only shape. Stage 3 publish
    /// happens in `process_url_inner` so the slug is known.
    slide_payload: Option<SlidePayload>,
}

#[derive(Debug)]
pub(crate) struct SlidePayload {
    manifest: crate::slides::SlideManifest,
    summary: crate::slides::SummaryOutput,
    /// Directory the slide JPEGs were materialized into; resolves the
    /// manifest's relative `frame_path` for the publish step.
    slides_source_root: PathBuf,
}

/// Splice the distilled sections BELOW a slide-published body (Phase 7,
/// defect #2). The slide body is the user-visible value of the slide pipeline
/// and stays first; the distilled `## Summary` / `## Claims` / `## Links` /
/// `## Transcript` sections (`rendered_distilled.body_markdown`) are APPENDED
/// beneath it rather than replacing it, so slide-published videos regain
/// claims-FTS text and transcript-chunk embeddings (which `index_vault` parses
/// out of the body). Some slide-vs-summary redundancy in these notes is
/// accepted; reach wins.
///
/// A blank line separates the two blocks so the appended `##` heading opens its
/// own paragraph. An empty `distilled_body` leaves the slide body untouched
/// (no trailing empty section).
pub(crate) fn append_distilled_below_slides(mut slide_body: String, distilled_body: &str) -> String {
    if distilled_body.trim().is_empty() {
        return slide_body;
    }
    if !slide_body.is_empty() {
        if !slide_body.ends_with('\n') {
            slide_body.push('\n');
        }
        if !slide_body.ends_with("\n\n") {
            slide_body.push('\n');
        }
    }
    slide_body.push_str(distilled_body);
    slide_body
}

/// Compose the slide-published note body subject to the `distill.slide-append`
/// toggle. When `append` is true the distilled `## Summary`/`## Claims`/
/// `## Links`/`## Transcript` sections are spliced beneath the slide body
/// (Phase 7 behavior); when false the slide body stands alone (pre-Phase-7
/// slide-only shape).
pub(crate) fn compose_slide_body(slide_body: String, distilled_body: &str, append: bool) -> String {
    log::debug!(
        "compose_slide_body: append={append} distilled_len={}",
        distilled_body.len()
    );
    if append {
        append_distilled_below_slides(slide_body, distilled_body)
    } else {
        slide_body
    }
}

/// Resolve the operator capture note subject to the `distill.capture-note`
/// toggle. When enabled the trimmed non-empty note is returned (it then feeds
/// the distiller context, `## Why Captured`, and the `capture-note:`
/// frontmatter key); when disabled it is dropped at this single seam so none
/// of the three are produced.
pub(crate) fn gate_capture_note(note: Option<&str>, enabled: bool) -> Option<&str> {
    if !enabled {
        return None;
    }
    note.map(str::trim).filter(|s| !s.is_empty())
}

/// Merge distiller-proposed candidate tags into the tag pipeline subject to the
/// `distill.propose-tags` toggle. When disabled only the proposed-tag merge is
/// skipped; the canonical filter and every other tag source (operator tags,
/// yt-dlp tags, fabric tags) are unaffected.
pub(crate) fn merge_proposed_tags(all_tags: &mut Vec<String>, proposed: &[String], enabled: bool) {
    log::debug!(
        "merge_proposed_tags: enabled={enabled} proposed_count={}",
        proposed.len()
    );
    if enabled {
        all_tags.extend(proposed.iter().map(|t| hygiene::sanitize_tag(t)));
    }
}

/// Hard ceiling on the final composed note body (2026-07-07
/// distillation-output-restore, Phase 3: see `config::MAX_NOTE_BYTES` for the
/// measurement behind the default). Checked just before the atomic write, on
/// the exact bytes about to hit disk. Returns the failure reason when
/// `note_bytes` exceeds `max_bytes`; `None` otherwise. Pulled out as a pure
/// function - mirroring `quality::detect_blocked_content` - so the boundary
/// is unit-testable without a live publish.
pub(crate) fn note_size_gate(note_bytes: usize, max_bytes: usize) -> Option<String> {
    if !crate::config::within_note_size_ceiling(note_bytes, max_bytes) {
        Some(format!(
            "Composed note is {note_bytes} bytes, exceeding the configured ceiling of {max_bytes} bytes \
             (pipeline.max-note-bytes) - likely a verbatim transcript leak"
        ))
    } else {
        None
    }
}

/// Top-level pipeline entry point. Dispatches to type-specific handlers based on content kind.
/// If `trace_id` is provided, it is used as-is; otherwise one is generated internally.
pub async fn process_content(
    content: ContentKind,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: Option<String>,
    // Operator capture note for the non-URL kinds (Signal attachment captions).
    // URL captures carry their note inside `ContentKind::Url { note }`; this
    // param is the carrier for attachment/text kinds whose variant has no note
    // field. `None` for every transport except the Signal attachment path.
    attachment_note: Option<String>,
) -> IngestResult {
    let trace_id = trace_id.unwrap_or_else(|| trace::generate(method));
    log::info!("[{trace_id}] Starting ingest: method={method}");

    // Register the trace as active *before* any await so the watchdog sees
    // it as live during the permit wait. Drop on scope exit (including
    // panic-unwind and future-cancel) removes the entry.
    let _active_guard = permits::ActiveTraceGuard::acquire(&trace_id);

    // Acquire the general permit. Every trace passes through this cap.
    log::debug!("process_content[{trace_id}]: acquiring general permit");
    let _general_permit = permits::GENERAL_PERMITS.acquire().await;
    log::debug!("process_content[{trace_id}]: general permit acquired");

    // Stage-0 rejection must flow through the SAME terminal-write chokepoint
    // as every other outcome (below). Returning early here left the receipts
    // row stuck in `received`, which the watchdog then mislabeled `crashed`
    // ~31 min later with the wrong stage. So set the failure result and fall
    // through instead of early-returning.
    let stage0 = crate::stages::raw::stage_0_init(config, &content, method, &trace_id);

    // Gate: distill.capture-note for the attachment/text kinds. The URL path
    // gates its own `note` inside `process_url_inner`; this drops the
    // attachment caption at the door when the toggle is off so it reaches
    // neither the distiller context nor the rendered note.
    let attachment_note = if config.distill.capture_note { attachment_note } else { None };

    // Every handler runs under the pipeline hard timeout. `process_url`
    // applies its own equivalent timeout internally; the non-URL handlers
    // previously awaited unbounded, so a wedged handler (e.g. a no-timeout
    // vision reqwest::Client, a blocked ffmpeg) held its GENERAL permit
    // forever, and the watchdog's active-trace exclusion skipped it - enough
    // wedged traces silently deadlocked all ingest.
    let mut result = if let Err(err) = stage0 {
        let reason = format!("{err:#}");
        log::warn!("[{trace_id}] Stage-0 rejected: {reason}");
        IngestResult {
            status: IngestStatus::Failed { reason },
            trace_id: Some(trace_id.clone()),
            method: Some(method),
            failure_stage: Some(FailureStage::IntakeRejected),
            ..Default::default()
        }
    } else {
        match content {
            ContentKind::Url { url, note } => process_url(&url, note, tags, method, force, config, &trace_id).await,
            ContentKind::Image { data, filename } => {
                with_hard_timeout(
                    process_image(
                        &data,
                        &filename,
                        tags,
                        method,
                        force,
                        config,
                        &trace_id,
                        attachment_note,
                    ),
                    config,
                    &trace_id,
                    method,
                    "image",
                )
                .await
            }
            ContentKind::Pdf { data, filename } => {
                with_hard_timeout(
                    process_document_file(
                        &data,
                        &filename,
                        tags,
                        method,
                        force,
                        config,
                        DocumentKind::Pdf,
                        &trace_id,
                        attachment_note,
                    ),
                    config,
                    &trace_id,
                    method,
                    "pdf",
                )
                .await
            }
            ContentKind::Audio { data, filename } => {
                with_hard_timeout(
                    process_audio(
                        &data,
                        &filename,
                        tags,
                        method,
                        force,
                        config,
                        &trace_id,
                        attachment_note,
                    ),
                    config,
                    &trace_id,
                    method,
                    "audio",
                )
                .await
            }
            ContentKind::Text(text) => {
                with_hard_timeout(
                    process_text(&text, tags, method, force, config, &trace_id),
                    config,
                    &trace_id,
                    method,
                    "text",
                )
                .await
            }
            ContentKind::Document { data, filename } => {
                with_hard_timeout(
                    process_document_file(
                        &data,
                        &filename,
                        tags,
                        method,
                        force,
                        config,
                        DocumentKind::Document,
                        &trace_id,
                        attachment_note,
                    ),
                    config,
                    &trace_id,
                    method,
                    "document",
                )
                .await
            }
            // Harvest-clyde-sessions design, Phase 5: the harvest publish
            // runner (`harvest::publish::publish_thread`) already
            // selected/clustered/fetched everything - this handler distills,
            // renders, and publishes.
            ContentKind::Session {
                body,
                members,
                primary_id,
                body_truncated,
            } => {
                with_hard_timeout(
                    process_session(
                        &body,
                        &members,
                        &primary_id,
                        body_truncated,
                        tags,
                        method,
                        force,
                        config,
                        &trace_id,
                    ),
                    config,
                    &trace_id,
                    method,
                    "session",
                )
                .await
            }
        }
    };
    result.trace_id = Some(trace_id.clone());
    // Close out the receipts row. This is the single chokepoint where the
    // terminal outcome lands: every successful path writes a `succeeded` row
    // and every failure path writes a `failed` row with its stage. The
    // receipts DB is the sole authoritative ingest-state store; the legacy
    // markdown DLQ was removed (see
    // docs/design/2026-06-03-receipts-log-legacy-markdown-excision.md).
    record_terminal_to_receipts(&trace_id, &result);
    result
}

/// Convert an `IngestResult` into the matching receipts UPDATE. Best-effort:
/// errors are logged but do NOT propagate - a terminal-write failure must not
/// mask the pipeline result the caller already produced. The receipts DB is
/// the authoritative state store; there is no longer a markdown DLQ behind it.
fn record_terminal_to_receipts(trace_id: &str, result: &IngestResult) {
    let conn = match receipts::open_default() {
        Ok(c) => c,
        Err(e) => {
            log::error!("receipts: failed to open DB for terminal write trace={trace_id}: {e:#}");
            return;
        }
    };
    match &result.status {
        IngestStatus::Completed | IngestStatus::Duplicate { .. } => {
            let note_path = result.note_path.as_deref().unwrap_or("");
            if let Err(e) = receipts::mark_succeeded(&conn, trace_id, note_path, result.degraded) {
                log::error!("receipts: mark_succeeded trace={trace_id} failed: {e:#}");
            }
        }
        IngestStatus::Failed { reason } => {
            // Typed stage carried on the result, classified at the failure
            // site. No substring matching on the free-form reason.
            let stage = terminal_failure_stage(result);
            if let Err(e) = receipts::mark_failed(&conn, trace_id, stage, reason) {
                log::error!("receipts: mark_failed trace={trace_id} failed: {e:#}");
            }
        }
        IngestStatus::Queued => {
            // Should not happen on a terminal result; log and leave the
            // receipts row in `received` so the watchdog can pick it up.
            log::warn!("receipts: trace={trace_id} returned status=Queued from process_content");
        }
    }
}

/// The receipts `FailureStage` for a terminal `Failed` result. Reads the typed
/// `failure_stage` classified at the failure site; a failure whose site did
/// not classify itself defaults to `FetchFailed`. This replaced the old
/// substring match on free-form reason text.
fn terminal_failure_stage(result: &IngestResult) -> FailureStage {
    result.failure_stage.unwrap_or(FailureStage::FetchFailed)
}

/// Wrap a content handler in the pipeline hard timeout. The non-URL handlers
/// return a terminal `IngestResult` directly (not a `Result`), so a timeout
/// is converted into a `Failed`/`PipelineTimedOut` result here, mirroring the
/// timeout arm `process_url` applies internally. Without this a wedged handler
/// holds its GENERAL permit indefinitely and the watchdog skips it.
async fn with_hard_timeout<F>(
    fut: F,
    config: &Config,
    trace_id: &str,
    method: IngestMethod,
    label: &str,
) -> IngestResult
where
    F: std::future::Future<Output = IngestResult>,
{
    log::debug!(
        "with_hard_timeout[{trace_id}]: label={label} timeout={}s",
        config.pipeline.hard_timeout_secs
    );
    let hard_timeout = std::time::Duration::from_secs(config.pipeline.hard_timeout_secs);
    match tokio::time::timeout(hard_timeout, fut).await {
        Ok(result) => result,
        Err(_) => {
            log::error!(
                "[{trace_id}] {label} handler timed out after {}s",
                config.pipeline.hard_timeout_secs
            );
            IngestResult {
                status: IngestStatus::Failed {
                    reason: "timeout".to_string(),
                },
                trace_id: Some(trace_id.to_string()),
                method: Some(method),
                failure_stage: Some(FailureStage::PipelineTimedOut),
                ..Default::default()
            }
        }
    }
}

pub async fn process_url(
    url: &str,
    note: Option<String>,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: &str,
) -> IngestResult {
    let start = Instant::now();
    let hard_timeout = std::time::Duration::from_secs(config.pipeline.hard_timeout_secs);
    let outcome = tokio::time::timeout(
        hard_timeout,
        process_url_inner(url, note, tags, method, force, config, trace_id),
    )
    .await;

    let make_failure = |reason: String, stage: FailureStage, elapsed: std::time::Duration| -> IngestResult {
        // Failures live in the receipts log only; the markdown ledger is
        // success-only as of Phase 4. The receipts row is closed out in
        // process_content's terminal write at the chokepoint, reading the
        // typed `failure_stage` set here.
        let canonical = hygiene::normalize_url(url, &config.canonicalization.rules).unwrap_or_else(|_| url.to_string());
        IngestResult {
            status: IngestStatus::Failed { reason },
            note_path: None,
            title: None,
            tags: vec![],
            elapsed_secs: Some(elapsed.as_secs_f64()),
            method: Some(method),
            canonical_url: Some(canonical),
            trace_id: None,
            obsidian_url: None,
            failure_stage: Some(stage),
            degraded: false,
        }
    };

    match outcome {
        Ok(Ok(mut result)) => {
            let elapsed = start.elapsed();
            log::info!("[{trace_id}] Pipeline completed for {url} in {elapsed:.2?}");
            result.elapsed_secs = Some(elapsed.as_secs_f64());
            result
        }
        Ok(Err(e)) => {
            let elapsed = start.elapsed();
            log::error!("[{trace_id}] Pipeline failed for {url} in {elapsed:.2?}: {e:?}");
            // Inflight guard is held inside the inner future; Drop ran when
            // the future returned Err and went out of scope. A bubbled-up
            // eyre error carries no stage, so it classifies as FetchFailed;
            // classify / quality / publish failures are returned as typed
            // Failed results inside process_url_inner and never reach here.
            make_failure(format!("{:#}", e), FailureStage::FetchFailed, elapsed)
        }
        Err(_elapsed) => {
            let elapsed = start.elapsed();
            log::error!(
                "[{trace_id}] Pipeline timed out after {}s for {url}",
                config.pipeline.hard_timeout_secs
            );
            // Same: when timeout fires, the inner future is dropped and the
            // InflightGuard's Drop releases the entry automatically.
            make_failure("timeout".to_string(), FailureStage::PipelineTimedOut, elapsed)
        }
    }
}

async fn process_url_inner(
    url: &str,
    note: Option<String>,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    log::debug!("Processing URL: {url} capture_note={}", note.is_some());
    // The capture note is the operator's own trusted text. It is rendered
    // verbatim in-note (`## Why Captured`) and passed to the distiller as a
    // labeled block (not injection-guarded) so it is available as context.
    // Gate: distill.capture-note. When off, dropped at this single seam so
    // neither the distiller context, `## Why Captured`, nor the
    // `capture-note:` frontmatter key are produced.
    let capture_note = gate_capture_note(note.as_deref(), config.distill.capture_note);

    // Normalize URL (clean + canonicalize) before classification
    let canonical = hygiene::normalize_url(url, &config.canonicalization.rules)?;
    log::debug!("Canonical URL: {canonical}");
    if canonical != url {
        log::info!("[{trace_id}] URL canonicalized: {url} -> {canonical}");
    }

    // Get timezone for log timestamps
    let tz = config.frontmatter.timezone_tz();
    let now = chrono::Utc::now().with_timezone(&tz);
    let log_date = now.format("%Y-%m-%d").to_string();
    let log_time = now.format("%H:%M").to_string();

    let mut original_date: Option<String> = None;
    let mut cortex_fields: Vec<(String, String)> = Vec::new();
    let mut old_slides_frontmatter: Vec<String> = Vec::new();
    // Phase 3: instead of deleting the old note up front (which would create
    // a window where the vault has no copy of the URL's note while the rest
    // of the pipeline runs), capture its path here and remove only after
    // the atomic write of the new note succeeds.
    let mut old_path_to_delete: Option<PathBuf> = None;
    let ledger_file = ledger::ledger_path()?;

    // Dedup guard: reject concurrent/duplicate ingestions (skip if --force).
    // Holding `inflight_guard` for the rest of this function keeps the URL
    // in the inflight set; on every return path (Ok, Err, panic-unwind, or
    // future-cancel from the outer hard-timeout) Drop releases it.
    let inflight_guard = if !force {
        match InflightGuard::try_acquire(&canonical) {
            Some(g) => Some(g),
            None => {
                log::info!("[{trace_id}] Duplicate URL (inflight): {canonical}");
                // Inflight duplicates do NOT get a ledger row (the ledger is
                // success-only as of Phase 4 and an inflight collision is
                // not a successful ingestion). The outer process_content
                // chokepoint closes out the receipts row to `succeeded`
                // because the IngestStatus is Duplicate, which is the
                // intentional "no-op success" mapping.
                return Ok(IngestResult {
                    status: IngestStatus::Duplicate {
                        original_date: "inflight".to_string(),
                    },
                    method: Some(method),
                    canonical_url: Some(canonical),
                    ..Default::default()
                });
            }
        }
    } else {
        None
    };

    // Replace existing note if found. Runs for both normal and --force ingestions.
    // We preserve the original date and write location so the new note overwrites
    // in place rather than demoting a promoted note back to inbox.
    let mut reingest_dest: Option<PathBuf> = None;
    if let Some(existing) = ledger::find_completed(&ledger_file, &canonical)? {
        log::info!(
            "[{trace_id}] Found existing entry for {canonical} (ingested {}), replacing",
            existing.date
        );
        let vault_root = config.vault_root()?;
        let old_note_path = find_note_by_source(&vault_root, &canonical).or_else(|| {
            if existing.filename != "-" {
                [vault_root.join("notes"), vault_root.join("inbox")]
                    .iter()
                    .map(|dir| dir.join(&existing.filename))
                    .find(|p| p.exists())
            } else {
                None
            }
        });
        if let Some(ref old_path) = old_note_path {
            original_date = read_note_date(old_path);
            cortex_fields = read_cortex_fields(old_path);
            reingest_dest = old_path.parent().map(|p| p.to_path_buf());
            // Capture the old note's `slides:` frontmatter list BEFORE removing it
            // so reingest cleanup can find any orphaned slide attachments.
            old_slides_frontmatter = crate::slides::cleanup::read_old_slides_frontmatter(old_path).unwrap_or_default();
            log::debug!("[{trace_id}] Preserved original date: {:?}", original_date);
            log::debug!("[{trace_id}] Preserved cortex fields: {:?}", cortex_fields);
            log::debug!(
                "[{trace_id}] Captured old slides for cleanup: {:?}",
                old_slides_frontmatter
            );
            log::debug!("[{trace_id}] Will overwrite in: {:?}", reingest_dest);
            // Phase 3: do NOT delete the old note here. The pipeline below
            // produces the new note bytes; we delete the old path only after
            // the atomic write of the new file succeeds. Track the old path
            // for that final cleanup.
            old_path_to_delete = Some(old_path.clone());
        }
        // Phase 4: the ledger is append-only and success-only. A reingest
        // produces a new ledger row alongside the original; the original
        // row stays as the historical record. mark_replaced is gone.
        log::info!(
            "[{trace_id}] Reingest will append a new ledger row; original row {} stays as history",
            existing.line_number
        );
    }

    let url_match = match router::classify_url(&canonical, &config.links) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("[{trace_id}] URL classification failed: {e:#}");
            return Ok(IngestResult {
                status: IngestStatus::Failed {
                    reason: format!("{e:#}"),
                },
                method: Some(method),
                canonical_url: Some(canonical.clone()),
                trace_id: Some(trace_id.to_string()),
                failure_stage: Some(FailureStage::ClassifyFailed),
                ..Default::default()
            });
        }
    };
    log::debug!(
        "URL classified as: {} (cleaned: {})",
        url_match.link_name,
        url_match.url
    );

    let use_fabric = fabric::is_available(&config.fabric);
    if !use_fabric {
        log::warn!("[{trace_id}] Fabric binary not available, transcript/summary will use fallbacks");
    }

    // Post-Phase-6 cutover: every URL kind produces a structured `Distilled`
    // which becomes the source of truth for the note body and for the
    // `cortex-*` frontmatter additions. The legacy prose-summary path is gone
    // for URL kinds; image/audio/text/vocab paths still flow through the
    // older `summary: String` field unchanged.
    let mut slide_payload: Option<SlidePayload> = None;
    let (title, scraped_title, distilled, content_type, raw_description, yt_tags) = if url_match.is_youtube_type() {
        let yt_result = process_youtube(&url_match.url, config, trace_id, capture_note).await?;
        slide_payload = yt_result.slide_payload;
        let yt_title = yt_result.title;
        (
            yt_title.clone(),
            yt_title,
            yt_result.distilled,
            yt_result.content_type,
            Some(yt_result.description),
            yt_result.yt_tags,
        )
    } else {
        // URL-kind predicates HOISTED above the fetch (finding #5): in-process
        // readability extraction is the preferred fetcher for PLAIN articles
        // ONLY. github roots and X/Reddit/HN threads keep the fabric-u -> Jina
        // chain and never invoke it; youtube was already dispatched above.
        let github_repo = crate::github::parse_repo_url(&url_match.url);
        let is_thread = crate::stages::raw::is_thread_url(&url_match.url);
        let is_plain_article = github_repo.is_none() && !is_thread;
        // `byline` is the article author surfaced by whichever fetcher could see
        // the source markup: `fabric -u`, readability extraction, and the Jina
        // markdown path all yield `None`; only the browser-UA fallback inside
        // `process_article_jina` carries one. It is folded into
        // `ContentType::Article { author }` below.
        // Preferred: in-process readability extraction (clean article markdown),
        // plain articles only. On Err / too-thin / block-page it logs the reason
        // and falls through to fabric-u -> Jina -> browser-UA.
        let readable_triple = if is_plain_article {
            match process_article_readable(&url_match.url, config, trace_id).await {
                Ok(triple) => Some(triple),
                Err(e) => {
                    log::warn!(
                        "[{trace_id}] readable extraction fell through for {}: {e:#}",
                        url_match.url
                    );
                    None
                }
            }
        } else {
            None
        };
        // Provenance for the transcript source gate: a stored `## Transcript` is
        // trustworthy ONLY when the in-process readable extractor produced it. Any
        // fallthrough (fabric-u / Jina / browser-UA) yields raw, non-readability
        // markdown that must NOT be stored as a transcript - the coarse ratio gate
        // alone keeps chrome-heavy-but-mixed-line fallback pages (measured on the
        // tg-1a0305 trainwreck: 0.61 short-line ratio, below the 0.90 threshold).
        let transcript_clean_source = readable_triple.is_some();
        let (scraped_title, article_md, byline) = match readable_triple {
            Some(triple) => triple,
            None if use_fabric => match process_article_fabric(&url_match.url, config, trace_id).await {
                Ok(triple) => triple,
                Err(e) => {
                    log::warn!("Fabric article fetch failed: {e:#}, falling back to Jina");
                    process_article_jina(&url_match.url, config, trace_id).await?
                }
            },
            None => process_article_jina(&url_match.url, config, trace_id).await?,
        };
        // For github repo URLs, the HTML <title> is unreliable: auth-walled
        // pages collapse to a generic login title, so distinct repos slug to
        // the same filename and clobber each other. The URL itself is the
        // canonical name, so derive `title` from parse_repo_url. The original
        // `scraped_title` is preserved so the quality gate below can still
        // see what the fetcher actually returned (and bail on auth-wall
        // bodies via BLOCKED_TITLE_INDICATORS). `github_repo` is computed above.
        let title = match &github_repo {
            Some((owner, repo)) => format!("{owner}/{repo}"),
            None => scraped_title.clone(),
        };
        // Resolve the note's ContentType once its kind-specific data is known.
        // A github repo root carries its owner (free at dispatch). Deep github
        // paths and every other non-thread URL fall through to the article
        // path - matching the distiller dispatch below, which keys on
        // `github_repo.is_some()`. The article byline (when a fetcher surfaced
        // one) rides into `Article { author }`. social/reddit keep dedicated
        // variants and carry no creator here.
        let ct = if let Some((owner, _)) = &github_repo {
            ContentType::GitHub { owner: owner.clone() }
        } else {
            match url_match.link_name.as_str() {
                "social" => ContentType::Social,
                "reddit" => ContentType::Reddit,
                _ => ContentType::Article { author: byline.clone() },
            }
        };
        // Dispatch by URL kind: github roots → repo distiller (fetches REST
        // metadata internally); X/Reddit/HN → thread distiller; everything
        // else → article distiller. The repo path uses `article_md` only as
        // a fallback when the GitHub API call fails.
        let distilled = if github_repo.is_some() {
            crate::stages::distill::distill_for_publish_repo(
                &config.fabric,
                &config.staging,
                trace_id,
                &url_match.url,
                &article_md,
                capture_note,
            )
            .await
        } else if is_thread {
            crate::stages::distill::distill_for_publish_thread(
                &config.fabric,
                &config.staging,
                trace_id,
                &url_match.url,
                &article_md,
                capture_note,
            )
            .await
        } else {
            // Source/quality gate (article-only) now runs INSIDE
            // distill_for_publish_article, before distilled.yml is persisted
            // (2026-07-07 distillation-output-restore, Phase 3 re-scope) - so
            // chrome junk from a non-clean fetch never reaches staging or
            // embeddings, not just the rendered note.
            crate::stages::distill::distill_for_publish_article(
                &config.fabric,
                &config.staging,
                trace_id,
                &url_match.url,
                &article_md,
                capture_note,
                transcript_clean_source,
            )
            .await
        };
        // Gate-2 runs against the concise Distilled summary, which is what
        // we now display to users; it is also what `fabric::generate_tags`
        // consumes below.
        crate::stages::raw::run_gate_2(config, trace_id, Some(&url_match.url), &distilled.summary)?;
        // Thread/social title override (design doc
        // `docs/design/2026-07-08-thread-title-generation.md`, Phase 2). Threads
        // NEVER consult the scraped page title (Strategy 3 degenerates to a bare
        // numeric post ID on the browser-UA fallback fetcher); the title is
        // rebuilt from the distilled author/tldr, with a generic
        // "<Platform> thread" floor on the fallback path so no `[reason]` string
        // can leak. Non-thread `title` (owner/repo, scraped article) is untouched.
        let title = crate::thread::resolve_title(is_thread, title, &distilled, trace_id);
        (title, scraped_title, distilled, ct, None, Vec::new())
    };

    // Quality gate: detect blocked/garbage content before creating a note.
    // Runs against the structured Distilled summary and `scraped_title` -
    // the title the fetcher actually returned, not any URL-derived override.
    // For github repo URLs `title` is `owner/repo`, which would never match
    // an auth-wall indicator; using `scraped_title` here keeps the gate
    // honest about what the fetcher saw.
    if let Some(reason) = crate::quality::detect_blocked_content(&distilled.summary, &scraped_title) {
        log::warn!("[{trace_id}] quality gate blocked content: {reason}");
        return Ok(IngestResult {
            status: IngestStatus::Failed {
                reason: format!("Content quality check failed: {reason}"),
            },
            method: Some(method),
            canonical_url: Some(canonical.clone()),
            trace_id: Some(trace_id.to_string()),
            failure_stage: Some(FailureStage::QualityBlocked),
            ..Default::default()
        });
    }

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    // Extractor-produced tags also flow into the tag pipeline so canonical
    // filtering applies to them uniformly. Gate: distill.propose-tags — when
    // off, the distiller-proposed tags are not merged (other sources stand).
    merge_proposed_tags(&mut all_tags, &distilled.tags, config.distill.propose_tags);

    // Extract hashtags from YouTube description and merge yt-dlp tags
    if let Some(ref desc) = raw_description {
        let hashtags = description::extract_hashtags(desc);
        all_tags.extend(hashtags.into_iter().map(|t| hygiene::sanitize_tag(&t)));
    }
    all_tags.extend(yt_tags.into_iter().map(|t| hygiene::sanitize_tag(&t)));

    // Generate tags via Fabric (graceful failure). Now driven by the
    // concise Distilled summary rather than a long prose body.
    if use_fabric {
        match fabric::generate_tags(&distilled.summary, &config.fabric).await {
            Ok(fabric_tags) => all_tags.extend(fabric_tags.into_iter().map(|t| hygiene::sanitize_tag(&t))),
            Err(e) => log::warn!("fabric generate_tags failed, continuing without fabric tags: {e}"),
        }
    }
    finalize_tags(&mut all_tags, config).await;

    let filtered_description = raw_description.as_deref().and_then(description::filter_description);

    let embed_code = if url_match.is_youtube_type() {
        youtube::extract_video_id(&url_match.url)
            .map(|vid| youtube::generate_embed_code(&vid, url_match.width, url_match.height))
    } else {
        None
    };

    // Render the Distilled into a structured body + frontmatter additions.
    // For slide-aware YouTube, `publish_slides` produces its own structured
    // body and we use that instead - the slide body is the user-visible
    // value of the slide pipeline. The Distilled-derived frontmatter
    // additions (cortex-video-*, distilled flag) still apply.
    let filename_stub = hygiene::sanitize_filename(&title);
    let vault_root_resolved: PathBuf = config.vault_root()?;
    // Transcript-emission policy (2026-07-07 distillation-output-restore). Every
    // URL kind renders here: Video, Repo, and Article publish transcript-free
    // (the verbatim text lives in the staged `distilled.yml` and is embedded
    // from there), while Thread is a verbatim-preservation kind that keeps its
    // in-note `## Transcript`. The choice is a typed policy keyed on the
    // `KindPayload` seam, not on any extractor string.
    let rendered_distilled = distillers::render(&distilled, distillers::RenderOptions::for_url_publish(&distilled));
    let (distilled_body, slide_paths) = if let Some(payload) = slide_payload.as_ref() {
        let published = match crate::slides::publish::publish_slides(
            &vault_root_resolved,
            &filename_stub,
            &payload.manifest,
            &payload.summary,
            &payload.slides_source_root,
            &chrono::Utc::now(),
        ) {
            Ok(result) => {
                log::info!(
                    "[{trace_id}] Slide-aware publish: shape={:?} slides={}",
                    result.shape,
                    result.slides.len(),
                );
                // Phase 7 (defect #2): APPEND the distilled sections below the
                // slide body rather than REPLACING them. Frontmatter is
                // untouched here - `rendered_distilled.frontmatter_additions` is
                // consumed below. Gate: distill.slide-append — when off, the
                // slide body stands alone (pre-Phase-7 slide-only shape).
                let body = compose_slide_body(
                    result.body,
                    &rendered_distilled.body_markdown,
                    config.distill.slide_append,
                );
                (body, result.slides)
            }
            Err(e) => {
                log::warn!("[{trace_id}] Slide publish failed: {e:#} - using rendered Distilled body");
                (rendered_distilled.body_markdown.clone(), Vec::new())
            }
        };
        // publish_slides has copied the JPEGs it needs into the vault (or
        // failed); the temp frames work dir (≤720p mp4 + every extracted
        // frame, under /tmp/borg-youtube-frames/<id>) is now dead weight and
        // was never cleaned up. Remove it so it can't accumulate unbounded.
        if let Err(e) = std::fs::remove_dir_all(&payload.slides_source_root) {
            log::debug!(
                "[{trace_id}] could not remove slide work dir {}: {e}",
                payload.slides_source_root.display()
            );
        }
        published
    } else {
        (rendered_distilled.body_markdown.clone(), Vec::new())
    };

    let note = NoteContent {
        title: title.clone(),
        source_url: Some(url_match.url.clone()),
        asset_path: None,
        tags: all_tags.clone(),
        summary: distilled.summary.clone(),
        description: filtered_description,
        capture_note: capture_note.map(str::to_string),
        content_type,
        embed_code,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: slide_paths.clone(),
        distilled_body: Some(distilled_body),
        frontmatter_additions: {
            // Stamp the absolute retention expiry alongside `trace`/`ingested`.
            // borg owns `retention_days`, so the number is resolved once here at
            // its single source of truth and frozen as a date. The note's
            // `ingested` instant is `now` (written below at publish), so the
            // expiry is `now`'s calendar date + retention_days. We inject via
            // `frontmatter_additions` rather than passing StagingConfig into the
            // renderer, keeping render_note's narrow FrontmatterConfig boundary.
            let mut additions = rendered_distilled.frontmatter_additions;
            let expires = retention::trace_expires_for(now.date_naive(), config.staging.retention_days);
            additions.insert("trace-expires".to_string(), serde_yaml::Value::String(expires));
            additions
        },
        // URL kinds keep the pre-existing `origin: assisted` / no `status:`
        // behavior; only harvest's Session handler sets these.
        origin: None,
        status: None,
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let filename = format!("{}.md", hygiene::sanitize_filename(&title));

    // Resolve write path: reingest preserves the original location, new ingests go to inbox
    let dest_path = match reingest_dest {
        Some(d) => d,
        None => config.inbox_dir()?,
    };
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = dest_path.join(&filename);

    // Phase 3: compose the FINAL note bytes in memory before any disk write.
    // The original three-write publish path (write rendered, patch date, patch
    // cortex) was non-atomic across the patches; a SIGKILL or panic between
    // them would desync the body from its date / cortex frontmatter. Now the
    // body, restored date, and cortex fields are baked into one string and
    // written via a single atomic-rename publish.
    let mut final_str = rendered;
    if let Some(ref orig_date) = original_date {
        final_str = apply_original_date(&final_str, orig_date);
        log::info!("[{trace_id}] Restored original date: {orig_date}");
    }
    if !cortex_fields.is_empty() {
        final_str = apply_cortex_fields(&final_str, &cortex_fields);
        log::info!(
            "[{trace_id}] Restored cortex fields: {:?}",
            cortex_fields.iter().map(|(k, _)| k).collect::<Vec<_>>()
        );
    }
    // `ingested:` records when borg LAST processed this note, to second
    // precision in the configured local timezone (ISO-8601 with offset, e.g.
    // 2026-06-05T08:27:25-07:00). Unconditional on every publish (original
    // ingest AND reingest) so views can sort/window by when borg did the work
    // rather than when the content was originally learned. The precise form
    // lets the borg-ledger.base view sort chronologically; `date:` remains the
    // original content date.
    let log_timestamp = now.format("%Y-%m-%dT%H:%M:%S%:z").to_string();
    final_str = apply_ingested_date(&final_str, &log_timestamp);
    log::debug!("[{trace_id}] Set ingested: {log_timestamp}");

    // Note-size hard gate (2026-07-07 distillation-output-restore, Phase 3):
    // with `## Transcript` gone from video/article/repo publish, an oversize
    // note is a bug - a verbatim leak the gates above should already have
    // caught - so this hard-fails rather than publishing degraded. Checked
    // on the exact bytes about to hit disk, right before the atomic write.
    if let Some(reason) = note_size_gate(final_str.len(), config.pipeline.max_note_bytes) {
        log::warn!("[{trace_id}] note-size gate blocked publish: {reason}");
        return Ok(IngestResult {
            status: IngestStatus::Failed { reason },
            method: Some(method),
            canonical_url: Some(canonical.clone()),
            trace_id: Some(trace_id.to_string()),
            failure_stage: Some(FailureStage::QualityBlocked),
            ..Default::default()
        });
    }

    if let Err(e) = write_atomic(&note_path, final_str.as_bytes()) {
        log::error!("[{trace_id}] atomic publish failed: {e:#}");
        return Ok(IngestResult {
            status: IngestStatus::Failed {
                reason: format!("Failed to atomically publish note: {e:#}"),
            },
            method: Some(method),
            canonical_url: Some(canonical.clone()),
            trace_id: Some(trace_id.to_string()),
            failure_stage: Some(FailureStage::PublishFailed),
            ..Default::default()
        });
    }

    // The new note exists at note_path. If we were replacing an old note at
    // a different path (rare - happens when the dir heuristic resolves the
    // new write to a different location than the old note lived in), delete
    // it now. A failure here is non-fatal: the new note already exists; the
    // user has a transient duplicate which cortex's existing duplicate
    // detection will surface.
    if let Some(old_path) = old_path_to_delete
        && old_path != note_path
    {
        match std::fs::remove_file(&old_path) {
            Ok(()) => log::info!("[{trace_id}] Removed old note: {}", old_path.display()),
            Err(e) => log::warn!(
                "[{trace_id}] Failed to remove old note {} after publishing new copy: {e}",
                old_path.display()
            ),
        }
    }

    log::info!("[{trace_id}] Wrote note: {}", note_path.display());

    // Slide cleanup: now that the new note is durable, archive any orphan
    // slide attachments the old note used to reference. Best-effort - if
    // rkvr fails we log and move on rather than failing the ingestion.
    if !old_slides_frontmatter.is_empty() || !slide_paths.is_empty() {
        let orphans = crate::slides::cleanup::compute_orphans(&old_slides_frontmatter, &slide_paths);
        if !orphans.is_empty() {
            let abs = crate::slides::cleanup::resolve_existing(&vault_root_resolved, &orphans);
            if let Err(e) = crate::slides::cleanup::rkvr_remove(&abs) {
                log::warn!("[{trace_id}] Slide cleanup failed: {e:#}");
            } else {
                log::info!("[{trace_id}] Archived {} orphan slide(s)", orphans.len());
            }
        }
    }

    // Log success to Borg Ledger
    ledger::append_entry(
        &ledger_file,
        &LedgerEntry {
            date: log_date,
            time: log_time,
            method,
            filename: extract_filename(&note_path),
            source: canonical.clone(),
            domain: None,
            trace_id: Some(trace_id.to_string()),
        },
    )?;

    // Inflight guard releases automatically when `inflight_guard` goes out
    // of scope at function return (success path here).
    drop(inflight_guard);

    let obsidian_url = build_obsidian_url(&note_path.to_string_lossy());

    Ok(IngestResult {
        status: IngestStatus::Completed,
        note_path: Some(note_path.to_string_lossy().to_string()),
        title: Some(title),
        tags: all_tags,
        elapsed_secs: None,
        method: Some(method),
        canonical_url: Some(canonical),
        trace_id: None,
        obsidian_url,
        failure_stage: None,
        // Degraded when the L2 distiller fell back OR an enumeration fell short
        // of its declared count (Phase 4, Resolved Decision 2026-07-07). Single
        // source of truth: `ValidationMeta::is_degraded`.
        degraded: distilled.meta.validation.is_degraded(),
    })
}

/// Whether a file-based document is a PDF or a generic document (docx, pptx, etc.).
#[derive(Debug, Clone, Copy)]
pub(crate) enum DocumentKind {
    Pdf,
    Document,
}

impl DocumentKind {
    fn subdirectory(self) -> &'static str {
        match self {
            DocumentKind::Pdf => "pdfs",
            DocumentKind::Document => "docs",
        }
    }

    fn label(self) -> &'static str {
        match self {
            DocumentKind::Pdf => "pdf",
            DocumentKind::Document => "document",
        }
    }

    fn default_tag(self) -> &'static str {
        match self {
            DocumentKind::Pdf => "pdf",
            DocumentKind::Document => "document",
        }
    }

    fn content_type(self, asset_path: String) -> ContentType {
        match self {
            DocumentKind::Pdf => ContentType::Pdf { asset_path },
            DocumentKind::Document => ContentType::Document { asset_path },
        }
    }
}

/// Detect structured text patterns before LLM classification.
#[derive(Debug, PartialEq)]
pub(crate) enum TextPattern {
    Define {
        word: String,
    },
    Clarify {
        word_a: String,
        word_b: String,
    },
    /// The text contains a URL: it becomes an annotated URL ingest (Phase 8).
    /// `note` is the surrounding prose (first-URL token removed) - the capture
    /// annotation. An `idea:` prefix bypasses this (forces an Idea note).
    ContainsUrl {
        url: String,
        note: Option<String>,
    },
    General,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;

#[cfg(test)]
mod timeouts;
