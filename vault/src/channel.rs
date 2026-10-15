//! Wire contract for YouTube channel search (`GET /channels/search`).
//!
//! Computed in ONE place, `borg::youtube::search_channel` (one `yt-dlp
//! --flat-playlist -J` call), and rendered verbatim by every client (`sb borg
//! channel-search`, oracle's `channel_search`). Shared here so the daemon and
//! its clients cannot disagree on the shape. Design:
//! `docs/design/2026-10-14-note-sources-intent-tools-channels.md`, Part 4.

use serde::{Deserialize, Serialize};

/// One video a channel search returned. Only `watch?v=` entries become hits;
/// playlists and other tab entries the search page also lists are dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ChannelHit {
    /// YouTube video id (`FUg1y-yv6cs`).
    pub id: String,
    pub title: String,
    /// The canonical `https://www.youtube.com/watch?v=<id>` URL.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_count: Option<u64>,
}

/// A successful channel search: the handle and query as asked, and the hits
/// in the order YouTube ranked them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ChannelSearch {
    pub handle: String,
    pub query: String,
    pub hits: Vec<ChannelHit>,
}

/// The error body of a failed channel search (HTTP 400 for bad input, 502 for
/// a yt-dlp failure or timeout). `stderr-tail` is the end of yt-dlp's stderr
/// when yt-dlp ran to exit; `None` when it never started, timed out, or exited
/// 0 with output that would not parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ChannelSearchFailure {
    pub error: String,
    pub stderr_tail: Option<String>,
}

#[cfg(test)]
mod tests;
