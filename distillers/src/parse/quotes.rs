//! Repair of malformed one-line double-quoted scalars in a pattern's YAML.
//!
//! Measured on ripr's staged `yaml-parse-error` fallbacks (2026-10-27): 65 of
//! 107 failures were one of three shapes on a `key: "..."` or `- "..."` line,
//! all fixable without guessing content:
//!
//! - an unescaped `"` inside the string (`the "knowledge graph" framing`);
//! - an opening `"` that is never closed before the next sibling key;
//! - a stray `,` after the closing `"` (JSON habit).
//!
//! An unclosed string at the end of the document is NOT closed: that is a
//! truncated output (or a whole document on one line), and closing it would
//! turn a lost tail into a silent success. It stays a parse error.

/// A line opening a double-quoted scalar: indentation, an optional `- `, an
/// optional `key: `, then `"`. Returns (indent, prefix up to and including the
/// opening quote, body after it).
fn split_quoted_line(line: &str) -> Option<(usize, &str, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let mut rest = &line[indent..];
    if let Some(after_dash) = rest.strip_prefix("- ") {
        rest = after_dash.trim_start_matches(' ');
    }
    if let Some(colon) = rest.find(": ") {
        let key = &rest[..colon];
        if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            rest = rest[colon + 2..].trim_start_matches(' ');
        }
    }
    let body = rest.strip_prefix('"')?;
    let prefix_len = line.len() - body.len();
    Some((indent, &line[..prefix_len], body))
}

/// A key or list-item line at `indent` or shallower: a double-quoted scalar
/// cannot continue onto it, so the scalar above must have ended.
fn starts_sibling(line: &str, indent: usize) -> bool {
    let this_indent = line.len() - line.trim_start_matches(' ').len();
    if this_indent > indent {
        return false;
    }
    let rest = &line[this_indent..];
    if rest.starts_with("- ") || rest == "-" {
        return true;
    }
    match rest.find(':') {
        Some(colon) if colon > 0 => {
            let key = &rest[..colon];
            let after = &rest[colon + 1..];
            key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                && (after.is_empty() || after.starts_with(' '))
        }
        _ => false,
    }
}

/// `key: |`, `key: >-`, `- |` and similar: the lines below are literal text.
fn opens_block_scalar(line: &str) -> bool {
    let trimmed = line.trim_end();
    let Some(marker) = trimmed.rsplit(' ').next() else {
        return false;
    };
    let mut chars = marker.chars();
    matches!(chars.next(), Some('|' | '>'))
        && chars.all(|c| matches!(c, '-' | '+' | '0'..='9'))
        && (trimmed.ends_with(&format!(": {marker}")) || trimmed.trim_start() == format!("- {marker}"))
}

fn is_escaped(bytes: &[u8], idx: usize) -> bool {
    let backslashes = bytes[..idx].iter().rev().take_while(|&&b| b == b'\\').count();
    backslashes % 2 == 1
}

fn escape_bare_quotes(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len() + 8);
    for (idx, ch) in text.char_indices() {
        if ch == '"' && !is_escaped(bytes, idx) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// The closing quote of `body`, when the line ends with one (optionally
/// followed by a stray `,`): (inner text, had a trailing comma).
fn closed_body(body: &str) -> Option<(&str, bool)> {
    let trimmed = body.trim_end();
    let (trimmed, comma) = match trimmed.strip_suffix(',') {
        Some(t) => (t.trim_end(), true),
        None => (trimmed, false),
    };
    let inner = trimmed.strip_suffix('"')?;
    (!is_escaped(trimmed.as_bytes(), trimmed.len() - 1)).then_some((inner, comma))
}

/// Rewrite every malformed one-line double-quoted scalar outside block-scalar
/// content. Returns the text and the number of lines rewritten.
pub(super) fn repair_double_quoted(raw: &str) -> (String, usize) {
    let lines: Vec<&str> = raw.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut repaired = 0;
    let mut block_indent: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if let Some(parent) = block_indent {
            if line.trim().is_empty() || indent > parent {
                out.push((*line).to_string());
                continue;
            }
            block_indent = None;
        }
        if opens_block_scalar(line) {
            block_indent = Some(indent);
            out.push((*line).to_string());
            continue;
        }
        let rewritten = split_quoted_line(line).and_then(|(indent, prefix, body)| match closed_body(body) {
            Some((inner, comma)) => {
                let escaped = escape_bare_quotes(inner);
                (escaped != inner || comma).then(|| format!("{prefix}{escaped}\""))
            }
            None => {
                let next = lines[i + 1..].iter().find(|l| !l.trim().is_empty())?;
                starts_sibling(next, indent).then(|| format!("{prefix}{}\"", escape_bare_quotes(body.trim_end())))
            }
        });
        match rewritten {
            Some(fixed) => {
                repaired += 1;
                out.push(fixed);
            }
            None => out.push((*line).to_string()),
        }
    }
    (out.join("\n"), repaired)
}

#[cfg(test)]
mod tests;
