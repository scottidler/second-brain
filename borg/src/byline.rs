//! Deterministic byline ("author") extraction from raw HTML.
//!
//! Pure: no network. A fetcher that already holds the page HTML hands it here
//! (the byline rides the *same* fetch that produced the body - see the design
//! doc's rejection of a standalone `GET`). Walks a fixed ladder of
//! meta/JSON-LD/link signals and returns the first resolvable author name, or
//! `None` - it never fabricates. `creator` is a single scalar, so when a page
//! lists co-authors the first is taken and the rest dropped (joining names
//! would create junk hub entities in the knowledge graph).
//!
//! Ladder (first hit wins):
//! 1. `<meta name="author" content="...">`
//! 2. JSON-LD `"author"` - string, object with `.name`, or an array of either
//!    (also descended through a top-level `@graph`)
//! 3. `<meta property="article:author">` / `og:article:author`
//! 4. `<a rel="author">text</a>`
//! 5. `None`

use regex::Regex;
use std::sync::LazyLock;

/// Cap on a returned author string. Meta `content=` attributes are sometimes
/// stuffed with entire paragraphs; a real byline is short. Over-long values
/// are rejected as junk rather than truncated (a truncated paragraph is not a
/// name).
const MAX_AUTHOR_LEN: usize = 200;

static META_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<meta\b[^>]*>").expect("static meta regex"));
static SCRIPT_LD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<script\b[^>]*type\s*=\s*['"]application/ld\+json['"][^>]*>(.*?)</script>"#)
        .expect("static jsonld regex")
});
static A_AUTHOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<a\b[^>]*\brel\s*=\s*['"]author['"][^>]*>(.*?)</a>"#).expect("static a-author regex")
});
static TAG_STRIP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<[^>]+>").expect("static tag-strip regex"));

/// Extract a byline from raw HTML. Returns the first resolvable author name or
/// `None`. See the module docs for the ladder.
pub fn extract(html: &str) -> Option<String> {
    log::debug!("byline::extract: html_len={}", html.len());

    // 1. <meta name="author" content="...">
    if let Some(raw) = meta_content(html, "name", "author")
        && let Some(name) = clean(&raw)
    {
        log::debug!("byline::extract: hit meta[name=author] -> {name:?}");
        return Some(name);
    }
    // 2. JSON-LD "author"
    if let Some(raw) = jsonld_author(html)
        && let Some(name) = clean(&raw)
    {
        log::debug!("byline::extract: hit json-ld author -> {name:?}");
        return Some(name);
    }
    // 3. <meta property="article:author"> / og:article:author
    if let Some(raw) =
        meta_content(html, "property", "article:author").or_else(|| meta_content(html, "property", "og:article:author"))
        && let Some(name) = clean(&raw)
    {
        log::debug!("byline::extract: hit meta[property=article:author] -> {name:?}");
        return Some(name);
    }
    // 4. <a rel="author">text</a>
    if let Some(cap) = A_AUTHOR.captures(html) {
        let stripped = TAG_STRIP.replace_all(&cap[1], "");
        if let Some(name) = clean(&stripped) {
            log::debug!("byline::extract: hit a[rel=author] -> {name:?}");
            return Some(name);
        }
    }
    log::debug!("byline::extract: no byline found");
    None
}

/// Find a `<meta>` tag whose `key` attribute equals `want` (case-insensitive)
/// and return its `content` attribute.
fn meta_content(html: &str, key: &str, want: &str) -> Option<String> {
    for m in META_TAG.find_iter(html) {
        let tag = m.as_str();
        if let Some(k) = attr(tag, key)
            && k.eq_ignore_ascii_case(want)
            && let Some(content) = attr(tag, "content")
        {
            return Some(content);
        }
    }
    None
}

/// Read a single quoted attribute value (`name="value"` / `name='value'`) out
/// of a tag string. Whole-key match only (a leading word boundary), so `name`
/// never matches inside another attribute. Avoids per-call regex compilation.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let bytes = tag.as_bytes();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(name) {
        let idx = from + pos;
        let boundary_ok = idx == 0 || bytes[idx - 1].is_ascii_whitespace() || bytes[idx - 1] == b'<';
        let after = idx + name.len();
        let rest = tag[after..].trim_start();
        if boundary_ok && let Some(eq) = rest.strip_prefix('=') {
            let val = eq.trim_start();
            let vbytes = val.as_bytes();
            if let Some(&q) = vbytes.first()
                && (q == b'"' || q == b'\'')
                && let Some(end) = val[1..].find(q as char)
            {
                return Some(val[1..1 + end].to_string());
            }
        }
        from = after;
    }
    None
}

/// Parse each JSON-LD `<script>` block and return the first resolvable author.
fn jsonld_author(html: &str) -> Option<String> {
    for cap in SCRIPT_LD.captures_iter(html) {
        let raw = cap[1].trim();
        let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        if let Some(name) = find_author(&val) {
            return Some(name);
        }
    }
    None
}

/// Recursively locate an `author` value, descending arrays and a top-level
/// `@graph`. Returns the first resolvable name.
fn find_author(val: &serde_json::Value) -> Option<String> {
    match val {
        serde_json::Value::Object(map) => {
            if let Some(author) = map.get("author")
                && let Some(name) = author_name(author)
            {
                return Some(name);
            }
            if let Some(graph) = map.get("@graph").and_then(|g| g.as_array()) {
                for item in graph {
                    if let Some(name) = find_author(item) {
                        return Some(name);
                    }
                }
            }
            None
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(find_author),
        _ => None,
    }
}

/// Resolve an `author` node to a name: a bare string, an object's `.name`, or
/// the first resolvable element of an array (co-authors -> first only).
fn author_name(author: &serde_json::Value) -> Option<String> {
    match author {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(map) => map.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()),
        serde_json::Value::Array(arr) => arr.iter().find_map(author_name),
        _ => None,
    }
}

/// Decode the handful of HTML entities that show up in names, collapse
/// internal whitespace, trim, and reject empty or pathologically long values.
fn clean(raw: &str) -> Option<String> {
    let decoded = decode_entities(raw);
    let normalized = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = normalized.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_AUTHOR_LEN {
        return None;
    }
    Some(trimmed.to_string())
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests;
