//! The error every YouTube fetch engine returns.
//!
//! One enum for both engines so `youtube::engine::fetch` can decide on a
//! fallback from the variant alone. The Data API key never appears in any
//! `Display`: variants carry a reason, not a request.

/// What [`FetchError::redact`] puts where a key was.
pub const REDACTED: &str = "[redacted]";

/// Why a YouTube fetch failed.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The Data API returned `items: []`: the video is gone.
    #[error("youtube data api: video {0} not found")]
    NotFound(String),
    /// `liveBroadcastContent` is `upcoming` or `live`: no finished recording.
    #[error("youtube data api: video {0} is not a finished video (live or upcoming)")]
    NotLive(String),
    /// The key resolved empty, or the API rejected it.
    #[error("youtube auth: {0}")]
    Auth(String),
    /// The API quota or rate limit is spent.
    #[error("youtube quota: {0}")]
    Quota(String),
    /// A 5xx from the API.
    #[error("youtube service error: {0}")]
    Service(String),
    /// The request never completed (connect, timeout, body read).
    #[error("youtube network error: {0}")]
    Network(String),
    /// A malformed body, an unexpected status, or a failed parse check.
    #[error("youtube response invalid: {0}")]
    Response(String),
    /// No speech in any window.
    #[error("youtube transcript is empty")]
    Empty,
    /// The gemini budget ran out.
    #[error("youtube gemini budget exhausted")]
    Timeout,
    /// The yt-dlp arm's existing errors, wrapped unchanged.
    #[error("yt-dlp: {0:#}")]
    YtDlp(eyre::Report),
}

impl FetchError {
    /// True when no other engine can do better, so the fallback is skipped.
    /// Exactly `NotFound` and `NotLive`.
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::NotFound(_) | Self::NotLive(_))
    }

    /// Replace every occurrence of `secret` in the error's text with
    /// `[redacted]`. The clients call it on every error they return, so a
    /// key the API echoes back inside its own message never reaches a
    /// `Display`, a log line, or a receipt. A blank `secret` is a no-op.
    pub fn redact(self, secret: &str) -> Self {
        let secret = secret.trim();
        if secret.is_empty() {
            return self;
        }
        let scrub = |text: String| text.replace(secret, REDACTED);
        match self {
            Self::NotFound(s) => Self::NotFound(scrub(s)),
            Self::NotLive(s) => Self::NotLive(scrub(s)),
            Self::Auth(s) => Self::Auth(scrub(s)),
            Self::Quota(s) => Self::Quota(scrub(s)),
            Self::Service(s) => Self::Service(scrub(s)),
            Self::Network(s) => Self::Network(scrub(s)),
            Self::Response(s) => Self::Response(scrub(s)),
            other @ (Self::Empty | Self::Timeout | Self::YtDlp(_)) => other,
        }
    }

    /// The error as the pipeline's `eyre::Report`. A yt-dlp arm error comes
    /// back as the original report, so its receipt reason reads as it did
    /// before the engine seam (`yt-dlp metadata failed: ...`).
    pub fn into_report(self) -> eyre::Report {
        match self {
            Self::YtDlp(report) => report,
            other => eyre::Report::new(other),
        }
    }
}

#[cfg(test)]
mod tests;
