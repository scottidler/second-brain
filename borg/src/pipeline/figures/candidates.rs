//! Image candidates parsed from fetched article markdown: `![alt](url)`.
//!
//! readable, fabric, and jina all emit this form. Substack lazy-loads its
//! images, so the destination arrives as
//! `<https://host/p/{"src":"https://substack-post-media...",...}>`: the real
//! URL is the JSON's `src`. Relative URLs resolve against the page URL.
//! htmd (the readable path) backslash-escapes every `(` and `)` in an image
//! link, and a wiki URL carries bare balanced parens (`File:Foo_(bar).png`),
//! so a bare destination takes `\x` escapes and one level of balanced
//! parens, and is unescaped as CommonMark does. An image inside a fenced code
//! block or an inline code span is code, not a figure, and is never a
//! candidate. The parser is pure; `figures::describe_article_media` calls it.

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
        Regex::new(concat!(
            r#"!\[((?:[^\]\\]|\\.)*)\]\(\s*"#,
            // `<dest>`, or a bare dest: `\x` escapes, and one level of balanced parens.
            r#"(?:<((?:[^>\\]|\\.)*)>|((?:[^\s()\\]|\\.|\((?:[^\s()\\]|\\.)*\))+))"#,
            r#"(?:\s+(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'))?\s*\)"#,
        ))
        .expect("markdown image regex")
    })
}

fn src_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""src"\s*:\s*"([^"]+)""#).expect("substack src regex"))
}

/// Every `![alt](url)` in `markdown` outside code, in source order, URLs
/// unescaped and made absolute against `page_url`. A destination that cannot
/// be resolved is skipped.
/// `data:` and other non-http(s) destinations are returned as written: the
/// prefilter drops them with a reason, so the drop is counted.
pub(crate) fn parse_markdown_images(markdown: &str, page_url: &str) -> Vec<MarkdownImage> {
    let base = url::Url::parse(page_url).ok();
    let code = code_ranges(markdown);
    image_re()
        .captures_iter(markdown)
        .filter_map(|caps| {
            let whole = caps.get(0)?;
            if code.iter().any(|range| range.contains(&whole.start())) {
                log::debug!("parse_markdown_images: skipping image in code at {}", whole.start());
                return None;
            }
            let raw = caps.get(2).or_else(|| caps.get(3))?.as_str();
            let url = resolve(unescape(&raw_destination(raw)), base.as_ref());
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

/// CommonMark backslash escapes: `\` before ASCII punctuation is the
/// punctuation itself; any other `\` is literal.
fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
            && next.is_ascii_punctuation()
        {
            out.push(next);
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

/// Byte ranges of `markdown` that are code: fenced blocks (a line opening
/// with three or more backticks or tildes, up to three spaces in, through the
/// closing fence of the same character at least as long, or the end of the
/// text when unclosed) and, outside them, inline code spans (a run of N
/// backticks through the next run of exactly N; an unmatched run is literal).
fn code_ranges(markdown: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut fence: Option<(char, usize, usize)> = None;
    let mut prose_start = 0;
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        let line_end = offset + line.len();
        let marker = fence_marker(line);
        match (fence, marker) {
            (None, Some((ch, len))) => {
                ranges.extend(inline_code_ranges(&markdown[prose_start..offset], prose_start));
                fence = Some((ch, len, offset));
            }
            (Some((ch, len, start)), Some((close_ch, close_len)))
                if close_ch == ch && close_len >= len && line.trim_start()[close_len..].trim().is_empty() =>
            {
                ranges.push(start..line_end);
                fence = None;
                prose_start = line_end;
            }
            _ => {}
        }
        offset = line_end;
    }
    match fence {
        Some((_, _, start)) => ranges.push(start..markdown.len()),
        None => ranges.extend(inline_code_ranges(&markdown[prose_start..], prose_start)),
    }
    ranges
}

/// The fence character and run length when `line` opens or closes a fence.
fn fence_marker(line: &str) -> Option<(char, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let ch = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = rest.chars().take_while(|c| *c == ch).count();
    (len >= 3).then_some((ch, len))
}

/// Inline code spans in `text`, as ranges shifted by `base`.
fn inline_code_ranges(text: &str, base: usize) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let run_at = |i: usize| bytes[i..].iter().take_while(|b| **b == b'`').count();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let open = run_at(i);
        let mut j = i + open;
        let mut close = None;
        while j < bytes.len() {
            if bytes[j] == b'`' {
                let run = run_at(j);
                if run == open {
                    close = Some(j + run);
                    break;
                }
                j += run;
            } else {
                j += 1;
            }
        }
        match close {
            Some(end) => {
                ranges.push(base + i..base + end);
                i = end;
            }
            None => i += open,
        }
    }
    ranges
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
