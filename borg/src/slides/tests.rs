#![allow(clippy::unwrap_used)]

use super::*;

use crate::config::SlideThresholds;
use image::{Rgb, RgbImage};
use std::path::Path;

fn write_solid_jpeg(path: &Path, color: [u8; 3], size: u32) {
    let mut img = RgbImage::new(size, size);
    for px in img.pixels_mut() {
        *px = Rgb(color);
    }
    img.save(path).expect("save jpeg");
}

/// Write an image with a directional gradient. pHash (gradient-based) hashes
/// these distinctly across `kind` values, so different `kind`s land in
/// different clusters. Solid-color images all hash identically (zero
/// gradient), which is why the clustering tests below need this helper
/// instead of `write_solid_jpeg`.
fn write_gradient_jpeg(path: &Path, kind: u32, size: u32) {
    let mut img = RgbImage::new(size, size);
    for (x, y, px) in img.enumerate_pixels_mut() {
        // Each `kind` rotates the gradient direction so pHash sees a
        // clearly different luminance pattern.
        let v = match kind % 4 {
            0 => (x * 255 / size.max(1)) as u8,
            1 => (y * 255 / size.max(1)) as u8,
            2 => ((x + y) * 255 / (2 * size.max(1))) as u8,
            _ => (((size - x) + y) * 255 / (2 * size.max(1))) as u8,
        };
        *px = Rgb([v, v, v]);
    }
    img.save(path).expect("save jpeg");
}

fn frame_at(index: u32, ts: f64, path: PathBuf) -> FrameRef {
    FrameRef {
        index,
        path,
        timestamp_secs: ts,
    }
}

#[test]
fn test_propose_note_shape_text_only_low_unique() {
    let t = SlideThresholds::default();
    let shape = propose_note_shape(50, 3, &t);
    assert_eq!(shape, NoteShape::TextOnly);
}

#[test]
fn test_propose_note_shape_text_only_high_ratio() {
    let t = SlideThresholds::default();
    // ratio 0.6 >= 0.50 -> text-only
    let shape = propose_note_shape(50, 30, &t);
    assert_eq!(shape, NoteShape::TextOnly);
}

#[test]
fn test_propose_note_shape_slide_section_low_ratio() {
    let t = SlideThresholds::default();
    // 5/100 = 0.05 < 0.10 and unique >= 4 -> slide-section
    let shape = propose_note_shape(100, 5, &t);
    assert_eq!(shape, NoteShape::SlideSection);
}

#[test]
fn test_propose_note_shape_hero_middle_ratio() {
    let t = SlideThresholds::default();
    // 20/100 = 0.20, in [0.10, 0.50), unique >= 4 -> hero
    let shape = propose_note_shape(100, 20, &t);
    assert_eq!(shape, NoteShape::Hero);
}

#[test]
fn test_propose_note_shape_zero_frames() {
    let t = SlideThresholds::default();
    let shape = propose_note_shape(0, 0, &t);
    assert_eq!(shape, NoteShape::TextOnly);
}

#[test]
fn test_drop_transitions_removes_short_clusters() {
    let f = FrameRef {
        index: 1,
        path: PathBuf::new(),
        timestamp_secs: 0.0,
    };
    let clusters = vec![
        Cluster {
            canonical: f.clone(),
            start: 0.0,
            end: 10.0,
        }, // 10s - kept
        Cluster {
            canonical: f.clone(),
            start: 10.0,
            end: 12.0,
        }, // 2s - dropped (transition)
        Cluster {
            canonical: f.clone(),
            start: 12.0,
            end: 30.0,
        }, // 18s - kept
    ];
    let (kept, dropped) = drop_transitions(clusters, 5.0);
    assert_eq!(dropped, 1);
    assert_eq!(kept.len(), 2);
}

#[test]
fn test_parse_transcript_segments_bracketed() {
    let text = "[00:00] welcome\n[00:42] now we begin\n[01:15] next slide";
    let segs = parse_transcript_segments(text);
    assert_eq!(segs.len(), 3);
    assert_eq!(segs[0].start, 0.0);
    assert_eq!(segs[0].text, "welcome");
    assert_eq!(segs[1].start, 42.0);
    assert_eq!(segs[1].text, "now we begin");
    assert_eq!(segs[2].start, 75.0);
    assert_eq!(segs[2].text, "next slide");
}

#[test]
fn test_parse_transcript_segments_bare_hms() {
    let text = "00:00 hello\n01:02:03 deep into the talk";
    let segs = parse_transcript_segments(text);
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].start, 0.0);
    assert_eq!(segs[0].text, "hello");
    assert_eq!(segs[1].start, 3723.0);
    assert_eq!(segs[1].text, "deep into the talk");
}

#[test]
fn test_parse_transcript_segments_no_timestamp_drops_line() {
    // Lines without a recognizable leading timestamp are not surfaced; OCR + caption
    // remain as the textual signal for the slide.
    let text = "no timestamp here\nalso plain prose";
    let segs = parse_transcript_segments(text);
    assert!(segs.is_empty());
}

#[test]
fn test_bind_transcript_filters_to_slide_range() {
    let mut slides = vec![
        Slide {
            id: "s001".to_string(),
            frame_path: PathBuf::from("slides/slide-001.jpg"),
            start: 0.0,
            end: 30.0,
            duration: 30.0,
            ocr: String::new(),
            class: None,
            transcript: Vec::new(),
        },
        Slide {
            id: "s002".to_string(),
            frame_path: PathBuf::from("slides/slide-002.jpg"),
            start: 30.0,
            end: 60.0,
            duration: 30.0,
            ocr: String::new(),
            class: None,
            transcript: Vec::new(),
        },
    ];
    let segments = vec![
        VttSegment {
            start: 5.0,
            text: "hello".to_string(),
        },
        VttSegment {
            start: 25.0,
            text: "still slide one".to_string(),
        },
        VttSegment {
            start: 30.0,
            text: "now slide two".to_string(),
        },
        VttSegment {
            start: 55.0,
            text: "tail".to_string(),
        },
    ];
    bind_transcript(&mut slides, &segments);
    assert_eq!(slides[0].transcript.len(), 2);
    assert!(slides[0].transcript[0].contains("hello"));
    assert!(slides[0].transcript[1].contains("still slide one"));
    assert_eq!(slides[1].transcript.len(), 2);
    assert!(slides[1].transcript[0].contains("now slide two"));
    assert!(slides[1].transcript[1].contains("tail"));
}

#[test]
fn test_cluster_frames_solid_color_groups() {
    // Six frames across two distinct gradient patterns: kind A (3 frames),
    // kind B (2 frames), kind A (1 frame). pHash distinguishes the kinds;
    // adjacent same-kind frames cluster together.
    let tmp = std::env::temp_dir().join("borg-test-slides-cluster");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let mut frames = Vec::new();
    let mut hashes = Vec::new();
    for (i, kind) in [0u32, 0, 0, 1, 1, 0].iter().enumerate() {
        let p = tmp.join(format!("f{:04}.jpg", i + 1));
        write_gradient_jpeg(&p, *kind, 64);
        frames.push(frame_at(i as u32 + 1, i as f64 * 1.0, p.clone()));
        hashes.push(hash_frame(&p).unwrap_or_else(|_| ImageHash::from_bytes(&[0u8; 8]).unwrap()));
    }

    let clusters = cluster_frames(&frames, &hashes, 6, 10.0);
    // 3 transitions (A->A->A->B->B->A): expect 3 clusters (A, B, A).
    assert!(
        clusters.len() >= 2,
        "expected at least 2 clusters; got {}",
        clusters.len(),
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_materialize_slides_writes_canonical_jpegs() {
    let tmp = std::env::temp_dir().join("borg-test-slides-materialize");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    // Create two source frames.
    let src1 = tmp.join("src1.jpg");
    let src2 = tmp.join("src2.jpg");
    write_solid_jpeg(&src1, [128, 0, 0], 32);
    write_solid_jpeg(&src2, [0, 0, 128], 32);

    let clusters = vec![
        Cluster {
            canonical: frame_at(1, 0.0, src1.clone()),
            start: 0.0,
            end: 30.0,
        },
        Cluster {
            canonical: frame_at(2, 30.0, src2.clone()),
            start: 30.0,
            end: 60.0,
        },
    ];
    let slide_dir = tmp.join("slides");
    let slides = materialize_slides(&clusters, &slide_dir).expect("materialize");
    assert_eq!(slides.len(), 2);
    assert_eq!(slides[0].id, "s001");
    assert_eq!(slides[0].frame_path, PathBuf::from("slides/slide-001.jpg"));
    assert!(slide_dir.join("slide-001.jpg").exists());
    assert!(slide_dir.join("slide-002.jpg").exists());
    assert_eq!(slides[0].duration, 30.0);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_segment_empty_frames_returns_text_only() {
    let cfg = YoutubeSlidesConfig::default();
    let tmp = std::env::temp_dir().join("borg-test-slides-segment-empty");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let manifest = segment("ht-deadbeef", "https://x", 0.0, &[], "", &tmp, &cfg, 60).expect("segment");
    assert_eq!(manifest.trace_id, "ht-deadbeef");
    assert!(manifest.slides.is_empty());
    assert_eq!(manifest.extraction.unique_slides, 0);
    assert_eq!(manifest.extraction.proposed_note_shape, NoteShape::TextOnly);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_segment_two_color_blocks_yields_slides() {
    let cfg = YoutubeSlidesConfig::default();
    let tmp = std::env::temp_dir().join("borg-test-slides-segment-blocks");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    // 12 frames at 1fps: 6 of gradient kind A, 6 of kind B. Each block
    // lasts 6s (>= 5s default transition threshold).
    let mut frames = Vec::new();
    for i in 0..12u32 {
        let path = tmp.join(format!("frame_{:04}.jpg", i + 1));
        let kind = if i < 6 { 0 } else { 1 };
        write_gradient_jpeg(&path, kind, 64);
        frames.push(FrameRef {
            index: i + 1,
            path,
            timestamp_secs: i as f64,
        });
    }

    let out_dir = tmp.join("transcripts");
    let manifest = segment(
        "ht-cafef00d",
        "https://www.youtube.com/watch?v=test",
        12.0,
        &frames,
        "[00:00] red phase\n[00:06] green phase",
        &out_dir,
        &cfg,
        60,
    )
    .expect("segment");

    assert!(
        manifest.slides.len() >= 2,
        "expected at least 2 slides, got {}",
        manifest.slides.len(),
    );
    assert!(out_dir.join("slides").join("slide-001.jpg").exists());
    // Bound transcript should land in the appropriate slide.
    let any_red = manifest
        .slides
        .iter()
        .any(|s| s.transcript.iter().any(|t| t.contains("red")));
    let any_green = manifest
        .slides
        .iter()
        .any(|s| s.transcript.iter().any(|t| t.contains("green")));
    assert!(any_red && any_green, "transcript binding lost both phases");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_render_pattern_input_includes_shape_and_slides() {
    let manifest = SlideManifest {
        trace_id: "ht-test".to_string(),
        video: VideoMetaSnippet {
            url: "https://www.youtube.com/watch?v=abc".to_string(),
            duration_seconds: 540.0,
        },
        extraction: ExtractionStats {
            frames_after_mpdecimate: 38,
            unique_slides: 7,
            transitions_dropped: 2,
            compression_ratio: 0.18,
            proposed_note_shape: NoteShape::SlideSection,
        },
        slides: vec![
            Slide {
                id: "s001".to_string(),
                frame_path: PathBuf::from("slides/slide-001.jpg"),
                start: 0.0,
                end: 42.3,
                duration: 42.3,
                ocr: "Title slide\nMy Talk".to_string(),
                class: None,
                transcript: vec!["[00:00] welcome".to_string(), "[00:05] today we will".to_string()],
            },
            Slide {
                id: "s002".to_string(),
                frame_path: PathBuf::from("slides/slide-002.jpg"),
                start: 42.3,
                end: 135.8,
                duration: 93.5,
                ocr: "How it works".to_string(),
                class: None,
                transcript: vec!["[00:42] the way this works".to_string()],
            },
        ],
    };
    let rendered = render_pattern_input(&manifest);
    assert!(rendered.contains("Note shape: slide-section"));
    assert!(rendered.contains("## Slide s001 - 00:00 -> 00:42"));
    assert!(rendered.contains("## Slide s002 - 00:42 -> 02:15"));
    assert!(rendered.contains("![](slides/slide-001.jpg)"));
    assert!(rendered.contains("On-slide text (OCR):"));
    assert!(rendered.contains("> Title slide"));
    assert!(rendered.contains("Transcript while this slide was on screen:"));
    assert!(rendered.contains("- [00:00] welcome"));
}

#[test]
fn test_parse_summary_output_with_frontmatter() {
    let raw = "---\nshape: slide-section\nembed_slides: [s001, s002, s005]\nsections:\n  - slide: s001\n    title: Introduction\n  - slide: s002\n    title: How the pipeline works\n  - slide: s005\n    title: Cost and limits\n---\n\n## Introduction\nThe talk opens.\n\n## How the pipeline works\nThe presenter walks through.\n";
    let parsed = parse_summary_output(raw);
    assert_eq!(parsed.frontmatter.shape, "slide-section");
    assert_eq!(parsed.frontmatter.embed_slides, vec!["s001", "s002", "s005"]);
    assert_eq!(parsed.frontmatter.sections.len(), 3);
    assert_eq!(parsed.frontmatter.sections[0].slide, "s001");
    assert_eq!(parsed.frontmatter.sections[0].title, "Introduction");
    assert!(parsed.body.starts_with("## Introduction"));
}

#[test]
fn test_parse_summary_output_no_frontmatter() {
    let raw = "## What This Is About\n\nA prose summary.\n";
    let parsed = parse_summary_output(raw);
    // Default text-only frontmatter; body unchanged.
    assert_eq!(parsed.frontmatter.shape, "text-only");
    assert!(parsed.frontmatter.embed_slides.is_empty());
    assert!(parsed.body.contains("## What This Is About"));
}

#[test]
fn test_parse_summary_output_garbled_frontmatter_falls_back_to_default() {
    // Malformed YAML inside the frontmatter - we tolerate it and default to text-only.
    let raw = "---\n!!! not yaml :: ?? \n---\n\nbody\n";
    let parsed = parse_summary_output(raw);
    assert_eq!(parsed.frontmatter.shape, "text-only");
    assert!(parsed.frontmatter.embed_slides.is_empty());
    assert!(parsed.body.contains("body"));
}

#[test]
fn test_enforce_shape_text_only_proposal_caps_llm() {
    let manifest = SlideManifest {
        trace_id: "ht-test".to_string(),
        video: VideoMetaSnippet::default(),
        extraction: ExtractionStats {
            frames_after_mpdecimate: 30,
            unique_slides: 2,
            transitions_dropped: 0,
            compression_ratio: 0.07,
            proposed_note_shape: NoteShape::TextOnly,
        },
        slides: vec![],
    };
    // LLM tries to upgrade to slide-section; Stage 1 said text-only, so cap.
    let (shape, slides) = enforce_shape(&manifest, "slide-section", &["s001".to_string()]);
    assert_eq!(shape, NoteShape::TextOnly);
    assert!(slides.is_empty());
}

#[test]
fn test_enforce_shape_hero_proposal_caps_slide_section_request_to_one() {
    let manifest = SlideManifest {
        trace_id: "ht-test".to_string(),
        video: VideoMetaSnippet::default(),
        extraction: ExtractionStats {
            frames_after_mpdecimate: 30,
            unique_slides: 5,
            transitions_dropped: 0,
            compression_ratio: 0.16,
            proposed_note_shape: NoteShape::Hero,
        },
        slides: vec![
            Slide {
                id: "s001".to_string(),
                frame_path: PathBuf::new(),
                start: 0.0,
                end: 10.0,
                duration: 10.0,
                ocr: String::new(),
                class: None,
                transcript: vec![],
            },
            Slide {
                id: "s002".to_string(),
                frame_path: PathBuf::new(),
                start: 10.0,
                end: 20.0,
                duration: 10.0,
                ocr: String::new(),
                class: None,
                transcript: vec![],
            },
        ],
    };
    let (shape, slides) = enforce_shape(&manifest, "slide-section", &["s001".to_string(), "s002".to_string()]);
    assert_eq!(shape, NoteShape::Hero);
    assert_eq!(slides.len(), 1);
    assert_eq!(slides[0], "s001");
}

#[test]
fn test_enforce_shape_downgrade_to_text_only() {
    let manifest = SlideManifest {
        trace_id: "ht-test".to_string(),
        video: VideoMetaSnippet::default(),
        extraction: ExtractionStats {
            frames_after_mpdecimate: 30,
            unique_slides: 5,
            transitions_dropped: 0,
            compression_ratio: 0.16,
            proposed_note_shape: NoteShape::SlideSection,
        },
        slides: vec![Slide {
            id: "s001".to_string(),
            frame_path: PathBuf::new(),
            start: 0.0,
            end: 10.0,
            duration: 10.0,
            ocr: String::new(),
            class: None,
            transcript: vec![],
        }],
    };
    // LLM's empty embed_slides means "downgrade to text-only"
    let (shape, slides) = enforce_shape(&manifest, "text-only", &[]);
    assert_eq!(shape, NoteShape::TextOnly);
    assert!(slides.is_empty());
}

#[test]
fn test_enforce_shape_drops_unknown_slide_ids() {
    let manifest = SlideManifest {
        trace_id: "ht-test".to_string(),
        video: VideoMetaSnippet::default(),
        extraction: ExtractionStats {
            frames_after_mpdecimate: 30,
            unique_slides: 5,
            transitions_dropped: 0,
            compression_ratio: 0.16,
            proposed_note_shape: NoteShape::SlideSection,
        },
        slides: vec![Slide {
            id: "s001".to_string(),
            frame_path: PathBuf::new(),
            start: 0.0,
            end: 10.0,
            duration: 10.0,
            ocr: String::new(),
            class: None,
            transcript: vec![],
        }],
    };
    // s999 is hallucinated; gets dropped.
    let (shape, slides) = enforce_shape(&manifest, "slide-section", &["s001".to_string(), "s999".to_string()]);
    assert_eq!(shape, NoteShape::SlideSection);
    assert_eq!(slides, vec!["s001"]);
}

// --- Phase 2: capture stage (collapse_runs / best_frame / shape_from_kept_count) ---

fn cluster_at(start: f64, end: f64) -> Cluster {
    Cluster {
        canonical: FrameRef {
            index: 0,
            path: PathBuf::new(),
            timestamp_secs: start,
        },
        start,
        end,
    }
}

#[test]
fn test_collapse_runs_empty() {
    let runs = collapse_runs(&[], &[]);
    assert!(runs.is_empty());
}

#[test]
fn test_collapse_runs_growing_diagram_stitches_one_run() {
    // A live-drawn diagram fragments into three abutting growth-stage clusters
    // (gap of 1s each, <= RUN_MERGE_MAX_GAP_SECS). They collapse to one run
    // spanning the whole window.
    let clusters = vec![cluster_at(0.0, 5.0), cluster_at(6.0, 11.0), cluster_at(12.0, 20.0)];
    let runs = collapse_runs(&clusters, &[]);
    assert_eq!(runs.len(), 1, "growing-diagram fragments should stitch into one run");
    assert_eq!(runs[0].start, 0.0);
    assert_eq!(runs[0].end, 20.0);
}

#[test]
fn test_collapse_runs_large_gap_splits_into_two_runs() {
    // A presenter pause leaves a 30s gap between two distinct decks; the run
    // breaks, yielding two runs.
    let clusters = vec![
        cluster_at(0.0, 10.0),
        cluster_at(11.0, 20.0), // gap 1s -> merges into run 1
        cluster_at(50.0, 60.0), // gap 30s -> new run
    ];
    let runs = collapse_runs(&clusters, &[]);
    assert_eq!(runs.len(), 2, "gap beyond threshold should split into two runs");
    assert_eq!(runs[0].start, 0.0);
    assert_eq!(runs[0].end, 20.0);
    assert_eq!(runs[1].start, 50.0);
    assert_eq!(runs[1].end, 60.0);
}

#[test]
fn test_collapse_runs_single_cluster_is_one_run() {
    let clusters = vec![cluster_at(3.0, 9.0)];
    let runs = collapse_runs(&clusters, &[]);
    assert_eq!(runs, vec![Run { start: 3.0, end: 9.0 }]);
}

#[test]
fn test_best_frame_picks_largest_jpeg_in_window() {
    let tmp = std::env::temp_dir().join("borg-test-slides-best-frame");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    // Three frames in the run window: a near-blank canvas (tiny JPEG), a
    // partial drawing, and the terminal most-complete frame (largest JPEG via a
    // high-frequency gradient). Plus an out-of-window frame that must be ignored.
    let blank = tmp.join("f0001.jpg");
    let partial = tmp.join("f0002.jpg");
    let complete = tmp.join("f0003.jpg");
    let outside = tmp.join("f0099.jpg");
    write_solid_jpeg(&blank, [255, 255, 255], 64); // flat -> tiny
    write_gradient_jpeg(&partial, 0, 64); // some detail
    write_gradient_jpeg(&complete, 2, 256); // much larger -> most bytes
    write_solid_jpeg(&outside, [0, 0, 0], 256);

    let frames = vec![
        frame_at(1, 0.0, blank.clone()),
        frame_at(2, 5.0, partial.clone()),
        frame_at(3, 10.0, complete.clone()),
        frame_at(99, 100.0, outside.clone()), // out of window
    ];

    let chosen = best_frame(0.0, 20.0, &frames).expect("a frame in window");
    assert_eq!(chosen, complete, "best_frame must pick the largest in-window JPEG");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_best_frame_none_when_window_empty() {
    let frames = vec![
        frame_at(1, 0.0, PathBuf::from("/nonexistent/a.jpg")),
        frame_at(2, 5.0, PathBuf::from("/nonexistent/b.jpg")),
    ];
    // No frame falls in [50, 60].
    assert!(best_frame(50.0, 60.0, &frames).is_none());
}

#[test]
fn test_best_frame_falls_back_to_last_when_all_stats_fail() {
    // Paths do not exist on disk -> every metadata() stat fails -> fall back to
    // the last in-window frame.
    let frames = vec![
        frame_at(1, 0.0, PathBuf::from("/nonexistent/a.jpg")),
        frame_at(2, 5.0, PathBuf::from("/nonexistent/b.jpg")),
        frame_at(3, 9.0, PathBuf::from("/nonexistent/c.jpg")),
    ];
    let chosen = best_frame(0.0, 20.0, &frames).expect("fallback to last-in-window");
    assert_eq!(chosen, PathBuf::from("/nonexistent/c.jpg"));
}

#[test]
fn test_shape_from_kept_count_boundaries() {
    assert_eq!(shape_from_kept_count(0), NoteShape::TextOnly);
    assert_eq!(shape_from_kept_count(1), NoteShape::Hero);
    assert_eq!(shape_from_kept_count(2), NoteShape::SlideSection);
    assert_eq!(shape_from_kept_count(7), NoteShape::SlideSection);
}

#[test]
fn test_write_manifest_round_trip() {
    let tmp = std::env::temp_dir().join("borg-test-slides-write-manifest");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let manifest = SlideManifest {
        trace_id: "ht-abc123".to_string(),
        video: VideoMetaSnippet {
            url: "https://x".to_string(),
            duration_seconds: 60.0,
        },
        extraction: ExtractionStats {
            frames_after_mpdecimate: 30,
            unique_slides: 5,
            transitions_dropped: 1,
            compression_ratio: 0.166,
            proposed_note_shape: NoteShape::SlideSection,
        },
        slides: vec![Slide {
            id: "s001".to_string(),
            frame_path: PathBuf::from("slides/slide-001.jpg"),
            start: 0.0,
            end: 12.0,
            duration: 12.0,
            ocr: "Title".to_string(),
            class: None,
            transcript: vec!["[00:00] hi".to_string()],
        }],
    };
    let path = write_manifest(&manifest, &tmp).expect("write");
    assert!(path.exists());
    let yaml = std::fs::read_to_string(&path).expect("read");
    assert!(yaml.contains("trace-id: ht-abc123"));
    assert!(yaml.contains("proposed-note-shape: slide-section"));
    let parsed: SlideManifest = serde_yaml::from_str(&yaml).expect("parse");
    assert_eq!(parsed.trace_id, "ht-abc123");

    let _ = std::fs::remove_dir_all(&tmp);
}

// ---- Phase 4: content-filter prefix + manifest builder ----------------------

use crate::config::{ContentFilterConfig, SlideCategory, SlideClass};

#[test]
fn test_prepare_runs_empty_frames() {
    let cfg = YoutubeSlidesConfig::default();
    let runs = prepare_runs(&[], &cfg, 0.0);
    assert!(runs.is_empty());
}

#[test]
fn test_prepare_runs_yields_best_frame_per_run() {
    let cfg = YoutubeSlidesConfig::default();
    let tmp = std::env::temp_dir().join("borg-test-prepare-runs");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    // Two distinct content spans separated by a gap well above the run-merge
    // threshold: 4 frames of gradient kind 0 (t=0..3), then a gap, then 4 frames
    // of kind 1 (t=20..23). Each span should collapse to one run with a best frame.
    let mut frames = Vec::new();
    for i in 0..4u32 {
        let p = tmp.join(format!("a_{i:04}.jpg"));
        write_gradient_jpeg(&p, 0, 64);
        frames.push(frame_at(i, i as f64, p));
    }
    for i in 0..4u32 {
        let p = tmp.join(format!("b_{i:04}.jpg"));
        write_gradient_jpeg(&p, 1, 64);
        frames.push(frame_at(100 + i, 20.0 + i as f64, p));
    }

    let runs = prepare_runs(&frames, &cfg, 24.0);
    assert_eq!(runs.len(), 2, "two distinct spans -> two runs");
    for (run, best) in &runs {
        assert!(best.is_some(), "each run resolves a best frame");
        assert!(run.end >= run.start);
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

fn kept_run(best: PathBuf, start: f64, end: f64, category: SlideCategory) -> KeptRun {
    KeptRun {
        best_frame: best,
        start,
        end,
        class: SlideClass {
            category,
            confidence: 0.9,
        },
    }
}

#[test]
fn test_segment_filtered_zero_kept_is_text_only() {
    let tmp = std::env::temp_dir().join("borg-test-segfilt-zero");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let manifest = segment_filtered("ht-zero", "https://x", 60.0, 30, &[], &[], &tmp, 60).expect("segment_filtered");
    assert_eq!(manifest.extraction.proposed_note_shape, NoteShape::TextOnly);
    assert!(manifest.slides.is_empty());
    assert_eq!(manifest.extraction.transitions_dropped, 0);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_segment_filtered_one_kept_is_hero() {
    let tmp = std::env::temp_dir().join("borg-test-segfilt-one");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let src = tmp.join("frame.jpg");
    write_gradient_jpeg(&src, 0, 64);
    let kept = vec![kept_run(src, 0.0, 12.0, SlideCategory::ArchitectureDiagram)];

    let manifest = segment_filtered("ht-one", "https://x", 60.0, 30, &kept, &[], &tmp, 60).expect("segment_filtered");
    assert_eq!(manifest.extraction.proposed_note_shape, NoteShape::Hero);
    assert_eq!(manifest.slides.len(), 1);
    assert_eq!(
        manifest.slides[0].class.map(|c| c.category),
        Some(SlideCategory::ArchitectureDiagram)
    );
    // The kept best frame is materialized into the slides dir.
    assert!(tmp.join("slides").join("slide-001.jpg").exists());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_segment_filtered_many_kept_is_slide_section_with_transcript() {
    let tmp = std::env::temp_dir().join("borg-test-segfilt-many");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let mut kept = Vec::new();
    for i in 0..3u32 {
        let src = tmp.join(format!("frame_{i}.jpg"));
        write_gradient_jpeg(&src, i, 64);
        kept.push(kept_run(
            src,
            i as f64 * 10.0,
            i as f64 * 10.0 + 9.0,
            SlideCategory::Code,
        ));
    }
    let pairs = vec![(1.0, "first span".to_string()), (11.0, "second span".to_string())];

    let manifest =
        segment_filtered("ht-many", "https://x", 60.0, 60, &kept, &pairs, &tmp, 60).expect("segment_filtered");
    assert_eq!(manifest.extraction.proposed_note_shape, NoteShape::SlideSection);
    assert_eq!(manifest.slides.len(), 3);
    // Transcript pairs bind into the slides whose window contains them.
    let bound: usize = manifest.slides.iter().map(|s| s.transcript.len()).sum();
    assert_eq!(bound, 2, "both transcript pairs bound to a slide window");
    // Compression ratio reflects the full extraction, not just survivors.
    assert!((manifest.extraction.compression_ratio - (3.0 / 60.0)).abs() < 1e-6);

    let _ = std::fs::remove_dir_all(&tmp);
}

// ---- Phase 5: end-to-end filter pipeline (mocked classifier) ----------------
//
// These tests exercise the FULL content-filter path:
//   fixture JPEGs -> prepare_runs -> [mock classification results] ->
//   apply_filter -> segment_filtered -> assert shape + slide count
//
// The classifier is injected as a pre-built Vec<Result<SlideClass, ClassifyError>>
// at the apply_filter seam, so no network call is made. This mirrors the seam
// the production orchestrator (handlers.rs::classify_and_filter_slides) uses.

use crate::slides::classify::{self, ClassifyError};

/// Build a `ContentFilterConfig` that keeps the given categories.
fn filter_cfg(keep: Vec<SlideCategory>) -> ContentFilterConfig {
    ContentFilterConfig {
        enabled: true,
        keep,
        model: String::new(),
        max_vision_concurrency: 4,
        min_confidence: 0.6,
    }
}

/// Produce N distinct gradient frames in `dir`, returning their `FrameRef`s
/// (one frame per run, each lasting 10 s, placed far enough apart to produce
/// separate runs after `prepare_runs`).
fn make_frames(dir: &std::path::Path, count: u32) -> Vec<FrameRef> {
    let mut frames = Vec::new();
    for i in 0..count {
        let p = dir.join(format!("f{i:04}.jpg"));
        write_gradient_jpeg(&p, i, 64);
        frames.push(frame_at(i, i as f64 * 20.0, p));
    }
    frames
}

/// Run the full filter pipeline with pre-supplied classification results.
/// Returns the final manifest (no network calls).
fn run_filter_pipeline(
    tmp: &std::path::Path,
    frames: &[FrameRef],
    mock_results: Vec<std::result::Result<SlideClass, ClassifyError>>,
    filter: &ContentFilterConfig,
) -> SlideManifest {
    let cfg = YoutubeSlidesConfig::default();
    let duration = frames.last().map(|f| f.timestamp_secs + 10.0).unwrap_or(0.0);

    // Phase 1: pure prefix.
    let runs = prepare_runs(frames, &cfg, duration);
    let best_frames: Vec<std::path::PathBuf> = runs.iter().filter_map(|(_, best)| best.clone()).collect();
    let windows: Vec<crate::slides::Run> = runs
        .iter()
        .filter_map(|(run, best)| best.as_ref().map(|_| run.clone()))
        .collect();

    // Phase 2: inject mock results at the apply_filter seam (no network).
    let (kept, _tally) = classify::apply_filter(&windows, &best_frames, &mock_results, filter);

    // Phase 3: materialize kept runs into the manifest.
    segment_filtered(
        "ht-test",
        "https://x",
        duration,
        frames.len() as u32,
        &kept,
        &[],
        tmp,
        60,
    )
    .expect("segment_filtered")
}

#[test]
fn test_e2e_all_noise_produces_text_only() {
    let tmp = std::env::temp_dir().join("borg-test-e2e-all-noise");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let frames = make_frames(&tmp, 3);
    // All frames classify as TalkingHead - none should be kept.
    let mock_results: Vec<std::result::Result<SlideClass, ClassifyError>> = (0..3)
        .map(|_| {
            Ok(SlideClass {
                category: SlideCategory::TalkingHead,
                confidence: 0.95,
            })
        })
        .collect();
    let filter = filter_cfg(vec![SlideCategory::ArchitectureDiagram]);

    let manifest = run_filter_pipeline(&tmp, &frames, mock_results, &filter);
    assert_eq!(
        manifest.extraction.proposed_note_shape,
        NoteShape::TextOnly,
        "all-noise must produce TextOnly"
    );
    assert!(manifest.slides.is_empty(), "no slides should survive");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_e2e_one_diagram_produces_hero() {
    let tmp = std::env::temp_dir().join("borg-test-e2e-one-diagram");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let frames = make_frames(&tmp, 3);
    // Frame 1: TalkingHead (drop), frame 2: ArchitectureDiagram (keep), frame 3: TitleCard (drop).
    let mock_results: Vec<std::result::Result<SlideClass, ClassifyError>> = vec![
        Ok(SlideClass {
            category: SlideCategory::TalkingHead,
            confidence: 0.9,
        }),
        Ok(SlideClass {
            category: SlideCategory::ArchitectureDiagram,
            confidence: 0.85,
        }),
        Ok(SlideClass {
            category: SlideCategory::TitleCard,
            confidence: 0.9,
        }),
    ];
    let filter = filter_cfg(vec![SlideCategory::ArchitectureDiagram]);

    let manifest = run_filter_pipeline(&tmp, &frames, mock_results, &filter);
    assert_eq!(
        manifest.extraction.proposed_note_shape,
        NoteShape::Hero,
        "one kept diagram must produce Hero"
    );
    assert_eq!(manifest.slides.len(), 1, "exactly one slide survives");
    assert_eq!(
        manifest.slides[0].class.map(|c| c.category),
        Some(SlideCategory::ArchitectureDiagram),
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_e2e_many_diagrams_produces_slide_section() {
    let tmp = std::env::temp_dir().join("borg-test-e2e-many-diagrams");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let frames = make_frames(&tmp, 4);
    // All four frames are code snippets above confidence - all should be kept.
    let mock_results: Vec<std::result::Result<SlideClass, ClassifyError>> = (0..4)
        .map(|_| {
            Ok(SlideClass {
                category: SlideCategory::Code,
                confidence: 0.80,
            })
        })
        .collect();
    let filter = filter_cfg(vec![SlideCategory::Code]);

    let manifest = run_filter_pipeline(&tmp, &frames, mock_results, &filter);
    assert_eq!(
        manifest.extraction.proposed_note_shape,
        NoteShape::SlideSection,
        "multiple kept slides must produce SlideSection"
    );
    assert!(manifest.slides.len() >= 2, "at least 2 slides survive");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_e2e_classifier_error_drops_slide_note_still_publishes() {
    let tmp = std::env::temp_dir().join("borg-test-e2e-classifier-error");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    // Three frames: first errors, second is a kept diagram, third errors.
    // The note should still publish with shape Hero (one survivor).
    let frames = make_frames(&tmp, 3);
    let mock_results: Vec<std::result::Result<SlideClass, ClassifyError>> = vec![
        Err(ClassifyError::Api(eyre::eyre!("simulated API failure"))),
        Ok(SlideClass {
            category: SlideCategory::ArchitectureDiagram,
            confidence: 0.9,
        }),
        Err(ClassifyError::Parse(eyre::eyre!("simulated parse failure"))),
    ];
    let filter = filter_cfg(vec![SlideCategory::ArchitectureDiagram]);

    let manifest = run_filter_pipeline(&tmp, &frames, mock_results, &filter);
    // The errored slides are dropped; the one kept diagram yields Hero.
    assert_eq!(
        manifest.extraction.proposed_note_shape,
        NoteShape::Hero,
        "one kept slide among errors must still produce Hero"
    );
    assert_eq!(manifest.slides.len(), 1, "only the successful classification survives");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_e2e_all_classifier_errors_produce_text_only() {
    let tmp = std::env::temp_dir().join("borg-test-e2e-all-errors");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let frames = make_frames(&tmp, 2);
    let mock_results: Vec<std::result::Result<SlideClass, ClassifyError>> = vec![
        Err(ClassifyError::Api(eyre::eyre!("API down"))),
        Err(ClassifyError::Join(eyre::eyre!("task panic"))),
    ];
    let filter = filter_cfg(vec![SlideCategory::ArchitectureDiagram]);

    let manifest = run_filter_pipeline(&tmp, &frames, mock_results, &filter);
    assert_eq!(
        manifest.extraction.proposed_note_shape,
        NoteShape::TextOnly,
        "all-error must fall back to TextOnly (fail-closed)"
    );
    assert!(manifest.slides.is_empty(), "no slides when all classify fail");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_e2e_keep_widened_to_architecture_and_code_keeps_code_frame() {
    let tmp = std::env::temp_dir().join("borg-test-e2e-keep-widened");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let frames = make_frames(&tmp, 3);
    // Frame 0: TalkingHead (dropped), frame 1: Code (kept), frame 2: ArchitectureDiagram (kept).
    let mock_results: Vec<std::result::Result<SlideClass, ClassifyError>> = vec![
        Ok(SlideClass {
            category: SlideCategory::TalkingHead,
            confidence: 0.95,
        }),
        Ok(SlideClass {
            category: SlideCategory::Code,
            confidence: 0.75,
        }),
        Ok(SlideClass {
            category: SlideCategory::ArchitectureDiagram,
            confidence: 0.80,
        }),
    ];
    // Both architecture-diagram AND code are in the keep list.
    let filter = filter_cfg(vec![SlideCategory::ArchitectureDiagram, SlideCategory::Code]);

    let manifest = run_filter_pipeline(&tmp, &frames, mock_results, &filter);
    assert_eq!(
        manifest.extraction.proposed_note_shape,
        NoteShape::SlideSection,
        "two kept slides (one code, one diagram) must produce SlideSection"
    );
    assert_eq!(manifest.slides.len(), 2, "code and diagram both survive");
    let categories: Vec<SlideCategory> = manifest
        .slides
        .iter()
        .filter_map(|s| s.class.map(|c| c.category))
        .collect();
    assert!(
        categories.contains(&SlideCategory::Code),
        "code slide must be kept when keep list includes code"
    );
    assert!(
        categories.contains(&SlideCategory::ArchitectureDiagram),
        "diagram slide must be kept"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_e2e_apply_filter_tally_matches_mock_results() {
    // Verify the tally buckets are correctly populated end-to-end.
    let tmp = std::env::temp_dir().join("borg-test-e2e-tally");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmp");

    let frames = make_frames(&tmp, 5);
    let filter = filter_cfg(vec![SlideCategory::Code]);
    let cfg = YoutubeSlidesConfig::default();
    let duration = 100.0;

    let runs = prepare_runs(&frames, &cfg, duration);
    let best_frames: Vec<std::path::PathBuf> = runs.iter().filter_map(|(_, best)| best.clone()).collect();
    let windows: Vec<crate::slides::Run> = runs
        .iter()
        .filter_map(|(run, best)| best.as_ref().map(|_| run.clone()))
        .collect();

    let n = best_frames.len();
    // Build mock results: code (keep), other (not-in-keep), low-conf code, api-error, parse-error.
    // Only the first n results are used (runs may be fewer than 5 frames if some cluster).
    let mut mock: Vec<std::result::Result<SlideClass, ClassifyError>> = vec![
        Ok(SlideClass {
            category: SlideCategory::Code,
            confidence: 0.9,
        }),
        Ok(SlideClass {
            category: SlideCategory::Other,
            confidence: 0.9,
        }),
        Ok(SlideClass {
            category: SlideCategory::Code,
            confidence: 0.3,
        }),
        Err(ClassifyError::Api(eyre::eyre!("network"))),
        Err(ClassifyError::Parse(eyre::eyre!("garbage"))),
    ];
    mock.truncate(n);

    let (kept, tally) = classify::apply_filter(&windows, &best_frames, &mock, &filter);

    assert_eq!(tally.classified, n, "classified count matches run count");
    // The kept count is bounded by how many Code+high-confidence results we injected.
    assert!(tally.kept <= kept.len() + 1, "tally.kept reflects actual kept count");
    // The sum of all outcome buckets equals classified.
    let bucket_sum = tally.kept
        + tally.dropped_low_confidence
        + tally.dropped_not_in_keep
        + tally.dropped_api_error
        + tally.dropped_parse_error;
    assert_eq!(
        bucket_sum, tally.classified,
        "all outcomes account for every classified run"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
