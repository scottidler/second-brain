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
            caption: None,
            transcript: Vec::new(),
        },
        Slide {
            id: "s002".to_string(),
            frame_path: PathBuf::from("slides/slide-002.jpg"),
            start: 30.0,
            end: 60.0,
            duration: 30.0,
            ocr: String::new(),
            caption: None,
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
                caption: Some("Title card with bold yellow text".to_string()),
                transcript: vec!["[00:00] welcome".to_string(), "[00:05] today we will".to_string()],
            },
            Slide {
                id: "s002".to_string(),
                frame_path: PathBuf::from("slides/slide-002.jpg"),
                start: 42.3,
                end: 135.8,
                duration: 93.5,
                ocr: "How it works".to_string(),
                caption: None,
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
    assert!(rendered.contains("Visual caption:"));
    assert!(rendered.contains("> Title card with bold yellow text"));
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
                caption: None,
                transcript: vec![],
            },
            Slide {
                id: "s002".to_string(),
                frame_path: PathBuf::new(),
                start: 10.0,
                end: 20.0,
                duration: 10.0,
                ocr: String::new(),
                caption: None,
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
            caption: None,
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
            caption: None,
            transcript: vec![],
        }],
    };
    // s999 is hallucinated; gets dropped.
    let (shape, slides) = enforce_shape(&manifest, "slide-section", &["s001".to_string(), "s999".to_string()]);
    assert_eq!(shape, NoteShape::SlideSection);
    assert_eq!(slides, vec!["s001"]);
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
            caption: None,
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
