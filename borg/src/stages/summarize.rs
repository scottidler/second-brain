//! Gate-2: paraphrase-detection backstop for block pages.
//!
//! Gate-1 inspects raw fetched bytes for known block-page signatures. Some
//! pages slip through (e.g. JavaScript challenges that render as ordinary
//! HTML), reach the distiller, and come back as a `Distilled.summary` that
//! literally describes the block ("the provided input contains an error
//! message indicating..."). Gate-2 catches that paraphrase and rejects
//! the ingestion before it lands in the vault.
//!
//! The `Summarizer` trait that previously lived here was retired by the
//! post-Phase-6 cutover - `pipeline.rs` now calls the structured
//! `distill_for_publish_*` functions directly and there is no longer a
//! pluggable summariser abstraction.

/// Patterns (case-insensitive) that indicate the distiller produced a
/// summary *of* a block/error page rather than a summary of real content.
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

#[cfg(test)]
mod tests;
