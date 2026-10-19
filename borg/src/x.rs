//! X (Twitter) posts fetched through the fxtwitter JSON API.
//!
//! The generic thread fetch (`fabric -u`, which reads through Jina) returns the
//! logged-out x.com page: login wall, "Relevant people", "Trending now",
//! counters, footer, and the replies interleaved with the quoted post. The
//! fxtwitter API returns the post, its quoted post and any X Article as JSON,
//! so the note is rendered from content alone. Replies, counters and
//! timestamps are never read.

use eyre::{Context, Result};
use serde::Deserialize;
use std::time::Duration;

/// fxtwitter API root; `/status/<id>` is appended.
pub const API_BASE: &str = "https://api.fxtwitter.com";
/// The `extractor:` label a post's `fetched.yml` carries.
pub const EXTRACTOR: &str = "fxtwitter-api";

const HOSTS: &[&str] = &[
    "x.com",
    "www.x.com",
    "twitter.com",
    "www.twitter.com",
    "mobile.twitter.com",
];
const ARTICLE_LINK_PREFIX: &str = "https://x.com/i/article/";

#[derive(Debug, Deserialize)]
struct Envelope {
    tweet: Option<Post>,
}

/// One post, or the post it quotes. Only the fields the note is built from.
#[derive(Debug, Deserialize)]
pub struct Post {
    pub id: String,
    /// Complete for long posts, t.co links already expanded.
    #[serde(default)]
    pub text: String,
    pub author: Author,
    pub quote: Option<Box<Post>>,
    /// Absent or JSON `null` when the post has no media.
    pub media: Option<Media>,
    pub article: Option<Article>,
}

#[derive(Debug, Deserialize)]
pub struct Author {
    pub name: String,
    pub screen_name: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Media {
    #[serde(default)]
    pub photos: Vec<Photo>,
}

#[derive(Debug, Deserialize)]
pub struct Photo {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// An X Article: the post's real body, as Draft.js blocks.
#[derive(Debug, Deserialize)]
pub struct Article {
    pub title: Option<String>,
    pub content: Option<ArticleContent>,
}

#[derive(Debug, Deserialize)]
pub struct ArticleContent {
    #[serde(default)]
    pub blocks: Vec<Block>,
}

#[derive(Debug, Deserialize)]
pub struct Block {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
}

/// Which post of a thread a photo belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Post,
    Quote,
}

/// A photo's already-built Description / Extracted Text section, ready to sit
/// under its post.
#[derive(Debug, Clone)]
pub struct DescribedImage {
    pub owner: Owner,
    pub section: String,
}

/// The status id of an X post URL (`/<user>/status/<id>` on x.com or
/// twitter.com, query ignored). Profiles and `x.com/i/article/<id>` links are `None`.
pub fn status_id(url: &str) -> Option<u64> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if !HOSTS.contains(&host.as_str()) {
        return None;
    }
    let segments: Vec<&str> = parsed.path_segments()?.collect();
    match segments.as_slice() {
        [user, "status", id, ..] if !user.is_empty() => id.parse().ok(),
        _ => None,
    }
}

/// Fetch status `id` from `api_base`. Fails on a non-2xx status, a body that
/// is not JSON by `Content-Type`, unparseable JSON, and `tweet: null`: the
/// caller fails the ingest rather than fall back to the page scrape.
pub async fn fetch_post(api_base: &str, id: u64, timeout_secs: u64) -> Result<Post> {
    log::debug!("x::fetch_post: api_base={api_base} id={id} timeout_secs={timeout_secs}");
    let client = vault::http::client(Duration::from_secs(timeout_secs)).context("build fxtwitter client")?;
    let response = client
        .get(format!("{api_base}/status/{id}"))
        .send()
        .await
        .with_context(|| format!("reach fxtwitter API for {id}"))?;
    let status = response.status();
    if !status.is_success() {
        eyre::bail!("fxtwitter API returned status {status} for post {id}");
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.starts_with("application/json") {
        eyre::bail!("fxtwitter API returned content-type {content_type:?} for post {id}, not JSON");
    }
    let envelope: Envelope = response
        .json()
        .await
        .with_context(|| format!("parse fxtwitter response for post {id}"))?;
    let post = envelope
        .tweet
        .ok_or_else(|| eyre::eyre!("fxtwitter API has no tweet for post {id}"))?;
    log::debug!(
        "x::fetch_post: id={id} author={} text_len={}",
        post.author.screen_name,
        post.text.len()
    );
    Ok(post)
}

/// Each photo on the post, then each on its quoted post, with its owner.
pub fn photos(post: &Post) -> Vec<(Owner, &Photo)> {
    let own = post.media.iter().flat_map(|m| &m.photos).map(|p| (Owner::Post, p));
    let quoted = post
        .quote
        .iter()
        .flat_map(|q| q.media.iter().flat_map(|m| &m.photos))
        .map(|p| (Owner::Quote, p));
    own.chain(quoted).collect()
}

/// Whether the post or its quote (each with any X Article blocks) carries
/// text the note can be built from. A bare Article link is not text,
/// matching [`render`].
pub fn has_text(post: &Post) -> bool {
    !body(post).is_empty() || post.quote.as_ref().is_some_and(|q| !body(q).is_empty())
}

/// The fxtwitter root the ingest path reads: [`API_BASE`] in a build.
#[cfg(not(test))]
pub(crate) fn api_base() -> Result<String> {
    Ok(API_BASE.to_string())
}

/// Tests reach only the stub named by [`bases::scope`]; outside one the
/// ingest path fails instead of calling fxtwitter.
#[cfg(test)]
pub(crate) fn api_base() -> Result<String> {
    bases::current()
}

/// Render a post as `(title, markdown)`: the post (or Article), its quoted
/// post one level deep, and each described photo under its owner. The title
/// is `@<handle> on X`.
pub fn render(post: &Post, images: &[DescribedImage]) -> (String, String) {
    let title = format!("@{} on X", post.author.screen_name);
    let mut out = format!("# {}\n\n", heading(post, "Post", "Article"));
    push_paragraph(&mut out, &body(post));
    push_images(&mut out, images, Owner::Post, &post.author.screen_name);
    if let Some(quote) = &post.quote {
        out.push_str(&format!("## {}\n\n", heading(quote, "Quoted post", "Quoted article")));
        push_paragraph(&mut out, &body(quote));
        push_images(&mut out, images, Owner::Quote, &quote.author.screen_name);
    }
    (title, out.trim_end().to_string() + "\n")
}

/// `<Post|Article> by <name> (@handle)`, with an Article's title appended.
fn heading(post: &Post, post_label: &str, article_label: &str) -> String {
    match &post.article {
        Some(article) => {
            let mut line = format!("{article_label} by {}", byline(&post.author));
            if let Some(title) = article.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                line.push_str(&format!(": {title}"));
            }
            line
        }
        None => format!("{post_label} by {}", byline(&post.author)),
    }
}

/// A post's text, then its X Article blocks. A bare Article link is dropped:
/// the blocks carry the Article.
fn body(post: &Post) -> String {
    let text = post.text.trim();
    let mut out = String::new();
    if !text.is_empty() && !(post.article.is_some() && is_bare_article_link(text)) {
        out.push_str(text);
        out.push_str("\n\n");
    }
    if let Some(content) = post.article.as_ref().and_then(|a| a.content.as_ref()) {
        out.push_str(&render_blocks(&content.blocks));
    }
    out.trim_end().to_string()
}

fn byline(author: &Author) -> String {
    format!("{} (@{})", author.name, author.screen_name)
}

fn is_bare_article_link(text: &str) -> bool {
    text.starts_with(ARTICLE_LINK_PREFIX) && !text.contains(char::is_whitespace)
}

fn push_paragraph(out: &mut String, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        out.push_str(text);
        out.push_str("\n\n");
    }
}

fn push_images(out: &mut String, images: &[DescribedImage], owner: Owner, handle: &str) {
    for (n, image) in images.iter().filter(|i| i.owner == owner).enumerate() {
        out.push_str(&format!("### Image {} (in @{handle}'s post)\n\n", n + 1));
        out.push_str(image.section.trim());
        out.push_str("\n\n");
    }
}

/// Draft.js blocks as markdown. `atomic` blocks (embedded media placeholders)
/// and empty blocks are dropped.
fn render_blocks(blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut ordinal = 0;
    let mut previous_kind = "";
    for block in blocks {
        let text = block.text.trim();
        if block.kind == "atomic" || text.is_empty() {
            continue;
        }
        ordinal = if block.kind == "ordered-list-item" { ordinal + 1 } else { 0 };
        let line = match block.kind.as_str() {
            "header-one" | "header-two" => format!("## {text}"),
            "header-three" | "header-four" | "header-five" | "header-six" => format!("### {text}"),
            "unordered-list-item" => format!("- {text}"),
            "ordered-list-item" => format!("{ordinal}. {text}"),
            "blockquote" => format!("> {text}"),
            "code-block" => format!("```\n{text}\n```"),
            _ => text.to_string(),
        };
        let continues_list = block.kind == previous_kind && block.kind.ends_with("-list-item");
        if !out.is_empty() {
            out.push_str(if continues_list { "\n" } else { "\n\n" });
        }
        out.push_str(&line);
        previous_kind = block.kind.as_str();
    }
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out
}

#[cfg(test)]
pub(crate) mod bases;

#[cfg(test)]
mod tests;
