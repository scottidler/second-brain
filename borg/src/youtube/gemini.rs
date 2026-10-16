//! Gemini Interactions API transcript client for the `gemini` engine.
//!
//! The video is split into fixed clip windows, each window is one sequential
//! `POST /interactions` call with `processing: static`, and every window's
//! `[HH:MM:SS] text` lines are parsed fail-closed into `(seconds, text)`
//! segments. The key rides the `x-goog-api-key` header, never the URL, and no
//! error `Display` carries it.
//! Design: `docs/design/2026-10-16-youtube-gemini-engine.md`, API Design.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use super::error::FetchError;
use crate::config::YoutubeGeminiConfig;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const API_KEY_HEADER: &str = "x-goog-api-key";

/// Slack either side of a window when checking that a timestamp belongs to it.
const BOUNDS_SLACK_SECS: f64 = 5.0;

/// A window fails when more than this share of its lines go backwards.
const MAX_OUT_OF_ORDER_RATIO: f64 = 0.02;

/// The longest error-body excerpt carried into a `FetchError`.
const ERROR_EXCERPT_CHARS: usize = 300;

/// The marker Gemini puts in a transient HTTP 400 body.
const RETRY_MARKER: &str = "Please retry";

/// The transcription instruction sent with every window.
pub const TRANSCRIPT_PROMPT: &str = "Transcribe the speech in this video verbatim. \
Do not summarize, paraphrase, translate, or omit anything that is said. \
Output one line per spoken segment, in the form `[HH:MM:SS] text`, where the \
timestamp is the absolute time in the video at which the segment starts. \
Output nothing else: no title, no preamble, no commentary, no markdown.";

static TIMESTAMP_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(?:(\d{1,2}):)?(\d{1,2}):(\d{2})\](?: (.*))?$").expect("valid timestamp regex"));

/// One Interactions call's slice of the video, in whole seconds.
/// `clipped` is false only for the single window of a video shorter than the
/// window length: that request omits the offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipWindow {
    pub start_secs: u64,
    pub end_secs: u64,
    pub clipped: bool,
}

/// The transcript a gemini fetch produced.
#[derive(Debug, Clone, PartialEq)]
pub struct GeminiTranscript {
    /// `(start seconds, text)`, non-decreasing, the shape `parse_vtt_segments` yields.
    pub segments: Vec<(f64, String)>,
}

impl GeminiTranscript {
    /// The segment texts joined with newlines: the plain transcript.
    pub fn plain_text(&self) -> String {
        self.segments
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// `[0, w), [w, 2w), ...` up to `duration_secs`. A video no longer than `w`
/// is one unclipped window. A zero duration plans no windows.
pub fn plan_windows(duration_secs: f64, window: Duration) -> Vec<ClipWindow> {
    let total = duration_secs.max(0.0).ceil() as u64;
    let step = window.as_secs().max(1);
    if total == 0 {
        return Vec::new();
    }
    if total <= step {
        return vec![ClipWindow {
            start_secs: 0,
            end_secs: total,
            clipped: false,
        }];
    }
    (0..total)
        .step_by(step as usize)
        .map(|start| ClipWindow {
            start_secs: start,
            end_secs: (start + step).min(total),
            clipped: true,
        })
        .collect()
}

/// The Interactions request body for one window of `video_id`.
pub fn build_request(model: &str, video_id: &str, window: &ClipWindow) -> serde_json::Value {
    let mut processing = json!({"type": "static"});
    if window.clipped {
        processing["start_offset"] = json!(format!("{}s", window.start_secs));
        processing["end_offset"] = json!(format!("{}s", window.end_secs));
    }
    json!({
        "model": model,
        "input": [
            {
                "type": "video",
                "uri": format!("https://www.youtube.com/watch?v={}", urlencoding::encode(video_id)),
                "processing": processing,
            },
            {"type": "text", "text": TRANSCRIPT_PROMPT},
        ],
    })
}

/// What one `completed` Interactions response said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOutput {
    /// The response's `model` field: the model that actually served the call,
    /// which need not be the one requested.
    pub served_model: Option<String>,
    /// `None` when the response carries no model text (no speech).
    pub text: Option<String>,
}

/// The model's text from an Interactions response body:
/// `steps[type == model_output].content[type == text].text`, joined with
/// newlines. The served model is logged at DEBUG.
/// Any status other than `completed` is `Response`.
pub fn extract_text(body: &str) -> Result<ModelOutput, FetchError> {
    let response: InteractionResponse =
        serde_json::from_str(body).map_err(|e| FetchError::Response(format!("gemini interactions body: {e}")))?;
    log::debug!(
        "gemini::extract_text: served model={:?} status={:?} steps={}",
        response.model,
        response.status,
        response.steps.len()
    );
    match response.status.as_deref() {
        Some("completed") => {}
        other => {
            return Err(FetchError::Response(format!(
                "gemini interaction status {other:?}, expected \"completed\""
            )));
        }
    }
    let parts: Vec<String> = response
        .steps
        .into_iter()
        .filter(|step| step.kind == "model_output")
        .flat_map(|step| step.content)
        .filter(|content| content.kind == "text")
        .filter_map(|content| content.text)
        .collect();
    let joined = parts.join("\n");
    Ok(ModelOutput {
        served_model: response.model,
        text: (!joined.trim().is_empty()).then_some(joined),
    })
}

/// Parse one window's text into segments, fail-closed:
/// - every timestamped line is `[MM:SS]`, `[H:MM:SS]` or `[HH:MM:SS]`, normalized to seconds
/// - a non-empty untimestamped line is appended to the previous segment;
///   one before any timestamp cannot be anchored and is `Response`
/// - a timestamp outside `[start - 5s, end + 5s]` is `Response`
/// - a timestamp below the previous one is raised to it; more than 2% of
///   the window's lines going backwards is `Response`
pub fn parse_window(text: &str, window: &ClipWindow) -> Result<Vec<(f64, String)>, FetchError> {
    log::debug!(
        "gemini::parse_window: window={}..{} bytes={}",
        window.start_secs,
        window.end_secs,
        text.len()
    );
    let low = window.start_secs as f64 - BOUNDS_SLACK_SECS;
    let high = window.end_secs as f64 + BOUNDS_SLACK_SECS;
    let mut segments: Vec<(f64, String)> = Vec::new();
    let mut timestamped = 0usize;
    let mut out_of_order = 0usize;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some(caps) = TIMESTAMP_LINE.captures(line) else {
            let Some((_, last)) = segments.last_mut() else {
                return Err(FetchError::Response(format!(
                    "gemini window {}..{}: text before the first timestamp: {:?}",
                    window.start_secs,
                    window.end_secs,
                    excerpt(line)
                )));
            };
            if !last.is_empty() {
                last.push(' ');
            }
            last.push_str(line);
            continue;
        };
        let secs = timestamp_secs(&caps).ok_or_else(|| {
            FetchError::Response(format!(
                "gemini window {}..{}: invalid timestamp in {:?}",
                window.start_secs,
                window.end_secs,
                excerpt(line)
            ))
        })?;
        if secs < low || secs > high {
            return Err(FetchError::Response(format!(
                "gemini window {}..{}: timestamp {secs}s outside the window in {:?}",
                window.start_secs,
                window.end_secs,
                excerpt(line)
            )));
        }
        timestamped += 1;
        let previous = segments.last().map(|(t, _)| *t);
        let at = match previous {
            Some(prev) if secs < prev => {
                out_of_order += 1;
                log::trace!("gemini::parse_window: {secs}s raised to {prev}s");
                prev
            }
            _ => secs,
        };
        let body = caps.get(4).map(|m| m.as_str().trim()).unwrap_or("");
        segments.push((at, body.to_string()));
    }
    if timestamped > 0 && out_of_order as f64 > timestamped as f64 * MAX_OUT_OF_ORDER_RATIO {
        return Err(FetchError::Response(format!(
            "gemini window {}..{}: {out_of_order} of {timestamped} lines out of order (more than 2%)",
            window.start_secs, window.end_secs
        )));
    }
    segments.retain(|(_, text)| !text.is_empty());
    log::debug!(
        "gemini::parse_window: window={}..{} segments={} repaired={out_of_order}",
        window.start_secs,
        window.end_secs,
        segments.len()
    );
    Ok(segments)
}

/// Seconds for a matched timestamp. With an hour field the minutes must be
/// below 60; a bare `[MM:SS]` may run past an hour (`[75:00]`). Seconds are
/// always below 60.
fn timestamp_secs(caps: &regex::Captures<'_>) -> Option<f64> {
    let field = |i: usize| caps.get(i).map(|m| m.as_str().parse::<u64>().ok());
    let hours = field(1).unwrap_or(Some(0))?;
    let minutes = field(2)??;
    let seconds = field(3)??;
    if seconds >= 60 || (caps.get(1).is_some() && minutes >= 60) {
        return None;
    }
    Some((hours * 3600 + minutes * 60 + seconds) as f64)
}

fn excerpt(text: &str) -> String {
    text.chars().take(ERROR_EXCERPT_CHARS).collect()
}

/// Why one HTTP exchange failed: reqwest's timeout is kept apart so the
/// caller can tell a budget expiry from a per-call one.
enum PostError {
    TimedOut,
    Failed(FetchError),
}

/// Why one attempt at a window failed, and whether a retry may help.
struct AttemptError {
    error: FetchError,
    retryable: bool,
}

pub struct GeminiClient {
    api_key: String,
    model: String,
    window: Duration,
    call_timeout: Duration,
    budget: Duration,
    base_url: String,
    http: reqwest::Client,
}

impl GeminiClient {
    /// A client holding `api_key` (the key itself, not the env var name) and
    /// the `youtube.gemini` model, window, per-call timeout and budget.
    /// An empty key is legal here and fails every fetch with `Auth` before any request.
    pub fn new(api_key: String, config: &YoutubeGeminiConfig) -> Result<Self, FetchError> {
        let http = vault::http::client(config.timeout)
            .map_err(|e| FetchError::Network(format!("cannot build the HTTP client: {e:#}")))?;
        Ok(Self {
            api_key,
            model: config.model.clone(),
            window: config.window,
            call_timeout: config.timeout,
            budget: config.budget,
            base_url: DEFAULT_BASE_URL.to_string(),
            http,
        })
    }

    /// Resolve the key named by `youtube.gemini.api-key` (env var name or
    /// secret file). A key that does not resolve is `Auth` at fetch time.
    pub fn from_config(config: &YoutubeGeminiConfig) -> Result<Self, FetchError> {
        let key = vault::config::resolve_secret(&config.api_key).unwrap_or_default();
        Self::new(key, config)
    }

    /// Point the client at a local stub server (`TranscriptionClient::with_groq_url` precedent).
    #[cfg(test)]
    pub fn with_base_url(api_key: String, config: &YoutubeGeminiConfig, base_url: &str) -> Result<Self, FetchError> {
        let mut client = Self::new(api_key, config)?;
        client.base_url = base_url.trim_end_matches('/').to_string();
        Ok(client)
    }

    /// The timestamped transcript of `video_id`, `duration_secs` long.
    ///
    /// `started` is when the whole fetch began (the first Data API request);
    /// every window and retry must finish within `budget` of it, else `Timeout`.
    /// Each call is also bounded by the per-call timeout. Windows run
    /// sequentially. Zero segments across every window is `Empty`.
    pub async fn fetch_transcript(
        &self,
        video_id: &str,
        duration_secs: f64,
        started: Instant,
    ) -> Result<GeminiTranscript, FetchError> {
        self.fetch_transcript_unredacted(video_id, duration_secs, started)
            .await
            .map_err(|e| e.redact(&self.api_key))
    }

    async fn fetch_transcript_unredacted(
        &self,
        video_id: &str,
        duration_secs: f64,
        started: Instant,
    ) -> Result<GeminiTranscript, FetchError> {
        log::debug!(
            "gemini::fetch_transcript: video_id={video_id} duration_secs={duration_secs} model={} window={:?} timeout={:?} budget={:?}",
            self.model,
            self.window,
            self.call_timeout,
            self.budget
        );
        if self.api_key.trim().is_empty() {
            return Err(FetchError::Auth("the gemini api key resolved empty".to_string()));
        }
        let windows = plan_windows(duration_secs, self.window);
        if windows.is_empty() {
            return Err(FetchError::Response(format!(
                "video {video_id}: duration {duration_secs}s plans no transcript windows"
            )));
        }
        let deadline = started + self.budget;
        let mut segments = Vec::new();
        for window in &windows {
            let mut window_segments = self.fetch_window(video_id, window, deadline).await?;
            segments.append(&mut window_segments);
        }
        if segments.is_empty() {
            log::warn!(
                "gemini::fetch_transcript: video_id={video_id} no speech in any of {} windows",
                windows.len()
            );
            return Err(FetchError::Empty);
        }
        log::debug!(
            "gemini::fetch_transcript: video_id={video_id} windows={} segments={} elapsed={:?}",
            windows.len(),
            segments.len(),
            started.elapsed()
        );
        Ok(GeminiTranscript { segments })
    }

    /// One window: an attempt plus at most one retry, both inside the deadline.
    async fn fetch_window(
        &self,
        video_id: &str,
        window: &ClipWindow,
        deadline: Instant,
    ) -> Result<Vec<(f64, String)>, FetchError> {
        let request = build_request(&self.model, video_id, window);
        let mut retried = false;
        loop {
            log::debug!(
                "gemini::fetch_window: video_id={video_id} window={}..{} clipped={} retry={retried}",
                window.start_secs,
                window.end_secs,
                window.clipped
            );
            match self.attempt(&request, window, deadline).await {
                Ok(segments) => return Ok(segments),
                Err(AttemptError { error, retryable }) if retryable && !retried => {
                    log::warn!(
                        "gemini: video {video_id} window {}..{} failed, retrying once: {error}",
                        window.start_secs,
                        window.end_secs
                    );
                    retried = true;
                }
                Err(AttemptError { error, .. }) => {
                    log::warn!(
                        "gemini: video {video_id} window {}..{} failed: {error}",
                        window.start_secs,
                        window.end_secs
                    );
                    return Err(error);
                }
            }
        }
    }

    async fn attempt(
        &self,
        request: &serde_json::Value,
        window: &ClipWindow,
        deadline: Instant,
    ) -> Result<Vec<(f64, String)>, AttemptError> {
        let fatal = |error| AttemptError {
            error,
            retryable: false,
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(fatal(FetchError::Timeout));
        }
        let budget_bound = remaining < self.call_timeout;
        let limit = remaining.min(self.call_timeout);
        // reqwest's own timeout (the per-call value, a backstop) and the tokio
        // bound can expire within a millisecond of each other; whichever fires,
        // a budget-bound attempt that times out is `Timeout`.
        let (status, body) = match tokio::time::timeout(limit, self.post(request)).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(PostError::Failed(e))) => return Err(fatal(e)),
            Ok(Err(PostError::TimedOut)) | Err(_) if budget_bound => return Err(fatal(FetchError::Timeout)),
            Ok(Err(PostError::TimedOut)) | Err(_) => return Err(fatal(self.call_timed_out())),
        };
        if !(200..300).contains(&status) {
            // Redacted here too: fetch_window WARNs this error directly.
            let AttemptError { error, retryable } = classify_failure(status, &body);
            return Err(AttemptError {
                error: error.redact(&self.api_key),
                retryable,
            });
        }
        let parsed = match extract_text(&body)?.text {
            Some(text) => parse_window(&text, window),
            None => {
                log::debug!(
                    "gemini::attempt: window={}..{} completed with no text (no speech)",
                    window.start_secs,
                    window.end_secs
                );
                Ok(Vec::new())
            }
        };
        parsed.map_err(|error| AttemptError { error, retryable: true })
    }

    async fn post(&self, request: &serde_json::Value) -> Result<(u16, String), PostError> {
        let url = format!("{}/interactions", self.base_url);
        let response = self
            .http
            .post(&url)
            .header(API_KEY_HEADER, &self.api_key)
            .json(request)
            .send()
            .await
            .map_err(|e| self.transport_error(e))?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|e| self.transport_error(e))?;
        Ok((status, body))
    }

    fn transport_error(&self, e: reqwest::Error) -> PostError {
        if e.is_timeout() {
            return PostError::TimedOut;
        }
        PostError::Failed(FetchError::Network(e.without_url().to_string()))
    }

    fn call_timed_out(&self) -> FetchError {
        FetchError::Network(format!(
            "gemini call exceeded the per-call timeout of {}",
            humantime::format_duration(self.call_timeout)
        ))
    }
}

/// A body that does not parse as the success shape is `Response` and retryable.
impl From<FetchError> for AttemptError {
    fn from(error: FetchError) -> Self {
        let retryable = matches!(error, FetchError::Response(_));
        Self { error, retryable }
    }
}

/// Map a non-2xx response to its `FetchError` and whether one retry is allowed:
/// 400 carrying `Please retry`, 429 and 5xx are; key problems and other 4xx are not.
fn classify_failure(status: u16, body: &str) -> AttemptError {
    let parsed: Option<ErrorEnvelope> = serde_json::from_str(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|e| e.error.message.clone())
        .unwrap_or_else(|| excerpt(body.trim()));
    let api_status = parsed.as_ref().and_then(|e| e.error.status.clone()).unwrap_or_default();
    let detail = format!("HTTP {status} {api_status}: {}", excerpt(&message));
    let key_rejected = body.contains("API_KEY_INVALID") || message.contains("API key not valid");
    let (error, retryable) = match status {
        400 if key_rejected => (FetchError::Auth(detail), false),
        400 if body.contains(RETRY_MARKER) => (FetchError::Response(detail), true),
        401 | 403 => (FetchError::Auth(detail), false),
        429 => (FetchError::Quota(detail), true),
        500..=599 => (FetchError::Service(detail), true),
        _ => (FetchError::Response(detail), false),
    };
    AttemptError { error, retryable }
}

#[derive(Deserialize)]
struct InteractionResponse {
    status: Option<String>,
    model: Option<String>,
    #[serde(default)]
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    content: Vec<Content>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "type", default)]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: Option<String>,
    status: Option<String>,
}

#[cfg(test)]
mod tests;
