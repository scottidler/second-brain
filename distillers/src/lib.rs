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
pub mod passthrough;
pub mod render;
pub mod validate;

use async_trait::async_trait;
use eyre::Result;
use vault::distilled::Distilled;

pub use article::{ArticleConfig, ArticleDistiller};
pub use dispatcher::{Dispatch, Dispatcher, DistillKind};
pub use fabric::{FabricCaller, FabricRequest, FabricShell, FakeFabric};
pub use idea::IdeaDistiller;
pub use passthrough::PassthroughDistiller;
pub use render::{RenderedDistilled, render};
pub use validate::{enforce_bounds, fallback_distilled};

/// Inputs every distiller receives. Mirrors the Stage-1 / Stage-0 contract.
#[derive(Debug, Clone)]
pub struct DistillInputs<'a> {
    /// Stage-1 transcript text (markitdown / VTT / thread render / OCR / user prose).
    pub transcript: &'a str,
    /// Stage-0 envelope-equivalent metadata. Optional `source_url` is the
    /// note's origin; idea/passthrough kinds may leave it `None`.
    pub source_url: Option<&'a str>,
    /// Best-effort title hint (Telegram caption, video title, etc.).
    pub title_hint: Option<&'a str>,
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
