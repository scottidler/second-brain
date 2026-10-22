//! Image candidates parsed from fetched article markdown: `![alt](url)`.
//!
//! readable, fabric, and jina all emit this form. Substack lazy-loads its
//! images, so the destination arrives as
//! `<https://host/p/{"src":"https://substack-post-media...",...}>`: the real
//! URL is the JSON's `src`. Relative URLs resolve against the page URL.
//! The parser is pure; `figures::describe_article_media` calls it.

use regex::Regex;
use std::ops::Range;
use std::sync::OnceLock;

/// One markdown image: its alt text, its absolute URL, and where in the
/// source the whole `![alt](url)` sits, so a caller can splice a
/// description in at its position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkdownImage {
    pub alt: String,
    pub url: String,
    pub span: Range<usize>,
}

fn image_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"!\[((?:[^\]\\]|\\.)*)\]\(\s*(?:<([^>]*)>|([^\s)]+))(?:\s+(?:"[^"]*"|'[^']*'))?\s*\)"#)
            .expect("markdown image regex")
    })
}

fn src_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""src"\s*:\s*"([^"]+)""#).expect("substack src regex"))
}

/// Every `![alt](url)` in `markdown`, in source order, URLs made absolute
/// against `page_url`. A destination that cannot be resolved is skipped.
/// `data:` and other non-http(s) destinations are returned as written: the
/// prefilter drops them with a reason, so the drop is counted.
pub(crate) fn parse_markdown_images(markdown: &str, page_url: &str) -> Vec<MarkdownImage> {
    let base = url::Url::parse(page_url).ok();
    image_re()
        .captures_iter(markdown)
        .filter_map(|caps| {
            let whole = caps.get(0)?;
            let raw = caps.get(2).or_else(|| caps.get(3))?.as_str();
            let url = resolve(raw_destination(raw), base.as_ref());
            if url.is_none() {
                log::debug!("parse_markdown_images: skipping unresolvable destination {raw:?}");
            }
            Some(MarkdownImage {
                alt: caps.get(1).map_or("", |m| m.as_str()).to_string(),
                url: url?,
                span: whole.range(),
            })
        })
        .collect()
}

/// The destination itself, or a Substack lazy-image blob's inner `src`.
fn raw_destination(raw: &str) -> String {
    let decoded;
    let text = if raw.contains("%22src%22") {
        decoded = urlencoding::decode(raw).map_or_else(|_| raw.to_string(), |d| d.into_owned());
        decoded.as_str()
    } else {
        raw
    };
    match src_re().captures(text) {
        Some(caps) => caps[1].replace("\\/", "/"),
        None => raw.to_string(),
    }
}

fn resolve(raw: String, base: Option<&url::Url>) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(absolute) = url::Url::parse(raw) {
        return Some(absolute.into());
    }
    base?.join(raw).ok().map(Into::into)
}

#[cfg(test)]
mod tests;
