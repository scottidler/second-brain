//! Hacker News threads fetched through the HN Algolia items API.
//!
//! The generic thread fetch (`fabric -u`, which reads through Jina) returns
//! the rendered HN page: nav bar, vote arrows, `[–]` collapse toggles
//! (`[[–]](javascript:void(0))`), `parent | next | prev | root` links, user
//! and `hide`/`favorite` links, spacer gifs, the footer. All of it reached the
//! thread distiller and the note's verbatim `## Transcript`. The items API
//! returns the story and its whole comment tree as JSON, so the thread is
//! rendered from content alone and no page chrome can get in.

use eyre::{Context, Result};
use serde::Deserialize;
use std::time::Duration;

/// Items endpoint of the HN Algolia API; the item id is appended.
pub const ITEMS_API_BASE: &str = "https://hn.algolia.com/api/v1/items/";
/// The `creator` an HN thread note carries.
pub const CREATOR: &str = "Hacker News";

/// One story or comment in the API's tree. A deleted comment has no author
/// and no text but may still carry live replies.
#[derive(Debug, Deserialize)]
pub struct Item {
    pub id: u64,
    pub author: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
    /// HN's comment HTML (`<p>`-separated, entity-escaped).
    pub text: Option<String>,
    pub points: Option<i64>,
    #[serde(default)]
    pub children: Vec<Item>,
}

/// The item id of an HN discussion URL (`news.ycombinator.com/item?id=N`).
pub fn item_id(url: &str) -> Option<u64> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if !host.ends_with("news.ycombinator.com") || parsed.path() != "/item" {
        return None;
    }
    parsed
        .query_pairs()
        .find(|(key, _)| key == "id")
        .and_then(|(_, value)| value.parse().ok())
}

/// Fetch item `id` from `api_base` and render it. Returns `(title, markdown)`.
pub async fn fetch_thread(api_base: &str, id: u64, timeout_secs: u64) -> Result<(String, String)> {
    log::debug!("hackernews::fetch_thread: api_base={api_base} id={id} timeout_secs={timeout_secs}");
    let client = vault::http::client(Duration::from_secs(timeout_secs)).context("build HN API client")?;
    let response = client
        .get(format!("{api_base}{id}"))
        .send()
        .await
        .with_context(|| format!("reach HN items API for {id}"))?;
    if !response.status().is_success() {
        eyre::bail!("HN items API returned status {} for item {id}", response.status());
    }
    let item: Item = response.json().await.with_context(|| format!("parse HN item {id}"))?;
    let rendered = render_thread(&item)?;
    log::debug!(
        "hackernews::fetch_thread: id={id} title={:?} markdown_len={}",
        rendered.0,
        rendered.1.len()
    );
    Ok(rendered)
}

/// Render a story and its comment tree as `(title, markdown)`. The markdown
/// opens with the `Title:` / `URL Source:` lines the fetchers emit, so
/// `extract_article_title` reads it the same way. Comments follow depth-first
/// in thread order, each headed by its author and the author it answers.
pub fn render_thread(item: &Item) -> Result<(String, String)> {
    let title = item
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("Hacker News item {}", item.id));
    let mut out = format!(
        "Title: {title}\nURL Source: https://news.ycombinator.com/item?id={}\n\n# {title}\n\n",
        item.id
    );
    let author = item.author.as_deref().unwrap_or("[deleted]");
    match item.points {
        Some(points) => out.push_str(&format!("Posted by {author} ({points} points)\n")),
        None => out.push_str(&format!("Posted by {author}\n")),
    }
    if let Some(link) = item.url.as_deref().filter(|u| !u.is_empty()) {
        out.push_str(&format!("Link: {link}\n"));
    }
    out.push('\n');
    if let Some(text) = item.text.as_deref() {
        out.push_str(&html_to_markdown(text)?);
        out.push_str("\n\n");
    }
    if !item.children.is_empty() {
        out.push_str("## Comments\n\n");
        for child in &item.children {
            render_comment(child, author, &mut out)?;
        }
    }
    Ok((title, out.trim_end().to_string() + "\n"))
}

fn render_comment(item: &Item, parent_author: &str, out: &mut String) -> Result<()> {
    let author = item.author.as_deref().unwrap_or("[deleted]");
    if let Some(text) = item.text.as_deref().filter(|t| !t.trim().is_empty()) {
        out.push_str(&format!("**{author}** (replying to {parent_author}):\n\n"));
        out.push_str(&html_to_markdown(text)?);
        out.push_str("\n\n");
    }
    for child in &item.children {
        render_comment(child, author, out)?;
    }
    Ok(())
}

fn html_to_markdown(html: &str) -> Result<String> {
    let markdown = htmd::convert(html).map_err(|e| eyre::eyre!("convert HN comment HTML: {e}"))?;
    Ok(markdown.trim().to_string())
}

#[cfg(test)]
mod tests;
