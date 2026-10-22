//! The deterministic prefilter: which downloaded images are worth a vision
//! call. Pure functions, no network (`docs/design/2026-10-22-capture-all-diagrams.md`).
//!
//! Format checks apply to every source. Content heuristics (URL tokens,
//! `min-side-px`) apply to web-article images only: an X photo or X Article
//! figure is placed by its author, so a small or oddly-named one is still
//! content. Alt text is never a signal (empty on most real diagrams).

use super::FigureMime;
use crate::config::ImagesConfig;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// Where a candidate image came from; decides which checks apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// A photo on an X post: author-chosen, never classified.
    XPhoto,
    /// A figure inside an X Article: author-placed, classified.
    XArticle,
    /// An image in a web article: content heuristics and classified.
    WebArticle,
}

impl Source {
    /// Whether the vision classifier gets a say. X post photos are the post's
    /// content and skip it.
    pub(crate) fn is_classified(self) -> bool {
        !matches!(self, Self::XPhoto)
    }

    fn applies_content_heuristics(self) -> bool {
        matches!(self, Self::WebArticle)
    }
}

/// Why the prefilter dropped an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropReason {
    /// `data:` URI, a non-http(s) scheme, or a MIME borg does not store (avif, bmp, tiff, ...).
    UnsupportedFormat,
    /// The URL names decoration (avatar, logo, icon, ...).
    UrlToken,
    /// Either axis under `min-side-px`.
    TooSmall,
    /// A web-article image whose header would not decode, so its size is unknown.
    Undecodable,
    /// Zero bytes.
    Empty,
}

impl DropReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedFormat => "unsupported-format",
            Self::UrlToken => "url-token",
            Self::TooSmall => "too-small",
            Self::Undecodable => "undecodable",
            Self::Empty => "empty",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Keep,
    Drop(DropReason),
}

/// URL tokens that only ever name decoration, matched as whole tokens of the
/// host and path split on non-alphanumerics (`icon` must not hit `silicon`).
const DECORATION_TOKENS: &[&str] = &[
    "avatar", "avatars", "gravatar", "authors", "logo", "logos", "icon", "icons", "favicon", "emoji", "pixel", "badge",
    "badges",
];

/// Hosts that serve only decoration.
const DECORATION_HOSTS: &[&str] = &["yt3.ggpht.com", "img.shields.io"];

/// The checks that need only the URL, so a candidate can be dropped before it
/// is downloaded. `Keep` here is not a verdict: [`prefilter`] still runs on the bytes.
pub(crate) fn prefilter_url(source: Source, url: &str) -> Verdict {
    let Ok(parsed) = url::Url::parse(url) else {
        // `data:` URIs parse; anything unparseable cannot be fetched either.
        return Verdict::Drop(DropReason::UnsupportedFormat);
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return Verdict::Drop(DropReason::UnsupportedFormat);
    }
    if source.applies_content_heuristics() && names_decoration(&parsed) {
        return Verdict::Drop(DropReason::UrlToken);
    }
    Verdict::Keep
}

fn names_decoration(url: &url::Url) -> bool {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if DECORATION_HOSTS.contains(&host.as_str()) {
        return true;
    }
    let haystack = format!("{host}/{}", url.path().to_ascii_lowercase());
    haystack
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| DECORATION_TOKENS.contains(&token))
}

/// Keep or drop one downloaded image. `mime` is the response `Content-Type`;
/// `dims` the decoded header's `(width, height)` (an SVG's intrinsic size), or
/// `None` when it would not decode. Duplicates are [`Dedupe`]'s job.
pub(crate) fn prefilter(
    cfg: &ImagesConfig,
    source: Source,
    url: &str,
    mime: &str,
    dims: Option<(u32, u32)>,
    bytes: &[u8],
) -> Verdict {
    if let drop @ Verdict::Drop(_) = prefilter_url(source, url) {
        return drop;
    }
    if FigureMime::from_content_type(mime).is_none() {
        return Verdict::Drop(DropReason::UnsupportedFormat);
    }
    if bytes.is_empty() {
        return Verdict::Drop(DropReason::Empty);
    }
    if source.applies_content_heuristics() {
        match dims {
            None => return Verdict::Drop(DropReason::Undecodable),
            Some((w, h)) if w.min(h) < cfg.min_side_px => return Verdict::Drop(DropReason::TooSmall),
            Some(_) => {}
        }
    }
    Verdict::Keep
}

/// Exact-duplicate detection within one note, by sha256. No perceptual hash:
/// two charts that differ only in their numbers would collide.
#[derive(Debug, Default)]
pub(crate) struct Dedupe {
    seen: HashSet<[u8; 32]>,
}

impl Dedupe {
    /// `true` the first time these bytes are seen, `false` for every repeat.
    pub(crate) fn first_seen(&mut self, bytes: &[u8]) -> bool {
        self.seen.insert(Sha256::digest(bytes).into())
    }
}

#[cfg(test)]
mod tests;
