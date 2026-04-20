//! Stage 1: offline extraction from Stage-0 artifacts.
//!
//! Extractors read `RawCapture` and produce a `Transcript`. Per the design
//! doc, they MUST NOT perform network I/O - Stage-0 owns the fetcher chain,
//! Stage-1 is pure bytes → text. Concrete extractors (Markitdown, Groq vision,
//! Whisper, etc.) arrive in Phase 4+ as we migrate summarization off the
//! legacy pipeline.rs path. For now the trait declares the contract and ships
//! a single trivial implementation so the module compiles and tests exist.

use eyre::Result;

use crate::types::{RawCapture, Transcript};

/// Pure bytes→text transform keyed by `IngestKind`. Implementations must
/// operate only on the `RawCapture` they receive - no network, no external
/// state, no side effects beyond (optionally) spawning a local subprocess
/// over stdin/stdout.
pub trait Extractor: Send + Sync {
    fn extract(&self, raw: &RawCapture) -> Result<Transcript>;
}

/// Trivial extractor: return the body bytes as a UTF-8 string. Used for
/// `Idea` captures and as a fallback for any kind where the Stage-0 body
/// already contains the final textual form.
pub struct PassthroughExtractor;

impl Extractor for PassthroughExtractor {
    fn extract(&self, raw: &RawCapture) -> Result<Transcript> {
        let text = String::from_utf8_lossy(&raw.body).to_string();
        Ok(Transcript {
            text,
            meta: crate::types::TraceMeta {
                extractor: "passthrough".to_string(),
                ..Default::default()
            },
        })
    }
}

#[cfg(test)]
mod tests;
