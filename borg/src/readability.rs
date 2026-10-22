//! In-process clean-article extraction: fetch HTML with a browser UA, extract
//! the main article with `dom_smoothie` (a pure-Rust Mozilla-Readability port),
//! and render it to markdown with `htmd`. No external binary or language
//! runtime - this replaces the earlier shell-out to the `defuddle` Node CLI, so
//! there is no PATH / daemon-reachability problem.

use std::time::Duration;

use thiserror::Error;
use vault::http::Timeouts;

/// A realistic desktop Firefox UA. Some sites gate plain/library UAs but serve
/// browser-looking clients; mirrors `stages::fetcher::BROWSER_UA`.
pub(crate) const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:120.0) Gecko/20100101 Firefox/120.0";

/// Typed failure modes for [`fetch_article_readable`]. The caller falls through
/// to the existing fabric-u/Jina/browser-UA chain on ANY variant, matching on
/// the enum rather than string-sniffing a message.
#[derive(Debug, Error)]
pub enum ReadableError {
    #[error("readable fetch failed for {url}: {source}")]
    Fetch {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("readable fetch for {url} returned HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("readability extraction failed for {url}: {reason}")]
    Extract { url: String, reason: String },
    #[error("readability produced empty content for {url}")]
    Empty { url: String },
    #[error("readable fetch for {url} returned {content_type}, not HTML")]
    NotHtml { url: String, content_type: String },
    #[error("readability refused {url}: article nests {depth} elements deep (limit {limit})")]
    TooDeep { url: String, depth: usize, limit: usize },
}

/// What a readable fetch found at the URL. A PDF is recognised by its
/// `Content-Type` or its `%PDF-` magic bytes, never by the URL path, so a
/// query string or a mislabeled server cannot send it down the HTML path.
#[derive(Debug)]
pub enum Fetched {
    Article(String),
    Pdf(Vec<u8>),
}

#[derive(Debug, PartialEq)]
enum BodyKind {
    Html,
    Pdf,
    Other,
}

const PDF_MAGIC: &[u8] = b"%PDF-";

fn body_kind(content_type: Option<&str>, body: &[u8]) -> BodyKind {
    let is_pdf_type = content_type.is_some_and(|ct| ct.to_ascii_lowercase().contains("application/pdf"));
    if is_pdf_type || body.starts_with(PDF_MAGIC) {
        BodyKind::Pdf
    } else if is_html_content_type(content_type) {
        BodyKind::Html
    } else {
        BodyKind::Other
    }
}

/// `htmd` walks the DOM recursively, a few frames per nesting level. A 14 MB
/// PDF served from a URL with a query string reached this path, parsed into
/// ~2,400 nested elements, and overflowed the 2 MiB blocking-thread stack,
/// which aborts the whole daemon. Real articles nest well under 100 deep.
const MAX_NESTING_DEPTH: usize = 256;

/// Whether a `Content-Type` header names an HTML document. A missing header
/// is treated as HTML (servers omit it); anything else (PDF, images, JSON)
/// falls through to the fetch chain built for it.
fn is_html_content_type(content_type: Option<&str>) -> bool {
    content_type.is_none_or(|ct| {
        let ct = ct.to_ascii_lowercase();
        ct.contains("text/html") || ct.contains("application/xhtml")
    })
}

/// Deepest element nesting in `html`, computed without recursion so it is safe
/// on any input. Counts start tags against end tags; void elements, self-closing
/// tags, comments, and declarations do not nest.
fn max_nesting_depth(html: &str) -> usize {
    const VOID: [&str; 14] = [
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr",
    ];
    let bytes = html.as_bytes();
    let (mut depth, mut max, mut i) = (0usize, 0usize, 0usize);
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let end = match html[i..].find('>') {
            Some(off) => i + off,
            None => break,
        };
        let tag = &html[i + 1..end];
        if let Some(rest) = tag.strip_prefix('/') {
            if !rest.is_empty() {
                depth = depth.saturating_sub(1);
            }
        } else if !(tag.starts_with('!') || tag.starts_with('?') || tag.ends_with('/')) {
            let name: String = tag
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if !name.is_empty() && !VOID.contains(&name.as_str()) {
                depth += 1;
                max = max.max(depth);
            }
        }
        i = end + 1;
    }
    max
}

/// Fetch `url` with a browser UA, extract the main article (`dom_smoothie`), and
/// render it to markdown (`htmd`). `timeout_secs` bounds the HTTP fetch. DOM
/// parsing + markdown rendering are CPU-bound and synchronous, so they run on a
/// blocking thread (never on the async runtime, per subprocess/async hygiene).
/// Returns the article markdown, the bytes of a PDF for the caller's PDF
/// extractor, or a typed [`ReadableError`] the caller treats as "fall through
/// to the existing fetch chain".
pub async fn fetch_article_readable(url: &str, timeout_secs: u64) -> Result<Fetched, ReadableError> {
    log::debug!("fetch_article_readable: url={url} timeout_secs={timeout_secs}");

    let client = vault::http::builder(Timeouts::total(Duration::from_secs(timeout_secs)))
        .user_agent(BROWSER_UA)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|source| ReadableError::Fetch {
            url: url.to_string(),
            source,
        })?;

    let response = client.get(url).send().await.map_err(|source| ReadableError::Fetch {
        url: url.to_string(),
        source,
    })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(ReadableError::Status {
            url: url.to_string(),
            status,
        });
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let is_pdf_type = body_kind(content_type.as_deref(), b"") == BodyKind::Pdf;
    if !is_pdf_type && !is_html_content_type(content_type.as_deref()) {
        return Err(ReadableError::NotHtml {
            url: url.to_string(),
            content_type: content_type.unwrap_or_default(),
        });
    }
    let body = response.bytes().await.map_err(|source| ReadableError::Fetch {
        url: url.to_string(),
        source,
    })?;
    if body_kind(content_type.as_deref(), &body) == BodyKind::Pdf {
        log::info!(
            "fetch_article_readable: url={url} is a PDF ({} bytes, content-type {})",
            body.len(),
            content_type.as_deref().unwrap_or("none")
        );
        return Ok(Fetched::Pdf(body.to_vec()));
    }
    let html = String::from_utf8_lossy(&body).into_owned();

    let url_owned = url.to_string();
    let markdown = match tokio::task::spawn_blocking(move || extract_markdown(&html, &url_owned)).await {
        Ok(result) => result?,
        Err(join) => {
            return Err(ReadableError::Extract {
                url: url.to_string(),
                reason: format!("extraction task failed: {join}"),
            });
        }
    };

    if markdown.trim().is_empty() {
        return Err(ReadableError::Empty { url: url.to_string() });
    }
    log::debug!(
        "fetch_article_readable: url={url} produced {} chars",
        markdown.chars().count()
    );
    Ok(Fetched::Article(markdown))
}

/// Sync extraction: `dom_smoothie` (HTML -> clean article HTML) -> `htmd`.
/// Separated from the async fetch so it is unit-testable over a static HTML
/// fixture without network.
fn extract_markdown(html: &str, url: &str) -> Result<String, ReadableError> {
    let mut readability =
        dom_smoothie::Readability::new(html, Some(url), None).map_err(|e| ReadableError::Extract {
            url: url.to_string(),
            reason: e.to_string(),
        })?;
    let article = readability.parse().map_err(|e| ReadableError::Extract {
        url: url.to_string(),
        reason: e.to_string(),
    })?;
    render_markdown(&article.content, url)
}

/// `htmd` over article HTML, refused past [`MAX_NESTING_DEPTH`] so a
/// pathological page fails this extraction instead of overflowing the stack.
fn render_markdown(content_html: &str, url: &str) -> Result<String, ReadableError> {
    let depth = max_nesting_depth(content_html);
    if depth > MAX_NESTING_DEPTH {
        return Err(ReadableError::TooDeep {
            url: url.to_string(),
            depth,
            limit: MAX_NESTING_DEPTH,
        });
    }
    htmd::convert(content_html).map_err(|e| ReadableError::Extract {
        url: url.to_string(),
        reason: e.to_string(),
    })
}

#[cfg(test)]
mod tests;
