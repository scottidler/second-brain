//! Stage 2: transcript → summary.
//!
//! Gate-2 is the paraphrase-detection backstop for block pages that slipped
//! past Gate-1. The idea is that Fabric will sometimes paraphrase a block
//! message ("anonymous access to domain blocked until...") into prose like
//! "The provided input contains an error message indicating..." - the
//! original signature is masked, but the paraphrase has its own signatures.
//!
//! This module declares the `Summarizer` trait (consumed by a future rewrite
//! of pipeline.rs's fabric paths) and the Gate-2 detection patterns. The
//! actual summarisation still flows through the legacy `fabric::summarize`
//! until Phase 8 decomposes pipeline.rs.

use eyre::Result;

use crate::config::FabricConfig;

/// Patterns (case-insensitive) that indicate Fabric produced a summary *of*
/// a block/error page rather than a summary of real content.
pub const GATE_2_PARAPHRASE_PATTERNS: &[&str] = &[
    "only an error message",
    "no actual content",
    "error message indicating",
    "content inaccessible",
    "access to the website is blocked",
    "anonymous access to domain",
];

/// Inspect a produced summary for paraphrase-of-a-block-page signatures.
/// Returns Some(reason) when a match fires.
pub fn detect_paraphrased_block(summary: &str) -> Option<String> {
    let lower = summary.to_ascii_lowercase();
    for pat in GATE_2_PARAPHRASE_PATTERNS {
        if lower.contains(pat) {
            return Some(format!("paraphrased block page (matched: {pat})"));
        }
    }
    None
}

/// Stage-2 summariser abstraction. Takes a transcript + pattern name, returns
/// a summary. Lives next to `Extractor` in the stages module but is declared
/// separately because different IngestKinds map to different Fabric patterns
/// (articles → summarize, GitHub → repo_summary, YouTube → summarize_video,
/// Thread → summarize_thread; Vocabulary skips entirely).
pub trait Summarizer: Send + Sync {
    fn summarize(&self, transcript: &str, pattern: &str) -> Result<String>;
}

/// Summariser wrapping the legacy Fabric CLI path. Runs synchronously on the
/// tokio blocking pool (Fabric is a subprocess).
pub struct FabricSummarizer {
    config: FabricConfig,
}

impl FabricSummarizer {
    pub fn new(config: FabricConfig) -> Self {
        Self { config }
    }
}

impl Summarizer for FabricSummarizer {
    fn summarize(&self, transcript: &str, pattern: &str) -> Result<String> {
        // Delegates to the shared vault::fabric helper (sync), using the
        // same binary/model/content-chars settings the rest of borg uses.
        vault::fabric::run_pattern(
            pattern,
            transcript,
            &self.config.binary,
            &self.config.model,
            self.config.max_content_chars,
        )
    }
}

#[cfg(test)]
mod tests;
