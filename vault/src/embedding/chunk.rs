//! Token-accurate windows over the text an embedding row is computed from.
//!
//! The encoder's window is counted in tokens, so the chunker counts tokens
//! with the same tokenizer the model loads. A window is a verbatim slice of
//! the source text, cut at token boundaries taken from the tokenizer's byte
//! offsets. WordPiece can re-tokenize a cut slice into more tokens than the
//! span it was cut from (a word split mid-way), so every final
//! `prefix + slice` is re-encoded with truncation disabled and shrunk one
//! token at a time until it fits. Nothing reaches the model longer than its
//! window, so the encoder's own truncation never fires on a chunk.
//!
//! [`CHUNK_OVERLAP_TOKENS`] and [`PREFIX_MAX_TOKENS`] are part of the
//! transcript-chunk recipe (`vault::search::TRANSCRIPT_CHUNK_RECIPE`): change
//! either and that recipe must be bumped, or old rows never re-embed.

use std::ops::Range;

use eyre::{Result, eyre};

/// Source tokens each window shares with the one before it, so a passage that
/// straddles a boundary is whole in at least one vector.
pub const CHUNK_OVERLAP_TOKENS: usize = 64;

/// Longest context prefix a window carries, in tokens. A longer prefix is cut
/// at a token boundary first, so a 512-token model keeps at least
/// `512 - 2 - 64 = 446` tokens of source per window, well above the overlap.
pub const PREFIX_MAX_TOKENS: usize = 64;

/// Tokens the encoder adds around every input (`[CLS]` and `[SEP]` for BERT).
pub const SPECIAL_TOKENS: usize = 2;

/// Joins a context prefix to its window.
const PREFIX_SEPARATOR: &str = "\n\n";

/// The tokenizer view the chunker needs. The real backends implement it over
/// their loaded `tokenizer.json` ([`TokenChunker`]); `MockEmbedder` uses a
/// whitespace stand-in.
pub trait Segmenter {
    /// Byte span in `text` of every token, in order, special tokens excluded.
    fn token_spans(&self, text: &str) -> Result<Vec<Range<usize>>>;

    /// Tokens `text` encodes to with the special tokens added and truncation
    /// disabled: the length the encoder would actually see.
    fn count_tokens(&self, text: &str) -> Result<usize>;
}

/// One window: the exact text to embed and the source tokens it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// `prefix + "\n\n" + slice`, or the bare slice when there is no prefix.
    pub text: String,
    /// Indices into the source text's tokens (`Segmenter::token_spans`).
    pub tokens: Range<usize>,
}

/// Chunk `text` into windows that each encode to at most `max_input_tokens`
/// tokens, `prefix` included. Returns the texts to embed.
pub fn chunk(segmenter: &dyn Segmenter, max_input_tokens: usize, prefix: &str, text: &str) -> Result<Vec<String>> {
    Ok(windows(segmenter, max_input_tokens, prefix, text)?
        .into_iter()
        .map(|w| w.text)
        .collect())
}

/// [`chunk`] with each window's source-token range, for callers and tests
/// that need to see what each window covers.
///
/// - Blank `text` gives no windows.
/// - Window capacity is `max_input_tokens - SPECIAL_TOKENS - tokens(prefix)`,
///   after the prefix is cut to [`PREFIX_MAX_TOKENS`]. A capacity at or below
///   [`CHUNK_OVERLAP_TOKENS`] is an error: the windows could not advance.
/// - Consecutive windows share [`CHUNK_OVERLAP_TOKENS`] source tokens (fewer
///   only if a window had to shrink below that, and never zero), so together
///   they cover every source token.
/// - A tokenizer error is returned, never papered over with a truncated chunk.
pub fn windows(segmenter: &dyn Segmenter, max_input_tokens: usize, prefix: &str, text: &str) -> Result<Vec<Window>> {
    log::trace!(
        "embedding::chunk::windows: max_input_tokens={max_input_tokens} prefix_bytes={} text_bytes={}",
        prefix.len(),
        text.len()
    );
    let prefix = cut_prefix(segmenter, prefix.trim())?;
    let prefix_tokens = if prefix.is_empty() {
        0
    } else {
        segmenter.count_tokens(prefix)?.saturating_sub(SPECIAL_TOKENS)
    };
    let capacity = max_input_tokens
        .checked_sub(SPECIAL_TOKENS + prefix_tokens)
        .filter(|c| *c > CHUNK_OVERLAP_TOKENS)
        .ok_or_else(|| {
            eyre!(
                "a {max_input_tokens}-token window with a {prefix_tokens}-token prefix leaves no room for more than \
                 the {CHUNK_OVERLAP_TOKENS}-token overlap"
            )
        })?;

    let spans = segmenter.token_spans(text)?;
    let n = spans.len();
    let mut out = Vec::new();
    let mut start = 0;
    while start < n {
        let mut end = (start + capacity).min(n);
        let window = loop {
            let slice = text.get(spans[start].start..spans[end - 1].end).ok_or_else(|| {
                eyre!(
                    "token span {}..{} is not a char boundary slice of the text",
                    spans[start].start,
                    spans[end - 1].end
                )
            })?;
            let composed = compose(prefix, slice);
            let count = segmenter.count_tokens(&composed)?;
            if count <= max_input_tokens {
                break Window {
                    text: composed,
                    tokens: start..end,
                };
            }
            if end - start == 1 {
                eyre::bail!("source token {start} alone encodes to {count} tokens, over the {max_input_tokens} window");
            }
            end -= 1;
        };
        out.push(window);
        if end == n {
            break;
        }
        start = end.saturating_sub(CHUNK_OVERLAP_TOKENS).max(start + 1);
    }
    log::trace!("embedding::chunk::windows: tokens={n} windows={}", out.len());
    Ok(out)
}

/// `prefix` cut at a token boundary to at most [`PREFIX_MAX_TOKENS`] tokens.
fn cut_prefix<'a>(segmenter: &dyn Segmenter, prefix: &'a str) -> Result<&'a str> {
    if prefix.is_empty() {
        return Ok(prefix);
    }
    let spans = segmenter.token_spans(prefix)?;
    if spans.len() <= PREFIX_MAX_TOKENS {
        return Ok(prefix);
    }
    let end = spans[PREFIX_MAX_TOKENS - 1].end;
    prefix
        .get(..end)
        .ok_or_else(|| eyre!("prefix token boundary {end} is not a char boundary"))
}

fn compose(prefix: &str, slice: &str) -> String {
    if prefix.is_empty() {
        slice.to_string()
    } else {
        format!("{prefix}{PREFIX_SEPARATOR}{slice}")
    }
}

/// [`Segmenter`] over a real `tokenizers::Tokenizer`, with truncation and
/// padding disabled so a count is the true encoded length.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub struct TokenChunker {
    tokenizer: tokenizers::Tokenizer,
}

#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
impl TokenChunker {
    /// Load `tokenizer.json` from `path`.
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        let tokenizer = tokenizers::Tokenizer::from_file(path)
            .map_err(|e| eyre!("failed to load tokenizer.json at {}: {e}", path.display()))?;
        Self::new(tokenizer)
    }

    /// Wrap `tokenizer`, turning its truncation and padding off.
    pub fn new(mut tokenizer: tokenizers::Tokenizer) -> Result<Self> {
        tokenizer
            .with_truncation(None)
            .map_err(|e| eyre!("failed to disable truncation: {e}"))?;
        tokenizer.with_padding(None);
        Ok(Self { tokenizer })
    }
}

#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
impl Segmenter for TokenChunker {
    fn token_spans(&self, text: &str) -> Result<Vec<Range<usize>>> {
        let encoding = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| eyre!("tokenizer encode failed: {e}"))?;
        Ok(encoding.get_offsets().iter().map(|&(s, e)| s..e).collect())
    }

    fn count_tokens(&self, text: &str) -> Result<usize> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| eyre!("tokenizer encode failed: {e}"))?;
        Ok(encoding.get_ids().len())
    }
}

/// Deterministic stand-in for a tokenizer: one token per whitespace-separated
/// word, plus [`SPECIAL_TOKENS`]. `MockEmbedder` chunks with it.
#[cfg(any(test, feature = "test-util"))]
pub struct WhitespaceSegmenter;

#[cfg(any(test, feature = "test-util"))]
impl Segmenter for WhitespaceSegmenter {
    fn token_spans(&self, text: &str) -> Result<Vec<Range<usize>>> {
        let mut spans = Vec::new();
        let mut start: Option<usize> = None;
        for (i, c) in text.char_indices() {
            match (c.is_whitespace(), start) {
                (true, Some(s)) => {
                    spans.push(s..i);
                    start = None;
                }
                (false, None) => start = Some(i),
                _ => {}
            }
        }
        if let Some(s) = start {
            spans.push(s..text.len());
        }
        Ok(spans)
    }

    fn count_tokens(&self, text: &str) -> Result<usize> {
        Ok(text.split_whitespace().count() + SPECIAL_TOKENS)
    }
}

#[cfg(test)]
mod tests;
