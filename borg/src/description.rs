use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use url::Url;
use vault::distilled::Link;

static HASHTAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#([\w][\w-]*)").expect("valid hashtag regex"));

static SECTION_KILLERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)follow me on",
        r"(?i)let's connect",
        r"(?i)connect with me",
        r"(?i)social media links",
        r"(?i)for business inquiries",
        r"(?i)my main.*channel",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("valid section killer regex"))
    .collect()
});

static LINE_KILLERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"sub_confirmation",
        r"(?i)subscribe for more",
        r"(?i)watch my most recent upload",
        r"(?i)consider becoming a patron",
        r"patreon\.com",
        r"(?i)sponsored by",
        r"(?i)affiliate",
        r"promo=",
        r"(?i)teespring\.com",
        r"(?i)\bmerch\b",
        r"(?i)if you find my content helpful",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("valid line killer regex"))
    .collect()
});

/// Regex to detect decorative separator lines (only emoji, whitespace, dashes, no alphanumeric).
static DECORATOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^\w]*$").expect("valid decorator regex"));

/// Extract hashtags from description text.
///
/// Returns lowercase, deduplicated tags with `#` stripped.
pub fn extract_hashtags(description: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut tags = Vec::new();
    for cap in HASHTAG_RE.captures_iter(description) {
        let tag = cap[1].to_lowercase();
        if seen.insert(tag.clone()) {
            tags.push(tag);
        }
    }
    tags
}

/// Filter a YouTube description to remove boilerplate noise.
///
/// Uses a state machine with section killers (kill-to-end) and line killers
/// (drop individual lines). Returns `None` if the result is empty.
pub fn filter_description(description: &str) -> Option<String> {
    let mut result_lines: Vec<String> = Vec::new();
    let mut in_killed_section = false;

    for line in description.lines() {
        // Section killers: once triggered, all remaining lines are dropped
        if !in_killed_section && SECTION_KILLERS.iter().any(|re| re.is_match(line)) {
            in_killed_section = true;
            continue;
        }

        if in_killed_section {
            continue;
        }

        // Line killers: drop individual lines
        if LINE_KILLERS.iter().any(|re| re.is_match(line)) {
            continue;
        }

        // Decorative separators: only emoji + whitespace + dashes, no alphanumeric
        let trimmed = line.trim();
        if !trimmed.is_empty() && DECORATOR_RE.is_match(trimmed) {
            continue;
        }

        // Strip hashtags from the text (they've been extracted into tags)
        let cleaned = HASHTAG_RE.replace_all(line, "").to_string();
        result_lines.push(cleaned);
    }

    // Post-processing: collapse runs of 3+ blank lines to 2
    let mut final_lines: Vec<String> = Vec::new();
    let mut consecutive_blanks = 0;
    for line in &result_lines {
        if line.trim().is_empty() {
            consecutive_blanks += 1;
            if consecutive_blanks <= 2 {
                final_lines.push(line.clone());
            }
        } else {
            consecutive_blanks = 0;
            final_lines.push(line.clone());
        }
    }

    // Trim leading/trailing whitespace
    let text = final_lines.join("\n").trim().to_string();

    if text.is_empty() { None } else { Some(text) }
}

/// Matches an absolute URL token: scheme'd (`http://`/`https://`) or a bare
/// `www.` domain. The `www.` alternative has no scheme and will fail
/// `Url::parse` later -- it is matched anyway so a scheme-less token is a
/// counted, logged drop rather than never considered at all (Data Model:
/// "scheme-less tokens dropped, with a log").
static URL_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:https?://|www\.)\S+").expect("valid url token regex"));

/// Matches a markdown link `[text](url)` whose target is a URL token per
/// `URL_TOKEN_RE`, capturing the link text and the (untrimmed) inner url.
/// The `[^\s)]*` target class stops at the first `)` or whitespace, so it
/// never needs the paren-balance trim that bare tokens do.
static MARKDOWN_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]\n]+)\]\(((?:https?://|www\.)[^\s)]*)\)").expect("valid markdown link regex"));

/// One URL occurrence found on a line, before trimming/validation.
struct UrlOccurrence {
    /// Raw (untrimmed) url text as it appeared on the line.
    raw: String,
    /// Byte offset of `raw`'s start within the line, used to look back for a
    /// preceding "Name:" label when this is the line's sole occurrence.
    start: usize,
    /// Link text, if this occurrence was markdown-wrapped (`[text](url)`).
    markdown_label: Option<String>,
}

/// Find every URL occurrence on a single line: markdown-wrapped links first
/// (so their url span is not double-counted as a bare token), then any
/// remaining bare `URL_TOKEN_RE` matches, in left-to-right order.
fn find_occurrences(line: &str) -> Vec<UrlOccurrence> {
    let mut occurrences = Vec::new();
    let mut covered: Vec<std::ops::Range<usize>> = Vec::new();

    for caps in MARKDOWN_LINK_RE.captures_iter(line) {
        let whole = caps.get(0).expect("group 0 is always present");
        let text = caps.get(1).expect("markdown link text group");
        let url = caps.get(2).expect("markdown link url group");
        covered.push(whole.range());
        occurrences.push(UrlOccurrence {
            raw: url.as_str().to_string(),
            start: url.start(),
            markdown_label: Some(text.as_str().to_string()),
        });
    }

    for m in URL_TOKEN_RE.find_iter(line) {
        if covered.iter().any(|r| r.start <= m.start() && m.end() <= r.end) {
            continue;
        }
        occurrences.push(UrlOccurrence {
            raw: m.as_str().to_string(),
            start: m.start(),
            markdown_label: None,
        });
    }

    occurrences.sort_by_key(|o| o.start);
    occurrences
}

/// Strip a leading list marker (`-`, `*`, `+`, `•`, or `N.`/`N)`) and any
/// leading run of non-alphanumeric characters (e.g. an emoji) that precedes
/// the "Name" text in a `- Name: url` / `1. Name: url` line.
fn strip_leading_marker(line: &str) -> &str {
    let mut s = line.trim_start();
    if let Some(rest) = s.strip_prefix(['-', '*', '+', '•']) {
        s = rest.trim_start();
    } else {
        let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 {
            // `digits` counts ASCII digit chars only, so it is always a valid
            // char boundary to slice at.
            if let Some(after) = s[digits..].strip_prefix('.').or_else(|| s[digits..].strip_prefix(')')) {
                s = after.trim_start();
            }
        }
    }
    s.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '"' && c != '\'')
}

/// Derive a "Name" label for a url that is the sole occurrence on its line
/// and is preceded by a `Name:` shape (leading list marker/emoji stripped).
/// `Name (owner)` is kept whole -- only the LAST colon before the url counts
/// as the separator, so a parenthesized qualifier survives.
fn name_url_label(line: &str, url_start: usize) -> Option<String> {
    // `url_start` comes from a regex match boundary, so it is a valid char
    // boundary to slice at.
    let prefix = line.get(..url_start)?.trim_end();
    let colon_pos = prefix.rfind(':')?;
    let name = strip_leading_marker(&prefix[..colon_pos]).trim();
    if name.is_empty() { None } else { Some(name.to_string()) }
}

/// Trim trailing prose punctuation from a raw url token: unconditionally for
/// `.`/`,`/`>`/`]`, and for a trailing `)` only when it is unbalanced (more
/// `)` than `(` in the token) -- a `)` that closes an earlier `(` inside the
/// same token (e.g. `Rust_(programming_language)`) survives.
fn trim_trailing_punctuation(raw: &str) -> String {
    let mut chars: Vec<char> = raw.chars().collect();
    loop {
        match chars.last() {
            Some('.') | Some(',') | Some('>') | Some(']') => {
                chars.pop();
            }
            Some(')') => {
                let opens = chars.iter().filter(|&&c| c == '(').count();
                let closes = chars.iter().filter(|&&c| c == ')').count();
                if closes > opens {
                    chars.pop();
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    chars.into_iter().collect()
}

/// Derive the labels for every URL occurrence on one line, applying the
/// label-derivation rule: a markdown-wrapped link keeps its text; else, if
/// the url is the line's sole occurrence, a leading `Name:` shape supplies
/// the label; else (or if the line has multiple urls) the url is bare.
fn line_links(line: &str) -> Vec<(String, Option<String>)> {
    let occurrences = find_occurrences(line);
    let sole = occurrences.len() == 1;
    occurrences
        .into_iter()
        .map(|occ| {
            let label = if sole {
                occ.markdown_label.or_else(|| name_url_label(line, occ.start))
            } else {
                None
            };
            (occ.raw, label)
        })
        .collect()
}

/// Extract absolute URLs from a (filtered) video description as `Link`s, in
/// first-seen order, deduped by EXACT full-url string (HTTP paths/queries are
/// case-sensitive -- unlike github slugs, `extract_repo_slugs`' dedup does
/// not generalize here). Applies the unwrap/trim and label rules above; a
/// token that fails `Url::parse` after trimming is dropped. Deterministic; no
/// network, no LLM.
pub fn extract_urls(description: &str) -> Vec<Link> {
    log::debug!("extract_urls: description_len={}", description.len());
    let mut links: Vec<Link> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut dropped = 0usize;

    for line in description.lines() {
        for (raw, label) in line_links(line) {
            let cleaned = trim_trailing_punctuation(&raw);
            if Url::parse(&cleaned).is_err() {
                log::debug!("extract_urls: dropping unparseable token {cleaned:?}");
                dropped += 1;
                continue;
            }
            if !seen.insert(cleaned.clone()) {
                continue;
            }
            links.push(Link { url: cleaned, label });
        }
    }

    log::debug!("extract_urls: extracted={} dropped={}", links.len(), dropped);
    links
}

/// Seam-level, injectable, unit-testable helper: extract the FILTERED
/// description's URLs and merge them into `links` (dedup EXACT on url vs any
/// existing/LLM-emitted links, keep first-seen). Returns `(added, dropped)`
/// counts for the seam's debug log.
pub fn merge_description_links(links: &mut Vec<Link>, filtered_description: &str) -> (usize, usize) {
    log::debug!(
        "merge_description_links: existing_links={} filtered_description_len={}",
        links.len(),
        filtered_description.len()
    );
    let mut seen: HashSet<String> = links.iter().map(|l| l.url.clone()).collect();
    let mut added = 0usize;
    let mut dropped = 0usize;

    for link in extract_urls(filtered_description) {
        if seen.insert(link.url.clone()) {
            links.push(link);
            added += 1;
        } else {
            dropped += 1;
        }
    }

    log::debug!(
        "merge_description_links: added={added} dropped={dropped} total={}",
        links.len()
    );
    (added, dropped)
}

#[cfg(test)]
mod tests;
