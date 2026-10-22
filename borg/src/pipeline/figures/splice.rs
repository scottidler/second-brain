//! Kept figures put back into article markdown where their image was, so the
//! distiller reads a diagram's description at the point the author placed it
//! (`docs/design/2026-10-22-capture-all-diagrams.md`, Distiller input).

use super::candidates::MarkdownImage;
use regex::Regex;
use std::sync::OnceLock;

/// What is left of a line once its image is cut out and still counts as "the
/// image's line": nothing, or the empty link a linked image leaves behind
/// (`[![alt](img)](href)` -> `[](href)`).
fn bare_link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\[\s*\]\([^)]*\)$").expect("bare link regex"))
}

/// The distiller's block for one kept figure: `### Image N`, the alt text in
/// italics when there is any, then the Description / Extracted Text section.
pub(crate) fn figure_block(number: usize, alt: &str, section: &str) -> String {
    let mut parts = vec![format!("### Image {number}")];
    let alt = alt.trim();
    if !alt.is_empty() {
        parts.push(format!("*{alt}*"));
    }
    parts.push(section.trim().to_string());
    parts.join("\n\n")
}

/// `markdown` with each `(image, block)` put where the image was: a line that
/// holds only the image (or only a linked image) is replaced whole; an image
/// inside prose has just its `![alt](url)` replaced, the block set off by
/// blank lines. `placed` must come from one parse of `markdown` (spans in
/// bounds, not overlapping); images not in `placed` stay as written.
pub(crate) fn splice(markdown: &str, placed: &[(&MarkdownImage, String)]) -> String {
    log::debug!("figures::splice: bytes={} placed={}", markdown.len(), placed.len());
    let mut order: Vec<&(&MarkdownImage, String)> = placed.iter().collect();
    order.sort_by_key(|(image, _)| std::cmp::Reverse(image.span.start));
    let mut out = markdown.to_string();
    for (image, block) in order {
        let span = image.span.clone();
        let line_start = markdown[..span.start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = markdown[span.end..].find('\n').map_or(markdown.len(), |i| span.end + i);
        let rest = format!("{}{}", &markdown[line_start..span.start], &markdown[span.end..line_end]);
        let rest = rest.trim();
        if rest.is_empty() || bare_link_re().is_match(rest) {
            out.replace_range(line_start..line_end, block);
        } else {
            out.replace_range(span, &format!("\n\n{block}\n\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests;
