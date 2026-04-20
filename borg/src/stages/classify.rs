//! Gate-1: block-page detection on the raw Stage-0 artifact.
//!
//! The superseded 2026-03-30 doc catalogued Jina's block-page signatures.
//! Patterns are matched case-insensitively against the raw response bytes
//! produced at Stage 0 (prior to any Fabric paraphrase that would mask them).

use chrono::{DateTime, Utc};

use crate::blocklist;

/// A Gate-1 match result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockPageMatch {
    pub reason: String,
    pub retriable_after: DateTime<Utc>,
}

/// Patterns (case-insensitive) that identify a Jina / upstream block page in
/// the raw fetched body. If any of these matches, Gate-1 rejects the trace.
pub const BLOCK_PAGE_PATTERNS: &[&str] = &[
    "anonymous access to domain",
    "blocked until",
    "securitycompromiseerror",
    "suspected ddos attacks",
];

/// Inspect a raw Stage-0 artifact for block-page signatures. Returns Some with
/// a reason + extracted retry-after when a block page is detected. Status is
/// from the fetch metadata (HTTP 451 alone is enough to trigger a block even
/// if the body is empty).
pub fn detect_block_page(bytes: &[u8], status: u16, now: DateTime<Utc>) -> Option<BlockPageMatch> {
    let text = String::from_utf8_lossy(bytes);
    let lower = text.to_ascii_lowercase();
    let mut matched_pattern: Option<&str> = None;
    for pat in BLOCK_PAGE_PATTERNS {
        if lower.contains(pat) {
            matched_pattern = Some(pat);
            break;
        }
    }
    if matched_pattern.is_none() && status != 451 {
        return None;
    }
    let reason = match matched_pattern {
        Some(p) => format!("raw block page (matched: {p})"),
        None => format!("HTTP {status} response without block-page body"),
    };
    let retriable_after = blocklist::parse_retry_after(&text, now);
    Some(BlockPageMatch {
        reason,
        retriable_after,
    })
}

#[cfg(test)]
mod tests;
