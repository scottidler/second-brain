//! Stage 1 slide segmentation, OCR, and transcript binding.
//!
//! Reads frame JPEGs Stage 0 wrote (`raw/<trace_id>/frames/`) plus the cleaned
//! VTT transcript, produces a `SlideManifest` (and the canonical
//! `slides/slide-NNN.jpg` files) under `transcripts/<trace_id>/`. Pure
//! bytes-on-disk work, no network. See
//! `docs/design/2026-04-29-frame-aware-youtube-ingestion.md` for the full
//! pipeline shape and the rationale behind every threshold.

use eyre::{Context, Result};
use image_hasher::{HashAlg, HasherConfig, ImageHash};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::{SlideThresholds, YoutubeSlidesConfig};
use crate::ocr;
use crate::youtube::FrameRef;

pub mod cleanup;
pub mod publish;

/// Note-shape proposal computed by Stage 1, confirmed/downgraded by Stage 2's
/// LLM via its `embed_slides` output, and enforced by Stage 3 Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoteShape {
    /// Talking-head / motion / undifferentiated content; no slides embedded.
    TextOnly,
    /// Mixed; embed one representative slide as note header.
    Hero,
    /// Slide-heavy; embed an LLM-selected subset as section markers.
    SlideSection,
}

/// One unique slide identified after the pHash dedupe pass. Each slide carries
/// its first-seen frame, the timestamp range it covered, OCR text, optional
/// vision caption, and the transcript segments bound to its time range.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Slide {
    pub id: String,
    /// Path to the canonical slide JPEG, relative to `transcripts/<trace_id>/`.
    pub frame_path: PathBuf,
    pub start: f64,
    pub end: f64,
    pub duration: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ocr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcript: Vec<String>,
}

/// A chronological pHash cluster - intermediate result of segmentation, before
/// transition-drop and ID assignment.
#[derive(Debug, Clone)]
pub struct Cluster {
    pub canonical: FrameRef,
    pub start: f64,
    pub end: f64,
}

/// Frame counts + compression ratio derived during segmentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ExtractionStats {
    pub frames_after_mpdecimate: u32,
    pub unique_slides: u32,
    pub transitions_dropped: u32,
    pub compression_ratio: f32,
    pub proposed_note_shape: NoteShape,
}

/// Snippet of video metadata embedded in `slides.yml` for replay self-containment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct VideoMetaSnippet {
    pub url: String,
    pub duration_seconds: f64,
}

/// Stage 1 manifest written as `transcripts/<trace_id>/slides.yml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SlideManifest {
    pub trace_id: String,
    pub video: VideoMetaSnippet,
    pub extraction: ExtractionStats,
    pub slides: Vec<Slide>,
}

/// One parsed VTT cue: start time + text. Speeds up transcript binding to a
/// single linear scan per slide (no re-parsing).
#[derive(Debug, Clone)]
pub struct VttSegment {
    pub start: f64,
    pub text: String,
}

/// Compute a pHash for one frame on disk. Returns `Err` on I/O or decode error.
pub fn hash_frame(path: &Path) -> Result<ImageHash> {
    let img = image::open(path).with_context(|| format!("decode {}", path.display()))?;
    let hasher = HasherConfig::new()
        .hash_alg(HashAlg::DoubleGradient)
        .hash_size(8, 8)
        .to_hasher();
    Ok(hasher.hash_image(&img))
}

/// Segment frames into pHash clusters. Walks frames in chronological (index)
/// order; a frame joins the previous cluster iff its hash is within the
/// configured Hamming threshold of that cluster's canonical-frame hash.
/// Otherwise the frame starts a new cluster as its own canonical first-seen.
///
/// `total_duration_secs` extends the final cluster's end time so the last slide
/// covers the tail of the video. Frames must be sorted by index ascending; the
/// caller (Stage 1) reads them from disk that way already.
pub fn cluster_frames(
    frames: &[FrameRef],
    hashes: &[ImageHash],
    threshold: u32,
    total_duration_secs: f64,
) -> Vec<Cluster> {
    assert_eq!(
        frames.len(),
        hashes.len(),
        "cluster_frames: frames and hashes must be parallel slices"
    );
    if frames.is_empty() {
        return Vec::new();
    }

    let mut clusters: Vec<Cluster> = Vec::new();
    let mut current = Cluster {
        canonical: frames[0].clone(),
        start: frames[0].timestamp_secs,
        end: frames[0].timestamp_secs,
    };
    let mut current_hash = hashes[0].clone();

    for i in 1..frames.len() {
        let dist = current_hash.dist(&hashes[i]);
        if dist <= threshold {
            current.end = frames[i].timestamp_secs;
        } else {
            clusters.push(current);
            current = Cluster {
                canonical: frames[i].clone(),
                start: frames[i].timestamp_secs,
                end: frames[i].timestamp_secs,
            };
            current_hash = hashes[i].clone();
        }
    }
    // Close out the final cluster, extending its end to the video duration so
    // the last slide carries trailing transcript.
    if total_duration_secs > current.end {
        current.end = total_duration_secs;
    }
    clusters.push(current);
    clusters
}

/// Drop clusters shorter than the configured transition threshold.
pub fn drop_transitions(clusters: Vec<Cluster>, transition_min_seconds: f32) -> (Vec<Cluster>, u32) {
    let min = transition_min_seconds as f64;
    let total_before = clusters.len();
    let kept: Vec<Cluster> = clusters.into_iter().filter(|c| (c.end - c.start) >= min).collect();
    let dropped = total_before.saturating_sub(kept.len()) as u32;
    (kept, dropped)
}

/// Decide the proposed note shape from frame counts and unique-slide counts.
/// See the table in the design doc; static slide deck collapses ratio low,
/// talking-head also collapses low - the `min_unique_slides` tiebreaker forces
/// talking-head into text-only.
pub fn propose_note_shape(frames_after_mpdecimate: u32, unique_slides: u32, thresholds: &SlideThresholds) -> NoteShape {
    if unique_slides < thresholds.min_unique_slides {
        return NoteShape::TextOnly;
    }
    if frames_after_mpdecimate == 0 {
        return NoteShape::TextOnly;
    }
    let ratio = unique_slides as f32 / frames_after_mpdecimate as f32;
    if ratio >= thresholds.text_only_max_ratio {
        return NoteShape::TextOnly;
    }
    if ratio < thresholds.slide_section_max_ratio {
        return NoteShape::SlideSection;
    }
    NoteShape::Hero
}

/// Parse a cleaned VTT-derived transcript (or any "[HH:MM] text" / "HH:MM text"
/// formatted block) into time-anchored segments. Tolerates both the rolling-
/// timestamp form Fabric/yt-dlp emits (`[00:42]`) and bare HH:MM form.
///
/// Falls back to an empty list when no recognizable timestamp prefix is found,
/// in which case slide.transcript stays empty - the on-slide OCR + caption
/// remain as the slide's textual content for the LLM.
pub fn parse_transcript_segments(text: &str) -> Vec<VttSegment> {
    let mut segments = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((start_secs, rest)) = strip_leading_timestamp(line) {
            segments.push(VttSegment {
                start: start_secs,
                text: rest.to_string(),
            });
        }
    }
    segments
}

/// Strip a leading timestamp from a line and return `(seconds, rest)`. Accepts:
///   `[00:42] text`  `[01:02:03] text`  `00:42 text`  `01:02:03 text`
fn strip_leading_timestamp(line: &str) -> Option<(f64, &str)> {
    let (ts, rest) = if let Some(close) = line.strip_prefix('[').and_then(|s| s.find(']').map(|i| (s, i))) {
        let inside = &close.0[..close.1];
        let after = &close.0[close.1 + 1..];
        (inside, after.trim_start())
    } else {
        let mut split = line.splitn(2, char::is_whitespace);
        let head = split.next()?;
        let tail = split.next().unwrap_or("");
        if !head.contains(':') {
            return None;
        }
        (head, tail)
    };
    let secs = parse_hms(ts)?;
    Some((secs, rest))
}

fn parse_hms(s: &str) -> Option<f64> {
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

/// Bind already-time-anchored `(start_secs, text)` pairs to slides. Used by
/// callers that have raw VTT and parsed it directly (preserving timestamps
/// the cleaned-text form would have stripped).
pub fn bind_transcript_pairs(slides: &mut [Slide], pairs: &[(f64, String)]) {
    for slide in slides.iter_mut() {
        slide.transcript = pairs
            .iter()
            .filter(|(start, _)| *start >= slide.start && *start < slide.end)
            .map(|(start, text)| format!("[{}] {}", format_mmss(*start), text))
            .collect();
    }
}

/// Bind transcript segments to slides. Each slide gets the segments whose
/// `start` falls within its `[start, end)` range. Segments are formatted as
/// `[MM:SS] text` for reproducibility in `slides.yml`.
pub fn bind_transcript(slides: &mut [Slide], segments: &[VttSegment]) {
    for slide in slides.iter_mut() {
        slide.transcript = segments
            .iter()
            .filter(|seg| seg.start >= slide.start && seg.start < slide.end)
            .map(|seg| format!("[{}] {}", format_mmss(seg.start), seg.text))
            .collect();
    }
}

fn format_mmss(secs: f64) -> String {
    let total = secs as i64;
    let m = total / 60;
    let s = total % 60;
    format!("{m:02}:{s:02}")
}

/// Run Tesseract OCR over each slide's canonical frame in parallel.
/// Caller passes the directory holding the canonical slide JPEGs (i.e. the
/// `slides/` directory inside `transcripts/<trace_id>/`). Failures degrade
/// gracefully to empty OCR text - we'd rather lose OCR for one slide than
/// fail the whole ingestion.
pub fn ocr_slides(slides: &mut [Slide], slide_dir: &Path, timeout_secs: u64) {
    let results: Vec<(usize, String)> = slides
        .par_iter()
        .enumerate()
        .map(|(i, slide)| {
            let abs = slide_dir.join(slide.frame_path.file_name().unwrap_or_default());
            let text = ocr::ocr_extract(&abs, timeout_secs).unwrap_or_else(|e| {
                log::warn!("OCR failed for {}: {e:#}", abs.display());
                String::new()
            });
            (i, text)
        })
        .collect();
    for (i, text) in results {
        slides[i].ocr = text;
    }
}

/// Copy each cluster's canonical frame to `slide_dir/slide-NNN.jpg` and build
/// the `Slide` records (with frame_path, start, end, duration). OCR/caption/
/// transcript are filled by later steps.
pub fn materialize_slides(clusters: &[Cluster], slide_dir: &Path) -> Result<Vec<Slide>> {
    std::fs::create_dir_all(slide_dir).with_context(|| format!("create slide dir: {}", slide_dir.display()))?;
    let mut slides = Vec::with_capacity(clusters.len());
    for (i, c) in clusters.iter().enumerate() {
        let id = format!("s{:03}", i + 1);
        let filename = format!("slide-{:03}.jpg", i + 1);
        let dest = slide_dir.join(&filename);
        std::fs::copy(&c.canonical.path, &dest)
            .with_context(|| format!("copy {} -> {}", c.canonical.path.display(), dest.display()))?;
        let frame_path = PathBuf::from("slides").join(&filename);
        slides.push(Slide {
            id,
            frame_path,
            start: c.start,
            end: c.end,
            duration: (c.end - c.start).max(0.0),
            ocr: String::new(),
            caption: None,
            transcript: Vec::new(),
        });
    }
    Ok(slides)
}

/// Variant of `segment` that takes already-parsed timestamped transcript
/// pairs (as produced by `youtube::parse_vtt_segments`). The text-form
/// `segment` builds these from a synthesized "[MM:SS] text" string, which
/// only works if the caller already had timestamps - VTT is the canonical
/// upstream form, so this lets the YouTube path skip the round-trip.
pub fn segment_with_pairs(
    trace_id: &str,
    video_url: &str,
    duration_secs: f64,
    frames: &[FrameRef],
    transcript_pairs: &[(f64, String)],
    out_dir: &Path,
    config: &YoutubeSlidesConfig,
    ocr_timeout_secs: u64,
) -> Result<SlideManifest> {
    let mut manifest = segment(
        trace_id,
        video_url,
        duration_secs,
        frames,
        "",
        out_dir,
        config,
        ocr_timeout_secs,
    )?;
    bind_transcript_pairs(&mut manifest.slides, transcript_pairs);
    Ok(manifest)
}

/// End-to-end Stage 1 segmentation: hash, cluster, drop transitions, copy
/// canonical frames into `out_dir/slides/`, OCR them, bind transcript, return
/// the populated manifest. Caller is responsible for vision captioning (which
/// depends on async config) and for writing the manifest YAML.
pub fn segment(
    trace_id: &str,
    video_url: &str,
    duration_secs: f64,
    frames: &[FrameRef],
    transcript_text: &str,
    out_dir: &Path,
    config: &YoutubeSlidesConfig,
    ocr_timeout_secs: u64,
) -> Result<SlideManifest> {
    log::debug!(
        "slides::segment: trace={trace_id} frames={} duration={duration_secs}",
        frames.len(),
    );
    let frames_after_mpdecimate = frames.len() as u32;

    if frames.is_empty() {
        // Empty manifest - extraction collapsed to nothing or was disabled.
        return Ok(SlideManifest {
            trace_id: trace_id.to_string(),
            video: VideoMetaSnippet {
                url: video_url.to_string(),
                duration_seconds: duration_secs,
            },
            extraction: ExtractionStats {
                frames_after_mpdecimate: 0,
                unique_slides: 0,
                transitions_dropped: 0,
                compression_ratio: 0.0,
                proposed_note_shape: NoteShape::TextOnly,
            },
            slides: Vec::new(),
        });
    }

    // Hash all frames in parallel - rayon is already a dep transitively, and
    // pHash-per-frame is microseconds but image decode is the real cost.
    let hashes: Vec<ImageHash> = frames
        .par_iter()
        .map(|f| {
            hash_frame(&f.path).unwrap_or_else(|e| {
                log::warn!("pHash failed for {}: {e:#}", f.path.display());
                // 64-bit zero hash so this frame still clusters with whatever's adjacent.
                ImageHash::from_bytes(&[0u8; 8]).expect("zero hash bytes")
            })
        })
        .collect();

    let raw_clusters = cluster_frames(frames, &hashes, config.phash_hamming_threshold, duration_secs);
    let (kept, transitions_dropped) = drop_transitions(raw_clusters, config.transition_min_seconds);
    let unique_slides = kept.len() as u32;

    let proposed_note_shape = propose_note_shape(frames_after_mpdecimate, unique_slides, &config.slide_thresholds);

    let slide_dir = out_dir.join("slides");
    let mut slides = materialize_slides(&kept, &slide_dir)?;

    ocr_slides(&mut slides, &slide_dir, ocr_timeout_secs);

    let segments = parse_transcript_segments(transcript_text);
    bind_transcript(&mut slides, &segments);

    let compression_ratio = if frames_after_mpdecimate == 0 {
        0.0
    } else {
        unique_slides as f32 / frames_after_mpdecimate as f32
    };

    Ok(SlideManifest {
        trace_id: trace_id.to_string(),
        video: VideoMetaSnippet {
            url: video_url.to_string(),
            duration_seconds: duration_secs,
        },
        extraction: ExtractionStats {
            frames_after_mpdecimate,
            unique_slides,
            transitions_dropped,
            compression_ratio,
            proposed_note_shape,
        },
        slides,
    })
}

/// Render a `slides.yml` manifest to disk under `out_dir/slides.yml`.
pub fn write_manifest(manifest: &SlideManifest, out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("create out dir: {}", out_dir.display()))?;
    let path = out_dir.join("slides.yml");
    let yaml = serde_yaml::to_string(manifest).context("serialize SlideManifest")?;
    std::fs::write(&path, yaml).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Render a `SlideManifest` to the markdown shape the
/// `obsidian-youtube-slides` Fabric pattern expects as input. Embeds image
/// references using the manifest's stored `frame_path` so the LLM has context
/// when deciding which slides to include in its `embed_slides` output.
pub fn render_pattern_input(manifest: &SlideManifest) -> String {
    let mut out = String::new();
    out.push_str("# Slide-aware video summary input\n\n");
    out.push_str(&format!("Video URL: {}\n", manifest.video.url));
    out.push_str(&format!("Duration: {:.1}s\n", manifest.video.duration_seconds,));
    let shape = match manifest.extraction.proposed_note_shape {
        NoteShape::TextOnly => "text-only",
        NoteShape::Hero => "hero",
        NoteShape::SlideSection => "slide-section",
    };
    out.push_str(&format!("Note shape: {shape}\n"));
    out.push_str(&format!(
        "Frames extracted: {} / Unique slides: {} / Compression ratio: {:.3}\n\n",
        manifest.extraction.frames_after_mpdecimate,
        manifest.extraction.unique_slides,
        manifest.extraction.compression_ratio,
    ));

    for slide in &manifest.slides {
        out.push_str(&format!(
            "## Slide {} - {} -> {}\n\n",
            slide.id,
            format_mmss(slide.start),
            format_mmss(slide.end),
        ));
        out.push_str(&format!("![]({})\n\n", slide.frame_path.display()));
        if !slide.ocr.trim().is_empty() {
            out.push_str("On-slide text (OCR):\n");
            for line in slide.ocr.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    out.push_str(&format!("> {trimmed}\n"));
                }
            }
            out.push('\n');
        }
        if let Some(caption) = &slide.caption
            && !caption.trim().is_empty()
        {
            out.push_str("Visual caption:\n");
            out.push_str(&format!("> {caption}\n\n"));
        }
        if !slide.transcript.is_empty() {
            out.push_str("Transcript while this slide was on screen:\n");
            for seg in &slide.transcript {
                out.push_str(&format!("- {seg}\n"));
            }
            out.push('\n');
        }
    }
    out
}

/// One LLM-named section in the published note (slide id + human title).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct SummarySection {
    pub slide: String,
    pub title: String,
}

/// Frontmatter at the top of the Stage 2 Fabric output, parsed from a YAML
/// block delimited by `---` lines. Field naming intentionally matches the
/// design doc's pattern-output spec: `embed_slides` (with underscore in the
/// LLM output) is mapped to a tidy Rust field via serde alias.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SummaryFrontmatter {
    pub shape: String,
    #[serde(alias = "embed_slides")]
    pub embed_slides: Vec<String>,
    #[serde(default)]
    pub sections: Vec<SummarySection>,
}

impl Default for SummaryFrontmatter {
    fn default() -> Self {
        Self {
            shape: "text-only".to_string(),
            embed_slides: Vec::new(),
            sections: Vec::new(),
        }
    }
}

/// Result of splitting the Stage 2 LLM output into frontmatter + body.
#[derive(Debug, Clone)]
pub struct SummaryOutput {
    pub frontmatter: SummaryFrontmatter,
    pub body: String,
}

/// Parse the LLM output produced by the `obsidian-youtube-slides` pattern.
/// Tolerant: when no leading `---` frontmatter is present, treats the whole
/// thing as body with a default `text-only` frontmatter (the pattern output
/// for shape `text-only` may legitimately be just prose).
pub fn parse_summary_output(text: &str) -> SummaryOutput {
    let trimmed = text.trim_start_matches('\n');
    if let Some(rest) = trimmed.strip_prefix("---\n")
        && let Some(end_idx) = rest.find("\n---")
    {
        let yaml = &rest[..end_idx];
        let after_close = &rest[end_idx + "\n---".len()..];
        let body = after_close.trim_start_matches('\n').to_string();
        let frontmatter = serde_yaml::from_str::<SummaryFrontmatter>(yaml).unwrap_or_default();
        return SummaryOutput { frontmatter, body };
    }
    SummaryOutput {
        frontmatter: SummaryFrontmatter::default(),
        body: text.to_string(),
    }
}

/// Stage 3 enforcement gate: validate the LLM's `embed_slides` against
/// Stage 1's proposed note shape and, if needed, downgrade.
///
/// Returns the (possibly downgraded) shape and the (possibly truncated)
/// list of slide IDs to embed. Slide IDs not present in the manifest are
/// dropped silently. The "downgrade only, no upgrade" rule comes from the
/// design doc: Stage 1's mechanical analysis is grounded in actual frame
/// data; an LLM upgrade beyond that is almost certainly hallucination.
pub fn enforce_shape(
    manifest: &SlideManifest,
    requested_shape: &str,
    requested_slides: &[String],
) -> (NoteShape, Vec<String>) {
    let manifest_ids: std::collections::HashSet<&str> = manifest.slides.iter().map(|s| s.id.as_str()).collect();
    let requested_existing: Vec<String> = requested_slides
        .iter()
        .filter(|id| manifest_ids.contains(id.as_str()))
        .cloned()
        .collect();

    let llm_shape = match requested_shape {
        "slide-section" => NoteShape::SlideSection,
        "hero" => NoteShape::Hero,
        _ => NoteShape::TextOnly,
    };

    // Cap the LLM's shape by Stage 1's proposal (downgrade-only).
    let final_shape = match (manifest.extraction.proposed_note_shape, llm_shape) {
        (NoteShape::TextOnly, _) => NoteShape::TextOnly,
        (NoteShape::Hero, NoteShape::SlideSection) => {
            log::warn!(
                "[{trace}] Stage 3: LLM proposed slide-section but Stage 1 only allows hero; downgrading",
                trace = manifest.trace_id,
            );
            NoteShape::Hero
        }
        (_, llm) => llm,
    };

    let final_slides = match final_shape {
        NoteShape::TextOnly => Vec::new(),
        NoteShape::Hero => requested_existing.into_iter().take(1).collect(),
        NoteShape::SlideSection => requested_existing,
    };

    (final_shape, final_slides)
}

#[cfg(test)]
mod tests;
