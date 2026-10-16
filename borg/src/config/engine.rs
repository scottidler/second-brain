//! The `youtube.engine` / `fallback` config and its validation: the
//! `yt-dlp | gemini` enum, the `youtube.gemini:` and `youtube.data-api:`
//! blocks, the structural rules `Config::validate` runs on every `sb borg`
//! command, and the env-dependent key check run only at daemon start.
//! Design: `docs/design/2026-10-16-youtube-gemini-engine.md`.

use eyre::Result;
use serde::{Deserialize, Deserializer, Serialize};
use std::time::Duration;

use super::{Config, deserialize_humantime, resolve_secret, serialize_humantime};

/// Which implementation fetches a YouTube video's metadata and transcript.
/// `YtDlp` is today's behavior; `Gemini` reads metadata from the YouTube Data
/// API and the timestamped transcript from Gemini, with no yt-dlp call.
/// Design: `docs/design/2026-10-16-youtube-gemini-engine.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum YoutubeEngine {
    #[default]
    YtDlp,
    Gemini,
}

impl YoutubeEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::YtDlp => "yt-dlp",
            Self::Gemini => "gemini",
        }
    }
}

pub const DEFAULT_GEMINI_MODEL: &str = "gemini-3.8-flash";
pub const DEFAULT_GEMINI_API_KEY: &str = "GEMINI_API_KEY";
pub const DEFAULT_YOUTUBE_API_KEY: &str = "YOUTUBE_API_KEY";
pub const DEFAULT_GEMINI_WINDOW: Duration = Duration::from_secs(30 * 60);
pub const DEFAULT_GEMINI_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub const DEFAULT_GEMINI_BUDGET: Duration = Duration::from_secs(15 * 60);
pub const MIN_GEMINI_WINDOW: Duration = Duration::from_secs(5 * 60);
pub const MIN_GEMINI_TIMEOUT: Duration = Duration::from_secs(30);
/// One Data API request (`videos.list` or `channels.list`): normally sub-second.
pub const DEFAULT_DATA_API_TIMEOUT: Duration = Duration::from_secs(30);
/// Below 1s a slow-but-healthy Data API reply would read as a network error.
pub const MIN_DATA_API_TIMEOUT: Duration = Duration::from_secs(1);

/// The `youtube.gemini:` block: the Interactions API transcript client.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct YoutubeGeminiConfig {
    pub model: String,
    /// Env var NAME (or secret file path) holding the key, never the key.
    pub api_key: String,
    /// Clip length per Interactions call; at least 5m.
    #[serde(
        deserialize_with = "deserialize_gemini_window",
        serialize_with = "serialize_humantime"
    )]
    pub window: Duration,
    /// Deadline for one call; at least 30s.
    #[serde(
        deserialize_with = "deserialize_gemini_timeout",
        serialize_with = "serialize_humantime"
    )]
    pub timeout: Duration,
    /// Deadline for all windows plus retries; at least `timeout`.
    #[serde(
        deserialize_with = "deserialize_gemini_budget",
        serialize_with = "serialize_humantime"
    )]
    pub budget: Duration,
}

impl Default for YoutubeGeminiConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_GEMINI_MODEL.to_string(),
            api_key: DEFAULT_GEMINI_API_KEY.to_string(),
            window: DEFAULT_GEMINI_WINDOW,
            timeout: DEFAULT_GEMINI_TIMEOUT,
            budget: DEFAULT_GEMINI_BUDGET,
        }
    }
}

/// The `youtube.data-api:` block: the YouTube Data API metadata client.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct YoutubeDataApiConfig {
    /// Env var NAME (or secret file path) holding the key, never the key.
    pub api_key: String,
    /// Deadline for one Data API request; at least 1s. The gemini budget
    /// also bounds it: each call gets the smaller of the two.
    #[serde(
        deserialize_with = "deserialize_data_api_timeout",
        serialize_with = "serialize_humantime"
    )]
    pub timeout: Duration,
}

impl Default for YoutubeDataApiConfig {
    fn default() -> Self {
        Self {
            api_key: DEFAULT_YOUTUBE_API_KEY.to_string(),
            timeout: DEFAULT_DATA_API_TIMEOUT,
        }
    }
}

fn deserialize_gemini_window<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Duration, D::Error> {
    deserialize_humantime(deserializer, "window")
}

fn deserialize_gemini_timeout<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Duration, D::Error> {
    deserialize_humantime(deserializer, "timeout")
}

fn deserialize_gemini_budget<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Duration, D::Error> {
    deserialize_humantime(deserializer, "budget")
}

fn deserialize_data_api_timeout<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Duration, D::Error> {
    deserialize_humantime(deserializer, "timeout")
}

impl Config {
    /// `youtube.engine` / `fallback` structural rules. Hard errors name the field.
    pub(super) fn validate_youtube_engine(&self) -> Result<()> {
        let yt = &self.youtube;
        if yt.fallback == Some(yt.engine) {
            eyre::bail!(
                "youtube.fallback must differ from youtube.engine (both are `{}`); remove `fallback` or pick the other engine",
                yt.engine.as_str()
            );
        }
        if yt.gemini.window < MIN_GEMINI_WINDOW {
            eyre::bail!(
                "youtube.gemini.window must be at least {} (got {})",
                humantime::format_duration(MIN_GEMINI_WINDOW),
                humantime::format_duration(yt.gemini.window)
            );
        }
        if yt.gemini.timeout < MIN_GEMINI_TIMEOUT {
            eyre::bail!(
                "youtube.gemini.timeout must be at least {} (got {})",
                humantime::format_duration(MIN_GEMINI_TIMEOUT),
                humantime::format_duration(yt.gemini.timeout)
            );
        }
        if yt.gemini.budget < yt.gemini.timeout {
            eyre::bail!(
                "youtube.gemini.budget ({}) must be at least youtube.gemini.timeout ({})",
                humantime::format_duration(yt.gemini.budget),
                humantime::format_duration(yt.gemini.timeout)
            );
        }
        if yt.data_api.timeout < MIN_DATA_API_TIMEOUT {
            eyre::bail!(
                "youtube.data-api.timeout must be at least {} (got {})",
                humantime::format_duration(MIN_DATA_API_TIMEOUT),
                humantime::format_duration(yt.data_api.timeout)
            );
        }
        Ok(())
    }

    /// Checks that read the daemon's environment, run once by
    /// `borg::serve_init` before anything binds. Only the daemon gets
    /// `borg.env`, so only the daemon can answer whether the gemini keys
    /// resolve. A gemini engine with no fallback and an empty key is a hard
    /// error; a gemini engine that can still fall back, or a gemini fallback,
    /// with an empty key only WARNs: the other engine still serves ingest.
    pub fn validate_daemon_start(&self) -> Result<()> {
        let yt = &self.youtube;
        let uses_gemini = yt.engine == YoutubeEngine::Gemini || yt.fallback == Some(YoutubeEngine::Gemini);
        if !uses_gemini {
            return Ok(());
        }
        let empty: Vec<String> = [
            ("youtube.gemini.api-key", &yt.gemini.api_key),
            ("youtube.data-api.api-key", &yt.data_api.api_key),
        ]
        .into_iter()
        .filter(|(_, name)| !secret_resolves(name))
        .map(|(field, name)| format!("{field} ({name})"))
        .collect();
        if empty.is_empty() {
            return Ok(());
        }
        let list = empty.join(", ");
        if yt.engine == YoutubeEngine::Gemini && yt.fallback.is_none() {
            eyre::bail!(
                "youtube.engine is gemini with no fallback, but these keys resolve empty: {list}; \
                 set the env vars (the daemon gets them from borg.env) or configure `fallback`"
            );
        }
        log::warn!(
            "youtube: gemini is configured but these keys resolve empty: {list}; \
             every gemini fetch will fail with an auth error until they are set"
        );
        Ok(())
    }
}

/// True when `name` (env var name or secret file path) yields a non-empty value.
fn secret_resolves(name: &str) -> bool {
    resolve_secret(name).is_ok_and(|v| !v.is_empty())
}
