//! Small markdown text utilities shared across distillers.
//!
//! These exist because cortex backfill reads existing note bodies as
//! distill inputs. Those bodies carry markdown structure (legacy `## Summary`,
//! `## Description`, etc.) that, if preserved verbatim into the rewritten
//! note's `## Transcript` section, would collide with the new L2 section
//! headings - producing duplicate `## Summary` H2s. Demoting any embedded
//! headings keeps the legacy archive in place but visually subordinate to
//! the new L2 structure.

/// Demote every markdown heading line in `text` by `levels` extra hashes.
///
/// A line counts as a heading iff it starts (after any leading whitespace)
/// with one or more `#` followed by at least one space. The leading run of
/// `#` characters gets `levels` more `#` prepended; the rest of the line
/// is unchanged. Non-heading lines (including `####no-space`, code-fence
/// shebangs, hashtag-style lines like `# tag`, and empty lines) pass through
/// untouched.
///
/// Example: `demote_headings("## Foo\nbody\n# Bar", 2)` returns
/// `"#### Foo\nbody\n### Bar"`.
pub fn demote_headings(text: &str, levels: usize) -> String {
    if levels == 0 || text.is_empty() {
        return text.to_string();
    }
    let prefix = "#".repeat(levels);
    let mut out = String::with_capacity(text.len() + levels * 8);
    let mut first = true;
    for line in text.split('\n') {
        if !first {
            out.push('\n');
        }
        first = false;
        if is_heading_line(line) {
            // Preserve any leading indentation, then inject the extra hashes
            // ahead of the existing heading marker.
            let indent_end = line.find('#').unwrap_or(0);
            out.push_str(&line[..indent_end]);
            out.push_str(&prefix);
            out.push_str(&line[indent_end..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Collapse internal line breaks (and the whitespace around them) into single
/// spaces so multi-line text renders as a single markdown list item. A claim
/// whose text spans lines would otherwise break the `- ` bullet structure when
/// rendered under `## Claims`. Empty lines are dropped; the result is trimmed.
///
/// Example: `flatten_lines("first\n\nsecond")` returns `"first second"`.
pub fn flatten_lines(text: &str) -> String {
    text.split('\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Put every `[[...]]` in model-written prose inside an inline code span so
/// Obsidian shows it as text, not a link. A distilled note carries no links
/// of the model's making; the cortex linker adds links later, against notes
/// that exist. A model summarizing a Rust session drops the backticks around
/// Cargo's `[[bin]]` table header and the note gains a broken `[[bin]]` link.
/// Embeds (`![[x]]`) and anything already in code are left as they are.
pub fn literal_wikilinks(text: &str) -> String {
    let spans: Vec<std::ops::Range<usize>> = vault::wikilink::parse(text)
        .filter(|link| !link.embed)
        .map(|link| link.span)
        .collect();
    if spans.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + spans.len() * 2);
    let mut cursor = 0;
    for span in spans {
        out.push_str(&text[cursor..span.start]);
        out.push('`');
        out.push_str(&text[span.clone()]);
        out.push('`');
        cursor = span.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// True when the trimmed line matches `^#+ ` (one or more hashes followed
/// by a space and at least one more character). Excludes bare `###`,
/// `#nospace`, and lines where the first non-space character is not `#`.
fn is_heading_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return false;
    }
    let after_hashes = trimmed.trim_start_matches('#');
    // Must have at least one `#`, then a space, then non-empty content.
    after_hashes.len() < trimmed.len() && after_hashes.starts_with(' ') && !after_hashes.trim().is_empty()
}

#[cfg(test)]
mod tests;
