use eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;
use tokio::process::Command as TokioCommand;

use crate::config::{PipelineConfig, YoutubeSlidesConfig};

/// Shared HTTP client for ad-hoc YouTube subtitle URL downloads. Built lazily;
/// the timeout below applies per-request (connect + body), so a hung CDN
/// cannot leave the pipeline waiting forever.
static SUBTITLE_HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60)) // hard ceiling; per-call timeout below tightens this
        .build()
        .expect("build subtitle reqwest client")
});

static VIDEO_ID_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?:youtube\.com/watch\?v=|youtu\.be/|youtube\.com/shorts/|youtube\.com/embed/)([a-zA-Z0-9_-]{11})",
    )
    .expect("valid regex")
});

/// Matches VTT inline markup that auto-generated captions embed in cue text:
/// classed/plain caption tags (`<c>`, `</c>`, `<c.colorE5E5E5>`) and per-word
/// timing tags (`<00:00:00.360>`) that rolling captions use to mark timing
/// within a growing cue. Neither is literal `<c>`/`</c>` only - the classed
/// and timing forms slipped through the old literal-string replace and
/// leaked into staged transcripts and note bodies.
static VTT_TAG_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"</?c[^>]*>|<\d{2}:\d{2}:\d{2}\.\d{3}>").expect("valid regex"));

/// Strip VTT inline markup from a cue's raw text: classed/timing tags via
/// regex, plus the italic tags VTT also emits.
fn strip_vtt_tags(text: &str) -> String {
    let stripped = VTT_TAG_REGEX.replace_all(text, "");
    stripped.replace("<i>", "").replace("</i>", "")
}

/// Outcome of comparing a newly-cleaned cue/line against the last accumulated
/// one, for the rolling-caption dedupe shared by `clean_vtt` and
/// `parse_vtt_segments`.
enum RollingAction {
    /// No overlap with the prior line; accept as a new entry.
    Push,
    /// The candidate extends the prior line (rolling caption grew by a word
    /// or two); replace the prior entry with the more complete candidate.
    Replace,
    /// The candidate is already covered by the prior line (a regression,
    /// silence-fill duplicate, or the settled repeat of a growing line);
    /// drop it.
    Skip,
}

/// Decide how a new cue's cleaned text merges into the rolling-caption
/// dedupe accumulator, given the previously accumulated text (if any).
/// Auto-generated "rolling" captions emit each spoken line multiple times as
/// it is built up word-by-word cue-to-cue, then repeat the settled line
/// verbatim once it stops growing - this collapses the extends/covered-by/
/// duplicate cases into a single accepted line. Both observations are from
/// `claude-video/scripts/transcribe.py:55-67`.
fn rolling_dedupe_action(last: Option<&str>, candidate: &str) -> RollingAction {
    match last {
        Some(last) if candidate.starts_with(last) => RollingAction::Replace,
        Some(last) if last.starts_with(candidate) => RollingAction::Skip,
        Some(last) if last == candidate => RollingAction::Skip,
        _ => RollingAction::Push,
    }
}

pub fn extract_video_id(url: &str) -> Option<String> {
    VIDEO_ID_REGEX
        .captures(url)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

pub fn generate_embed_code(video_id: &str, width: usize, height: usize) -> String {
    format!(
        r#"<iframe width="{width}" height="{height}" src="https://www.youtube.com/embed/{video_id}" frameborder="0" allowfullscreen></iframe>"#
    )
}

#[derive(Debug)]
pub struct VideoMetadata {
    pub title: String,
    pub uploader: String,
    pub duration_secs: f64,
    pub description: String,
    pub tags: Vec<String>,
}

pub async fn fetch_metadata(url: &str, timeout_secs: u64) -> Result<VideoMetadata> {
    log::debug!("yt-dlp: fetching metadata for {url} (timeout={timeout_secs}s)");
    let yt_dlp_fut = TokioCommand::new("yt-dlp")
        .args(["--dump-json", "--no-download", "--no-warnings", url])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Failed to spawn yt-dlp - is it installed?")?
        .wait_with_output();

    let output = match tokio::time::timeout(Duration::from_secs(timeout_secs), yt_dlp_fut).await {
        Ok(res) => res.context("Failed to wait for yt-dlp")?,
        Err(_) => bail!("yt-dlp metadata timed out after {timeout_secs}s for {url}"),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::error!("yt-dlp metadata failed (exit {}): {stderr}", output.status);
        bail!("yt-dlp failed: {stderr}");
    }
    log::debug!(
        "yt-dlp: metadata fetch succeeded ({} bytes stdout)",
        output.stdout.len()
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).context("Failed to parse yt-dlp JSON")?;

    Ok(VideoMetadata {
        title: json["title"].as_str().unwrap_or("Unknown").to_string(),
        uploader: json["uploader"].as_str().unwrap_or("Unknown").to_string(),
        duration_secs: json["duration"].as_f64().unwrap_or(0.0),
        description: json["description"].as_str().unwrap_or("").to_string(),
        tags: json["tags"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect())
            .unwrap_or_default(),
    })
}

pub async fn fetch_subtitles(url: &str, pipeline: &PipelineConfig) -> Result<Option<String>> {
    let raw = fetch_subtitles_raw(url, pipeline).await?;
    Ok(raw.map(|v| clean_vtt(&v)))
}

/// Fetch the raw VTT for a video (timestamps preserved). Used by the
/// frame-aware slide pipeline to bind transcript snippets to per-slide
/// time ranges. Returns None when no English captions are available.
///
/// Bounded by `pipeline.yt_dlp_timeout_secs` (yt-dlp child) and
/// `pipeline.subtitle_fetch_timeout_secs` (the subtitle-URL HTTP fetch).
/// `kill_on_drop(true)` on the yt-dlp child ensures the OS process is
/// terminated when the timeout future is dropped.
pub async fn fetch_subtitles_raw(url: &str, pipeline: &PipelineConfig) -> Result<Option<String>> {
    log::debug!("yt-dlp: fetching subtitles for {url}");
    let yt_dlp_fut = TokioCommand::new("yt-dlp")
        .args([
            "--write-auto-sub",
            "--sub-lang",
            "en",
            "--sub-format",
            "vtt",
            "--skip-download",
            "--print",
            "%(requested_subtitles)j",
            url,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Failed to spawn yt-dlp - is it installed?")?
        .wait_with_output();

    let output = match tokio::time::timeout(Duration::from_secs(pipeline.yt_dlp_timeout_secs), yt_dlp_fut).await {
        Ok(res) => res.context("Failed to wait for yt-dlp")?,
        Err(_) => {
            log::warn!(
                "yt-dlp subtitles timed out after {}s for {url}",
                pipeline.yt_dlp_timeout_secs
            );
            return Ok(None);
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::debug!("yt-dlp subtitles failed (exit {}): {stderr}", output.status);
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    log::debug!("yt-dlp subtitles output: {trimmed}");

    if trimmed == "NA" || trimmed == "null" || trimmed.is_empty() {
        log::debug!("No subtitles available (output was: {trimmed})");
        return Ok(None);
    }

    let subs: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_default();
    if let Some(en_sub) = subs.get("en") {
        if let Some(filepath) = en_sub.get("filepath").and_then(|f| f.as_str()) {
            log::debug!("Reading subtitle file: {filepath}");
            let content = std::fs::read_to_string(filepath).context("Failed to read subtitle file")?;
            let _ = std::fs::remove_file(filepath);
            return Ok(Some(content));
        }
        if let Some(sub_url) = en_sub.get("url").and_then(|u| u.as_str()) {
            log::debug!("Downloading subtitles from URL: {sub_url}");
            let response = match tokio::time::timeout(
                Duration::from_secs(pipeline.subtitle_fetch_timeout_secs),
                SUBTITLE_HTTP_CLIENT.get(sub_url).send(),
            )
            .await
            {
                Ok(res) => res.context("Failed to download subtitle VTT")?,
                Err(_) => {
                    log::warn!(
                        "subtitle URL fetch timed out after {}s",
                        pipeline.subtitle_fetch_timeout_secs
                    );
                    return Ok(None);
                }
            };
            if response.status().is_success() {
                let content = response.text().await.context("Failed to read subtitle response")?;
                return Ok(Some(content));
            }
            log::warn!("Subtitle download returned status {}", response.status());
        }
    }

    log::debug!("No usable 'en' subtitle entry found in JSON");
    Ok(None)
}

/// Parse a raw VTT subtitle file into `(start_secs, text)` segments. Each
/// VTT cue begins with a `HH:MM:SS.mmm --> HH:MM:SS.mmm` line followed by
/// one or more text lines; we keep only the start time and concatenate the
/// text. Classed/plain caption tags (`<c>`, `<c.colorE5E5E5>`), per-word
/// timing tags (`<00:00:00.360>`), and italic tags (`<i>`) are stripped, and
/// rolling-caption overlap is collapsed (see `rolling_dedupe_action`) so a
/// spoken line that grows cue-to-cue is emitted once. Returns segments in
/// the shape that `slides::bind_transcript` consumes after format-rendering.
pub fn parse_vtt_segments(vtt: &str) -> Vec<(f64, String)> {
    log::debug!("parse_vtt_segments: parsing vtt ({} bytes)", vtt.len());
    let mut segments: Vec<(f64, String)> = Vec::new();
    let mut current_start: Option<f64> = None;
    let mut current_text = String::new();

    let push_current = |segments: &mut Vec<(f64, String)>, start: &mut Option<f64>, text: &mut String| {
        if let Some(s) = start.take() {
            let cleaned = strip_vtt_tags(text).trim().to_string();
            if !cleaned.is_empty() {
                match rolling_dedupe_action(segments.last().map(|(_, t)| t.as_str()), &cleaned) {
                    RollingAction::Replace => {
                        // Keep the earliest start time for the growing line;
                        // the candidate is the same utterance, just more complete.
                        let (prev_start, _) = segments.pop().expect("last existed for Replace action");
                        segments.push((prev_start, cleaned));
                    }
                    RollingAction::Skip => {}
                    RollingAction::Push => segments.push((s, cleaned)),
                }
            }
            text.clear();
        }
    };

    for line in vtt.lines() {
        let line = line.trim();
        if line.starts_with("WEBVTT") || line.starts_with("Kind:") || line.starts_with("Language:") {
            continue;
        }
        if let Some(arrow_idx) = line.find("-->") {
            push_current(&mut segments, &mut current_start, &mut current_text);
            let start_str = line[..arrow_idx].trim();
            current_start = parse_vtt_timestamp(start_str);
            continue;
        }
        if line.is_empty() {
            push_current(&mut segments, &mut current_start, &mut current_text);
            continue;
        }
        if line.parse::<u32>().is_ok() {
            continue;
        }
        if !current_text.is_empty() {
            current_text.push(' ');
        }
        current_text.push_str(line);
    }
    push_current(&mut segments, &mut current_start, &mut current_text);
    log::debug!("parse_vtt_segments: parsed {} segments", segments.len());
    segments
}

fn parse_vtt_timestamp(s: &str) -> Option<f64> {
    // VTT timestamps: HH:MM:SS.mmm or MM:SS.mmm
    let parts: Vec<&str> = s.split(':').collect();
    let nums: Vec<f64> = parts
        .iter()
        .map(|p| p.parse::<f64>().ok())
        .collect::<Option<Vec<_>>>()?;
    match nums.len() {
        2 => Some(nums[0] * 60.0 + nums[1]),
        3 => Some(nums[0] * 3600.0 + nums[1] * 60.0 + nums[2]),
        _ => None,
    }
}

/// Extract audio bound for Whisper transcription. Output is mono 16kHz mp3
/// at ~64kbps, which packs ~480KB/min and stays under Whisper's 25MB upload
/// limit for ~50min mono audio. The `-vn -ac 1 -ar 16000 -b:a 64k` ffmpeg
/// post-processor args come from `claude-video/scripts/whisper.py` (lifted
/// per the design doc). `-ac 1` matters: stereo doubles the upload bytes
/// while Whisper down-mixes anyway.
pub async fn extract_audio(url: &str, output_dir: &str, ffmpeg_threads: usize, timeout_secs: u64) -> Result<String> {
    log::debug!(
        "yt-dlp: extracting audio for {url} to {output_dir} (ffmpeg-threads={ffmpeg_threads} timeout={timeout_secs}s)"
    );
    let output_template = format!("{output_dir}/%(id)s.%(ext)s");
    let postprocessor_args = format!("ffmpeg:-threads {ffmpeg_threads} -vn -ac 1 -ar 16000 -b:a 64k");

    // tokio::process + timeout + kill_on_drop: this is a full yt-dlp download
    // (potentially large/slow) run from an async fn. A blocking
    // std::process::Command::output() here could not be interrupted even by
    // the pipeline hard timeout, since that only cancels the future, not a
    // blocked OS thread.
    let yt_dlp_fut = TokioCommand::new("yt-dlp")
        .args([
            "-x",
            "--audio-format",
            "mp3",
            // Whisper-tuned ffmpeg args: drop video, mono, 16kHz, 64kbps.
            // `-threads` capped per youtube.ffmpeg-threads config.
            "--postprocessor-args",
            &postprocessor_args,
            "-o",
            &output_template,
            url,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Failed to spawn yt-dlp for audio extraction")?
        .wait_with_output();

    let output = match tokio::time::timeout(Duration::from_secs(timeout_secs), yt_dlp_fut).await {
        Ok(res) => res.context("Failed to run yt-dlp for audio extraction")?,
        Err(_) => bail!("yt-dlp audio extraction timed out after {timeout_secs}s for {url}"),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::error!("yt-dlp audio extraction failed (exit {}): {stderr}", output.status);
        bail!("yt-dlp audio extraction failed: {stderr}");
    }

    // Find the output file
    let stdout = String::from_utf8_lossy(&output.stdout);
    log::debug!("yt-dlp audio extraction stdout:\n{stdout}");
    for line in stdout.lines() {
        if line.contains("[ExtractAudio] Destination:")
            && let Some(path) = line.split("Destination:").nth(1)
        {
            return Ok(path.trim().to_string());
        }
    }

    bail!("Could not determine audio output path from yt-dlp")
}

/// One extracted frame: where it lives on disk and when in the source video it occurred.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FrameRef {
    pub index: u32,
    pub path: PathBuf,
    pub timestamp_secs: f64,
}

/// Sidecar written next to the extracted frames as `frames.yml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FramesSidecar {
    pub video_duration_secs: f64,
    pub frame_budget: u32,
    pub effective_fps: f32,
    pub frames_extracted: u32,
    pub frames: Vec<FrameRef>,
}

/// Compute the effective source-resampling fps given a video duration.
/// Returns `(frame_budget, effective_fps)`, derived from the auto-fps table in
/// `claude-video/scripts/frames.py:94-110` (lightly adapted). The actual
/// JPEG count after mpdecimate may be lower than the budget for static content.
pub fn frame_budget(duration_secs: f64, config: &YoutubeSlidesConfig) -> (u32, f32) {
    let raw_fps: f32 = if duration_secs <= 30.0 {
        1.0
    } else if duration_secs <= 60.0 {
        // 40 frames over the duration, capped by max_fps
        40.0 / duration_secs as f32
    } else if duration_secs <= 180.0 {
        60.0 / duration_secs as f32
    } else if duration_secs <= 600.0 {
        80.0 / duration_secs as f32
    } else {
        100.0 / duration_secs as f32
    };

    let budget: u32 = if duration_secs <= 30.0 {
        30
    } else if duration_secs <= 60.0 {
        40
    } else if duration_secs <= 180.0 {
        60
    } else if duration_secs <= 600.0 {
        80
    } else {
        100
    };

    let effective_fps = raw_fps.min(config.max_fps).max(0.001);
    let capped_budget = budget.min(config.max_frames);
    (capped_budget, effective_fps)
}

/// Extract a budget-bounded set of frames from a video to JPEGs.
///
/// Filter chain order is load-bearing: `fps -> mpdecimate -> scale`.
/// `fps` resamples the source to the budget rate first; `mpdecimate` then
/// drops near-identical neighbors from the downsampled stream; `scale`
/// resizes for token efficiency. The reverse order would let `fps` re-fill
/// gaps mpdecimate created by duplicating the previous frame, undoing dedupe.
///
/// Writes `<out_dir>/frame_NNNN.jpg` (1-indexed, 4-digit zero-padded) plus
/// `<out_dir>/../frames.yml` sidecar listing each frame's source-video timestamp.
/// The output directory is created if missing.
pub async fn extract_frames(
    video_path: &Path,
    out_dir: &Path,
    duration_secs: f64,
    config: &YoutubeSlidesConfig,
    thread_args: &[String; 4],
    timeout_secs: u64,
) -> Result<Vec<FrameRef>> {
    if !config.enabled {
        log::debug!("extract_frames: disabled by config; returning empty");
        return Ok(Vec::new());
    }
    log::debug!(
        "extract_frames: video={} out_dir={} duration_secs={duration_secs} thread_args={thread_args:?}",
        video_path.display(),
        out_dir.display(),
    );

    std::fs::create_dir_all(out_dir).with_context(|| format!("Failed to create frames dir: {}", out_dir.display()))?;

    let (budget, effective_fps) = frame_budget(duration_secs, config);

    let filter = format!(
        "fps={fps},mpdecimate=hi={hi}:lo={lo}:frac={frac},scale={px}:-2",
        fps = effective_fps,
        hi = config.mpdecimate_hi,
        lo = config.mpdecimate_lo,
        frac = config.mpdecimate_frac,
        px = config.frame_resolution_px,
    );
    let frames_glob = out_dir.join("frame_%04d.jpg");
    let frames_arg = frames_glob.to_string_lossy().to_string();

    let video_path_str = video_path.to_string_lossy().to_string();
    let budget_str = budget.to_string();
    let argv: [&str; 19] = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        &thread_args[0],
        &thread_args[1],
        &thread_args[2],
        &thread_args[3],
        "-i",
        &video_path_str,
        "-vf",
        &filter,
        "-frames:v",
        &budget_str,
        "-q:v",
        "4",
        "-vsync",
        "vfr",
        &frames_arg,
    ];
    // tokio::process + timeout + kill_on_drop: ffmpeg frame extraction runs
    // from an async fn and can be slow on long videos; a blocking
    // Command::output() here is uninterruptible even by the pipeline hard
    // timeout (it only cancels the future, not a blocked thread).
    let ffmpeg_fut = TokioCommand::new("ffmpeg")
        .args(argv)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Failed to spawn ffmpeg - is it installed?")?
        .wait_with_output();

    let output = match tokio::time::timeout(Duration::from_secs(timeout_secs), ffmpeg_fut).await {
        Ok(res) => res.context("Failed to run ffmpeg")?,
        Err(_) => bail!("ffmpeg frame extraction timed out after {timeout_secs}s"),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::error!("ffmpeg frame extraction failed (exit {}): {stderr}", output.status);
        bail!("ffmpeg frame extraction failed: {stderr}");
    }

    let mut frames: Vec<FrameRef> = std::fs::read_dir(out_dir)
        .with_context(|| format!("Failed to read frames dir: {}", out_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jpg"))
        .collect::<Vec<_>>()
        .into_iter()
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?;
            let idx_str = stem.strip_prefix("frame_")?;
            let index: u32 = idx_str.parse().ok()?;
            // Timestamp = (index - 1) / effective_fps. ffmpeg writes frame_0001
            // for the first emitted frame, which corresponds to time 0 after fps
            // resampling. The mpdecimate filter may have dropped intermediate
            // frames so this is a lower bound on the actual source-video time;
            // good enough for the slide-segmentation step which uses ranges.
            let timestamp_secs = ((index.saturating_sub(1)) as f64) / (effective_fps as f64);
            Some(FrameRef {
                index,
                path,
                timestamp_secs,
            })
        })
        .collect();
    frames.sort_by_key(|f| f.index);

    let sidecar = FramesSidecar {
        video_duration_secs: duration_secs,
        frame_budget: budget,
        effective_fps,
        frames_extracted: frames.len() as u32,
        frames: frames.clone(),
    };
    let sidecar_path = out_dir
        .parent()
        .map(|p| p.join("frames.yml"))
        .unwrap_or_else(|| out_dir.join("frames.yml"));
    let sidecar_yaml = serde_yaml::to_string(&sidecar).context("Failed to serialize frames sidecar")?;
    std::fs::write(&sidecar_path, sidecar_yaml)
        .with_context(|| format!("Failed to write frames sidecar: {}", sidecar_path.display()))?;

    log::info!(
        "extract_frames: wrote {} frames to {} (budget={budget}, fps={effective_fps:.3})",
        frames.len(),
        out_dir.display(),
    );

    Ok(frames)
}

/// Strip VTT plumbing (header, timestamps, cue numbers, HTML tags) and
/// collapse consecutive prefix-overlapping cues into one line. Auto-generated
/// captions emit a "rolling prefix" - each cue extends the previous text by
/// a word or two - which the naive consecutive-dedupe misses. The rolling
/// dedupe pops the previous line when the new line starts with it (the new
/// is the more complete continuation), and skips the new line when the
/// previous line already covers it (regression / silence-fill duplicate).
/// Both observations are from `claude-video/scripts/transcribe.py:55-67`.
fn clean_vtt(vtt: &str) -> String {
    log::debug!("clean_vtt: cleaning vtt ({} bytes)", vtt.len());
    let mut lines: Vec<String> = Vec::new();

    for line in vtt.lines() {
        let line = line.trim();

        if line.starts_with("WEBVTT")
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
            || line.contains("-->")
            || line.is_empty()
        {
            continue;
        }
        if line.parse::<u32>().is_ok() {
            continue;
        }

        let cleaned = strip_vtt_tags(line);

        if cleaned.is_empty() {
            continue;
        }

        match rolling_dedupe_action(lines.last().map(|s| s.as_str()), &cleaned) {
            RollingAction::Replace => {
                lines.pop();
                lines.push(cleaned);
            }
            RollingAction::Skip => {}
            RollingAction::Push => lines.push(cleaned),
        }
    }

    let result = lines.join(" ");
    log::debug!("clean_vtt: collapsed to {} lines ({} bytes)", lines.len(), result.len());
    result
}

#[cfg(test)]
mod tests;
