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
/// fxtwitter answers 401 to a request with no `User-Agent` and asks for a
/// descriptive one; reqwest sends none by default.
pub const USER_AGENT: &str = "second-brain-borg-x-fetcher/1.0 (+https://github.com/scottidler/second-brain)";

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

/// An X Article: the post's real body, as Draft.js blocks, plus the media
/// its `atomic` blocks point at.
#[derive(Debug, Deserialize)]
pub struct Article {
    pub title: Option<String>,
    pub content: Option<ArticleContent>,
    #[serde(default)]
    pub media_entities: Vec<MediaEntity>,
}

#[derive(Debug, Deserialize)]
pub struct ArticleContent {
    #[serde(default)]
    pub blocks: Vec<Block>,
    /// In fxtwitter a list of `{key, value}`, not a Draft.js dict.
    #[serde(rename = "entityMap", default)]
    pub entity_map: Vec<Entity>,
}

#[derive(Debug, Deserialize)]
pub struct Block {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(rename = "entityRanges", default)]
    pub entity_ranges: Vec<EntityRange>,
}

/// A span of a block bound to an entity. An `atomic` block's first range
/// names the media entity it embeds.
///
/// The entity fields borg reads are all tolerant (`None` when missing or
/// malformed): a bad entity skips its `atomic` block in [`article_figures`],
/// it never fails the post.
#[derive(Debug, Deserialize)]
pub struct EntityRange {
    /// A number in the block, a string in the entity map: read as a string.
    #[serde(default, deserialize_with = "key_as_string")]
    pub key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Entity {
    #[serde(default, deserialize_with = "key_as_string")]
    pub key: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub value: Option<EntityValue>,
}

#[derive(Debug, Deserialize)]
pub struct EntityValue {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub data: EntityData,
}

/// A `MEDIA` entity's data; a `LINK` entity carries none of these fields.
#[derive(Debug, Default, Deserialize)]
pub struct EntityData {
    pub caption: Option<String>,
    #[serde(rename = "mediaItems", default)]
    pub media_items: Vec<MediaItem>,
}

#[derive(Debug, Deserialize)]
pub struct MediaItem {
    #[serde(rename = "mediaId", default, deserialize_with = "key_as_string")]
    pub media_id: Option<String>,
}

/// One image an Article carries, keyed by the id a `MediaItem` names.
#[derive(Debug, Deserialize)]
pub struct MediaEntity {
    #[serde(default, deserialize_with = "key_as_string")]
    pub media_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub media_info: Option<MediaInfo>,
}

#[derive(Debug, Deserialize)]
pub struct MediaInfo {
    pub original_img_url: Option<String>,
}

/// An Article figure resolved from its `atomic` block: the block's index,
/// the image URL, and the entity's caption (`Figure 2: Auto Crawler.`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleFigure {
    pub block: usize,
    pub url: String,
    pub caption: Option<String>,
}

/// Where a described image sits in the render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A post photo, under its owning post.
    Photo,
    /// The owning post's Article figure at this index of [`article_figures`],
    /// at its `atomic` block.
    Figure(usize),
}

/// An entity key or media id: a string, or a number read as a string. Any
/// other shape is `None`, never an error.
fn key_as_string<'de, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(s) => Some(s),
        serde_json::Value::Number(n) => Some(n.to_string()),
        other => {
            log::debug!("x: entity key {other} is not a string or number; ignored");
            None
        }
    })
}

/// `T` when the value parses as one, else `None`, never an error.
fn lenient<'de, D: serde::Deserializer<'de>, T: serde::de::DeserializeOwned>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value)
        .inspect_err(|e| log::debug!("x: malformed entity field ignored: {e}"))
        .ok())
}

/// Which post of a thread a photo belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Post,
    Quote,
}

/// An image's already-built Description / Extracted Text section, ready to
/// sit under its post (a photo) or at its `atomic` block (an Article figure).
#[derive(Debug, Clone)]
pub struct DescribedImage {
    pub owner: Owner,
    pub place: Place,
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
    let client = vault::http::builder(vault::http::Timeouts::total(Duration::from_secs(timeout_secs)))
        .user_agent(USER_AGENT)
        .build()
        .context("build fxtwitter client")?;
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

/// The figures of a post's own X Article, in block order. Each `atomic`
/// block resolves `entityRanges[0].key` -> the `entityMap` entry with that key
/// -> a `MEDIA` value -> each `mediaItems[].mediaId` -> the `media_entities`
/// entry with that `media_id` -> `media_info.original_img_url`; the caption
/// is the entity's. A block that does not resolve is skipped with a log line,
/// never an error: the rest of the Article still renders.
pub fn article_figures(post: &Post) -> Vec<ArticleFigure> {
    let Some(article) = post.article.as_ref() else {
        return Vec::new();
    };
    let Some(content) = article.content.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (index, block) in content.blocks.iter().enumerate() {
        if block.kind != "atomic" {
            continue;
        }
        match resolve_atomic(block, content, &article.media_entities) {
            Ok(found) => out.extend(found.into_iter().map(|(url, caption)| ArticleFigure {
                block: index,
                url,
                caption,
            })),
            Err(why) => log::info!("x::article_figures: post {} block {index} skipped: {why}", post.id),
        }
    }
    log::debug!("x::article_figures: post={} figures={}", post.id, out.len());
    out
}

/// One `atomic` block's `(url, caption)` pairs, or why it does not resolve.
fn resolve_atomic(
    block: &Block,
    content: &ArticleContent,
    media: &[MediaEntity],
) -> std::result::Result<Vec<(String, Option<String>)>, String> {
    let key = block
        .entity_ranges
        .first()
        .ok_or("no entity range")?
        .key
        .as_deref()
        .ok_or("entity range has no usable key")?;
    let entity = content
        .entity_map
        .iter()
        .find(|e| e.key.as_deref() == Some(key))
        .ok_or_else(|| format!("no entity {key}"))?;
    let value = entity
        .value
        .as_ref()
        .ok_or_else(|| format!("entity {key} has no usable value"))?;
    if value.kind != "MEDIA" {
        return Err(format!("entity {key} is {:?}, not MEDIA", value.kind));
    }
    let caption = value
        .data
        .caption
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    let mut found = Vec::new();
    for item in &value.data.media_items {
        let Some(media_id) = item.media_id.as_deref() else {
            log::info!("x::article_figures: entity {key} has a media item with no usable id");
            continue;
        };
        let url = media
            .iter()
            .find(|m| m.media_id.as_deref() == Some(media_id))
            .and_then(|m| m.media_info.as_ref())
            .and_then(|i| i.original_img_url.clone());
        match url {
            Some(url) => found.push((url, caption.clone())),
            None => log::info!("x::article_figures: entity {key} media {media_id} has no image url"),
        }
    }
    if found.is_empty() {
        return Err(format!("entity {key} names no resolvable media"));
    }
    Ok(found)
}

/// Whether the post or its quote (each with any X Article blocks) carries
/// text the note can be built from. A bare Article link is not text,
/// matching [`render`].
pub fn has_text(post: &Post) -> bool {
    !body(post, 2, &[]).is_empty() || post.quote.as_ref().is_some_and(|q| !body(q, 3, &[]).is_empty())
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
/// post one level deep, each described photo under its owner, and each
/// described Article figure at its `atomic` block. The title is
/// `@<handle> on X`.
pub fn render(post: &Post, images: &[DescribedImage]) -> (String, String) {
    let title = format!("@{} on X", post.author.screen_name);
    let mut out = format!("# {}\n\n", heading(post, "Post", "Article"));
    push_paragraph(&mut out, &body(post, 2, &owned_figures(images, Owner::Post)));
    push_images(&mut out, images, Owner::Post, &post.author.screen_name);
    if let Some(quote) = &post.quote {
        out.push_str(&format!("## {}\n\n", heading(quote, "Quoted post", "Quoted article")));
        push_paragraph(&mut out, &body(quote, 3, &owned_figures(images, Owner::Quote)));
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

/// `owner`'s described Article figures as `(figure index, section)`.
fn owned_figures(images: &[DescribedImage], owner: Owner) -> Vec<(usize, &str)> {
    images
        .iter()
        .filter(|i| i.owner == owner)
        .filter_map(|i| match i.place {
            Place::Figure(n) => Some((n, i.section.as_str())),
            Place::Photo => None,
        })
        .collect()
}

/// A post's text, then its X Article blocks with their top headings at
/// `level` (`##` under the post, `###` under a quoted post), each described
/// figure (`(index into article_figures, section)`) at its `atomic` block. A
/// bare Article link is dropped: the blocks carry the Article.
fn body(post: &Post, level: usize, described: &[(usize, &str)]) -> String {
    let text = post.text.trim();
    let mut out = String::new();
    if !text.is_empty() && !(post.article.is_some() && is_bare_article_link(text)) {
        out.push_str(text);
        out.push_str("\n\n");
    }
    if let Some(content) = post.article.as_ref().and_then(|a| a.content.as_ref()) {
        let resolved = if described.is_empty() { Vec::new() } else { article_figures(post) };
        let placed: Vec<PlacedFigure> = described
            .iter()
            .filter_map(|(n, section)| {
                resolved.get(*n).map(|f| PlacedFigure {
                    block: f.block,
                    caption: f.caption.as_deref(),
                    section,
                })
            })
            .collect();
        out.push_str(&render_blocks(&content.blocks, level, &placed));
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
    let photos = images.iter().filter(|i| i.owner == owner && i.place == Place::Photo);
    for (n, image) in photos.enumerate() {
        out.push_str(&format!("### Image {} (in @{handle}'s post)\n\n", n + 1));
        out.push_str(image.section.trim());
        out.push_str("\n\n");
    }
}

/// A described Article figure placed at its `atomic` block.
struct PlacedFigure<'a> {
    block: usize,
    caption: Option<&'a str>,
    section: &'a str,
}

/// Draft.js blocks as markdown, `header-one`/`-two` at `level` and deeper
/// headers one below. An `atomic` block (an embedded media placeholder)
/// becomes its placed figures, `<sub> Image N`, the caption in italics, then
/// the section; with none placed it is dropped, as are empty blocks.
fn render_blocks(blocks: &[Block], level: usize, figures: &[PlacedFigure]) -> String {
    let top = "#".repeat(level);
    let sub = "#".repeat(level + 1);
    let mut out = String::new();
    let mut ordinal = 0;
    let mut previous_kind = "";
    let mut figure_number = 0;
    for (index, block) in blocks.iter().enumerate() {
        if block.kind == "atomic" {
            for figure in figures.iter().filter(|f| f.block == index) {
                figure_number += 1;
                let mut parts = vec![format!("{sub} Image {figure_number}")];
                if let Some(caption) = figure.caption {
                    parts.push(format!("*{caption}*"));
                }
                parts.push(figure.section.trim().to_string());
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&parts.join("\n\n"));
                previous_kind = "atomic";
                ordinal = 0;
            }
            continue;
        }
        let text = block.text.trim();
        if text.is_empty() {
            continue;
        }
        ordinal = if block.kind == "ordered-list-item" { ordinal + 1 } else { 0 };
        let line = match block.kind.as_str() {
            "header-one" | "header-two" => format!("{top} {text}"),
            "header-three" | "header-four" | "header-five" | "header-six" => format!("{sub} {text}"),
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
