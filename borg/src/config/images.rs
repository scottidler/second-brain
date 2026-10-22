//! The `images:` block of `borg.yml`: which figures borg stores from X posts,
//! X Articles, and web articles, and how it bounds them
//! (`docs/design/2026-10-22-capture-all-diagrams.md`, API Design).
//!
//! Vision concurrency is deliberately absent: every vision call shares the one
//! process-wide pool sized by `youtube.slides.content-filter.max-vision-concurrency`.

use serde::{Deserialize, Serialize};

use super::SlideCategory;

const DEFAULT_MAX_CANDIDATES: usize = 40;
const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;
const DEFAULT_MAX_PER_NOTE: usize = 20;
const DEFAULT_MAX_SECTION_BYTES: usize = 1500;
const DEFAULT_MIN_SIDE_PX: u32 = 200;
const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_MAX_SIDE_PX: u32 = 2048;
const DEFAULT_FIGURE_MIN_CONFIDENCE: f32 = 0.6;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ImagesConfig {
    /// Master switch. `false` restores the pre-feature behavior: X photos are
    /// described but not stored, articles untouched. A reingest then writes
    /// `images: []`, so the old note's files are cleaned up.
    pub enabled: bool,
    /// Images downloaded per note, in source order.
    pub max_candidates: usize,
    /// One image over this many bytes is dropped before decode.
    pub max_download_bytes: u64,
    /// Images kept per note; the cap runs before describe.
    pub max_per_note: usize,
    /// Description plus extracted text kept per image in the note.
    pub max_section_bytes: usize,
    /// Web articles only: an image under this on either axis is decoration.
    pub min_side_px: u32,
    /// A stored file over this many bytes is downscaled to `max-side-px`.
    pub max_bytes: u64,
    /// Longer-side bound for a downscale, also the SVG raster bound.
    pub max_side_px: u32,
    pub content_filter: FigureFilterConfig,
}

impl Default for ImagesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_candidates: DEFAULT_MAX_CANDIDATES,
            max_download_bytes: DEFAULT_MAX_DOWNLOAD_BYTES,
            max_per_note: DEFAULT_MAX_PER_NOTE,
            max_section_bytes: DEFAULT_MAX_SECTION_BYTES,
            min_side_px: DEFAULT_MIN_SIDE_PX,
            max_bytes: DEFAULT_MAX_BYTES,
            max_side_px: DEFAULT_MAX_SIDE_PX,
            content_filter: FigureFilterConfig::default(),
        }
    }
}

/// The vision classifier gate for web-article images and X Article figures
/// (X post photos always skip it). Reuses [`SlideCategory`] and its parser, so
/// an unknown category is a load error. It is not `ContentFilterConfig`: that
/// struct defaults to `enabled: false, keep: [architecture-diagram]` and
/// carries the shared vision-pool knob.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct FigureFilterConfig {
    pub enabled: bool,
    pub keep: Vec<SlideCategory>,
    /// Empty inherits `llm.model`.
    pub model: String,
    pub min_confidence: f32,
}

impl Default for FigureFilterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            keep: vec![
                SlideCategory::ArchitectureDiagram,
                SlideCategory::SequenceDiagram,
                SlideCategory::Flowchart,
                SlideCategory::Chart,
                SlideCategory::Infographic,
            ],
            model: String::new(),
            min_confidence: DEFAULT_FIGURE_MIN_CONFIDENCE,
        }
    }
}

#[cfg(test)]
mod tests;
