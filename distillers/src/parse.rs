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
/// a re-synthesized summary over the per-chunk summaries plus (Phase 5) the
/// claims the reduce pattern SELECTED from the pooled chunk claims. `claims`
/// is serde-defaulted so a reduce pattern that emits only `summary` (the
/// pre-Phase-5 shape) still parses; the distiller falls back to the
/// chronological chunk-claim merge in that case.
#[derive(Debug, Deserialize, Serialize)]
pub struct ReduceYaml {
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub claims: Option<Vec<PatternClaim>>,
}

/// Assemble the Phase 5 two-section reduce input. The reduce pattern selects
/// the final claim set from this pool, so the pool is both the selection
/// source and (on selection failure) the caller's chronological fallback.
///
/// - `## Chunk Summaries`: the per-chunk summaries, chronological, blank-line
///   joined (identical to the pre-Phase-5 reduce input).
/// - `## Claim Pool`: every pooled chunk claim, one per line, prefixed with its
///   `[HH:MM:SS]` anchor when it carries one (voice-note claims carry none, so
///   those lines are plain text).
pub fn build_reduce_input(chunk_summaries: &[String], pool_claims: &[Claim]) -> String {
    let summaries = chunk_summaries.join("\n\n");
    let pool = pool_claims
        .iter()
        .map(|c| match c.anchor.as_deref() {
            Some(a) if !a.trim().is_empty() => format!("[{}] {}", normalize_anchor(a), c.text),
            _ => c.text.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("## Chunk Summaries\n\n{summaries}\n\n## Claim Pool\n\n{pool}\n")
}

/// Apply the Phase 5 anchor-honesty rule to the claims the reduce pattern
/// selected, resolving each against the pooled chunk claims.
///
/// The rule tolerates paraphrase without permitting invention:
/// - a selected claim WITH an anchor that matches a pool anchor is kept (the
///   anchor is normalized to the bracket-free form);
/// - a selected claim WITH an anchor absent from the pool has the anchor
///   stripped to `None` and counts toward `anchors_stripped` (an invented
///   timestamp) — the claim text is retained, never dropped;
/// - a selected claim WITHOUT an anchor is accepted as a legitimate synthesis
///   across pool claims, with NO text-match gate, so consolidation is never
///   discarded as "invented".
///
/// Returns `None` when the selection is empty (every claim had empty text, or
/// the pattern selected nothing), signalling the caller to fall back to the
/// chronological chunk-claim merge.
pub fn select_reduce_claims(
    reduce_claims: Vec<PatternClaim>,
    pool_claims: &[Claim],
    anchors_stripped: &mut u32,
) -> Option<Vec<Claim>> {
    let pool_anchors: std::collections::HashSet<String> = pool_claims
        .iter()
        .filter_map(|c| c.anchor.as_deref())
        .filter(|a| !a.trim().is_empty())
        .map(normalize_anchor)
        .collect();
    let mut selected: Vec<Claim> = Vec::new();
    for pc in reduce_claims {
        let mut claim = pc.into_claim();
        if claim.text.is_empty() {
            continue;
        }
        if let Some(anchor) = claim.anchor.take() {
            let norm = normalize_anchor(&anchor);
            if !norm.is_empty() && pool_anchors.contains(&norm) {
                claim.anchor = Some(norm);
            } else {
                // Anchor not present in the pool: an invented/altered timestamp.
                // Strip it and count it; keep the claim text.
                *anchors_stripped = anchors_stripped.saturating_add(1);
            }
        }
        selected.push(claim);
    }
    if selected.is_empty() { None } else { Some(selected) }
}

/// Thread long-path reduce input (Phase 6). Same two labeled sections as
/// [`build_reduce_input`], PLUS a leading `## Thread Head` section carrying the
/// verbatim transcript head. Thread metadata (the author handle and post
/// structure) lives at the top of the rendered thread, so the thread reduce
/// pattern reads `author`/`post-count` from this head — the mechanism that
/// keeps `KindPayload::Thread` fields alive through the map-reduce path (a
/// chunked thread's individual chunks otherwise never see the whole author
/// line, and the single-call parse that used to extract it no longer runs).
pub fn build_thread_reduce_input(head: &str, chunk_summaries: &[String], pool_claims: &[Claim]) -> String {
    let base = build_reduce_input(chunk_summaries, pool_claims);
    format!("## Thread Head\n\n{head}\n\n{base}")
}

/// Loud sub-threshold truncation signal (Phase 6). `vault::fabric::run_pattern`
/// calls `truncate_input`, which silently cuts the tail of any single-call
/// distiller input longer than `max_chars`, logging only a daemon-log WARN with
/// no trace id (the LLM-free vault crate has none in scope). A distiller
/// detects the same cut at its own boundary — where the source URL is in scope —
/// and records this distinct `bounds_truncations` entry so the truncation is
/// visible in the distillation metadata, not just a stray log line. Returns
/// `None` when no cut would happen (`max_chars == 0` means "no limit", matching
/// `truncate_input`'s own short-circuit).
pub fn input_truncation_tag(char_count: usize, max_chars: usize) -> Option<String> {
    if max_chars > 0 && char_count > max_chars {
        Some(format!("input:{char_count}>{max_chars}"))
    } else {
        None
    }
}

/// Normalize an anchor for pool matching: trim whitespace and strip a single
/// pair of surrounding brackets so `[00:00:05]` and `00:00:05` compare equal.
fn normalize_anchor(anchor: &str) -> String {
    anchor
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim()
        .to_string()
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
