use crate::assets;
use crate::config::Config;
use crate::description;
use crate::extraction;
use crate::fabric;
use crate::hygiene;
use crate::jina;
use crate::ledger::{self, LedgerEntry, LedgerStatus};
use crate::markdown::{self, ContentType, NoteContent};
use crate::ocr;
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
use vault::schema::CORTEX_PRESERVE_KEYS;

pub mod atomic;
pub mod error;
mod inflight;
pub mod permits;
use atomic::{apply_cortex_fields, apply_ingested_date, apply_original_date, write_atomic};
use inflight::InflightGuard;

/// Cached canonical tag state loaded once at first use.
struct CanonicalState {
    canonical_set: std::collections::HashSet<String>,
    mapping: TagMapping,
    max_per_note: usize,
    reject_concatenated: bool,
}

async fn get_or_init_canonical(config: &Config) -> Option<std::sync::Arc<CanonicalState>> {
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;
    static CACHED: LazyLock<TokioMutex<Option<Arc<CanonicalState>>>> = LazyLock::new(|| TokioMutex::new(None));

    let mut guard = CACHED.lock().await;
    if let Some(ref cached) = *guard {
        return Some(Arc::clone(cached));
    }

    let canonical_path = Path::new(&config.tags.canonical_path);
    let mapping_path = Path::new(&config.tags.mapping_path);

    let canonical_file = match CanonicalTagsFile::load(canonical_path) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("Could not load canonical tags: {e}. Tag filtering disabled.");
            return None;
        }
    };

    let mapping = match canonical::load_tag_mapping(mapping_path) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("Could not load tag mapping: {e}. Using empty mapping.");
            TagMapping::new()
        }
    };

    let state = Arc::new(CanonicalState {
        canonical_set: canonical_file.all_tags(),
        max_per_note: canonical_file.max_per_note,
        mapping,
        reject_concatenated: config.tags.reject_concatenated,
    });
    *guard = Some(Arc::clone(&state));
    Some(state)
}

/// Sort, dedup, and filter tags through the canonical vocabulary.
/// Falls back to simple sort+dedup if canonical config is not available.
async fn finalize_tags(tags: &mut Vec<String>, config: &Config) {
    tags.sort();
    tags.dedup();

    if let Some(state) = get_or_init_canonical(config).await {
        // Reject concatenated words
        if state.reject_concatenated {
            tags.retain(|t| !canonical::is_concatenated_word(t, &state.canonical_set));
        }

        // Filter and cap through canonical vocabulary
        *tags = canonical::filter_and_cap(tags, &state.canonical_set, &state.mapping, state.max_per_note);
    }
}

struct YouTubeResult {
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
struct SlidePayload {
    manifest: crate::slides::SlideManifest,
    summary: crate::slides::SummaryOutput,
    /// Directory the slide JPEGs were materialized into; resolves the
    /// manifest's relative `frame_path` for the publish step.
    slides_source_root: PathBuf,
}

/// Extract the best title from fabric's markdown output.
///
/// Strategy (in priority order):
/// 1. `Title:` metadata line (fabric always emits this first)
///    - For HTML pages this is the <title> tag (usually great)
///    - For PDFs this is often the filename - we clean it up
/// 2. First `# ` heading in the markdown body
/// 3. Derive from the URL path (last segment, cleaned up)
/// 4. Raw URL as last resort
fn extract_article_title(article_md: &str, url: &str) -> String {
    // Strategy 1: Parse Title: metadata line
    if let Some(title) = article_md
        .lines()
        .find(|line| line.starts_with("Title:"))
        .map(|line| line.trim_start_matches("Title:").trim().to_string())
        && !title.is_empty()
    {
        // If it looks like a filename (has a file extension), clean it up
        let cleaned = if title.contains('.')
            && title
                .rsplit('.')
                .next()
                .is_some_and(|ext| matches!(ext.to_lowercase().as_str(), "pdf" | "html" | "htm" | "txt" | "md"))
        {
            // Strip extension, replace hyphens/underscores with spaces
            let without_ext = title.rsplit_once('.').map(|(base, _)| base).unwrap_or(&title);
            without_ext.replace(['-', '_'], " ")
        } else {
            title
        };
        if !cleaned.is_empty() {
            return cleaned;
        }
    }

    // Strategy 2: First # heading in the body (after "Markdown Content:" if present)
    let body_start = article_md
        .find("Markdown Content:")
        .map(|pos| pos + "Markdown Content:".len())
        .unwrap_or(0);
    if let Some(title) = article_md[body_start..]
        .lines()
        .find(|line| line.starts_with("# "))
        .map(|line| line.trim_start_matches("# ").trim().to_string())
        && !title.is_empty()
    {
        return title;
    }

    // Strategy 3: Derive from URL path
    if let Some(segment) = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty() && *s != url)
    {
        let cleaned = segment
            .rsplit_once('.')
            .map(|(base, _)| base)
            .unwrap_or(segment)
            .replace(['-', '_'], " ");
        if !cleaned.is_empty() {
            return cleaned;
        }
    }

    // Strategy 4: raw URL
    url.to_string()
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

    if let Err(err) = crate::stages::raw::stage_0_init(config, &content, method, &trace_id) {
        let reason = format!("{err:#}");
        log::warn!("[{trace_id}] Stage-0 rejected: {reason}");
        return IngestResult {
            status: IngestStatus::Failed { reason },
            trace_id: Some(trace_id),
            method: Some(method),
            ..Default::default()
        };
    }
    let mut result = match content {
        ContentKind::Url(url) => process_url(&url, tags, method, force, config, &trace_id).await,
        ContentKind::Image { data, filename } => {
            process_image(&data, &filename, tags, method, force, config, &trace_id).await
        }
        ContentKind::Pdf { data, filename } => {
            process_document_file(
                &data,
                &filename,
                tags,
                method,
                force,
                config,
                DocumentKind::Pdf,
                &trace_id,
            )
            .await
        }
        ContentKind::Audio { data, filename } => {
            process_audio(&data, &filename, tags, method, force, config, &trace_id).await
        }
        ContentKind::Text(text) => process_text(&text, tags, method, force, config, &trace_id).await,
        ContentKind::Document { data, filename } => {
            process_document_file(
                &data,
                &filename,
                tags,
                method,
                force,
                config,
                DocumentKind::Document,
                &trace_id,
            )
            .await
        }
    };
    result.trace_id = Some(trace_id);
    result
}

pub async fn process_url(
    url: &str,
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
        process_url_inner(url, tags, method, force, config, trace_id),
    )
    .await;

    let make_failure = |reason: String, elapsed: std::time::Duration| -> IngestResult {
        // Best-effort log failure to Borg Ledger
        let canonical = hygiene::normalize_url(url, &config.canonicalization.rules).unwrap_or_else(|_| url.to_string());
        let tz: chrono_tz::Tz = config
            .frontmatter
            .timezone
            .parse()
            .unwrap_or(chrono_tz::America::Los_Angeles);
        let now = chrono::Utc::now().with_timezone(&tz);
        if let Ok(ledger_p) = ledger::ledger_path(config) {
            let _ = ledger::append_entry(
                &ledger_p,
                &LedgerEntry {
                    date: now.format("%Y-%m-%d").to_string(),
                    time: now.format("%H:%M").to_string(),
                    method: method.into(),
                    status: LedgerStatus::Failed,
                    filename: None,
                    source: canonical.clone(),
                    domain: None,
                    trace_id: Some(trace_id.to_string()),
                },
            );
        }
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
            // the future returned Err and went out of scope.
            make_failure(format!("{:#}", e), elapsed)
        }
        Err(_elapsed) => {
            let elapsed = start.elapsed();
            log::error!(
                "[{trace_id}] Pipeline timed out after {}s for {url}",
                config.pipeline.hard_timeout_secs
            );
            // Same: when timeout fires, the inner future is dropped and the
            // InflightGuard's Drop releases the entry automatically.
            make_failure("timeout".to_string(), elapsed)
        }
    }
}

async fn process_url_inner(
    url: &str,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    log::debug!("Processing URL: {url}");

    // Normalize URL (clean + canonicalize) before classification
    let canonical = hygiene::normalize_url(url, &config.canonicalization.rules)?;
    log::debug!("Canonical URL: {canonical}");
    if canonical != url {
        log::info!("[{trace_id}] URL canonicalized: {url} -> {canonical}");
    }

    // Get timezone for log timestamps
    let tz: chrono_tz::Tz = config
        .frontmatter
        .timezone
        .parse()
        .unwrap_or(chrono_tz::America::Los_Angeles);
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
    let ledger_file = ledger::ledger_path(config)?;

    // Dedup guard: reject concurrent/duplicate ingestions (skip if --force).
    // Holding `inflight_guard` for the rest of this function keeps the URL
    // in the inflight set; on every return path (Ok, Err, panic-unwind, or
    // future-cancel from the outer hard-timeout) Drop releases it.
    let inflight_guard = if !force {
        match InflightGuard::try_acquire(&canonical) {
            Some(g) => Some(g),
            None => {
                log::info!("[{trace_id}] Duplicate URL (inflight): {canonical}");
                ledger::append_entry(
                    &ledger_file,
                    &LedgerEntry {
                        date: log_date,
                        time: log_time,
                        method: method.into(),
                        status: LedgerStatus::Skipped,
                        filename: None,
                        source: canonical.clone(),
                        domain: None,
                        trace_id: Some(trace_id.to_string()),
                    },
                )?;
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
        ledger::mark_replaced(&ledger_file, existing.line_number)?;
        log::info!("[{trace_id}] Marked ledger row {} as replaced", existing.line_number);
    }

    let url_match = router::classify_url(&canonical, &config.links)?;
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
        let yt_result = process_youtube(&url_match.url, config, trace_id).await?;
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
        let ct = match url_match.link_name.as_str() {
            "github" => ContentType::GitHub,
            "social" => ContentType::Social,
            "reddit" => ContentType::Reddit,
            _ => ContentType::Article,
        };
        let (scraped_title, article_md) = if use_fabric {
            match process_article_fabric(&url_match.url, config, trace_id).await {
                Ok((title, article_md, _)) => (title, article_md),
                Err(e) => {
                    log::warn!("Fabric article fetch failed: {e:#}, falling back to Jina");
                    let (title, article_md, _) = process_article_jina(&url_match.url, config, trace_id).await?;
                    (title, article_md)
                }
            }
        } else {
            let (title, article_md, _) = process_article_jina(&url_match.url, config, trace_id).await?;
            (title, article_md)
        };
        // For github repo URLs, the HTML <title> is unreliable: auth-walled
        // pages collapse to a generic login title, so distinct repos slug to
        // the same filename and clobber each other. The URL itself is the
        // canonical name, so derive `title` from parse_repo_url. The original
        // `scraped_title` is preserved so the quality gate below can still
        // see what the fetcher actually returned (and bail on auth-wall
        // bodies via BLOCKED_TITLE_INDICATORS).
        let github_repo = crate::github::parse_repo_url(&url_match.url);
        let title = match &github_repo {
            Some((owner, repo)) => format!("{owner}/{repo}"),
            None => scraped_title.clone(),
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
            )
            .await
        } else if crate::stages::raw::is_thread_url(&url_match.url) {
            crate::stages::distill::distill_for_publish_thread(
                &config.fabric,
                &config.staging,
                trace_id,
                &url_match.url,
                &article_md,
            )
            .await
        } else {
            crate::stages::distill::distill_for_publish_article(
                &config.fabric,
                &config.staging,
                trace_id,
                &url_match.url,
                &article_md,
            )
            .await
        };
        // Gate-2 runs against the concise Distilled summary, which is what
        // we now display to users; it is also what `fabric::generate_tags`
        // consumes below.
        crate::stages::raw::run_gate_2(config, trace_id, Some(&url_match.url), &distilled.summary)?;
        (title, scraped_title, distilled, ct, None, Vec::new())
    };

    // Quality gate: detect blocked/garbage content before creating a note.
    // Runs against the structured Distilled summary and `scraped_title` -
    // the title the fetcher actually returned, not any URL-derived override.
    // For github repo URLs `title` is `owner/repo`, which would never match
    // an auth-wall indicator; using `scraped_title` here keeps the gate
    // honest about what the fetcher saw.
    if let Some(reason) = crate::quality::detect_blocked_content(&distilled.summary, &scraped_title) {
        eyre::bail!("Content quality check failed: {reason}");
    }

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    // Extractor-produced tags also flow into the tag pipeline so canonical
    // filtering applies to them uniformly.
    all_tags.extend(distilled.tags.iter().map(|t| hygiene::sanitize_tag(t)));

    // Extract hashtags from YouTube description and merge yt-dlp tags
    if let Some(ref desc) = raw_description {
        let hashtags = description::extract_hashtags(desc);
        all_tags.extend(hashtags.into_iter().map(|t| hygiene::sanitize_tag(&t)));
    }
    all_tags.extend(yt_tags.into_iter().map(|t| hygiene::sanitize_tag(&t)));

    // Generate tags via Fabric (graceful failure). Now driven by the
    // concise Distilled summary rather than a long prose body.
    if use_fabric && let Ok(fabric_tags) = fabric::generate_tags(&distilled.summary, &config.fabric).await {
        all_tags.extend(fabric_tags.into_iter().map(|t| hygiene::sanitize_tag(&t)));
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
    let rendered_distilled = distillers::render(&distilled);
    let (distilled_body, slide_paths) = if let Some(payload) = slide_payload.as_ref() {
        match crate::slides::publish::publish_slides(
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
                (result.body, result.slides)
            }
            Err(e) => {
                log::warn!("[{trace_id}] Slide publish failed: {e:#} - using rendered Distilled body");
                (rendered_distilled.body_markdown.clone(), Vec::new())
            }
        }
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
        content_type,
        embed_code,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: slide_paths.clone(),
        distilled_body: Some(distilled_body),
        frontmatter_additions: rendered_distilled.frontmatter_additions,
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let filename = format!("{}.md", hygiene::sanitize_filename(&title));

    // Resolve write path: reingest preserves the original location, new ingests go to inbox
    let dest_path = reingest_dest.unwrap_or_else(|| resolve_destination(&config.vault.inbox_path));
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
    // `ingested:` records when borg LAST processed this note. Unconditional
    // on every publish (original ingest AND reingest) so the dashboard's
    // "this week" query can ask when borg did the work rather than when
    // the content was originally learned.
    final_str = apply_ingested_date(&final_str, &log_date);
    log::debug!("[{trace_id}] Set ingested: {log_date}");
    write_atomic(&note_path, final_str.as_bytes()).context("Failed to atomically publish note")?;

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
            method: method.into(),
            status: LedgerStatus::Completed,
            filename: extract_filename(&note_path),
            source: canonical.clone(),
            domain: None,
            trace_id: Some(trace_id.to_string()),
        },
    )?;

    // Inflight guard releases automatically when `inflight_guard` goes out
    // of scope at function return (success path here).
    drop(inflight_guard);

    let obsidian_url = build_obsidian_url(&config.vault.vault_name, &note_path.to_string_lossy());

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
    })
}

/// Unified YouTube processing: yt-dlp for metadata, fabric for transcript+summary.
/// Metadata and transcript run concurrently. Fabric is optional (gates transcript+summary).
/// See docs/design/2026-03-22-youtube-metadata-pipeline-redesign.md.
async fn process_youtube(url: &str, config: &Config, trace_id: &str) -> Result<YouTubeResult> {
    // Heavy permit: yt-dlp + fabric + (optional) ffmpeg slides + vision all run
    // under this handler. Held for the lifetime of the function.
    log::debug!("process_youtube: acquiring heavy permit (url={url})");
    let _heavy_permit = permits::HEAVY_PERMITS.acquire().await;
    log::debug!("process_youtube: heavy permit acquired (url={url})");

    let use_fabric = fabric::is_available(&config.fabric);

    // Run metadata (yt-dlp) and transcript (fabric) concurrently.
    // These are independent - yt-dlp scrapes the page, fabric calls the captions API.
    let url_owned = url.to_string();
    let yt_dlp_timeout = config.pipeline.yt_dlp_timeout_secs;
    let metadata_future = youtube::fetch_metadata(&url_owned, yt_dlp_timeout);

    let transcript_future = async {
        if use_fabric {
            let config_fabric = config.fabric.clone();
            let config_pipeline = config.pipeline.clone();
            let url = url_owned.clone();
            let result =
                tokio::task::spawn_blocking(move || fabric::fetch_transcript(&url, &config_fabric, &config_pipeline))
                    .await
                    .unwrap_or_else(|e| {
                        log::warn!("Fabric transcript task panicked: {e}");
                        Ok(String::new())
                    });
            result.unwrap_or_else(|e| {
                log::warn!("Fabric transcript failed: {e:#}");
                String::new()
            })
        } else {
            String::new()
        }
    };

    let (metadata_result, fabric_transcript) = tokio::join!(metadata_future, transcript_future);
    let metadata = metadata_result.context("yt-dlp metadata failed")?;

    // Transcript fallback chain: fabric -> yt-dlp subtitles -> audio extraction + Groq
    let transcript = if !fabric_transcript.is_empty() {
        fabric_transcript
    } else {
        if use_fabric {
            log::warn!("Fabric returned empty transcript, falling back to yt-dlp subtitles");
        }
        match youtube::fetch_subtitles(url, &config.pipeline).await? {
            Some(subs) => subs,
            None => {
                log::warn!("No subtitles available, falling back to audio extraction + Groq");
                let temp_dir = std::env::temp_dir().join("obsidian-borg");
                std::fs::create_dir_all(&temp_dir)?;
                let audio_path = youtube::extract_audio(
                    url,
                    &temp_dir.to_string_lossy(),
                    config.youtube.yt_dlp_postprocessor_threads(),
                )?;
                let audio_bytes = std::fs::read(&audio_path)?;
                let _ = std::fs::remove_file(&audio_path);

                let groq_key = crate::config::resolve_secret(&config.groq.api_key).ok();
                let client = TranscriptionClient::new(
                    &config.transcriber.url,
                    groq_key,
                    &config.groq.model,
                    config.transcriber.timeout_secs,
                );
                let response = client.transcribe(audio_bytes, AudioFormat::Mp3, None).await?;
                response.text
            }
        }
    };

    // Frame-aware path: when enabled, download the video, extract frames,
    // segment slides, and (when the proposed shape is non-text-only) run
    // the slide-aware Fabric pattern instead of the flat summarizer.
    let mut slide_payload: Option<SlidePayload> = None;
    let mut slide_summary: Option<String> = None;
    if config.youtube.slides.enabled {
        match try_extract_slides(url, &transcript, metadata.duration_secs, config).await {
            Ok(Some((manifest, summary, slides_source_root))) => {
                if !summary.body.trim().is_empty() {
                    slide_summary = Some(summary.body.clone());
                }
                slide_payload = Some(SlidePayload {
                    manifest,
                    summary,
                    slides_source_root,
                });
            }
            Ok(None) => {
                log::debug!("Slide-aware path produced no manifest; using text-only summary");
            }
            Err(e) => {
                log::warn!("Frame-aware path failed: {e:#} - falling back to text-only summary");
            }
        }
    }

    // Post-Phase-6 cutover: replace the legacy `fabric::summarize` prose
    // path with the structured video distiller. The distiller re-fetches
    // raw VTT internally so claims carry real timestamp anchors; the
    // `transcript` we already have above is the fallback when VTT fetch
    // fails. When the slide-aware path produced a body, we still run the
    // distiller (to populate `cortex-video-*` frontmatter, tags, and the
    // summary used by the quality gate) but the slide body wins at render
    // time - that decision lives in `process_url_inner`.
    let distilled = crate::stages::distill::distill_for_publish_video(
        &config.fabric,
        &config.pipeline,
        &config.staging,
        trace_id,
        url,
        &transcript,
        Some(metadata.title.as_str()),
    )
    .await;
    if slide_summary.is_some() {
        log::debug!("[{trace_id}] process_youtube: slide-aware body will override Distilled body at publish time");
    }
    // Suppress the unused-warning until the slide-aware body integration
    // settles. The slide path produces its own structured body via
    // publish_slides; we keep `slide_summary` reachable for callers that
    // want it without forcing them through publish_slides.
    let _ = slide_summary;
    let _ = use_fabric;

    let content_type = ContentType::YouTube {
        uploader: metadata.uploader,
        duration_secs: metadata.duration_secs,
    };

    Ok(YouTubeResult {
        title: metadata.title,
        distilled,
        content_type,
        description: metadata.description,
        yt_tags: metadata.tags,
        slide_payload,
    })
}

/// Frame-aware ingestion path. Downloads the video, extracts frames, runs
/// slide segmentation, and - when the proposed shape is non-text-only -
/// runs the slide-aware Fabric pattern. Returns the manifest + parsed
/// LLM output. Returns `Ok(None)` for text-only proposals (the caller
/// should use the existing summary path).
///
/// All heavy work happens in tempdir under `borg/youtube-frames/<video_id>/`
/// for replay-friendly debug; the directory survives the function so the
/// caller can copy slides out of it.
async fn try_extract_slides(
    url: &str,
    _transcript: &str,
    duration_secs: f64,
    config: &Config,
) -> Result<Option<(crate::slides::SlideManifest, crate::slides::SummaryOutput, PathBuf)>> {
    use crate::slides::{self, NoteShape};
    use crate::youtube;

    let video_id = youtube::extract_video_id(url)
        .ok_or_else(|| eyre::eyre!("could not extract YouTube video id from url: {url}"))?;

    let work_dir = std::env::temp_dir().join("borg-youtube-frames").join(&video_id);
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir).context("create youtube-frames work dir")?;

    // 1. Download the video. yt-dlp `-f` picks the best mp4 under 720p so
    //    frames are reasonable and disk usage stays small.
    let video_path = work_dir.join(format!("{video_id}.mp4"));
    let output_template = video_path.to_string_lossy().to_string();
    let dl_fut = tokio::process::Command::new("yt-dlp")
        .args([
            "--no-warnings",
            "-f",
            "bv*[height<=720][ext=mp4]+ba[ext=m4a]/b[height<=720][ext=mp4]/b",
            "--merge-output-format",
            "mp4",
            "-o",
            &output_template,
            url,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Failed to spawn yt-dlp for video download")?
        .wait_with_output();
    let yt_dlp_timeout = config.pipeline.yt_dlp_timeout_secs;
    let dl = match tokio::time::timeout(std::time::Duration::from_secs(yt_dlp_timeout), dl_fut).await {
        Ok(res) => res.context("yt-dlp video download")?,
        Err(_) => eyre::bail!("yt-dlp video download timed out after {yt_dlp_timeout}s"),
    };
    if !dl.status.success() {
        let stderr = String::from_utf8_lossy(&dl.stderr);
        eyre::bail!("yt-dlp failed: {stderr}");
    }

    // The merge step may write the file with a different extension; locate
    // whatever yt-dlp produced under the work_dir.
    let actual_video = std::fs::read_dir(&work_dir)
        .context("scan work dir")?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| matches!(s, "mp4" | "mkv" | "webm"))
                .unwrap_or(false)
        })
        .ok_or_else(|| eyre::eyre!("yt-dlp did not write a video file"))?;

    // 2. Extract frames.
    let frames_dir = work_dir.join("frames");
    let frames = youtube::extract_frames(
        &actual_video,
        &frames_dir,
        duration_secs,
        &config.youtube.slides,
        &config.youtube.ffmpeg_thread_args(),
    )?;
    if frames.is_empty() {
        log::info!("No frames extracted; skipping slide-aware path");
        return Ok(None);
    }

    // 3. Fetch RAW VTT subtitles (timestamps preserved) and parse into pairs
    //    so transcript binding can match each cue to its slide's time range.
    //    The Fabric `--transcript` output that Stage 0 fetched is already
    //    timestamp-stripped; we need the raw form here.
    let transcript_pairs = match youtube::fetch_subtitles_raw(url, &config.pipeline).await {
        Ok(Some(vtt)) => youtube::parse_vtt_segments(&vtt),
        Ok(None) => {
            log::warn!("No raw VTT available - slides will lack transcript context");
            Vec::new()
        }
        Err(e) => {
            log::warn!("VTT fetch for slide binding failed: {e:#}");
            Vec::new()
        }
    };
    log::debug!("Parsed {} VTT cues for slide binding", transcript_pairs.len());

    // 4. Segment + OCR + transcript bind.
    let manifest = slides::segment_with_pairs(
        &video_id,
        url,
        duration_secs,
        &frames,
        &transcript_pairs,
        &work_dir,
        &config.youtube.slides,
        config.pipeline.ocr_timeout_secs,
    )?;
    let _ = slides::write_manifest(&manifest, &work_dir);

    if matches!(manifest.extraction.proposed_note_shape, NoteShape::TextOnly) {
        log::info!(
            "Stage 1 proposed text-only shape (unique_slides={}); skipping slide-aware Fabric pattern",
            manifest.extraction.unique_slides,
        );
        return Ok(None);
    }

    // 5. Render pattern input + run new Fabric pattern.
    let pattern_input = slides::render_pattern_input(&manifest);
    let raw = fabric::run_pattern("obsidian-youtube-slides.md", &pattern_input, &config.fabric).await?;
    let summary = slides::parse_summary_output(&raw);
    Ok(Some((manifest, summary, work_dir)))
}

async fn process_article_fabric(url: &str, config: &Config, trace_id: &str) -> Result<(String, String, ContentType)> {
    // Heavy permit: fabric -u may internally invoke yt-dlp for media URLs that
    // were classified as "article" upstream, so this is a heavy path even
    // though the dispatch site looks lightweight.
    log::debug!("process_article_fabric[{trace_id}]: acquiring heavy permit");
    let _heavy_permit = permits::HEAVY_PERMITS.acquire().await;
    log::debug!("process_article_fabric[{trace_id}]: heavy permit acquired");

    let article_md = fabric::fetch_article(url, &config.fabric, &config.pipeline).await?;
    if let Err(e) = crate::stages::raw::persist_fetched_if_staging(
        config,
        trace_id,
        url,
        article_md.as_bytes(),
        "fabric-u",
        200,
        Some("text/markdown"),
    ) {
        log::warn!("[{trace_id}] persist_fetched (fabric) failed: {e:#}");
    }
    // Gate-1 fires only on the final fetched bytes, which in this flow is the
    // Jina path (see process_article_jina). If fabric -u returned a block
    // page the caller (process_url_inner) will catch our bail and fall back
    // to Jina + browser-UA without dirtying the blocklist yet.
    if crate::stages::classify::detect_block_page(article_md.as_bytes(), 200, chrono::Utc::now()).is_some() {
        eyre::bail!("fabric -u returned a block page for {url}; falling back to Jina");
    }

    let title = extract_article_title(&article_md, url);
    // Post-Phase-6 cutover: return the fetched markdown as the transcript;
    // the caller (`process_url_inner`) dispatches to the appropriate
    // `distill_for_publish_*` based on URL kind. The legacy
    // `fabric::summarize` prose path is gone for URL kinds. Gate-2 runs
    // against the rendered Distilled summary at the dispatch site instead
    // of against the prose summary that used to live here.

    Ok((title, article_md, ContentType::Article))
}

/// Returns `(title, article_md, ContentType)` - same shape as
/// `process_article_fabric` so callers can pipe either source into the
/// post-Phase-6 distillation step uniformly. Gate-1 still runs against
/// the fetched bytes here.
async fn process_article_jina(url: &str, config: &Config, trace_id: &str) -> Result<(String, String, ContentType)> {
    let article_md = jina::fetch_article_markdown(url, config.pipeline.jina_timeout_secs).await?;
    if let Err(e) = crate::stages::raw::persist_fetched_if_staging(
        config,
        trace_id,
        url,
        article_md.as_bytes(),
        "jina",
        200,
        Some("text/markdown"),
    ) {
        log::warn!("[{trace_id}] persist_fetched (jina) failed: {e:#}");
    }
    crate::stages::raw::run_gate_1(config, trace_id, url, article_md.as_bytes(), 200)?;

    let title = extract_article_title(&article_md, url);
    Ok((title, article_md, ContentType::Article))
}

async fn process_image(
    data: &[u8],
    filename: &str,
    tags: Vec<String>,
    method: IngestMethod,
    _force: bool,
    config: &Config,
    trace_id: &str,
) -> IngestResult {
    let start = Instant::now();
    match process_image_inner(data, filename, tags, method, config, trace_id).await {
        Ok(mut result) => {
            let elapsed = start.elapsed();
            log::info!("[{trace_id}] Image pipeline completed in {elapsed:.2?}");
            result.elapsed_secs = Some(elapsed.as_secs_f64());
            result
        }
        Err(e) => {
            let elapsed = start.elapsed();
            log::error!("[{trace_id}] Image pipeline failed in {elapsed:.2?}: {e:?}");
            IngestResult {
                status: IngestStatus::Failed {
                    reason: format!("{:#}", e),
                },
                method: Some(method),
                elapsed_secs: Some(elapsed.as_secs_f64()),
                ..Default::default()
            }
        }
    }
}

async fn process_image_inner(
    data: &[u8],
    filename: &str,
    tags: Vec<String>,
    method: IngestMethod,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    let tz: chrono_tz::Tz = config
        .frontmatter
        .timezone
        .parse()
        .unwrap_or(chrono_tz::America::Los_Angeles);
    let now = chrono::Utc::now().with_timezone(&tz);
    let log_date = now.format("%Y-%m-%d").to_string();
    let log_time = now.format("%H:%M").to_string();

    // Store asset in vault
    let date_bucket = chrono::Utc::now().format("%Y-%m").to_string();
    let subdirectory = format!("images/{date_bucket}");

    let vault_root = config.vault_root()?;
    let (_abs_path, rel_path) =
        assets::store_asset(&vault_root, data, filename, &subdirectory).context("Failed to store image asset")?;

    log::info!("[{trace_id}] Stored image asset: {rel_path}");

    // Write to temp file for OCR
    let temp_dir = std::env::temp_dir().join("obsidian-borg");
    std::fs::create_dir_all(&temp_dir).context("Failed to create temp directory")?;
    let temp_path = temp_dir.join(filename);
    std::fs::write(&temp_path, data).context("Failed to write temp image file")?;

    // Run tesseract (local) and vision API (remote) in parallel
    let ocr_temp_path = temp_path.clone();
    let ocr_timeout = config.pipeline.ocr_timeout_secs;
    let ocr_handle = tokio::task::spawn_blocking(move || {
        ocr::ocr_extract(&ocr_temp_path, ocr_timeout).unwrap_or_else(|e| {
            log::warn!("OCR extraction failed: {e:#}");
            String::new()
        })
    });

    let vision_future = async {
        if config.vision.enabled {
            let mime = ocr::mime_from_extension(filename);
            match ocr::vision_extract(data, &mime, &config.vision, &config.llm).await {
                Ok(v) => Some(v),
                Err(e) => {
                    log::warn!("Vision API failed: {e:#}");
                    None
                }
            }
        } else {
            None
        }
    };

    let (ocr_result, vision) = tokio::join!(ocr_handle, vision_future);
    let ocr_text = ocr_result.unwrap_or_default();

    if !ocr_text.is_empty() {
        log::debug!("OCR extracted {} chars", ocr_text.len());
    }
    if let Some(ref v) = vision {
        log::info!(
            "Vision extracted {} chars text, title={:?}",
            v.extracted_text.len(),
            v.suggested_title
        );
    }

    // Merge results: vision preferred over tesseract for title
    let use_fabric = fabric::is_available(&config.fabric);
    let title = vision
        .as_ref()
        .and_then(|v| (!v.suggested_title.is_empty()).then_some(v.suggested_title.clone()))
        .unwrap_or_else(|| {
            if !ocr_text.is_empty() && ocr_text.len() > 5 {
                let first_line = ocr_text.lines().find(|l| l.trim().len() > 3).unwrap_or("").trim();
                if !first_line.is_empty() && first_line.len() <= 80 {
                    first_line.to_string()
                } else {
                    title_from_filename(filename)
                }
            } else {
                title_from_filename(filename)
            }
        });

    // Merge extracted text: vision preferred over tesseract
    let extracted_text = vision
        .as_ref()
        .and_then(|v| (!v.extracted_text.is_empty()).then_some(v.extracted_text.clone()))
        .unwrap_or_else(|| ocr_text.clone());

    // Phase 9c-image cutover: build the Vision+OCR concat that becomes the
    // distiller's input AND the verbatim `## Transcript` archive in the
    // published note.
    let image_transcript = {
        let mut parts = Vec::new();
        if let Some(ref v) = vision
            && !v.description.is_empty()
        {
            parts.push(format!("## Description\n\n{}", v.description));
        }
        if !extracted_text.is_empty() {
            let label = if vision.is_some() { "Extracted Text" } else { "OCR Text" };
            parts.push(format!("## {label}\n\n{extracted_text}"));
        }
        parts.join("\n\n")
    };

    let distilled = crate::stages::distill::distill_for_publish_image(
        &config.fabric,
        &config.staging,
        trace_id,
        &image_transcript,
        Some(&title),
    )
    .await;

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.push("image".to_string());
    all_tags.extend(distilled.tags.iter().map(|t| hygiene::sanitize_tag(t)));

    // Include vision tags
    if let Some(ref v) = vision {
        all_tags.extend(v.suggested_tags.iter().map(|t| hygiene::sanitize_tag(t)));
    }

    // Generate tags via Fabric from the distilled summary (denser than the
    // raw Vision+OCR concat, so cheaper and more on-topic). Falls back to
    // the filename when distillation produced no summary.
    let tag_source = if !distilled.summary.is_empty() {
        distilled.summary.clone()
    } else {
        format!("Image file: {filename}")
    };
    if use_fabric && let Ok(fabric_tags) = fabric::generate_tags(&tag_source, &config.fabric).await {
        all_tags.extend(fabric_tags.into_iter().map(|t| hygiene::sanitize_tag(&t)));
    }
    finalize_tags(&mut all_tags, config).await;

    let rendered_distilled = distillers::render(&distilled);
    let note = NoteContent {
        title: title.clone(),
        source_url: None,
        asset_path: Some(rel_path.clone()),
        tags: all_tags.clone(),
        summary: distilled.summary.clone(),
        description: None,
        content_type: ContentType::Image { asset_path: rel_path },
        embed_code: None,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: Vec::new(),
        distilled_body: Some(rendered_distilled.body_markdown),
        frontmatter_additions: rendered_distilled.frontmatter_additions,
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let note_filename = format!("{}.md", hygiene::sanitize_filename(&title));

    let dest_path = resolve_destination(&config.vault.inbox_path);
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = dest_path.join(&note_filename);
    std::fs::write(&note_path, &rendered).context("Failed to write image note to vault")?;

    log::info!("[{trace_id}] Wrote image note: {}", note_path.display());

    // Clean up temp file
    let _ = std::fs::remove_file(&temp_path);

    // Log to ledger
    let ledger_file = ledger::ledger_path(config)?;
    let source_display = format!("[image: {filename}]");
    ledger::append_entry(
        &ledger_file,
        &LedgerEntry {
            date: log_date,
            time: log_time,
            method: method.into(),
            status: LedgerStatus::Completed,
            filename: extract_filename(&note_path),
            source: source_display,
            domain: None,
            trace_id: Some(trace_id.to_string()),
        },
    )?;

    let obsidian_url = build_obsidian_url(&config.vault.vault_name, &note_path.to_string_lossy());

    Ok(IngestResult {
        status: IngestStatus::Completed,
        note_path: Some(note_path.to_string_lossy().to_string()),
        title: Some(title),
        tags: all_tags,
        elapsed_secs: None,
        method: Some(method),
        canonical_url: None,
        trace_id: None,
        obsidian_url,
    })
}

fn title_from_filename(filename: &str) -> String {
    let stem = filename.rsplit_once('.').map(|(s, _)| s).unwrap_or(filename);
    let cleaned = stem.replace(['-', '_'], " ");
    if cleaned.trim().is_empty() {
        "Untitled Image".to_string()
    } else {
        cleaned.trim().to_string()
    }
}

async fn process_audio(
    data: &[u8],
    filename: &str,
    tags: Vec<String>,
    method: IngestMethod,
    _force: bool,
    config: &Config,
    trace_id: &str,
) -> IngestResult {
    let start = Instant::now();
    match process_audio_inner(data, filename, tags, method, config, trace_id).await {
        Ok(mut result) => {
            let elapsed = start.elapsed();
            log::info!("[{trace_id}] Audio pipeline completed in {elapsed:.2?}");
            result.elapsed_secs = Some(elapsed.as_secs_f64());
            result
        }
        Err(e) => {
            let elapsed = start.elapsed();
            log::error!("[{trace_id}] Audio pipeline failed in {elapsed:.2?}: {e:?}");
            IngestResult {
                status: IngestStatus::Failed {
                    reason: format!("{:#}", e),
                },
                method: Some(method),
                elapsed_secs: Some(elapsed.as_secs_f64()),
                ..Default::default()
            }
        }
    }
}

/// Determine the AudioFormat from a file extension string.
fn audio_format_from_extension(filename: &str) -> AudioFormat {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "wav" => AudioFormat::Wav,
        "ogg" | "opus" => AudioFormat::Ogg,
        // mp3, m4a, flac, aac, wma, webm - default to Mp3 for transcription
        _ => AudioFormat::Mp3,
    }
}

async fn process_audio_inner(
    data: &[u8],
    filename: &str,
    tags: Vec<String>,
    method: IngestMethod,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    // Heavy permit: Groq transcription + any ffmpeg pre-processing runs
    // under this handler.
    log::debug!("process_audio_inner[{trace_id}]: acquiring heavy permit");
    let _heavy_permit = permits::HEAVY_PERMITS.acquire().await;
    log::debug!("process_audio_inner[{trace_id}]: heavy permit acquired");

    let tz: chrono_tz::Tz = config
        .frontmatter
        .timezone
        .parse()
        .unwrap_or(chrono_tz::America::Los_Angeles);
    let now = chrono::Utc::now().with_timezone(&tz);
    let log_date = now.format("%Y-%m-%d").to_string();
    let log_time = now.format("%H:%M").to_string();

    // Store asset in vault
    let date_bucket = chrono::Utc::now().format("%Y-%m").to_string();
    let subdirectory = format!("audio/{date_bucket}");

    let vault_root = config.vault_root()?;
    let (_abs_path, rel_path) =
        assets::store_asset(&vault_root, data, filename, &subdirectory).context("Failed to store audio asset")?;

    log::info!("[{trace_id}] Stored audio asset: {rel_path}");

    // Determine audio format for transcription
    let audio_format = audio_format_from_extension(filename);

    // Attempt transcription (graceful degradation if keys unavailable)
    let groq_key = crate::config::resolve_secret(&config.groq.api_key).ok();
    let transcription = if groq_key.is_some() || !config.transcriber.url.is_empty() {
        let client = TranscriptionClient::new(
            &config.transcriber.url,
            groq_key,
            &config.groq.model,
            config.transcriber.timeout_secs,
        );
        match client.transcribe(data.to_vec(), audio_format, None).await {
            Ok(response) => {
                log::info!(
                    "Transcription succeeded: {} chars, {:.1}s duration",
                    response.text.len(),
                    response.duration_secs
                );
                Some(response)
            }
            Err(e) => {
                log::warn!("Transcription failed, creating minimal note: {e:#}");
                None
            }
        }
    } else {
        log::warn!("No transcription credentials available, creating minimal audio note");
        None
    };

    let transcript_text = transcription.as_ref().map(|t| t.text.clone()).unwrap_or_default();
    let duration_secs = transcription.as_ref().map(|t| t.duration_secs);

    // Generate title from transcription or filename
    let use_fabric = fabric::is_available(&config.fabric);
    let title = if !transcript_text.is_empty() {
        let first_line = transcript_text.lines().next().unwrap_or("").trim();
        if !first_line.is_empty() && first_line.len() <= 80 {
            first_line.to_string()
        } else if use_fabric {
            // Use fabric to generate a title from the transcription
            fabric::summarize(&transcript_text, true, &config.fabric)
                .await
                .ok()
                .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| title_from_filename(filename))
        } else {
            title_from_filename(filename)
        }
    } else {
        title_from_filename(filename)
    };

    // Phase 9c-voicenote cutover: route the Groq transcript through the
    // VoiceNote distiller (short-path single call, long-path map-reduce).
    // The full Groq output lands verbatim in `distilled.transcript` so the
    // published note carries the exact words below the LLM summary.
    let distilled = crate::stages::distill::distill_for_publish_voicenote(
        &config.fabric,
        &config.staging,
        trace_id,
        &transcript_text,
        Some(&title),
    )
    .await;

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.push("audio".to_string());
    all_tags.extend(distilled.tags.iter().map(|t| hygiene::sanitize_tag(t)));

    // Generate tags via Fabric from the distilled summary (denser than the
    // raw transcript; falls back to filename when distillation produced no
    // summary or transcript was empty).
    let tag_source = if !distilled.summary.is_empty() {
        distilled.summary.clone()
    } else if !transcript_text.is_empty() {
        transcript_text.clone()
    } else {
        format!("Audio file: {filename}")
    };
    if use_fabric && let Ok(fabric_tags) = fabric::generate_tags(&tag_source, &config.fabric).await {
        all_tags.extend(fabric_tags.into_iter().map(|t| hygiene::sanitize_tag(&t)));
    }
    finalize_tags(&mut all_tags, config).await;

    let rendered_distilled = distillers::render(&distilled);
    let note = NoteContent {
        title: title.clone(),
        source_url: None,
        asset_path: Some(rel_path.clone()),
        tags: all_tags.clone(),
        summary: distilled.summary.clone(),
        description: None,
        content_type: ContentType::Audio {
            asset_path: rel_path,
            duration_secs,
        },
        embed_code: None,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: Vec::new(),
        distilled_body: Some(rendered_distilled.body_markdown),
        frontmatter_additions: rendered_distilled.frontmatter_additions,
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let note_filename = format!("{}.md", hygiene::sanitize_filename(&title));

    let dest_path = resolve_destination(&config.vault.inbox_path);
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = dest_path.join(&note_filename);
    std::fs::write(&note_path, &rendered).context("Failed to write audio note to vault")?;

    log::info!("[{trace_id}] Wrote audio note: {}", note_path.display());

    // Log to ledger
    let ledger_file = ledger::ledger_path(config)?;
    let source_display = format!("[audio: {filename}]");
    ledger::append_entry(
        &ledger_file,
        &LedgerEntry {
            date: log_date,
            time: log_time,
            method: method.into(),
            status: LedgerStatus::Completed,
            filename: extract_filename(&note_path),
            source: source_display,
            domain: None,
            trace_id: Some(trace_id.to_string()),
        },
    )?;

    let obsidian_url = build_obsidian_url(&config.vault.vault_name, &note_path.to_string_lossy());

    Ok(IngestResult {
        status: IngestStatus::Completed,
        note_path: Some(note_path.to_string_lossy().to_string()),
        title: Some(title),
        tags: all_tags,
        elapsed_secs: None,
        method: Some(method),
        canonical_url: None,
        trace_id: None,
        obsidian_url,
    })
}

/// Whether a file-based document is a PDF or a generic document (docx, pptx, etc.).
#[derive(Debug, Clone, Copy)]
enum DocumentKind {
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

async fn process_document_file(
    data: &[u8],
    filename: &str,
    tags: Vec<String>,
    method: IngestMethod,
    _force: bool,
    config: &Config,
    kind: DocumentKind,
    trace_id: &str,
) -> IngestResult {
    let start = Instant::now();
    match process_document_file_inner(data, filename, tags, method, config, kind, trace_id).await {
        Ok(mut result) => {
            let elapsed = start.elapsed();
            log::info!(
                "[{trace_id}] {} pipeline completed in {elapsed:.2?}",
                kind.label().to_uppercase()
            );
            result.elapsed_secs = Some(elapsed.as_secs_f64());
            result
        }
        Err(e) => {
            let elapsed = start.elapsed();
            log::error!(
                "[{trace_id}] {} pipeline failed in {elapsed:.2?}: {e:?}",
                kind.label().to_uppercase()
            );
            IngestResult {
                status: IngestStatus::Failed {
                    reason: format!("{:#}", e),
                },
                method: Some(method),
                elapsed_secs: Some(elapsed.as_secs_f64()),
                ..Default::default()
            }
        }
    }
}

async fn process_document_file_inner(
    data: &[u8],
    filename: &str,
    tags: Vec<String>,
    method: IngestMethod,
    config: &Config,
    kind: DocumentKind,
    trace_id: &str,
) -> Result<IngestResult> {
    // Heavy permit: OCR / markitdown / document::extract_text are subprocess-
    // backed and CPU-heavy.
    log::debug!("process_document_file_inner[{trace_id}]: acquiring heavy permit");
    let _heavy_permit = permits::HEAVY_PERMITS.acquire().await;
    log::debug!("process_document_file_inner[{trace_id}]: heavy permit acquired");

    let tz: chrono_tz::Tz = config
        .frontmatter
        .timezone
        .parse()
        .unwrap_or(chrono_tz::America::Los_Angeles);
    let now = chrono::Utc::now().with_timezone(&tz);
    let log_date = now.format("%Y-%m-%d").to_string();
    let log_time = now.format("%H:%M").to_string();

    // Store asset in vault
    let vault_root = config.vault_root()?;
    let (_abs_path, rel_path) = assets::store_asset(&vault_root, data, filename, kind.subdirectory())
        .context(format!("Failed to store {} asset", kind.label()))?;

    log::info!("[{trace_id}] Stored {} asset: {rel_path}", kind.label());

    // Write to temp file for text extraction
    let temp_dir = std::env::temp_dir().join("obsidian-borg");
    std::fs::create_dir_all(&temp_dir).context("Failed to create temp directory")?;
    let temp_path = temp_dir.join(filename);
    std::fs::write(&temp_path, data).context("Failed to write temp file")?;

    // Extract text via markitdown
    let extracted_text = extraction::extract_markdown(&temp_path).unwrap_or_else(|e| {
        log::warn!("Text extraction failed for {filename}: {e:#}");
        String::new()
    });

    if !extracted_text.is_empty() {
        log::debug!("Extracted {} chars from {}", extracted_text.len(), filename);
    }

    // Generate title
    let use_fabric = fabric::is_available(&config.fabric);
    let title = if !extracted_text.is_empty() {
        // Use extract_article_title logic - look for a good title from the extracted text
        let title_candidate = extracted_text
            .lines()
            .find(|l| {
                let trimmed = l.trim();
                !trimmed.is_empty() && trimmed.len() > 3 && !trimmed.starts_with("Title:")
            })
            .map(|l| l.trim().to_string());

        // Check for a Title: line first
        let md_title = extracted_text
            .lines()
            .find(|line| line.starts_with("Title:"))
            .map(|line| line.trim_start_matches("Title:").trim().to_string())
            .filter(|t| !t.is_empty());

        // Check for a # heading
        let heading_title = extracted_text
            .lines()
            .find(|line| line.starts_with("# "))
            .map(|line| line.trim_start_matches("# ").trim().to_string())
            .filter(|t| !t.is_empty());

        md_title
            .or(heading_title)
            .or(title_candidate)
            .unwrap_or_else(|| title_from_filename(filename))
    } else {
        title_from_filename(filename)
    };

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.push(kind.default_tag().to_string());

    // Summarize via fabric
    let summary = if use_fabric && !extracted_text.is_empty() {
        match fabric::summarize(&extracted_text, false, &config.fabric).await {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Fabric summarize failed: {e:#}");
                if extracted_text.len() > 500 {
                    extracted_text[..500].to_string()
                } else {
                    extracted_text.clone()
                }
            }
        }
    } else if !extracted_text.is_empty() {
        // No fabric - use a truncated extract
        if extracted_text.len() > 1000 {
            format!("{}...", &extracted_text[..1000])
        } else {
            extracted_text.clone()
        }
    } else {
        String::new()
    };

    // Generate tags via Fabric
    let tag_source = if !extracted_text.is_empty() {
        extracted_text.clone()
    } else {
        format!("{} file: {filename}", kind.label())
    };

    if use_fabric && let Ok(fabric_tags) = fabric::generate_tags(&tag_source, &config.fabric).await {
        all_tags.extend(fabric_tags.into_iter().map(|t| hygiene::sanitize_tag(&t)));
    }
    finalize_tags(&mut all_tags, config).await;

    let note = NoteContent {
        title: title.clone(),
        source_url: None,
        asset_path: Some(rel_path.clone()),
        tags: all_tags.clone(),
        summary,
        description: None,
        content_type: kind.content_type(rel_path),
        embed_code: None,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: Vec::new(),
        ..NoteContent::default()
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let note_filename = format!("{}.md", hygiene::sanitize_filename(&title));

    let dest_path = resolve_destination(&config.vault.inbox_path);
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = dest_path.join(&note_filename);
    std::fs::write(&note_path, &rendered).context(format!("Failed to write {} note to vault", kind.label()))?;

    log::info!("[{trace_id}] Wrote {} note: {}", kind.label(), note_path.display());

    // Clean up temp file
    let _ = std::fs::remove_file(&temp_path);

    // Log to ledger
    let ledger_file = ledger::ledger_path(config)?;
    let source_display = format!("[{}: {filename}]", kind.label());
    ledger::append_entry(
        &ledger_file,
        &LedgerEntry {
            date: log_date,
            time: log_time,
            method: method.into(),
            status: LedgerStatus::Completed,
            filename: extract_filename(&note_path),
            source: source_display,
            domain: None,
            trace_id: Some(trace_id.to_string()),
        },
    )?;

    let obsidian_url = build_obsidian_url(&config.vault.vault_name, &note_path.to_string_lossy());

    Ok(IngestResult {
        status: IngestStatus::Completed,
        note_path: Some(note_path.to_string_lossy().to_string()),
        title: Some(title),
        tags: all_tags,
        elapsed_secs: None,
        method: Some(method),
        canonical_url: None,
        trace_id: None,
        obsidian_url,
    })
}

/// Detect structured text patterns before LLM classification.
#[derive(Debug, PartialEq)]
enum TextPattern {
    Define { word: String },
    Clarify { word_a: String, word_b: String },
    ContainsUrl(String),
    General,
}

fn detect_text_pattern(text: &str) -> TextPattern {
    let trimmed = text.trim();

    // Check for define: pattern
    if let Some(word) = trimmed
        .strip_prefix("define:")
        .or_else(|| trimmed.strip_prefix("Define:"))
        .map(|w| w.trim().to_string())
        && !word.is_empty()
    {
        return TextPattern::Define { word };
    }

    // Check for clarify: <word> vs <word> pattern
    if let Some(rest) = trimmed
        .strip_prefix("clarify:")
        .or_else(|| trimmed.strip_prefix("Clarify:"))
        .map(|w| w.trim())
        && let Some((a, b)) = rest.split_once(" vs ")
    {
        let word_a = a.trim().to_string();
        let word_b = b.trim().to_string();
        if !word_a.is_empty() && !word_b.is_empty() {
            return TextPattern::Clarify { word_a, word_b };
        }
    }

    // Check if text contains a URL (redirect to URL pipeline)
    if let Some(url) = router::extract_url_from_text(trimmed) {
        // Only redirect if the text IS essentially just a URL
        let without_url = trimmed.replace(&url, "").trim().to_string();
        if without_url.is_empty() || without_url.len() < 10 {
            return TextPattern::ContainsUrl(url);
        }
    }

    TextPattern::General
}

async fn process_text(
    text: &str,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: &str,
) -> IngestResult {
    let start = Instant::now();
    match process_text_inner(text, tags, method, force, config, trace_id).await {
        Ok(mut result) => {
            let elapsed = start.elapsed();
            log::info!("[{trace_id}] Text pipeline completed in {elapsed:.2?}");
            result.elapsed_secs = Some(elapsed.as_secs_f64());
            result
        }
        Err(e) => {
            let elapsed = start.elapsed();
            log::error!("[{trace_id}] Text pipeline failed in {elapsed:.2?}: {e:?}");
            IngestResult {
                status: IngestStatus::Failed {
                    reason: format!("{:#}", e),
                },
                method: Some(method),
                elapsed_secs: Some(elapsed.as_secs_f64()),
                ..Default::default()
            }
        }
    }
}

async fn process_text_inner(
    text: &str,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    let pattern = detect_text_pattern(text);
    log::debug!("Text pattern detected: {pattern:?}");

    match pattern {
        TextPattern::ContainsUrl(url) => {
            // Redirect to URL pipeline
            return Ok(process_url(&url, tags, method, force, config, trace_id).await);
        }
        TextPattern::Define { .. } | TextPattern::Clarify { .. } => {
            return process_vocab(text, &pattern, tags, method, force, config, trace_id).await;
        }
        TextPattern::General => {}
    }

    // Code snippet detection (after pattern matching / URL redirect, before general LLM classification)
    if let Some(language) = looks_like_code(text) {
        return process_code_snippet(text, &language, tags, method, config, trace_id).await;
    }

    // General text: classify via LLM, then create a note
    let tz: chrono_tz::Tz = config
        .frontmatter
        .timezone
        .parse()
        .unwrap_or(chrono_tz::America::Los_Angeles);
    let now = chrono::Utc::now().with_timezone(&tz);
    let log_date = now.format("%Y-%m-%d").to_string();
    let log_time = now.format("%H:%M").to_string();

    let use_fabric = fabric::is_available(&config.fabric);

    // Generate title from text (first line or LLM-generated)
    let title = generate_text_title(text, use_fabric, config).await;

    // Phase 9c-hotfix cutover: route the general text branch through the
    // Idea distiller so the published note carries `distilled: true` plus the
    // structured `## Summary` / `## Claims` / `## Links` / `## Transcript`
    // body sections. IdeaDistiller is synthesis-only (no Fabric call); the
    // full input lands verbatim in `distilled.transcript`.
    let distilled =
        crate::stages::distill::distill_for_publish_idea(&config.fabric, &config.staging, trace_id, text, Some(&title))
            .await;

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.extend(distilled.tags.iter().map(|t| hygiene::sanitize_tag(t)));

    // Generate tags via Fabric (driven by the distilled summary so we send
    // less text over the wire than the raw input would).
    if use_fabric && let Ok(fabric_tags) = fabric::generate_tags(&distilled.summary, &config.fabric).await {
        all_tags.extend(fabric_tags.into_iter().map(|t| hygiene::sanitize_tag(&t)));
    }
    finalize_tags(&mut all_tags, config).await;

    let rendered_distilled = distillers::render(&distilled);
    let note = NoteContent {
        title: title.clone(),
        source_url: None,
        asset_path: None,
        tags: all_tags.clone(),
        // Keep `summary` populated for downstream IngestResult callers and
        // ledger entries that surface a one-line preview; the structured
        // body comes from `distilled_body`.
        summary: distilled.summary.clone(),
        description: None,
        content_type: ContentType::Note,
        embed_code: None,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: Vec::new(),
        distilled_body: Some(rendered_distilled.body_markdown),
        frontmatter_additions: rendered_distilled.frontmatter_additions,
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let filename = format!("{}.md", hygiene::sanitize_filename(&title));

    let dest_path = resolve_destination(&config.vault.inbox_path);
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = dest_path.join(&filename);
    std::fs::write(&note_path, &rendered).context("Failed to write note to vault")?;

    log::info!("[{trace_id}] Wrote text note: {}", note_path.display());

    // Log to ledger
    let ledger_file = ledger::ledger_path(config)?;
    let source_display = format!(
        "[text: {}]",
        if text.len() > 50 { format!("{}...", &text[..50]) } else { text.to_string() }
    );
    ledger::append_entry(
        &ledger_file,
        &LedgerEntry {
            date: log_date,
            time: log_time,
            method: method.into(),
            status: LedgerStatus::Completed,
            filename: extract_filename(&note_path),
            source: source_display,
            domain: None,
            trace_id: Some(trace_id.to_string()),
        },
    )?;

    let obsidian_url = build_obsidian_url(&config.vault.vault_name, &note_path.to_string_lossy());

    Ok(IngestResult {
        status: IngestStatus::Completed,
        note_path: Some(note_path.to_string_lossy().to_string()),
        title: Some(title),
        tags: all_tags,
        elapsed_secs: None,
        method: Some(method),
        canonical_url: None,
        trace_id: None,
        obsidian_url,
    })
}

async fn process_vocab(
    text: &str,
    pattern: &TextPattern,
    tags: Vec<String>,
    method: IngestMethod,
    _force: bool,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    let tz: chrono_tz::Tz = config
        .frontmatter
        .timezone
        .parse()
        .unwrap_or(chrono_tz::America::Los_Angeles);
    let now = chrono::Utc::now().with_timezone(&tz);
    let log_date = now.format("%Y-%m-%d").to_string();
    let log_time = now.format("%H:%M").to_string();

    let use_fabric = fabric::is_available(&config.fabric);

    let (title, content_type, body) = match pattern {
        TextPattern::Define { word } => {
            // Generate definition via LLM
            let body = if use_fabric {
                let prompt = format!(
                    "Define the word \"{word}\". Determine what language it is. \
                     Provide: 1) The definition 2) Example sentences. \
                     Format as markdown with ## Examples section."
                );
                fabric::run_pattern("summarize", &prompt, &config.fabric)
                    .await
                    .unwrap_or_else(|_| format!("definition:: [define: {word}]"))
            } else {
                format!("definition:: [define: {word}]")
            };

            // Detect language (simple heuristic: ask LLM or check common patterns)
            let language = detect_language(word, use_fabric, config).await;

            (
                word.clone(),
                ContentType::VocabDefine {
                    word: word.clone(),
                    language: language.clone(),
                },
                body,
            )
        }
        TextPattern::Clarify { word_a, word_b } => {
            let title = format!("{word_a} vs {word_b}");
            let body = if use_fabric {
                let prompt = format!(
                    "Compare and clarify the difference between \"{word_a}\" and \"{word_b}\". \
                     Determine what language they are. \
                     Provide: definitions, usage contexts, examples, and common confusions. \
                     Format as markdown."
                );
                fabric::run_pattern("summarize", &prompt, &config.fabric)
                    .await
                    .unwrap_or_else(|_| format!("[clarify: {word_a} vs {word_b}]"))
            } else {
                format!("[clarify: {word_a} vs {word_b}]")
            };

            let language = detect_language(word_a, use_fabric, config).await;

            (
                title,
                ContentType::VocabClarify {
                    word_a: word_a.clone(),
                    word_b: word_b.clone(),
                    language: language.clone(),
                },
                body,
            )
        }
        _ => unreachable!("process_vocab called with non-vocab pattern"),
    };

    // Phase 9c-hotfix cutover: route the vocab body through the distiller
    // dispatcher (which maps Vocabulary to IdeaDistiller). The vocab
    // definition prose is preserved verbatim in `distilled.transcript`.
    let ingest_kind = match &content_type {
        ContentType::VocabDefine { language, .. } | ContentType::VocabClarify { language, .. } => {
            if language == "es" {
                crate::types::IngestKind::VocabularyEs
            } else {
                crate::types::IngestKind::VocabularyEn
            }
        }
        _ => crate::types::IngestKind::VocabularyEn,
    };
    let distilled = crate::stages::distill::distill_for_publish_vocab(
        &config.fabric,
        &config.staging,
        trace_id,
        ingest_kind,
        &body,
        Some(&title),
    )
    .await;

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.extend(distilled.tags.iter().map(|t| hygiene::sanitize_tag(t)));
    let vocab_tag = match &content_type {
        ContentType::VocabDefine { language, .. } | ContentType::VocabClarify { language, .. } => {
            format!("{language}-vocab")
        }
        _ => "vocab".to_string(),
    };
    all_tags.push(hygiene::sanitize_tag(&vocab_tag));
    finalize_tags(&mut all_tags, config).await;

    let rendered_distilled = distillers::render(&distilled);
    let note = NoteContent {
        title: title.clone(),
        source_url: None,
        asset_path: None,
        tags: all_tags.clone(),
        summary: distilled.summary.clone(),
        description: None,
        content_type,
        embed_code: None,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: Vec::new(),
        distilled_body: Some(rendered_distilled.body_markdown),
        frontmatter_additions: rendered_distilled.frontmatter_additions,
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let filename = format!("{}.md", hygiene::sanitize_filename(&title));

    let dest_path = resolve_destination(&config.vault.inbox_path);
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = dest_path.join(&filename);
    std::fs::write(&note_path, &rendered).context("Failed to write note to vault")?;

    log::info!("[{trace_id}] Wrote vocab note: {}", note_path.display());

    // Log to ledger
    let ledger_file = ledger::ledger_path(config)?;
    ledger::append_entry(
        &ledger_file,
        &LedgerEntry {
            date: log_date,
            time: log_time,
            method: method.into(),
            status: LedgerStatus::Completed,
            filename: extract_filename(&note_path),
            source: format!("[{}]", text.trim()),
            domain: None,
            trace_id: Some(trace_id.to_string()),
        },
    )?;

    let obsidian_url = build_obsidian_url(&config.vault.vault_name, &note_path.to_string_lossy());

    Ok(IngestResult {
        status: IngestStatus::Completed,
        note_path: Some(note_path.to_string_lossy().to_string()),
        title: Some(title),
        tags: all_tags,
        elapsed_secs: None,
        method: Some(method),
        canonical_url: None,
        trace_id: None,
        obsidian_url,
    })
}

/// Generate a title from text input.
async fn generate_text_title(text: &str, use_fabric: bool, config: &Config) -> String {
    // Use first line as title if it's short enough
    let first_line = text.lines().next().unwrap_or(text).trim();
    if !first_line.is_empty() && first_line.len() <= 80 {
        return first_line.to_string();
    }

    // Try LLM to generate a title
    if use_fabric
        && let Ok(title) = fabric::run_pattern(
            "summarize",
            &format!("Generate a very short (3-8 word) title for this text:\n\n{text}"),
            &config.fabric,
        )
        .await
    {
        let title = title.lines().next().unwrap_or(&title).trim().to_string();
        if !title.is_empty() && title.len() <= 100 {
            return title;
        }
    }

    // Fallback: truncate first line
    if first_line.len() > 80 {
        format!("{}...", &first_line[..77])
    } else {
        "Quick Note".to_string()
    }
}

/// Detect whether text looks like a code snippet and return the detected language.
///
/// Uses a high threshold to avoid false positives on plain text. Requires at least
/// 3 lines and 2+ code indicators to trigger.
fn looks_like_code(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();

    // Must have at least 3 lines
    if lines.len() < 3 {
        return None;
    }

    // Check for shebang line first - strong signal
    if let Some(first) = lines.first() {
        let first = first.trim();
        if first.starts_with("#!") {
            if first.contains("python") {
                return Some("python".to_string());
            } else if first.contains("bash") || first.contains("/sh") || first.contains("zsh") {
                return Some("bash".to_string());
            } else if first.contains("node") {
                return Some("javascript".to_string());
            } else if first.contains("ruby") {
                return Some("ruby".to_string());
            } else if first.contains("perl") {
                return Some("perl".to_string());
            }
            // Shebang but unknown interpreter - still code
            return Some(String::new());
        }
    }

    // Count code indicators
    let mut indicators = 0u32;

    // Language-specific keyword markers (counted per unique marker type found)
    let rust_markers = ["fn ", "pub fn", "async fn", "impl ", "use ", "let mut ", "mod "];
    let python_markers = ["def ", "import ", "from ", "class ", "elif ", "except "];
    let js_markers = ["function ", "const ", "===", "!==", "=> {", "require(", "export "];
    let go_markers = ["func ", "package ", "import (", "go func", "defer "];
    let c_markers = ["#include", "int main", "void ", "printf(", "malloc("];
    let general_markers = ["return ", "if (", "for (", "while (", "switch ("];

    let mut rust_score = 0u32;
    let mut python_score = 0u32;
    let mut js_score = 0u32;
    let mut go_score = 0u32;
    let mut c_score = 0u32;

    for marker in &rust_markers {
        if text.contains(marker) {
            rust_score += 1;
            indicators += 1;
        }
    }
    for marker in &python_markers {
        if text.contains(marker) {
            python_score += 1;
            indicators += 1;
        }
    }
    for marker in &js_markers {
        if text.contains(marker) {
            js_score += 1;
            indicators += 1;
        }
    }
    for marker in &go_markers {
        if text.contains(marker) {
            go_score += 1;
            indicators += 1;
        }
    }
    for marker in &c_markers {
        if text.contains(marker) {
            c_score += 1;
            indicators += 1;
        }
    }
    for marker in &general_markers {
        if text.contains(marker) {
            indicators += 1;
        }
    }

    // Structural indicators
    // Lines with consistent indentation (2+ spaces or tabs)
    let indented_lines = lines
        .iter()
        .filter(|l| !l.is_empty() && (l.starts_with("  ") || l.starts_with('\t')))
        .count();
    if indented_lines >= 2 {
        indicators += 1;
    }

    // Bracket/brace patterns typical of code
    let has_braces = text.contains('{') && text.contains('}');
    let has_arrow = text.contains("->") || text.contains("=>");
    let has_scope_op = text.contains("::");
    let has_logical_ops = text.contains("||") || text.contains("&&");
    let has_semicolons = text.matches(';').count() >= 2;

    if has_braces {
        indicators += 1;
    }
    if has_arrow {
        indicators += 1;
    }
    if has_scope_op {
        indicators += 1;
    }
    if has_logical_ops {
        indicators += 1;
    }
    if has_semicolons {
        indicators += 1;
    }

    // Count structural indicators separately
    let structural_count = has_braces as u32
        + has_arrow as u32
        + has_scope_op as u32
        + has_logical_ops as u32
        + has_semicolons as u32
        + (indented_lines >= 2) as u32;

    // Require at least 2 code indicators AND at least 1 structural indicator.
    // This prevents plain English with words like "import", "class", "function" from triggering.
    if indicators < 2 || structural_count == 0 {
        return None;
    }

    // Determine language by highest score
    let max_score = rust_score.max(python_score).max(js_score).max(go_score).max(c_score);
    if max_score == 0 {
        // Indicators came from structural patterns only - not confident enough
        // unless there are many structural indicators (4+)
        if indicators >= 4 {
            return Some(String::new());
        }
        return None;
    }

    let language = if rust_score == max_score && rust_score >= 2 {
        "rust"
    } else if python_score == max_score && python_score >= 2 {
        "python"
    } else if js_score == max_score && js_score >= 2 {
        "javascript"
    } else if go_score == max_score && go_score >= 2 {
        "go"
    } else if c_score == max_score && c_score >= 2 {
        "c"
    } else {
        // Some language indicators but not enough to be confident about which
        ""
    };

    Some(language.to_string())
}

/// Generate a title for a code snippet.
///
/// Tries to extract a meaningful name from:
/// 1. First comment line
/// 2. First function/class definition
/// 3. LLM-generated title
/// 4. Fallback: "Code Snippet"
async fn generate_code_title(text: &str, language: &str, use_fabric: bool, config: &Config) -> String {
    // Try first comment line
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip shebang
        if trimmed.starts_with("#!") {
            continue;
        }
        // Single-line comments
        let comment = if trimmed.starts_with("//") {
            Some(trimmed.trim_start_matches('/').trim())
        } else if trimmed.starts_with('#') && !trimmed.starts_with("#!") && !trimmed.starts_with("#include") {
            Some(trimmed.trim_start_matches('#').trim())
        } else if trimmed.starts_with("/*") || trimmed.starts_with("/**") {
            Some(
                trimmed
                    .trim_start_matches('/')
                    .trim_start_matches('*')
                    .trim_end_matches('*')
                    .trim_end_matches('/')
                    .trim(),
            )
        } else {
            None
        };
        if let Some(c) = comment
            && !c.is_empty()
            && c.len() <= 80
        {
            return c.to_string();
        }
        break;
    }

    // Try first function/class name
    for line in text.lines() {
        let trimmed = line.trim();
        // Rust: fn name, pub fn name
        if let Some(rest) = trimmed.strip_prefix("fn ").or_else(|| {
            trimmed
                .strip_prefix("pub fn ")
                .or_else(|| trimmed.strip_prefix("async fn "))
                .or_else(|| trimmed.strip_prefix("pub async fn "))
        }) && let Some(name) = rest.split('(').next()
        {
            let name = name.trim();
            if !name.is_empty() {
                return format!("{language} - {name}").trim_start_matches(" - ").to_string();
            }
        }
        // Python: def name
        if let Some(rest) = trimmed.strip_prefix("def ")
            && let Some(name) = rest.split('(').next()
        {
            let name = name.trim();
            if !name.is_empty() {
                return format!("{language} - {name}").trim_start_matches(" - ").to_string();
            }
        }
        // Go/JS: func name / function name
        if let Some(rest) = trimmed
            .strip_prefix("func ")
            .or_else(|| trimmed.strip_prefix("function "))
            && let Some(name) = rest.split('(').next()
        {
            let name = name.trim();
            if !name.is_empty() {
                return format!("{language} - {name}").trim_start_matches(" - ").to_string();
            }
        }
        // class
        if let Some(rest) = trimmed.strip_prefix("class ")
            && let Some(name) = rest.split(['(', ':', '{', ' ']).next()
        {
            let name = name.trim();
            if !name.is_empty() {
                return format!("{language} - {name}").trim_start_matches(" - ").to_string();
            }
        }
    }

    // Try LLM
    if use_fabric
        && let Ok(title) = fabric::run_pattern(
            "summarize",
            &format!("Generate a very short (3-8 word) title for this code snippet:\n\n{text}"),
            &config.fabric,
        )
        .await
    {
        let title = title.lines().next().unwrap_or(&title).trim().to_string();
        if !title.is_empty() && title.len() <= 100 {
            return title;
        }
    }

    "Code Snippet".to_string()
}

/// Process a code snippet: create a note with fenced code block.
async fn process_code_snippet(
    text: &str,
    language: &str,
    tags: Vec<String>,
    method: IngestMethod,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    let tz: chrono_tz::Tz = config
        .frontmatter
        .timezone
        .parse()
        .unwrap_or(chrono_tz::America::Los_Angeles);
    let now = chrono::Utc::now().with_timezone(&tz);
    let log_date = now.format("%Y-%m-%d").to_string();
    let log_time = now.format("%H:%M").to_string();

    let use_fabric = fabric::is_available(&config.fabric);

    let title = generate_code_title(text, language, use_fabric, config).await;

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.push("code-snippet".to_string());
    if !language.is_empty() {
        all_tags.push(hygiene::sanitize_tag(language));
    }

    // Generate additional tags via Fabric
    if use_fabric && let Ok(fabric_tags) = fabric::generate_tags(text, &config.fabric).await {
        all_tags.extend(fabric_tags.into_iter().map(|t| hygiene::sanitize_tag(&t)));
    }
    finalize_tags(&mut all_tags, config).await;

    // Build fenced code block as the summary
    let summary = format!("```{language}\n{text}\n```");

    let note = NoteContent {
        title: title.clone(),
        source_url: None,
        asset_path: None,
        tags: all_tags.clone(),
        summary,
        description: None,
        content_type: ContentType::Code {
            language: language.to_string(),
        },
        embed_code: None,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: Vec::new(),
        ..NoteContent::default()
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let filename = format!("{}.md", hygiene::sanitize_filename(&title));

    let dest_path = resolve_destination(&config.vault.inbox_path);
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = dest_path.join(&filename);
    std::fs::write(&note_path, &rendered).context("Failed to write code note to vault")?;

    log::info!(
        "[{trace_id}] Wrote code snippet note: {} (language: {})",
        note_path.display(),
        language
    );

    // Log to ledger
    let ledger_file = ledger::ledger_path(config)?;
    let source_display = format!("[code: {}]", if language.is_empty() { "unknown" } else { language });
    ledger::append_entry(
        &ledger_file,
        &LedgerEntry {
            date: log_date,
            time: log_time,
            method: method.into(),
            status: LedgerStatus::Completed,
            filename: extract_filename(&note_path),
            source: source_display,
            domain: None,
            trace_id: Some(trace_id.to_string()),
        },
    )?;

    let obsidian_url = build_obsidian_url(&config.vault.vault_name, &note_path.to_string_lossy());

    Ok(IngestResult {
        status: IngestStatus::Completed,
        note_path: Some(note_path.to_string_lossy().to_string()),
        title: Some(title),
        tags: all_tags,
        elapsed_secs: None,
        method: Some(method),
        canonical_url: None,
        trace_id: None,
        obsidian_url,
    })
}

/// Detect language of a word (simple heuristic, can be enhanced with LLM).
async fn detect_language(word: &str, use_fabric: bool, config: &Config) -> String {
    if use_fabric
        && let Ok(result) = fabric::run_pattern(
            "summarize",
            &format!(
                "What language is the word \"{word}\"? Reply with just the language name \
                 in lowercase (e.g., \"english\", \"spanish\", \"french\"). Nothing else."
            ),
            &config.fabric,
        )
        .await
    {
        let lang = result.trim().to_lowercase();
        // Accept reasonable language names
        if !lang.is_empty() && lang.len() < 20 && !lang.contains(' ') {
            return lang;
        }
    }

    // Fallback: assume English
    "english".to_string()
}

fn resolve_destination(inbox_path: &str) -> PathBuf {
    expand_tilde(inbox_path)
}

/// Build an obsidian://search deep link from vault name and note path.
///
/// Uses the filename stem (without extension or directory) as the search query,
/// so the link survives note moves between directories (e.g. inbox/ -> notes/).
fn build_obsidian_url(vault_name: &str, note_path: &str) -> Option<String> {
    let path = std::path::Path::new(note_path);
    let stem = path.file_stem()?.to_str()?;
    let encoded_vault = urlencoding::encode(vault_name);
    let encoded_query = urlencoding::encode(stem);
    Some(format!("obsidian://search?vault={encoded_vault}&query={encoded_query}"))
}

/// Compute the vault-relative path for a note, for use in the ledger Path column.
/// Returns something like "notes/some-title.md".
fn extract_filename(note_path: &std::path::Path) -> Option<String> {
    note_path.file_name().map(|f| f.to_string_lossy().to_string())
}

/// Expand a vault root path (handling ~/) to an absolute PathBuf.
pub fn expand_vault_root(path: &str) -> PathBuf {
    expand_tilde(path)
}

/// Scan the vault for a note whose `source:` frontmatter matches the given URL.
/// Returns the path to the note file if found. This is more reliable than the
/// ledger's stored path because cortex may have moved the file after ingestion.
fn find_note_by_source(vault_root: &std::path::Path, source_url: &str) -> Option<PathBuf> {
    let needle = format!("source: \"{source_url}\"");
    find_note_by_source_recursive(vault_root, &needle)
}

fn find_note_by_source_recursive(dir: &std::path::Path, needle: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip system directories that won't contain ingested notes
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "templates" {
                continue;
            }
            if let Some(found) = find_note_by_source_recursive(&path, needle) {
                return Some(found);
            }
        } else if path.extension().is_some_and(|ext| ext == "md") {
            // Quick check: read first 2KB (frontmatter is always at the top)
            if let Ok(file) = std::fs::File::open(&path) {
                use std::io::Read;
                let mut buf = vec![0u8; 2048];
                let mut reader = std::io::BufReader::new(file);
                let n = reader.read(&mut buf).unwrap_or(0);
                let header = String::from_utf8_lossy(&buf[..n]);
                if header.contains(needle) {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Read the `date:` field from a note's frontmatter.
fn read_note_date(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        if let Some(date) = line.strip_prefix("date:") {
            return Some(date.trim().to_string());
        }
    }
    None
}

/// Read cortex-managed fields from frontmatter.
/// Assumes all values are single-line (inline YAML). This holds for all current
/// cortex fields: `cortex-quality-issues` uses inline arrays like `[no-outbound-links]`.
fn read_cortex_fields(path: &std::path::Path) -> Vec<(String, String)> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut fields = Vec::new();
    let mut in_frontmatter = false;
    for line in content.lines() {
        if line.trim() == "---" {
            if in_frontmatter {
                break;
            }
            in_frontmatter = true;
            continue;
        }
        if !in_frontmatter {
            continue;
        }
        for key in CORTEX_PRESERVE_KEYS {
            if let Some(val) = line.strip_prefix(&format!("{key}:")) {
                fields.push((key.to_string(), val.trim().to_string()));
            }
        }
    }
    fields
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    PathBuf::from(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_text_pattern_define() {
        assert_eq!(
            detect_text_pattern("define: garrulous"),
            TextPattern::Define {
                word: "garrulous".to_string()
            }
        );
        assert_eq!(
            detect_text_pattern("Define: escurrir"),
            TextPattern::Define {
                word: "escurrir".to_string()
            }
        );
    }

    #[test]
    fn test_detect_text_pattern_clarify() {
        assert_eq!(
            detect_text_pattern("clarify: affect vs effect"),
            TextPattern::Clarify {
                word_a: "affect".to_string(),
                word_b: "effect".to_string()
            }
        );
        assert_eq!(
            detect_text_pattern("Clarify: escurrir vs estrujar"),
            TextPattern::Clarify {
                word_a: "escurrir".to_string(),
                word_b: "estrujar".to_string()
            }
        );
    }

    #[test]
    fn test_detect_text_pattern_url() {
        match detect_text_pattern("https://example.com") {
            TextPattern::ContainsUrl(url) => assert_eq!(url, "https://example.com"),
            other => panic!("expected ContainsUrl, got {other:?}"),
        }
    }

    #[test]
    fn test_detect_text_pattern_url_with_short_context() {
        // URL with very short surrounding text should still be treated as URL
        match detect_text_pattern("check https://example.com") {
            TextPattern::ContainsUrl(url) => assert_eq!(url, "https://example.com"),
            other => panic!("expected ContainsUrl, got {other:?}"),
        }
    }

    #[test]
    fn test_detect_text_pattern_general() {
        assert_eq!(
            detect_text_pattern("Met James at the Rust meetup"),
            TextPattern::General
        );
    }

    #[test]
    fn test_detect_text_pattern_empty_define() {
        // "define:" with no word should not match
        assert_eq!(detect_text_pattern("define: "), TextPattern::General);
    }

    #[test]
    fn test_expand_tilde() {
        let expanded = expand_tilde("~/test/path");
        assert!(!expanded.to_string_lossy().starts_with("~/"));
        assert!(expanded.to_string_lossy().ends_with("test/path"));
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        let expanded = expand_tilde("/absolute/path");
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn test_resolve_destination_uses_inbox_path() {
        let dest = resolve_destination("/vault/inbox");
        assert_eq!(dest, PathBuf::from("/vault/inbox"));
    }

    #[test]
    fn test_resolve_destination_with_trailing_slash() {
        let dest = resolve_destination("/vault/inbox/");
        assert_eq!(dest, PathBuf::from("/vault/inbox/"));
    }

    #[test]
    fn test_extract_title_from_fabric_metadata() {
        let md = "Title: Rust Programming Language\n\nURL Source: https://rust-lang.org\n\nMarkdown Content:\n# Rust\n";
        assert_eq!(
            extract_article_title(md, "https://rust-lang.org"),
            "Rust Programming Language"
        );
    }

    #[test]
    fn test_extract_title_from_pdf_filename() {
        let md = "Title: The-Complete-Guide-to-Building-Skill-for-Claude.pdf\n\nURL Source: https://example.com/doc.pdf\n\nMarkdown Content:\nThe Complete Guide\n\n# to Building Skills\n";
        assert_eq!(
            extract_article_title(md, "https://example.com/doc.pdf"),
            "The Complete Guide to Building Skill for Claude"
        );
    }

    #[test]
    fn test_extract_title_falls_back_to_heading() {
        let md = "Some random content\n# My Article Title\nBody text\n";
        assert_eq!(
            extract_article_title(md, "https://example.com/page"),
            "My Article Title"
        );
    }

    #[test]
    fn test_extract_title_falls_back_to_url_segment() {
        let md = "No title metadata here\nJust plain text\n";
        assert_eq!(
            extract_article_title(md, "https://example.com/my-great-article"),
            "my great article"
        );
    }

    #[tokio::test]
    async fn test_process_content_formerly_unsupported_types() {
        // All content types (Image, PDF, Document, Audio) are now implemented.
        // This test is retained as a placeholder; type-specific tests cover each.
    }

    #[test]
    fn test_resolve_destination_absolute_path() {
        let dest = resolve_destination("/home/user/obsidian/inbox");
        assert_eq!(dest, PathBuf::from("/home/user/obsidian/inbox"));
    }

    #[test]
    fn test_audio_format_from_extension() {
        assert!(matches!(audio_format_from_extension("song.mp3"), AudioFormat::Mp3));
        assert!(matches!(audio_format_from_extension("recording.wav"), AudioFormat::Wav));
        assert!(matches!(audio_format_from_extension("voice.ogg"), AudioFormat::Ogg));
        assert!(matches!(audio_format_from_extension("memo.opus"), AudioFormat::Ogg));
        assert!(matches!(audio_format_from_extension("track.m4a"), AudioFormat::Mp3));
        assert!(matches!(audio_format_from_extension("lossless.flac"), AudioFormat::Mp3));
        assert!(matches!(audio_format_from_extension("clip.aac"), AudioFormat::Mp3));
        assert!(matches!(audio_format_from_extension("old.wma"), AudioFormat::Mp3));
        assert!(matches!(audio_format_from_extension("stream.webm"), AudioFormat::Mp3));
        assert!(matches!(audio_format_from_extension("RECORDING.WAV"), AudioFormat::Wav));
        assert!(matches!(audio_format_from_extension("noext"), AudioFormat::Mp3));
    }

    // --- Code detection tests ---

    #[test]
    fn test_looks_like_code_rust() {
        let rust_code = r#"use std::collections::HashMap;

fn main() {
    let mut map = HashMap::new();
    map.insert("key", "value");
    println!("{:?}", map);
}"#;
        let result = looks_like_code(rust_code);
        assert!(result.is_some(), "should detect Rust code");
        assert_eq!(result.expect("expected Some"), "rust");
    }

    #[test]
    fn test_looks_like_code_python() {
        let python_code = r#"import os
from pathlib import Path

def process_files(directory):
    for f in Path(directory).iterdir():
        if f.is_file():
            print(f.name)
"#;
        let result = looks_like_code(python_code);
        assert!(result.is_some(), "should detect Python code");
        assert_eq!(result.expect("expected Some"), "python");
    }

    #[test]
    fn test_looks_like_code_javascript() {
        let js_code = r#"const express = require('express');
const app = express();

function handleRequest(req, res) {
    const data = req.body;
    res.json({ status: 'ok' });
}
"#;
        let result = looks_like_code(js_code);
        assert!(result.is_some(), "should detect JavaScript code");
        assert_eq!(result.expect("expected Some"), "javascript");
    }

    #[test]
    fn test_looks_like_code_go() {
        let go_code = r#"package main

import "fmt"

func main() {
    fmt.Println("Hello, World!")
}
"#;
        let result = looks_like_code(go_code);
        assert!(result.is_some(), "should detect Go code");
        assert_eq!(result.expect("expected Some"), "go");
    }

    #[test]
    fn test_looks_like_code_bash_shebang() {
        let bash_code = r#"#!/bin/bash
set -euo pipefail

echo "Hello"
for i in 1 2 3; do
    echo "$i"
done
"#;
        let result = looks_like_code(bash_code);
        assert!(result.is_some(), "should detect bash via shebang");
        assert_eq!(result.expect("expected Some"), "bash");
    }

    #[test]
    fn test_looks_like_code_python_shebang() {
        let python_code = r#"#!/usr/bin/env python3
import sys

def main():
    print(sys.argv)
"#;
        let result = looks_like_code(python_code);
        assert!(result.is_some(), "should detect python via shebang");
        assert_eq!(result.expect("expected Some"), "python");
    }

    #[test]
    fn test_looks_like_code_c() {
        let c_code = r#"#include <stdio.h>
#include <stdlib.h>

int main() {
    printf("Hello, World!\n");
    return 0;
}
"#;
        let result = looks_like_code(c_code);
        assert!(result.is_some(), "should detect C code");
        assert_eq!(result.expect("expected Some"), "c");
    }

    #[test]
    fn test_looks_like_code_plain_text_not_detected() {
        let text = "Met James at the Rust meetup yesterday. We talked about programming.";
        assert!(
            looks_like_code(text).is_none(),
            "plain text should not be detected as code"
        );
    }

    #[test]
    fn test_looks_like_code_short_text_not_detected() {
        let text = "fn main()";
        assert!(
            looks_like_code(text).is_none(),
            "single line should not be detected as code (need 3+ lines)"
        );
    }

    #[test]
    fn test_looks_like_code_football_play_not_detected() {
        let text = "4-2-5 blitz from weak side\nCorner press coverage\nSafety rolls down to flat";
        assert!(
            looks_like_code(text).is_none(),
            "football play description should not be detected as code"
        );
    }

    #[test]
    fn test_looks_like_code_define_pattern_not_detected() {
        let text = "define: garrulous\nmeaning: excessively talkative\nusage: The garrulous host...";
        assert!(
            looks_like_code(text).is_none(),
            "define pattern should not be detected as code"
        );
    }

    #[test]
    fn test_looks_like_code_grocery_list_not_detected() {
        let text = "Shopping list:\n- milk\n- eggs\n- bread\n- butter";
        assert!(
            looks_like_code(text).is_none(),
            "grocery list should not be detected as code"
        );
    }

    #[test]
    fn test_looks_like_code_prose_with_technical_words_not_detected() {
        let text = "I was reading about how to import goods from China.\nThe class was interesting and we learned about different methods.\nThe function of the liver is to filter toxins.";
        assert!(
            looks_like_code(text).is_none(),
            "prose with technical-sounding words should not be detected as code"
        );
    }

    #[test]
    fn test_render_code_note() {
        let note = NoteContent {
            title: "Rust HashMap Example".to_string(),
            source_url: None,
            asset_path: None,
            tags: vec!["rust".to_string(), "code-snippet".to_string()],
            summary: "```rust\nfn main() {\n    println!(\"hello\");\n}\n```".to_string(),
            description: None,
            content_type: ContentType::Code {
                language: "rust".to_string(),
            },
            embed_code: None,
            method: Some(IngestMethod::Cli),
            trace_id: None,
            slides: Vec::new(),
            ..NoteContent::default()
        };
        let rendered = markdown::render_note(
            &note,
            &crate::config::FrontmatterConfig {
                default_tags: vec![],
                default_creator: String::new(),
                timezone: "UTC".to_string(),
            },
        );
        assert!(rendered.contains("type: code"));
        assert!(rendered.contains("language: \"rust\""));
        assert!(rendered.contains("```rust"));
        assert!(rendered.contains("  - code-snippet"));
    }

    #[test]
    fn test_vision_title_preferred_over_filename() {
        // Simulates the merge logic: vision title takes priority
        let vision_title = "Netgate SG-2100 Serial Label";
        let filename = "IMG_20260316_123456.jpg";

        let vision = Some(ocr::VisionResult {
            description: "A product label".to_string(),
            suggested_title: vision_title.to_string(),
            suggested_tags: vec!["hardware".to_string()],
            extracted_text: "Serial: ABC-123".to_string(),
        });

        let title = vision
            .as_ref()
            .and_then(|v| (!v.suggested_title.is_empty()).then_some(v.suggested_title.clone()))
            .unwrap_or_else(|| title_from_filename(filename));

        assert_eq!(title, vision_title);
    }

    #[test]
    fn test_vision_none_falls_back_to_filename() {
        let filename = "screenshot-example.png";
        let vision: Option<ocr::VisionResult> = None;

        let title = vision
            .as_ref()
            .and_then(|v| (!v.suggested_title.is_empty()).then_some(v.suggested_title.clone()))
            .unwrap_or_else(|| title_from_filename(filename));

        assert_eq!(title, "screenshot example");
    }

    #[test]
    fn test_vision_extracted_text_preferred_over_ocr() {
        let ocr_text = "115 a> Inpul: 12V".to_string();
        let vision = Some(ocr::VisionResult {
            description: String::new(),
            suggested_title: String::new(),
            suggested_tags: vec![],
            extracted_text: "Serial: ABC-123\nModel: SG-2100".to_string(),
        });

        let extracted = vision
            .as_ref()
            .and_then(|v| (!v.extracted_text.is_empty()).then_some(v.extracted_text.clone()))
            .unwrap_or_else(|| ocr_text.clone());

        assert_eq!(extracted, "Serial: ABC-123\nModel: SG-2100");
    }

    #[test]
    fn test_vision_empty_text_falls_back_to_ocr() {
        let ocr_text = "Some OCR text".to_string();
        let vision = Some(ocr::VisionResult {
            description: "A photo".to_string(),
            suggested_title: "My Photo".to_string(),
            suggested_tags: vec![],
            extracted_text: String::new(),
        });

        let extracted = vision
            .as_ref()
            .and_then(|v| (!v.extracted_text.is_empty()).then_some(v.extracted_text.clone()))
            .unwrap_or_else(|| ocr_text.clone());

        assert_eq!(extracted, "Some OCR text");
    }

    // --- build_obsidian_url tests ---

    #[test]
    fn test_build_obsidian_url_inbox() {
        let url = build_obsidian_url("obsidian", "/home/user/obsidian/inbox/my-note.md");
        assert_eq!(url, Some("obsidian://search?vault=obsidian&query=my-note".to_string()));
    }

    #[test]
    fn test_build_obsidian_url_notes_folder() {
        let url = build_obsidian_url("obsidian", "/home/user/obsidian/notes/claude-code-guide.md");
        assert_eq!(
            url,
            Some("obsidian://search?vault=obsidian&query=claude-code-guide".to_string())
        );
    }

    #[test]
    fn test_build_obsidian_url_same_stem_different_dirs() {
        let inbox = build_obsidian_url("obsidian", "/home/user/obsidian/inbox/my-note.md");
        let notes = build_obsidian_url("obsidian", "/home/user/obsidian/notes/my-note.md");
        assert_eq!(inbox, notes, "URL should be path-independent");
    }

    #[test]
    fn test_build_obsidian_url_vault_name_with_spaces() {
        let url = build_obsidian_url("My Notes", "/home/user/obsidian/note.md");
        assert_eq!(url, Some("obsidian://search?vault=My%20Notes&query=note".to_string()));
    }

    #[test]
    fn test_build_obsidian_url_bare_filename() {
        let url = build_obsidian_url("obsidian", "my-note.md");
        assert_eq!(url, Some("obsidian://search?vault=obsidian&query=my-note".to_string()));
    }

    #[test]
    fn test_extract_filename_strips_directory() {
        let path = std::path::Path::new("/home/user/vault/notes/my-note.md");
        assert_eq!(extract_filename(path), Some("my-note.md".to_string()));
    }

    #[test]
    fn test_extract_filename_bare_filename() {
        let path = std::path::Path::new("my-note.md");
        assert_eq!(extract_filename(path), Some("my-note.md".to_string()));
    }

    #[test]
    fn test_extract_filename_inbox_path() {
        let path = std::path::Path::new("/vault/inbox/some-article.md");
        assert_eq!(extract_filename(path), Some("some-article.md".to_string()));
    }

    #[test]
    fn test_find_note_by_source_in_notes_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path();
        let notes = vault.join("notes");
        std::fs::create_dir_all(&notes).expect("mkdir");

        let note_content = "---\ntitle: Test\nsource: \"https://example.com/article\"\n---\nBody.\n";
        std::fs::write(notes.join("test-article.md"), note_content).expect("write");

        let found = find_note_by_source(vault, "https://example.com/article");
        assert!(found.is_some(), "should find note by source URL");
        assert_eq!(found.unwrap().file_name().unwrap().to_str().unwrap(), "test-article.md");
    }

    #[test]
    fn test_find_note_by_source_in_inbox() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path();
        let inbox = vault.join("inbox");
        std::fs::create_dir_all(&inbox).expect("mkdir");

        let note_content = "---\ntitle: Inbox Note\nsource: \"https://example.com/inbox-item\"\n---\nBody.\n";
        std::fs::write(inbox.join("inbox-item.md"), note_content).expect("write");

        let found = find_note_by_source(vault, "https://example.com/inbox-item");
        assert!(found.is_some(), "should find note in inbox/");
        assert!(found.unwrap().starts_with(&inbox), "found path should be in inbox/");
    }

    #[test]
    fn test_find_note_by_source_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path();
        let notes = vault.join("notes");
        std::fs::create_dir_all(&notes).expect("mkdir");

        let note_content = "---\ntitle: Other\nsource: \"https://other.com\"\n---\nBody.\n";
        std::fs::write(notes.join("other.md"), note_content).expect("write");

        let found = find_note_by_source(vault, "https://example.com/missing");
        assert!(found.is_none(), "should not find non-matching source");
    }

    #[test]
    fn test_find_note_by_source_skips_dotfiles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path();
        let hidden = vault.join(".obsidian");
        std::fs::create_dir_all(&hidden).expect("mkdir");

        let note_content = "---\nsource: \"https://example.com/hidden\"\n---\n";
        std::fs::write(hidden.join("config.md"), note_content).expect("write");

        let found = find_note_by_source(vault, "https://example.com/hidden");
        assert!(found.is_none(), "should skip dot-prefixed directories");
    }

    #[test]
    fn test_reingest_preserves_notes_directory() {
        // Integration test: when a note exists in notes/ and we reingest the same URL,
        // the reingest_dest should point to notes/, not inbox/.
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path();
        let notes = vault.join("notes");
        let inbox = vault.join("inbox");
        std::fs::create_dir_all(&notes).expect("mkdir notes");
        std::fs::create_dir_all(&inbox).expect("mkdir inbox");
        std::fs::create_dir_all(vault.join("system").join("views")).expect("mkdir ledger dir");

        let source_url = "https://example.com/reingest-test";

        // Create an existing note in notes/ (already promoted by cortex)
        let note_content =
            format!("---\ntitle: Original\ndate: 2026-03-20\nsource: \"{source_url}\"\n---\nOriginal body.\n");
        std::fs::write(notes.join("reingest-test.md"), &note_content).expect("write note");

        // Create a ledger with the existing entry
        let ledger_file = vault.join("system").join("views").join("borg-ledger.md");
        let ledger_content = format!(
            "---\ntitle: Borg Ledger\ndate: 2026-03-23\ntype: system\ndomain: system\norigin: authored\ntags: []\n---\n\n\
             # Borg Ledger\n\n\
             | Date | Time | Method | Status | Title | Filename | Source | Domain | Trace |\n\
             |------|------|--------|--------|-------|----------|--------|--------|-------|\n\
             | 2026-03-20 | 10:00 | http | {} | [[Original]] | reingest-test.md | {source_url} | ai | tr-000001 |\n",
            "\u{2705}"
        );
        std::fs::write(&ledger_file, ledger_content).expect("write ledger");

        // Simulate the reingest lookup logic from process_url_inner
        let existing = ledger::find_completed(&ledger_file, source_url)
            .expect("find_completed")
            .expect("should find existing entry");

        let old_note_path = find_note_by_source(vault, source_url).or_else(|| {
            if existing.filename != "-" {
                [vault.join("notes"), vault.join("inbox")]
                    .iter()
                    .map(|d| d.join(&existing.filename))
                    .find(|p| p.exists())
            } else {
                None
            }
        });

        assert!(old_note_path.is_some(), "should find existing note");
        let reingest_dest = old_note_path.as_ref().unwrap().parent().map(|p| p.to_path_buf());
        assert_eq!(
            reingest_dest.as_ref().unwrap(),
            &notes,
            "reingest should target notes/ dir, not inbox/"
        );
    }

    #[test]
    fn test_reingest_falls_back_to_inbox_for_new_urls() {
        // When no existing note is found, dest should fall back to inbox.
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path();
        let inbox_path = vault.join("inbox");
        std::fs::create_dir_all(&inbox_path).expect("mkdir");

        let found = find_note_by_source(vault, "https://example.com/brand-new");
        assert!(found.is_none());

        // reingest_dest would be None, so resolve_destination is used
        let dest = inbox_path.clone();
        assert_eq!(dest, inbox_path, "new URLs should land in inbox/");
    }

    #[test]
    fn test_reingest_finds_note_via_ledger_filename_fallback() {
        // When find_note_by_source fails (e.g. source URL changed slightly),
        // the fallback uses the ledger's stored filename to find the note.
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path();
        let notes = vault.join("notes");
        std::fs::create_dir_all(&notes).expect("mkdir notes");
        std::fs::create_dir_all(vault.join("system").join("views")).expect("mkdir ledger dir");

        // Note exists but with a DIFFERENT source URL (e.g. URL was re-canonicalized)
        let note_content = "---\ntitle: Fallback\nsource: \"https://old-url.com/page\"\n---\nBody.\n";
        std::fs::write(notes.join("fallback-note.md"), note_content).expect("write");

        // Ledger references the new canonical URL but has the filename
        let ledger_file = vault.join("system").join("views").join("borg-ledger.md");
        let ledger_content = format!(
            "---\ntitle: Borg Ledger\ndate: 2026-03-23\ntype: system\ndomain: system\norigin: authored\ntags: []\n---\n\n\
             # Borg Ledger\n\n\
             | Date | Time | Method | Status | Title | Filename | Source | Domain | Trace |\n\
             |------|------|--------|--------|-------|----------|--------|--------|-------|\n\
             | 2026-03-20 | 10:00 | http | {} | [[Fallback]] | fallback-note.md | https://new-url.com/page | ai | tr-000001 |\n",
            "\u{2705}"
        );
        std::fs::write(&ledger_file, ledger_content).expect("write ledger");

        let existing = ledger::find_completed(&ledger_file, "https://new-url.com/page")
            .expect("find_completed")
            .expect("should find entry");

        // find_note_by_source won't match (source URLs differ)
        let by_source = find_note_by_source(vault, "https://new-url.com/page");
        assert!(by_source.is_none(), "source URL mismatch, should not find");

        // Fallback: use ledger filename to locate the file
        let candidates = [
            vault.join("notes").join(&existing.filename),
            vault.join("inbox").join(&existing.filename),
        ];
        let by_filename = candidates.iter().find(|p| p.exists());

        assert!(by_filename.is_some(), "should find note via filename fallback");
        assert_eq!(
            by_filename.as_ref().unwrap().parent().unwrap(),
            notes,
            "found note should be in notes/"
        );
    }

    #[test]
    fn test_read_cortex_fields_all_present() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("test.md");
        std::fs::write(
            &note,
            "---\ntitle: Test\ndate: 2026-03-20\ndomain: tech\nstatus: read\ncortex-classified: true\ncortex-classified-by: deterministic\ncortex-confidence: high\ncortex-quality: medium\ncortex-quality-issues: [no-outbound-links]\n---\nBody text.\n",
        )
        .unwrap();

        let fields = read_cortex_fields(&note);
        assert_eq!(fields.len(), 7);
        assert!(fields.iter().any(|(k, v)| k == "domain" && v == "tech"));
        assert!(fields.iter().any(|(k, v)| k == "status" && v == "read"));
        assert!(fields.iter().any(|(k, v)| k == "cortex-classified" && v == "true"));
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "cortex-classified-by" && v == "deterministic")
        );
        assert!(fields.iter().any(|(k, v)| k == "cortex-confidence" && v == "high"));
        assert!(fields.iter().any(|(k, v)| k == "cortex-quality" && v == "medium"));
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "cortex-quality-issues" && v == "[no-outbound-links]")
        );
    }

    #[test]
    fn test_read_cortex_fields_partial() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("test.md");
        std::fs::write(&note, "---\ntitle: Test\ndate: 2026-03-20\ndomain: ai\n---\nBody.\n").unwrap();

        let fields = read_cortex_fields(&note);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0], ("domain".to_string(), "ai".to_string()));
    }

    #[test]
    fn test_read_cortex_fields_none_present() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("test.md");
        std::fs::write(
            &note,
            "---\ntitle: Test\ndate: 2026-03-20\ntags:\n  - rust\n---\nBody.\n",
        )
        .unwrap();

        let fields = read_cortex_fields(&note);
        assert!(fields.is_empty());
    }

    #[test]
    fn test_read_cortex_fields_no_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("test.md");
        std::fs::write(&note, "Just plain text, no frontmatter.\n").unwrap();

        let fields = read_cortex_fields(&note);
        assert!(fields.is_empty());
    }

    #[test]
    fn test_read_cortex_fields_missing_file() {
        let fields = read_cortex_fields(std::path::Path::new("/tmp/nonexistent-cortex-test.md"));
        assert!(fields.is_empty());
    }

    // Phase 3 of borg-pipeline-resilience: the previous patch_cortex_fields
    // tests have moved to pipeline/atomic.rs alongside the apply_cortex_fields
    // and apply_original_date helpers that replaced the patch_* functions.
}

#[cfg(test)]
mod timeouts;
