//! The one Obsidian wikilink parser and resolver.
//!
//! `parse` yields every `[[target#heading|alias]]` / `![[embed]]` in a note
//! body that Obsidian would render as a link: links inside code (fences,
//! inline spans, indented lines) are literal text and are not yielded.
//! `Resolver` maps a link target to the notes it points at by Obsidian's path
//! rules, through hash indexes built once per pass.

use std::collections::HashMap;
use std::ops::Range;

/// One wikilink occurrence in a body. Every `&str` borrows from the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLink<'a> {
    /// The note reference before any `#` or `|`; empty for a same-note link (`[[#h]]`).
    pub target: &'a str,
    /// `[[a#h]]` -> `h`.
    pub heading: Option<&'a str>,
    /// `[[a#^x]]` -> `x`.
    pub block: Option<&'a str>,
    /// Everything after the first `|`, so `[[a|b|c]]` -> `b|c`.
    pub alias: Option<&'a str>,
    /// A preceding `!` (`![[e]]`).
    pub embed: bool,
    /// Byte range in the body from `[[` (or the embed's `!`) through `]]`.
    pub span: Range<usize>,
}

impl WikiLink<'_> {
    /// The target's note stem: last path segment, `.md` stripped, lowercased.
    pub fn stem(&self) -> String {
        stem(self.target)
    }
}

/// Last path segment of `target`, `.md` stripped, lowercased: `Dir/A.md` -> `a`.
pub fn stem(target: &str) -> String {
    file_stem(target).to_lowercase()
}

/// Last path segment of `target` with `.md` stripped, case kept: `Dir/A.md` -> `A`.
pub fn file_stem(target: &str) -> &str {
    strip_md(target.rsplit('/').next().unwrap_or(target))
}

/// Every wikilink in `body` outside code, in body order.
pub fn parse(body: &str) -> impl Iterator<Item = WikiLink<'_>> {
    prose_lines(body).flat_map(move |(start, end)| links_in_line(body, start, end))
}

/// True when byte `pos` of `body` sits in code by the same rules `parse`
/// skips: a fence line or fenced line, an indented line, or an inline span.
pub fn in_code(body: &str, pos: usize) -> bool {
    match prose_lines(body).find(|&(start, end)| start <= pos && pos <= end) {
        Some((start, end)) => inline_code_spans(body, start, end).iter().any(|c| c.contains(&pos)),
        None => true,
    }
}

fn strip_md(s: &str) -> &str {
    let n = s.len();
    if n >= 3 && s.is_char_boundary(n - 3) && s[n - 3..].eq_ignore_ascii_case(".md") {
        &s[..n - 3]
    } else {
        s
    }
}

/// An open fence: its character and run length.
struct Fence {
    ch: u8,
    len: usize,
}

/// `(char, run length, rest of line)` when `line` (leading spaces already
/// stripped) opens with 3+ backticks or tildes.
fn fence_run(line: &str) -> Option<(u8, usize, &str)> {
    let ch = *line.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = line.bytes().take_while(|&b| b == ch).count();
    (len >= 3).then(|| (ch, len, &line[len..]))
}

fn is_indented_code(line: &str) -> bool {
    line.starts_with("    ") || line.starts_with('\t')
}

/// Byte ranges `(start, end)` of the body's lines that are prose: not inside a
/// fence, not a fence line, not indented code. `end` excludes the line ending.
fn prose_lines(body: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut fence: Option<Fence> = None;
    let mut offset = 0;
    body.split_inclusive('\n').filter_map(move |raw| {
        let start = offset;
        offset += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        let leading = line.trim_start_matches(' ');
        if let Some(open) = &fence {
            if let Some((ch, len, rest)) = fence_run(leading)
                && ch == open.ch
                && len >= open.len
                && rest.trim().is_empty()
            {
                fence = None;
            }
            return None;
        }
        if is_indented_code(line) {
            return None;
        }
        if let Some((ch, len, rest)) = fence_run(leading) {
            // A backtick info string cannot hold a backtick: "```x```" is an inline span.
            if ch == b'~' || !rest.contains('`') {
                fence = Some(Fence { ch, len });
                return None;
            }
        }
        Some((start, start + line.len()))
    })
}

/// Inline code spans on one line, as body byte ranges: a run of N backticks
/// opens, the next run of exactly N closes. An unmatched run is literal.
fn inline_code_spans(body: &str, start: usize, end: usize) -> Vec<Range<usize>> {
    let bytes = body.as_bytes();
    let run_at = |i: usize| bytes[i..end].iter().take_while(|&&b| b == b'`').count();
    let mut spans = Vec::new();
    let mut i = start;
    while i < end {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let open = run_at(i);
        let mut j = i + open;
        let mut close = None;
        while j < end {
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
            Some(c) => {
                spans.push(i..c);
                i = c;
            }
            None => i += open,
        }
    }
    spans
}

fn links_in_line(body: &str, start: usize, end: usize) -> Vec<WikiLink<'_>> {
    let bytes = body.as_bytes();
    let code = inline_code_spans(body, start, end);
    let mut links = Vec::new();
    let mut i = start;
    while i + 1 < end {
        if bytes[i] != b'[' || bytes[i + 1] != b'[' {
            i += 1;
            continue;
        }
        let inner_start = i + 2;
        let mut j = inner_start;
        let mut close = None;
        while j < end {
            match bytes[j] {
                b'[' => break,
                b']' => {
                    if j + 1 < end && bytes[j + 1] == b']' {
                        close = Some(j);
                    }
                    break;
                }
                _ => j += 1,
            }
        }
        let Some(close) = close else {
            // `[[a [[b]]`: restart one byte on so the inner `[[` is found.
            i += 1;
            continue;
        };
        let embed = i > start && bytes[i - 1] == b'!';
        let span = if embed { i - 1 } else { i }..close + 2;
        if !code.iter().any(|c| c.start < span.end && span.start < c.end)
            && let Some(link) = build(&body[inner_start..close], embed, span)
        {
            links.push(link);
        }
        i = close + 2;
    }
    links
}

fn non_empty(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then_some(s)
}

/// Split the text between `[[` and `]]`; `None` when it names nothing.
fn build(inner: &str, embed: bool, span: Range<usize>) -> Option<WikiLink<'_>> {
    let (reference, alias) = match inner.split_once('|') {
        // In a table the pipe is written `\|`; the backslash is not part of the target.
        Some((reference, alias)) => (reference.strip_suffix('\\').unwrap_or(reference), non_empty(alias)),
        None => (inner, None),
    };
    let (target, heading, block) = match reference.split_once('#') {
        Some((target, fragment)) => match fragment.strip_prefix('^') {
            Some(block) => (target, None, non_empty(block)),
            None => (target, non_empty(fragment), None),
        },
        None => (reference, None, None),
    };
    let target = target.trim();
    if target.is_empty() && heading.is_none() && block.is_none() {
        return None;
    }
    Some(WikiLink {
        target,
        heading,
        block,
        alias,
        embed,
        span,
    })
}

/// Resolves link targets to note paths by Obsidian's rules, built once per
/// pass so each lookup is a hash probe rather than a scan of the note set.
///
/// - a target with a `/` matches the note whose lowercased path minus `.md`
///   equals it or ends with `/` + it (`dir/x` matches `a/dir/x.md`, never
///   `otherdir/x.md`)
/// - a bare target matches by stem; a stem shared by several notes resolves
///   to all of them
pub struct Resolver {
    paths: Vec<String>,
    /// Every component-boundary suffix of every lowercased path minus `.md`
    /// (`a/dir/x` -> `a/dir/x`, `dir/x`, `x`), so both rules are one lookup.
    by_suffix: HashMap<String, Vec<usize>>,
}

impl Resolver {
    /// Index `paths` (vault-relative, `/`-separated, e.g. `notes/x.md`).
    pub fn new<I, S>(paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let paths: Vec<String> = paths.into_iter().map(Into::into).collect();
        log::debug!("Resolver::new: indexing {} note paths", paths.len());
        let mut by_suffix: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, path) in paths.iter().enumerate() {
            let key = strip_md(path).to_lowercase();
            let mut suffix = key.as_str();
            loop {
                by_suffix.entry(suffix.to_string()).or_default().push(idx);
                match suffix.split_once('/') {
                    Some((_, rest)) => suffix = rest,
                    None => break,
                }
            }
        }
        log::debug!("Resolver::new: {} distinct suffix keys", by_suffix.len());
        Self { paths, by_suffix }
    }

    /// The note paths `target` points at; empty for an unresolved or
    /// same-note (empty) target.
    pub fn resolve(&self, target: &str) -> impl Iterator<Item = &str> {
        let key = strip_md(target.trim()).to_lowercase();
        self.by_suffix
            .get(&key)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|&idx| self.paths[idx].as_str())
    }
}

#[cfg(test)]
pub(crate) mod tests;
