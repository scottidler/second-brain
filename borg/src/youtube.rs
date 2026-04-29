use eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use crate::config::YoutubeSlidesConfig;

static VIDEO_ID_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?:youtube\.com/watch\?v=|youtu\.be/|youtube\.com/shorts/|youtube\.com/embed/)([a-zA-Z0-9_-]{11})",
    )
    .expect("valid regex")
});

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

pub fn fetch_metadata(url: &str) -> Result<VideoMetadata> {
    log::debug!("yt-dlp: fetching metadata for {url}");
    let output = Command::new("yt-dlp")
        .args(["--dump-json", "--no-download", "--no-warnings", url])
        .output()
        .context("Failed to run yt-dlp - is it installed?")?;

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

pub async fn fetch_subtitles(url: &str) -> Result<Option<String>> {
    log::debug!("yt-dlp: fetching subtitles for {url}");
    let output = Command::new("yt-dlp")
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
        .output()
        .context("Failed to run yt-dlp for subtitles")?;

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

    // Try to get the subtitle content from the JSON
    let subs: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_default();
    log::debug!(
        "Parsed subtitles JSON keys: {:?}",
        subs.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );
    if let Some(en_sub) = subs.get("en") {
        // Prefer local filepath if yt-dlp wrote the file
        if let Some(filepath) = en_sub.get("filepath").and_then(|f| f.as_str()) {
            log::debug!("Reading subtitle file: {filepath}");
            let content = std::fs::read_to_string(filepath).context("Failed to read subtitle file")?;
            let cleaned = clean_vtt(&content);
            log::debug!("Subtitle file read and cleaned: {} chars", cleaned.len());
            let _ = std::fs::remove_file(filepath);
            return Ok(Some(cleaned));
        }
        // Fall back to downloading from the URL
        if let Some(sub_url) = en_sub.get("url").and_then(|u| u.as_str()) {
            log::debug!("Downloading subtitles from URL: {sub_url}");
            let response = reqwest::get(sub_url).await.context("Failed to download subtitle VTT")?;
            if response.status().is_success() {
                let content = response.text().await.context("Failed to read subtitle response")?;
                let cleaned = clean_vtt(&content);
                log::debug!("Downloaded and cleaned subtitles: {} chars", cleaned.len());
                return Ok(Some(cleaned));
            }
            log::warn!("Subtitle download returned status {}", response.status());
        }
    }

    log::debug!("No usable 'en' subtitle entry found in JSON");
    Ok(None)
}

pub fn extract_audio(url: &str, output_dir: &str) -> Result<String> {
    log::debug!("yt-dlp: extracting audio for {url} to {output_dir}");
    let output_template = format!("{output_dir}/%(id)s.%(ext)s");

    let output = Command::new("yt-dlp")
        .args([
            "-x",
            "--audio-format",
            "mp3",
            "--audio-quality",
            "5",
            "-o",
            &output_template,
            url,
        ])
        .output()
        .context("Failed to run yt-dlp for audio extraction")?;

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
pub fn extract_frames(
    video_path: &Path,
    out_dir: &Path,
    duration_secs: f64,
    config: &YoutubeSlidesConfig,
) -> Result<Vec<FrameRef>> {
    if !config.enabled {
        log::debug!("extract_frames: disabled by config; returning empty");
        return Ok(Vec::new());
    }
    log::debug!(
        "extract_frames: video={} out_dir={} duration_secs={duration_secs}",
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

    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            &video_path.to_string_lossy(),
            "-vf",
            &filter,
            "-frames:v",
            &budget.to_string(),
            "-q:v",
            "4",
            "-vsync",
            "vfr",
            &frames_arg,
        ])
        .output()
        .context("Failed to run ffmpeg - is it installed?")?;

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

fn clean_vtt(vtt: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut last_line = String::new();

    for line in vtt.lines() {
        let line = line.trim();

        // Skip VTT headers and timestamps
        if line.starts_with("WEBVTT")
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
            || line.contains("-->")
            || line.is_empty()
        {
            continue;
        }

        // Skip numeric cue identifiers
        if line.parse::<u32>().is_ok() {
            continue;
        }

        // Remove HTML tags
        let cleaned = line
            .replace("<c>", "")
            .replace("</c>", "")
            .replace("<i>", "")
            .replace("</i>", "");

        // Deduplicate consecutive identical lines
        if cleaned != last_line {
            lines.push(cleaned.clone());
            last_line = cleaned;
        }
    }

    lines.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_vtt_removes_headers() {
        let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:00.000 --> 00:00:05.000\nHello world\n\n00:00:05.000 --> 00:00:10.000\nThis is a test";
        let result = clean_vtt(vtt);
        assert_eq!(result, "Hello world This is a test");
    }

    #[test]
    fn test_clean_vtt_removes_html_tags() {
        let vtt = "00:00:00.000 --> 00:00:05.000\n<c>Hello</c> <i>world</i>";
        let result = clean_vtt(vtt);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_clean_vtt_deduplicates() {
        let vtt = "00:00:00.000 --> 00:00:05.000\nHello\n\n00:00:05.000 --> 00:00:10.000\nHello\n\n00:00:10.000 --> 00:00:15.000\nWorld";
        let result = clean_vtt(vtt);
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn test_extract_video_id_watch() {
        let id = extract_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(id, Some("dQw4w9WgXcQ".to_string()));
    }

    #[test]
    fn test_extract_video_id_short() {
        let id = extract_video_id("https://youtu.be/dQw4w9WgXcQ");
        assert_eq!(id, Some("dQw4w9WgXcQ".to_string()));
    }

    #[test]
    fn test_extract_video_id_shorts() {
        let id = extract_video_id("https://youtube.com/shorts/dQw4w9WgXcQ");
        assert_eq!(id, Some("dQw4w9WgXcQ".to_string()));
    }

    #[test]
    fn test_extract_video_id_none() {
        let id = extract_video_id("https://example.com/page");
        assert_eq!(id, None);
    }

    #[test]
    fn test_generate_embed_code() {
        let code = generate_embed_code("abc123_-XYZ", 854, 480);
        assert!(code.contains("abc123_-XYZ"));
        assert!(code.contains("854"));
        assert!(code.contains("480"));
        assert!(code.contains("iframe"));
    }

    #[test]
    fn test_frame_budget_short_video() {
        let cfg = YoutubeSlidesConfig::default();
        let (budget, fps) = frame_budget(15.0, &cfg);
        assert_eq!(budget, 30);
        assert!((fps - 1.0).abs() < 1e-3, "expected ~1 fps for short video, got {fps}");
    }

    #[test]
    fn test_frame_budget_one_minute() {
        let cfg = YoutubeSlidesConfig::default();
        let (budget, fps) = frame_budget(60.0, &cfg);
        assert_eq!(budget, 40);
        // 40 / 60 = 0.666...
        assert!(
            (fps - 0.6667).abs() < 1e-3,
            "expected ~0.667 fps for 60s video, got {fps}"
        );
    }

    #[test]
    fn test_frame_budget_three_minutes() {
        let cfg = YoutubeSlidesConfig::default();
        let (budget, fps) = frame_budget(180.0, &cfg);
        assert_eq!(budget, 60);
        // 60 / 180 = 0.333...
        assert!(
            (fps - 0.3333).abs() < 1e-3,
            "expected ~0.333 fps for 180s video, got {fps}"
        );
    }

    #[test]
    fn test_frame_budget_long_video_caps_at_100() {
        let cfg = YoutubeSlidesConfig::default();
        let (budget, _) = frame_budget(3600.0, &cfg);
        assert_eq!(budget, 100);
    }

    #[test]
    fn test_frame_budget_respects_max_fps() {
        let cfg = YoutubeSlidesConfig {
            max_fps: 0.5,
            ..YoutubeSlidesConfig::default()
        };
        let (_, fps) = frame_budget(15.0, &cfg);
        assert!(fps <= 0.5, "fps should be capped at max_fps; got {fps}");
    }

    #[test]
    fn test_frame_budget_respects_max_frames() {
        let cfg = YoutubeSlidesConfig {
            max_frames: 50,
            ..YoutubeSlidesConfig::default()
        };
        let (budget, _) = frame_budget(3600.0, &cfg);
        assert_eq!(budget, 50);
    }

    #[test]
    fn test_extract_frames_disabled_returns_empty() {
        let cfg = YoutubeSlidesConfig {
            enabled: false,
            ..YoutubeSlidesConfig::default()
        };
        let tmp = std::env::temp_dir().join("borg-test-frames-disabled");
        let _ = std::fs::remove_dir_all(&tmp);
        let frames = extract_frames(Path::new("/nonexistent.mp4"), &tmp.join("frames"), 30.0, &cfg)
            .expect("disabled path should not error");
        assert!(frames.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Synthesize a small test mp4 with `ffmpeg -f lavfi` and run frame extraction.
    /// Skipped if ffmpeg is not on PATH; serves as a smoke test that the filter
    /// chain is well-formed and the sidecar gets written.
    #[test]
    fn test_extract_frames_synthetic_video() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("ffmpeg not found; skipping test_extract_frames_synthetic_video");
            return;
        }
        let tmp = std::env::temp_dir().join("borg-test-frames-synth");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("create tmp");
        let video = tmp.join("synthetic.mp4");

        // 10s of testsrc at 5fps, 320x240. Plenty of motion so mpdecimate
        // does not collapse it to nothing.
        let synth = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=10:rate=5:size=320x240",
                "-pix_fmt",
                "yuv420p",
                &video.to_string_lossy(),
            ])
            .output()
            .expect("ffmpeg synth");
        assert!(
            synth.status.success(),
            "ffmpeg synth: {:?}",
            String::from_utf8_lossy(&synth.stderr)
        );

        let cfg = YoutubeSlidesConfig::default();
        let frames_dir = tmp.join("frames");
        let frames = extract_frames(&video, &frames_dir, 10.0, &cfg).expect("extract_frames");

        assert!(
            !frames.is_empty(),
            "expected at least one frame from a 10s testsrc video"
        );
        // Auto-fps for 10s video is 1fps from the table; mpdecimate will not collapse
        // testsrc's continuously-changing pattern. Expect close to budget=30.
        assert!(
            frames.len() <= 30,
            "frames {} should not exceed budget=30",
            frames.len(),
        );

        // Frames should be named frame_NNNN.jpg, sequentially indexed.
        for (i, fr) in frames.iter().enumerate() {
            let expected = format!("frame_{:04}.jpg", i + 1);
            assert_eq!(fr.path.file_name().and_then(|s| s.to_str()), Some(expected.as_str()),);
            assert!(fr.path.exists(), "frame should exist on disk: {}", fr.path.display());
        }

        // Sidecar was written.
        let sidecar_path = tmp.join("frames.yml");
        assert!(sidecar_path.exists(), "frames.yml sidecar should exist");
        let sidecar_yaml = std::fs::read_to_string(&sidecar_path).expect("read sidecar");
        let sidecar: FramesSidecar = serde_yaml::from_str(&sidecar_yaml).expect("parse sidecar");
        assert_eq!(sidecar.frames_extracted as usize, frames.len());
        assert_eq!(sidecar.video_duration_secs, 10.0);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
