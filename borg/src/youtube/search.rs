//! Channel search: the videos a YouTube channel's own search page returns for
//! a query, from one `yt-dlp --flat-playlist -J` call on
//! `https://www.youtube.com/@<handle>/search?query=<q>`.
//!
//! Served to remote clients by the bearer-gated `GET /channels/search` route
//! and run in-process by `sb borg channel-search`. The wire shape is
//! `vault::channel`. Design:
//! `docs/design/2026-10-14-note-sources-intent-tools-channels.md`, Part 4.
//!
//! The handle and query reach yt-dlp as ONE URL argument through
//! `vault::process::run` (no shell). The handle must match
//! [`HANDLE_PATTERN`] and is checked before anything is spawned; the query is
//! percent-encoded into the URL.

use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use vault::channel::ChannelHit;
use vault::process::{self, Outcome};

/// What a channel handle must look like (`@letsgetrusty`).
pub const HANDLE_PATTERN: &str = r"^@[A-Za-z0-9._-]+$";

/// Bound on one yt-dlp search call. Below the daemon client's 10 s default
/// (`vault::daemon::DEFAULT_REQUEST_TIMEOUT`) so a slow search reaches a
/// client as a 502 with a reason, not as a client-side timeout.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(8);

/// `--limit` when the caller names none.
pub const DEFAULT_LIMIT: usize = 10;

/// Largest accepted `--limit`. yt-dlp walks the search page's continuation
/// pages for larger values, which a single 8 s call cannot be counted on to
/// finish.
pub const MAX_LIMIT: usize = 50;

/// How much of yt-dlp's stderr a failure carries: the last lines, then at
/// most this many characters of them.
const STDERR_TAIL_LINES: usize = 10;
const STDERR_TAIL_MAX_CHARS: usize = 2000;

/// The marker that separates a video entry from the playlists (and other
/// tab entries) the channel search page also lists.
const VIDEO_URL_MARKER: &str = "watch?v=";

static HANDLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(HANDLE_PATTERN).expect("HANDLE_PATTERN compiles"));

/// Why a channel search returned no hits. The `Invalid*` variants are the
/// caller's input (HTTP 400, nothing spawned); the rest are yt-dlp (HTTP 502).
#[derive(Debug, thiserror::Error)]
pub enum ChannelSearchError {
    #[error("invalid channel handle {handle:?}: must match {HANDLE_PATTERN}")]
    InvalidHandle { handle: String },
    #[error("invalid limit {limit}: must be 1 to {MAX_LIMIT}")]
    InvalidLimit { limit: usize },
    #[error("empty search query")]
    EmptyQuery,
    #[error("yt-dlp could not run: {reason}")]
    Spawn { reason: String },
    #[error("yt-dlp timed out after {}s", after.as_secs())]
    TimedOut { after: Duration },
    #[error("yt-dlp failed ({status}): {stderr_tail}")]
    Failed { status: String, stderr_tail: String },
    #[error("yt-dlp -J output did not parse: {reason}")]
    Parse { reason: String },
}

impl ChannelSearchError {
    /// True when the caller's input was refused before yt-dlp ran.
    pub fn is_invalid_input(&self) -> bool {
        matches!(
            self,
            Self::InvalidHandle { .. } | Self::InvalidLimit { .. } | Self::EmptyQuery
        )
    }

    /// The end of yt-dlp's stderr, when yt-dlp ran to a non-zero exit.
    pub fn stderr_tail(&self) -> Option<&str> {
        match self {
            Self::Failed { stderr_tail, .. } => Some(stderr_tail),
            _ => None,
        }
    }
}

/// Refuse a handle that does not match [`HANDLE_PATTERN`].
pub fn validate_handle(handle: &str) -> Result<(), ChannelSearchError> {
    if HANDLE_RE.is_match(handle) {
        Ok(())
    } else {
        Err(ChannelSearchError::InvalidHandle {
            handle: handle.to_string(),
        })
    }
}

/// Validate every input; nothing is spawned unless this passes.
fn validate(handle: &str, query: &str, limit: usize) -> Result<(), ChannelSearchError> {
    validate_handle(handle)?;
    if query.trim().is_empty() {
        return Err(ChannelSearchError::EmptyQuery);
    }
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ChannelSearchError::InvalidLimit { limit });
    }
    Ok(())
}

/// The channel search page URL. `handle` must already be validated; `query`
/// is percent-encoded here.
pub fn search_url(handle: &str, query: &str) -> String {
    format!(
        "https://www.youtube.com/{handle}/search?query={}",
        urlencoding::encode(query)
    )
}

/// The last [`STDERR_TAIL_LINES`] non-blank lines of `stderr`, capped at
/// [`STDERR_TAIL_MAX_CHARS`] characters (the end is kept).
pub fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(STDERR_TAIL_LINES)..].join("\n");
    let chars = tail.chars().count();
    if chars <= STDERR_TAIL_MAX_CHARS {
        tail
    } else {
        tail.chars().skip(chars - STDERR_TAIL_MAX_CHARS).collect()
    }
}

/// Parse `yt-dlp --flat-playlist -J` stdout into hits, keeping only
/// `watch?v=` entries in their listed order. A body with no `entries` array,
/// or a video entry with no `id` or `title`, is a parse error.
pub fn parse_hits(stdout: &[u8]) -> Result<Vec<ChannelHit>, ChannelSearchError> {
    let json: serde_json::Value =
        serde_json::from_slice(stdout).map_err(|e| ChannelSearchError::Parse { reason: e.to_string() })?;
    let entries = json["entries"].as_array().ok_or_else(|| ChannelSearchError::Parse {
        reason: "no `entries` array".to_string(),
    })?;
    let mut hits = Vec::new();
    for entry in entries {
        let Some(url) = entry["url"].as_str().filter(|u| u.contains(VIDEO_URL_MARKER)) else {
            log::trace!(
                "youtube::parse_hits: dropping non-video entry url={:?}",
                entry["url"].as_str()
            );
            continue;
        };
        let field = |key: &str| {
            entry[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| ChannelSearchError::Parse {
                    reason: format!("video entry {url} has no `{key}`"),
                })
        };
        hits.push(ChannelHit {
            id: field("id")?,
            title: field("title")?,
            url: url.to_string(),
            duration_secs: entry["duration"].as_f64().map(|d| d.round() as u64),
            view_count: entry["view_count"].as_u64(),
        });
    }
    log::debug!("youtube::parse_hits: entries={} hits={}", entries.len(), hits.len());
    Ok(hits)
}

/// Append the search arguments to `base` (the yt-dlp program). The URL is one
/// argument; nothing is shell-interpreted.
fn search_command(mut base: Command, url: &str, limit: usize) -> Command {
    base.args(["--flat-playlist", "-J", "--playlist-end", &limit.to_string(), url]);
    base
}

/// Validate, then run `base` + the search arguments under `timeout`.
/// `base` is `yt-dlp` in production; tests substitute a stand-in program.
fn run_search(
    base: Command,
    handle: &str,
    query: &str,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<ChannelHit>, ChannelSearchError> {
    validate(handle, query, limit)?;
    let url = search_url(handle, query);
    log::debug!("youtube::search_channel: url={url} limit={limit} timeout={timeout:?}");
    let cmd = search_command(base, &url, limit);
    match process::run(cmd, None, timeout, "yt-dlp channel search") {
        Err(e) => Err(ChannelSearchError::Spawn {
            reason: format!("{e:#}"),
        }),
        Ok(Outcome::TimedOut { after }) => {
            log::warn!("youtube::search_channel: {url} timed out after {after:?}");
            Err(ChannelSearchError::TimedOut { after })
        }
        Ok(Outcome::Exited { status, stdout, stderr }) => {
            if !status.success() {
                let tail = stderr_tail(&stderr);
                log::warn!("youtube::search_channel: {url} exited {status}: {tail}");
                return Err(ChannelSearchError::Failed {
                    status: status.to_string(),
                    stderr_tail: tail,
                });
            }
            parse_hits(&stdout)
        }
    }
}

/// Search the channel `handle` (`@letsgetrusty`) for `query`, returning at
/// most `limit` videos. `limit` bounds the entries yt-dlp reads
/// (`--playlist-end`), so dropped playlist entries can leave fewer hits.
/// Bounded by [`SEARCH_TIMEOUT`]; a bad handle, empty query or out-of-range
/// limit is refused before yt-dlp is spawned.
pub async fn search_channel(handle: &str, query: &str, limit: usize) -> Result<Vec<ChannelHit>, ChannelSearchError> {
    log::debug!("youtube::search_channel: handle={handle} query={query:?} limit={limit}");
    let (handle, query) = (handle.to_string(), query.to_string());
    // `vault::process::run` blocks until the child exits; keep it off the
    // tokio workers.
    let result =
        tokio::task::spawn_blocking(move || run_search(Command::new("yt-dlp"), &handle, &query, limit, SEARCH_TIMEOUT))
            .await
            .unwrap_or_else(|join_err| {
                Err(ChannelSearchError::Spawn {
                    reason: format!("search task failed: {join_err}"),
                })
            });
    match &result {
        Ok(hits) => log::debug!("youtube::search_channel: ok hits={}", hits.len()),
        Err(e) => log::debug!("youtube::search_channel: failed: {e}"),
    }
    result
}

#[cfg(test)]
mod tests;
