#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]

//! Per-kind Stage-2 distillers.
//!
//! Each distiller takes a Stage-1 transcript plus Stage-0 envelope and
//! emits a `vault::distilled::Distilled` payload. The dispatcher dispatches
//! by `IngestKind` over concrete fields (no `Box<dyn>`), so the trait below
//! exists mainly for per-impl testability.

pub mod article;
pub mod dispatcher;
pub mod fabric;
pub mod idea;
pub mod image;
pub mod passthrough;
pub mod render;
pub mod repo;
pub mod text;
pub mod thread;
pub mod validate;
pub mod video;
pub mod voicenote;

use async_trait::async_trait;
use eyre::Result;
use vault::distilled::Distilled;

pub use article::{ArticleConfig, ArticleDistiller};
pub use dispatcher::{Dispatch, Dispatcher, DistillKind};
pub mod parse;
pub use fabric::{FabricCaller, FabricRequest, FabricShell, FakeFabric};
pub use idea::IdeaDistiller;
pub use image::{ImageConfig, ImageDistiller};
pub use passthrough::PassthroughDistiller;
pub use render::{RenderedDistilled, render};
pub use repo::{RepoConfig, RepoDistiller, RepoMetadata};
pub use text::demote_headings;
pub use thread::{ThreadConfig, ThreadDistiller, infer_platform};
pub use validate::{enforce_bounds, fallback_distilled, max_claims};
pub use video::{VideoConfig, VideoDistiller, VideoMetadata};
pub use voicenote::{VoiceNoteConfig, VoiceNoteDistiller};

/// Inputs every distiller receives. Mirrors the Stage-1 / Stage-0 contract.
#[derive(Debug, Clone, Default)]
pub struct DistillInputs<'a> {
    /// Stage-1 transcript text (markitdown / VTT / thread render / OCR / user prose).
    pub transcript: &'a str,
    /// Stage-0 envelope-equivalent metadata. Optional `source_url` is the
    /// note's origin; idea/passthrough kinds may leave it `None`.
    pub source_url: Option<&'a str>,
    /// Best-effort title hint (Telegram caption, video title, etc.).
    pub title_hint: Option<&'a str>,
    /// Repo-specific Stage-0 metadata (stars, primary language, last commit,
    /// topics). Populated by the GitHub fetcher; `None` for cortex backfill
    /// or non-repo kinds. `RepoDistiller` reads it to construct
    /// `Distilled.kind_specific`; other distillers ignore it.
    pub repo_metadata: Option<&'a RepoMetadata>,
    /// Video-specific Stage-0 metadata (channel, duration, published_at).
    /// Populated by borg's yt-dlp metadata; `None` for cortex backfill or
    /// non-video kinds. `VideoDistiller` reads it for anchor validation
    /// (`duration_seconds`) and `Distilled.kind_specific`; other distillers
    /// ignore it.
    pub video_metadata: Option<&'a VideoMetadata>,
}

/// Per-kind extractor contract. Async because the LLM-bound distillers shell
/// out to Fabric; the passthrough impls satisfy this trait synchronously by
/// returning a ready `Distilled` without any `.await`.
#[async_trait]
pub trait DistillExtractor: Send + Sync {
    /// Stable identifier including version, e.g. "distill-idea-v1".
    fn id(&self) -> &'static str;

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled>;
}

#[cfg(test)]
mod tests;
