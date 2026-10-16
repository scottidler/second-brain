//! YouTube Data API v3 metadata client for the `gemini` engine.
//!
//! `videos.list` fills a [`VideoMetadata`]; `channels.list` adds the channel
//! handle. The key rides the `x-goog-api-key` header, never the URL, and no
//! error `Display` carries it.
//! Design: `docs/design/2026-10-16-youtube-gemini-engine.md`, API Design.

use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Deserialize;

use super::VideoMetadata;
use super::error::FetchError;
use super::search::HANDLE_PATTERN;
use crate::config::YoutubeDataApiConfig;

const DEFAULT_BASE_URL: &str = "https://www.googleapis.com/youtube/v3";
const API_KEY_HEADER: &str = "x-goog-api-key";

static HANDLE_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(HANDLE_PATTERN).expect("valid regex"));

/// Reasons Google gives a 403/429 when the quota or rate is spent. Any other
/// 403 reason (`forbidden`, `accessNotConfigured`, `ipRefererBlocked`) is a key problem.
const QUOTA_REASONS: &[&str] = &[
    "quotaExceeded",
    "rateLimitExceeded",
    "dailyLimitExceeded",
    "userRateLimitExceeded",
];

pub struct DataApiClient {
    api_key: String,
    base_url: String,
    http: reqwest::Client,
}

impl DataApiClient {
    /// A client holding `api_key` (the key itself, not the env var name).
    /// An empty key is legal here and fails every fetch with `Auth` before any request.
    pub fn new(api_key: String, timeout: Duration) -> Result<Self, FetchError> {
        let http = vault::http::client(timeout)
            .map_err(|e| FetchError::Network(format!("cannot build the HTTP client: {e:#}")))?;
        Ok(Self {
            api_key,
            base_url: DEFAULT_BASE_URL.to_string(),
            http,
        })
    }

    /// Resolve the key named by `youtube.data-api.api-key` (env var name or
    /// secret file), with `youtube.data-api.timeout` per request. A key that
    /// does not resolve is `Auth`.
    pub fn from_config(config: &YoutubeDataApiConfig) -> Result<Self, FetchError> {
        let key = vault::config::resolve_secret(&config.api_key).unwrap_or_default();
        Self::new(key, config.timeout)
    }

    /// Point the client at a local stub server (`TranscriptionClient::with_groq_url` precedent).
    #[cfg(test)]
    pub fn with_base_url(api_key: String, base_url: &str, timeout: Duration) -> Result<Self, FetchError> {
        let mut client = Self::new(api_key, timeout)?;
        client.base_url = base_url.trim_end_matches('/').to_string();
        Ok(client)
    }

    /// Metadata for `video_id`. `channels.list` failing leaves `channel_handle` `None` (WARN).
    /// Every error is redacted of the key.
    pub async fn fetch_metadata(&self, video_id: &str) -> Result<VideoMetadata, FetchError> {
        self.fetch_metadata_unredacted(video_id)
            .await
            .map_err(|e| e.redact(&self.api_key))
    }

    async fn fetch_metadata_unredacted(&self, video_id: &str) -> Result<VideoMetadata, FetchError> {
        log::debug!("dataapi::fetch_metadata: video_id={video_id}");
        if self.api_key.trim().is_empty() {
            return Err(FetchError::Auth("the youtube data api key resolved empty".to_string()));
        }
        let url = format!(
            "{}/videos?part=snippet,contentDetails&id={}",
            self.base_url,
            urlencoding::encode(video_id)
        );
        let body = self.get(&url).await?;
        let list: VideoList =
            serde_json::from_str(&body).map_err(|e| FetchError::Response(format!("videos.list body: {e}")))?;
        let item = list
            .items
            .into_iter()
            .next()
            .ok_or_else(|| FetchError::NotFound(video_id.to_string()))?;
        // Map first: a NotLive or malformed video never costs a channels.list call.
        let mut metadata = map_video(video_id, item)?;
        if let Some(id) = &metadata.channel_id {
            match self.fetch_channel_handle(id).await {
                Ok(handle) => metadata.channel_handle = handle,
                Err(e) => {
                    log::warn!("dataapi: channels.list failed for {id} (video {video_id}), no channel handle: {e}")
                }
            }
        }
        log::debug!(
            "dataapi::fetch_metadata: video_id={video_id} duration_secs={} handle={:?}",
            metadata.duration_secs,
            metadata.channel_handle
        );
        Ok(metadata)
    }

    async fn fetch_channel_handle(&self, channel_id: &str) -> Result<Option<String>, FetchError> {
        let url = format!(
            "{}/channels?part=snippet&id={}",
            self.base_url,
            urlencoding::encode(channel_id)
        );
        let body = self.get(&url).await?;
        let list: ChannelList =
            serde_json::from_str(&body).map_err(|e| FetchError::Response(format!("channels.list body: {e}")))?;
        let custom_url = list
            .items
            .into_iter()
            .next()
            .and_then(|c| c.snippet)
            .and_then(|s| s.custom_url);
        Ok(custom_url.filter(|u| HANDLE_REGEX.is_match(u)))
    }

    async fn get(&self, url: &str) -> Result<String, FetchError> {
        let response = self
            .http
            .get(url)
            .header(API_KEY_HEADER, &self.api_key)
            .send()
            .await
            .map_err(|e| FetchError::Network(e.without_url().to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| FetchError::Network(e.without_url().to_string()))?;
        if status.is_success() {
            Ok(body)
        } else {
            // Redacted here too: the channels.list WARN logs this error directly.
            Err(classify_failure(status.as_u16(), &body).redact(&self.api_key))
        }
    }
}

/// Map a non-2xx response to its `FetchError`. The reason is the API's own
/// `error.errors[0].reason` / `error.message`; the key is never in either.
fn classify_failure(status: u16, body: &str) -> FetchError {
    let parsed: Option<ErrorEnvelope> = serde_json::from_str(body).ok();
    let reason = parsed
        .as_ref()
        .and_then(|e| e.error.errors.first())
        .and_then(|d| d.reason.clone())
        .unwrap_or_default();
    let message = parsed
        .as_ref()
        .and_then(|e| e.error.message.clone())
        .unwrap_or_else(|| "no error body".to_string());
    let detail = format!("HTTP {status} {reason}: {message}");
    match status {
        429 => FetchError::Quota(detail),
        403 if QUOTA_REASONS.contains(&reason.as_str()) => FetchError::Quota(detail),
        401 | 403 => FetchError::Auth(detail),
        400 if reason == "keyInvalid" || message.contains("API key not valid") => FetchError::Auth(detail),
        500..=599 => FetchError::Service(detail),
        _ => FetchError::Response(detail),
    }
}

fn map_video(video_id: &str, item: VideoItem) -> Result<VideoMetadata, FetchError> {
    let snippet = item.snippet;
    if matches!(snippet.live_broadcast_content.as_deref(), Some("upcoming" | "live")) {
        return Err(FetchError::NotLive(video_id.to_string()));
    }
    let raw_duration = item
        .content_details
        .and_then(|c| c.duration)
        .ok_or_else(|| FetchError::Response(format!("video {video_id}: no contentDetails.duration")))?;
    let duration = parse_iso8601_duration(&raw_duration)
        .ok_or_else(|| FetchError::Response(format!("video {video_id}: unparseable duration {raw_duration:?}")))?;
    if duration == 0 {
        return Err(FetchError::Response(format!(
            "video {video_id}: zero duration on a finished video"
        )));
    }
    let published_at = snippet
        .published_at
        .as_deref()
        .map(|raw| {
            DateTime::parse_from_rfc3339(raw)
                .map(|t| t.with_timezone(&Utc).format("%Y-%m-%d").to_string())
                .map_err(|e| FetchError::Response(format!("video {video_id}: publishedAt {raw:?}: {e}")))
        })
        .transpose()?;
    let channel_id = snippet.channel_id.filter(|s| !s.is_empty());
    Ok(VideoMetadata {
        title: snippet.title.unwrap_or_default(),
        uploader: snippet.channel_title.unwrap_or_default(),
        duration_secs: duration as f64,
        description: snippet.description.unwrap_or_default(),
        tags: snippet.tags.unwrap_or_default(),
        channel_url: channel_id
            .as_ref()
            .map(|id| format!("https://www.youtube.com/channel/{id}")),
        channel_id,
        channel_handle: None,
        published_at,
    })
}

/// `PT8M32S` -> 512, `P1DT2H` -> 93600, `P0D` -> 0. Any of D/H/M/S may be
/// omitted, at least one must be present, and nothing else (years, months,
/// weeks, fractions) is accepted.
pub fn parse_iso8601_duration(raw: &str) -> Option<u64> {
    let rest = raw.strip_prefix('P')?;
    let (date, time) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };
    if time == Some("") {
        return None;
    }
    let mut total: u64 = 0;
    let mut seen = false;
    for (part, units) in [
        (date, &[('D', 86_400u64)][..]),
        (time.unwrap_or(""), &[('H', 3600), ('M', 60), ('S', 1)][..]),
    ] {
        let mut digits = String::new();
        let mut next_unit = 0;
        for ch in part.chars() {
            if ch.is_ascii_digit() {
                digits.push(ch);
                continue;
            }
            let idx = units[next_unit..].iter().position(|(u, _)| *u == ch)? + next_unit;
            let n: u64 = digits.parse().ok()?;
            total = total.checked_add(n.checked_mul(units[idx].1)?)?;
            digits.clear();
            next_unit = idx + 1;
            seen = true;
        }
        if !digits.is_empty() {
            return None;
        }
    }
    seen.then_some(total)
}

/// `items` is required: only an explicit `items: []` means the video is gone
/// (`NotFound`, permanent). A 200 body without it (`{}`) is malformed, so
/// `Response`, which leaves the fallback free to run.
#[derive(Deserialize)]
struct VideoList {
    items: Vec<VideoItem>,
}

#[derive(Deserialize)]
struct VideoItem {
    snippet: VideoSnippet,
    #[serde(rename = "contentDetails")]
    content_details: Option<ContentDetails>,
}

#[derive(Deserialize)]
struct VideoSnippet {
    #[serde(rename = "publishedAt")]
    published_at: Option<String>,
    #[serde(rename = "channelId")]
    channel_id: Option<String>,
    title: Option<String>,
    description: Option<String>,
    #[serde(rename = "channelTitle")]
    channel_title: Option<String>,
    tags: Option<Vec<String>>,
    #[serde(rename = "liveBroadcastContent")]
    live_broadcast_content: Option<String>,
}

#[derive(Deserialize)]
struct ContentDetails {
    duration: Option<String>,
}

#[derive(Deserialize)]
struct ChannelList {
    #[serde(default)]
    items: Vec<ChannelItem>,
}

#[derive(Deserialize)]
struct ChannelItem {
    snippet: Option<ChannelSnippet>,
}

#[derive(Deserialize)]
struct ChannelSnippet {
    #[serde(rename = "customUrl")]
    custom_url: Option<String>,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: Option<String>,
    #[serde(default)]
    errors: Vec<ErrorDetail>,
}

#[derive(Deserialize)]
struct ErrorDetail {
    reason: Option<String>,
}

#[cfg(test)]
mod tests;
