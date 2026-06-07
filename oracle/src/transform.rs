//! Query-transform stage (Phase 5 of the configurable-retrieval-pipeline doc).
//!
//! This is the LLM-bearing pre-retrieval stage, and it lives in **oracle**, not
//! `vault`: it issues a Fabric subprocess call (`vault::fabric::run_pattern`),
//! and oracle is the composition point that owns the query orchestration. The
//! `vault` retriever primitives stay LLM-free; oracle passes the rewritten
//! query/queries *down* into them.
//!
//! Two methods, both off by default:
//!
//! - **HyDE** - generate one hypothetical answer to the query, then retrieve
//!   with *that* text (so the vector retriever embeds a doc-shaped string rather
//!   than a question). Returns a single transformed query.
//! - **multi-query** - generate N rewrites and retrieve with the original plus
//!   each rewrite, unioning the candidate lists before fusion.
//!
//! The Fabric call itself is not exercised in CI (no LLM); the pure pieces
//! (`parse_transform_output`, `union_lists`) are unit-tested, and the live path
//! is validated by enabling the stage against `sb oracle eval`.

use eyre::Result;

use crate::config::{QueryTransformConfig, TransformMethod};

/// Fabric binary name (resolved on `PATH`, like the eval judge).
const TRANSFORM_BINARY: &str = "fabric";
/// Char budget for the query text sent to the transform pattern (queries are
/// short; this is a generous ceiling).
const TRANSFORM_MAX_CHARS: usize = 4000;
/// Per-call Fabric timeout for a transform (one LLM completion).
const TRANSFORM_TIMEOUT_SECS: u64 = 60;

/// Run the configured transform against `query`, returning the query variant(s)
/// to retrieve with. HyDE => one hypothetical-answer string; multi-query => the
/// original query plus up to `variants` rewrites. Errors propagate so the caller
/// can fail open to the original query.
pub fn fabric_transform(cfg: &QueryTransformConfig, query: &str) -> Result<Vec<String>> {
    let raw = vault::fabric::run_pattern(
        &cfg.pattern,
        query,
        TRANSFORM_BINARY,
        &cfg.model,
        TRANSFORM_MAX_CHARS,
        TRANSFORM_TIMEOUT_SECS,
    )?;
    Ok(parse_transform_output(cfg.method, query, &raw, cfg.variants))
}

/// Parse a Fabric pattern's raw output into the query variants to retrieve with.
/// Pure, so the parsing contract is unit-testable without an LLM.
///
/// - HyDE: the whole output is the hypothetical answer (one variant). Empty
///   output falls back to the original query so retrieval never runs on "".
/// - multi-query: each non-empty line is a rewrite; the original query is kept
///   as the first variant and up to `variants` rewrites follow it.
pub fn parse_transform_output(method: TransformMethod, query: &str, raw: &str, variants: u8) -> Vec<String> {
    match method {
        TransformMethod::Hyde => {
            let h = raw.trim();
            if h.is_empty() { vec![query.to_string()] } else { vec![h.to_string()] }
        }
        TransformMethod::MultiQuery => {
            let mut out = vec![query.to_string()];
            out.extend(
                raw.lines()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .take(variants as usize)
                    .map(str::to_string),
            );
            out
        }
    }
}

/// Union ranked lists (one per query variant) into a single list, preserving
/// first-seen order and dropping duplicates. This is the multi-query "union
/// before fusion" step; it also reduces to the identity for a single list, so
/// the retrieve stage can always run through it. Done *before* per-retriever
/// top-k truncation upstream so each rewrite contributes at full depth (the
/// design doc's resolved open question).
pub fn union_lists(lists: Vec<Vec<String>>) -> Vec<String> {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for list in lists {
        for path in list {
            if seen.insert(path.clone()) {
                out.push(path);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
