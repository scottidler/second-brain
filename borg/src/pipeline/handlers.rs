use super::*;

/// Extract the best title from fabric's markdown output.
///
/// Strategy (in priority order):
/// 1. `Title:` metadata line (fabric always emits this first)
///    - For HTML pages this is the <title> tag (usually great)
///    - For PDFs this is often the filename - we clean it up
/// 2. First `# ` heading in the markdown body
/// 3. Derive from the URL path (last segment, cleaned up)
/// 4. Raw URL as last resort
pub(crate) fn extract_article_title(article_md: &str, url: &str) -> String {
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

/// Unified YouTube processing: yt-dlp for metadata, fabric for transcript+summary.
/// Metadata and transcript run concurrently. Fabric is optional (gates transcript+summary).
/// See docs/design/2026-03-22-youtube-metadata-pipeline-redesign.md.
pub(crate) async fn process_youtube(url: &str, config: &Config, trace_id: &str) -> Result<YouTubeResult> {
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
pub(crate) async fn try_extract_slides(
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

/// Returns `(title, article_md, byline)`. `fabric -u` exposes no HTML, so the
/// byline is always `None` on this path.
pub(crate) async fn process_article_fabric(
    url: &str,
    config: &Config,
    trace_id: &str,
) -> Result<(String, String, Option<String>)> {
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

    Ok((title, article_md, None))
}

/// Returns `(title, article_md, byline)` - same shape as
/// `process_article_fabric` so callers can pipe either source into the
/// post-Phase-6 distillation step uniformly. Gate-1 still runs against
/// the fetched bytes here. The byline is `None` on the Jina markdown path and
/// carries the browser-UA fallback's `byline::extract` result otherwise.
pub(crate) async fn process_article_jina(
    url: &str,
    config: &Config,
    trace_id: &str,
) -> Result<(String, String, Option<String>)> {
    let (article_md, byline) = jina::fetch_article_markdown(url, config.pipeline.jina_timeout_secs).await?;
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
    Ok((title, article_md, byline))
}

pub(crate) async fn process_image(
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

pub(crate) async fn process_image_inner(
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

    let dest_path = config.inbox_dir()?;
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = dest_path.join(&note_filename);
    std::fs::write(&note_path, &rendered).context("Failed to write image note to vault")?;

    log::info!("[{trace_id}] Wrote image note: {}", note_path.display());

    // Clean up temp file
    let _ = std::fs::remove_file(&temp_path);

    // Log to ledger
    let ledger_file = ledger::ledger_path()?;
    let source_display = format!("[image: {filename}]");
    ledger::append_entry(
        &ledger_file,
        &LedgerEntry {
            date: log_date,
            time: log_time,
            method: method.into(),
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
        failure_stage: None,
    })
}

pub(crate) fn title_from_filename(filename: &str) -> String {
    let stem = filename.rsplit_once('.').map(|(s, _)| s).unwrap_or(filename);
    let cleaned = stem.replace(['-', '_'], " ");
    if cleaned.trim().is_empty() {
        "Untitled Image".to_string()
    } else {
        cleaned.trim().to_string()
    }
}

pub(crate) async fn process_audio(
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
pub(crate) fn audio_format_from_extension(filename: &str) -> AudioFormat {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "wav" => AudioFormat::Wav,
        "ogg" | "opus" => AudioFormat::Ogg,
        // mp3, m4a, flac, aac, wma, webm - default to Mp3 for transcription
        _ => AudioFormat::Mp3,
    }
}

pub(crate) async fn process_audio_inner(
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

    let dest_path = config.inbox_dir()?;
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = dest_path.join(&note_filename);
    std::fs::write(&note_path, &rendered).context("Failed to write audio note to vault")?;

    log::info!("[{trace_id}] Wrote audio note: {}", note_path.display());

    // Log to ledger
    let ledger_file = ledger::ledger_path()?;
    let source_display = format!("[audio: {filename}]");
    ledger::append_entry(
        &ledger_file,
        &LedgerEntry {
            date: log_date,
            time: log_time,
            method: method.into(),
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
        failure_stage: None,
    })
}

pub(crate) async fn process_document_file(
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

pub(crate) async fn process_document_file_inner(
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
                vault::text::truncate(&extracted_text, 500).to_string()
            }
        }
    } else if !extracted_text.is_empty() {
        // No fabric - use a truncated extract
        vault::text::truncate_with_ellipsis(&extracted_text, 1000)
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

    let dest_path = config.inbox_dir()?;
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = dest_path.join(&note_filename);
    std::fs::write(&note_path, &rendered).context(format!("Failed to write {} note to vault", kind.label()))?;

    log::info!("[{trace_id}] Wrote {} note: {}", kind.label(), note_path.display());

    // Clean up temp file
    let _ = std::fs::remove_file(&temp_path);

    // Log to ledger
    let ledger_file = ledger::ledger_path()?;
    let source_display = format!("[{}: {filename}]", kind.label());
    ledger::append_entry(
        &ledger_file,
        &LedgerEntry {
            date: log_date,
            time: log_time,
            method: method.into(),
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
        failure_stage: None,
    })
}
