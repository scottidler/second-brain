//! Shared parse helpers for the per-kind distillers: fence stripping (with the
//! truncation bug fixed), token estimation, and the common `Pattern*` YAML leaf
//! structs. Consolidated in Phase 9 from six near-identical copies.

use serde::{Deserialize, Serialize};
use vault::distilled::{Claim, ClaimKind};

/// ~4 chars per token (English-prose rule of thumb); good enough for budget
/// reporting.
pub const CHARS_PER_TOKEN: usize = 4;

/// Rough character-to-token approximation. Returns `usize`; callers writing the
/// `u32` Distilled meta fields cast at the boundary.
pub fn approx_tokens(chars: usize) -> usize {
    chars / CHARS_PER_TOKEN
}

/// Strip a leading ` ```yaml ... ``` ` (or bare ` ``` ... ``` `) fence if the
/// LLM added one despite the prompt asking it not to. We do not repair
/// otherwise-malformed YAML.
///
/// FIX (Phase 9 consolidation): only strip a CLOSING fence when an OPENING
/// fence was actually present. The previous six copies ran `rfind("```")`
/// unconditionally, so any unfenced output containing an embedded code fence
/// was silently truncated at that fence.
pub fn strip_fences(raw: &str) -> &str {
    let trimmed = raw.trim();
    match trimmed.strip_prefix("```yaml").or_else(|| trimmed.strip_prefix("```")) {
        Some(rest) => {
            let stripped = rest.trim_start_matches('\n');
            if let Some(close) = stripped.rfind("```") {
                stripped[..close].trim_end()
            } else {
                stripped
            }
        }
        None => trimmed,
    }
}

/// The YAML leaf mirroring `vault::distilled::Claim` as a distiller pattern
/// emits it. All Phase 3 fields are serde-defaulted, so a pattern that omits
/// `kind` / `who` / `quote` (every pre-Phase-4 pattern) parses unchanged and
/// the forward-compat `ClaimKind` shim absorbs any drifting `kind:` value.
#[derive(Debug, Deserialize, Serialize)]
pub struct PatternClaim {
    pub text: String,
    #[serde(default)]
    pub anchor: Option<String>,
    #[serde(default)]
    pub kind: ClaimKind,
    #[serde(default)]
    pub who: Option<String>,
    #[serde(default)]
    pub quote: Option<String>,
}

impl PatternClaim {
    /// Convert a parsed pattern claim into the canonical `Claim`, trimming the
    /// text and dropping empty optional decorations. Empty-text filtering is
    /// the caller's responsibility (the per-kind distillers already do it).
    pub fn into_claim(self) -> Claim {
        Claim {
            text: self.text.trim().to_string(),
            anchor: self.anchor.filter(|s| !s.is_empty()),
            kind: self.kind,
            who: self.who.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            quote: self.quote.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PatternLink {
    pub url: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// The YAML shape of the map-reduce reduce step (video / voicenote long path):
/// just a re-synthesized summary over the per-chunk summaries.
#[derive(Debug, Deserialize, Serialize)]
pub struct ReduceYaml {
    #[serde(default)]
    pub summary: Option<String>,
}

/// Find a clean break point in `transcript` for chunking: scan backwards from
/// `end` (bounded lookback) for a newline or sentence terminator, falling back
/// to `end`. Shared by the video and voicenote map-reduce chunkers (identical
/// logic). The caller snaps the result to a char boundary (the ASCII matches
/// here are byte-safe, but `end` may not be).
pub fn find_boundary(transcript: &str, start: usize, end: usize) -> usize {
    let bytes = transcript.as_bytes();
    let lookback = end.saturating_sub(start).min(2048);
    let floor = end.saturating_sub(lookback);
    let mut i = end;
    while i > floor {
        i -= 1;
        let b = bytes[i];
        if b == b'\n' {
            return i + 1;
        }
        if (b == b'.' || b == b'!' || b == b'?') && i + 1 < bytes.len() && bytes[i + 1].is_ascii_whitespace() {
            return i + 1;
        }
    }
    end
}

/// The common distiller YAML shape (article / image / video / voicenote). repo
/// and thread add kind-specific fields and keep their own struct, reusing
/// `PatternClaim` / `PatternLink`.
#[derive(Debug, Deserialize, Serialize)]
pub struct PatternYaml {
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub claims: Option<Vec<PatternClaim>>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub links: Option<Vec<PatternLink>>,
}

#[cfg(test)]
mod tests;
