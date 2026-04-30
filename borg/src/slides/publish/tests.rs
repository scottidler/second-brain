#![allow(clippy::unwrap_used)]

use super::*;

use crate::slides::{
    ExtractionStats, NoteShape, Slide, SlideManifest, SummaryFrontmatter, SummaryOutput, SummarySection,
    VideoMetaSnippet,
};
use chrono::TimeZone;

/// Build a manifest with N slides whose source JPEGs live as absolute paths
/// under `staging_dir/slides/`. Each slide is named `slide-NNN.jpg`.
fn fixture_manifest(staging_dir: &Path, slide_ids: &[&str]) -> SlideManifest {
    let slide_dir = staging_dir.join("slides");
    std::fs::create_dir_all(&slide_dir).unwrap();
    let mut slides = Vec::new();
    for (i, id) in slide_ids.iter().enumerate() {
        let filename = format!("slide-{:03}.jpg", i + 1);
        let abs = slide_dir.join(&filename);
        // Tiny sentinel content so we can verify the copy.
        std::fs::write(&abs, format!("slide content {id}")).unwrap();
        slides.push(Slide {
            id: id.to_string(),
            frame_path: abs,
            start: i as f64 * 30.0,
            end: (i + 1) as f64 * 30.0,
            duration: 30.0,
            ocr: String::new(),
            caption: None,
            transcript: vec![],
        });
    }
    SlideManifest {
        trace_id: "ht-fixture".to_string(),
        video: VideoMetaSnippet {
            url: "https://x".to_string(),
            duration_seconds: (slide_ids.len() as f64) * 30.0,
        },
        extraction: ExtractionStats {
            frames_after_mpdecimate: 30,
            unique_slides: slide_ids.len() as u32,
            transitions_dropped: 0,
            compression_ratio: 0.1,
            proposed_note_shape: NoteShape::SlideSection,
        },
        slides,
    }
}

fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap()
}

#[test]
fn test_publish_text_only_no_files_copied() {
    let tmp = std::env::temp_dir().join("borg-test-publish-textonly");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let staging = tmp.join("staging");
    let vault = tmp.join("vault");
    std::fs::create_dir_all(&vault).unwrap();

    let manifest = fixture_manifest(&staging, &["s001", "s002"]);
    let summary = SummaryOutput {
        frontmatter: SummaryFrontmatter {
            shape: "text-only".to_string(),
            embed_slides: vec![],
            sections: vec![],
        },
        body: "## What This Is About\n\nA prose summary.\n".to_string(),
    };
    let result = publish_slides(
        &vault,
        "my-talk",
        &manifest,
        &summary,
        &staging.join("slides"),
        &fixed_now(),
    )
    .unwrap();
    assert_eq!(result.shape, NoteShape::TextOnly);
    assert!(result.slides.is_empty());
    assert!(result.body.contains("## What This Is About"));
    // Nothing under attachments.
    let attachments = vault.join("system").join("attachments");
    assert!(!attachments.exists() || std::fs::read_dir(&attachments).map(|d| d.count()).unwrap_or(0) == 0);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_publish_hero_copies_one_and_embeds() {
    let tmp = std::env::temp_dir().join("borg-test-publish-hero");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let staging = tmp.join("staging");
    let vault = tmp.join("vault");

    let mut manifest = fixture_manifest(&staging, &["s001", "s002", "s003"]);
    manifest.extraction.proposed_note_shape = NoteShape::Hero;

    let summary = SummaryOutput {
        frontmatter: SummaryFrontmatter {
            shape: "hero".to_string(),
            embed_slides: vec!["s001".to_string()],
            sections: vec![],
        },
        body: "> [!tldr]\n> A concise pitch.\n\n## What This Is About\n\nPara.\n".to_string(),
    };
    let result = publish_slides(
        &vault,
        "my-talk",
        &manifest,
        &summary,
        &staging.join("slides"),
        &fixed_now(),
    )
    .unwrap();
    assert_eq!(result.shape, NoteShape::Hero);
    assert_eq!(result.slides.len(), 1);
    assert!(result.slides[0].starts_with("system/attachments/images/2026-04/"));
    assert!(result.slides[0].ends_with(".jpg"));
    // Slide JPEG actually copied to vault.
    let dest = vault.join(&result.slides[0]);
    assert!(dest.exists());
    assert_eq!(std::fs::read(&dest).unwrap(), b"slide content s001");
    // Wikilink at top of body.
    assert!(result.body.starts_with("![["));
    assert!(result.body.contains("![[my-talk-slide-001.jpg]]"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_publish_slide_section_per_section_embeds() {
    let tmp = std::env::temp_dir().join("borg-test-publish-section");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let staging = tmp.join("staging");
    let vault = tmp.join("vault");

    let manifest = fixture_manifest(&staging, &["s001", "s002", "s003"]);

    let summary = SummaryOutput {
        frontmatter: SummaryFrontmatter {
            shape: "slide-section".to_string(),
            embed_slides: vec!["s001".to_string(), "s002".to_string(), "s003".to_string()],
            sections: vec![
                SummarySection {
                    slide: "s001".to_string(),
                    title: "Introduction".to_string(),
                },
                SummarySection {
                    slide: "s002".to_string(),
                    title: "How it works".to_string(),
                },
                SummarySection {
                    slide: "s003".to_string(),
                    title: "Cost and limits".to_string(),
                },
            ],
        },
        body:
            "## Introduction\n\nThe talk opens.\n\n## How it works\n\nThree steps.\n\n## Cost and limits\n\nBounded.\n"
                .to_string(),
    };
    let result = publish_slides(
        &vault,
        "my-talk",
        &manifest,
        &summary,
        &staging.join("slides"),
        &fixed_now(),
    )
    .unwrap();
    assert_eq!(result.shape, NoteShape::SlideSection);
    assert_eq!(result.slides.len(), 3);

    // All three slide files present in the vault.
    for path in &result.slides {
        assert!(vault.join(path).exists(), "expected {} in vault", path);
    }

    // Each section heading is followed by its embed.
    let body = &result.body;
    let intro_idx = body.find("## Introduction").unwrap();
    let intro_embed_idx = body.find("![[my-talk-slide-001.jpg]]").unwrap();
    assert!(intro_idx < intro_embed_idx, "intro embed should follow heading");
    assert!(body.contains("![[my-talk-slide-002.jpg]]"));
    assert!(body.contains("![[my-talk-slide-003.jpg]]"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_publish_slide_section_unmatched_section_is_appended() {
    let tmp = std::env::temp_dir().join("borg-test-publish-section-mismatch");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let staging = tmp.join("staging");
    let vault = tmp.join("vault");

    let manifest = fixture_manifest(&staging, &["s001", "s002"]);

    // The body has section "Introduction" but the LLM section title says
    // "Welcome" - mismatch. The mismatched section should be appended at end.
    let summary = SummaryOutput {
        frontmatter: SummaryFrontmatter {
            shape: "slide-section".to_string(),
            embed_slides: vec!["s001".to_string(), "s002".to_string()],
            sections: vec![
                SummarySection {
                    slide: "s001".to_string(),
                    title: "Welcome".to_string(),
                },
                SummarySection {
                    slide: "s002".to_string(),
                    title: "How it works".to_string(),
                },
            ],
        },
        body: "## Introduction\n\nDifferent name in the body.\n\n## How it works\n\nStuff.\n".to_string(),
    };
    let result = publish_slides(
        &vault,
        "my-talk",
        &manifest,
        &summary,
        &staging.join("slides"),
        &fixed_now(),
    )
    .unwrap();
    assert_eq!(result.shape, NoteShape::SlideSection);
    // Unmatched section ("Welcome" / s001) should be appended.
    assert!(result.body.contains("## Welcome"));
    assert!(result.body.contains("![[my-talk-slide-001.jpg]]"));
    // Matched section embed inserted under "## How it works".
    assert!(result.body.contains("![[my-talk-slide-002.jpg]]"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_publish_collision_picks_new_sequence() {
    let tmp = std::env::temp_dir().join("borg-test-publish-collision");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let staging = tmp.join("staging");
    let vault = tmp.join("vault");
    let attachments = vault.join("system").join("attachments").join("images").join("2026-04");
    std::fs::create_dir_all(&attachments).unwrap();
    // Pre-place a file at the slot the publish would normally use.
    std::fs::write(attachments.join("my-talk-slide-001.jpg"), b"old content").unwrap();

    let mut manifest = fixture_manifest(&staging, &["s001"]);
    manifest.extraction.proposed_note_shape = NoteShape::Hero;
    let summary = SummaryOutput {
        frontmatter: SummaryFrontmatter {
            shape: "hero".to_string(),
            embed_slides: vec!["s001".to_string()],
            sections: vec![],
        },
        body: "## summary\n".to_string(),
    };
    let result = publish_slides(
        &vault,
        "my-talk",
        &manifest,
        &summary,
        &staging.join("slides"),
        &fixed_now(),
    )
    .unwrap();
    assert_eq!(result.slides.len(), 1);
    // Collision => not -001, must be a different number.
    assert!(!result.slides[0].ends_with("my-talk-slide-001.jpg"));
    // Old content preserved.
    assert_eq!(
        std::fs::read(attachments.join("my-talk-slide-001.jpg")).unwrap(),
        b"old content"
    );
    // New slide present.
    assert!(vault.join(&result.slides[0]).exists());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_publish_unknown_slide_id_dropped() {
    let tmp = std::env::temp_dir().join("borg-test-publish-unknown");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let staging = tmp.join("staging");
    let vault = tmp.join("vault");

    let manifest = fixture_manifest(&staging, &["s001"]);

    let summary = SummaryOutput {
        frontmatter: SummaryFrontmatter {
            shape: "slide-section".to_string(),
            embed_slides: vec!["s001".to_string(), "s999".to_string()],
            sections: vec![
                SummarySection {
                    slide: "s001".to_string(),
                    title: "Real".to_string(),
                },
                SummarySection {
                    slide: "s999".to_string(),
                    title: "Hallucinated".to_string(),
                },
            ],
        },
        body: "## Real\n\nbody.\n".to_string(),
    };
    let result = publish_slides(
        &vault,
        "ghost",
        &manifest,
        &summary,
        &staging.join("slides"),
        &fixed_now(),
    )
    .unwrap();
    assert_eq!(result.slides.len(), 1);
    assert!(result.slides[0].ends_with("ghost-slide-001.jpg"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_month_subdir_pads() {
    let dt = Utc.with_ymd_and_hms(2026, 1, 5, 0, 0, 0).unwrap();
    assert_eq!(month_subdir(&dt), "images/2026-01");
    let dt = Utc.with_ymd_and_hms(2026, 12, 31, 0, 0, 0).unwrap();
    assert_eq!(month_subdir(&dt), "images/2026-12");
}
