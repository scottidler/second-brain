//! Diagnostic summary of a verbose (`-v`) yt-dlp download (audio extraction and
//! the slides video download), for the intermittent
//! `HTTP Error 403: Forbidden`. It pulls out what upstream
//! needs to tell the 403 causes apart: which player client served the formats,
//! which format/itag was fetched, the stream's client (`c=`) and experiment
//! flags (`fexp=`), and whether a PO token rode the stream URL.
//!
//! Stream URLs are signed and embed the caller's IP; only the named query
//! parameters below are ever extracted, never the URL itself.

use std::fmt;

#[derive(Debug, Default, PartialEq)]
pub(crate) struct DownloadDiagnostics {
    pub yt_dlp_version: Option<String>,
    pub player_clients: Vec<String>,
    pub formats: Option<String>,
    pub stream_client: Option<String>,
    pub stream_itag: Option<String>,
    pub stream_mime: Option<String>,
    pub stream_has_pot: bool,
    pub stream_fexp: Option<String>,
    pub po_token_providers: Option<String>,
    pub js_runtimes: Option<String>,
    pub notes: Vec<String>,
    pub error: Option<String>,
}

pub(crate) fn download_diagnostics(stdout: &str, stderr: &str) -> DownloadDiagnostics {
    let mut diag = DownloadDiagnostics::default();
    for line in stdout.lines().chain(stderr.lines()) {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("[debug] yt-dlp version ") {
            diag.yt_dlp_version = rest.split_whitespace().next().map(str::to_string);
        } else if let Some(client) = player_client(line) {
            if !diag.player_clients.contains(&client) {
                diag.player_clients.push(client);
            }
        } else if let Some((_, formats)) = line.split_once("Downloading ").filter(|_| line.starts_with("[info]"))
            && formats.contains("format(s):")
        {
            diag.formats = Some(formats.trim_start_matches("1 ").to_string());
        } else if line.starts_with("[debug] Invoking ") && line.contains("videoplayback?") {
            // The last downloader invocation is the one that succeeded or 403'd.
            let query = line
                .split_once("videoplayback?")
                .map(|(_, q)| q.trim_end_matches('"'))
                .unwrap_or_default();
            diag.stream_client = query_param(query, "c");
            diag.stream_itag = query_param(query, "itag");
            diag.stream_mime = query_param(query, "mime").map(|m| m.replace("%2F", "/"));
            diag.stream_fexp = query_param(query, "fexp").map(|f| f.replace("%2C", ","));
            diag.stream_has_pot = query_param(query, "pot").is_some();
        } else if let Some(rest) = line.strip_prefix("[debug] [youtube] [pot] PO Token Providers: ") {
            diag.po_token_providers = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("[debug] JS runtimes: ") {
            diag.js_runtimes = Some(rest.to_string());
        } else if line.starts_with("ERROR:") {
            diag.error = Some(line.to_string());
        } else if line.starts_with("WARNING:") || line.contains("experiment") || line.contains("SABR") {
            diag.notes.push(line.to_string());
        }
    }
    diag
}

pub(crate) fn is_http_403(stderr: &str) -> bool {
    stderr
        .lines()
        .any(|l| l.starts_with("ERROR:") && l.contains("HTTP Error 403"))
}

/// The final `ERROR:` line, or the last non-empty line. Verbose stderr is far
/// too large to carry into the pipeline error (it reaches Telegram/desktop toasts).
pub(crate) fn error_summary(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|l| l.starts_with("ERROR:"))
        .or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("(no stderr)")
        .trim()
        .to_string()
}

/// `[youtube] <id>: Downloading <client> player API JSON` -> `<client>`.
fn player_client(line: &str) -> Option<String> {
    let rest = line.strip_prefix("[youtube] ")?;
    let (_, after) = rest.split_once(": Downloading ")?;
    after.strip_suffix(" player API JSON").map(str::to_string)
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
        .map(str::to_string)
}

impl fmt::Display for DownloadDiagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".to_string());
        write!(
            f,
            "yt-dlp={} player-clients=[{}] formats={} stream-client={} itag={} mime={} pot={} po-token-providers={} js-runtimes={} fexp={}",
            opt(&self.yt_dlp_version),
            self.player_clients.join(","),
            opt(&self.formats),
            opt(&self.stream_client),
            opt(&self.stream_itag),
            opt(&self.stream_mime),
            self.stream_has_pot,
            opt(&self.po_token_providers),
            opt(&self.js_runtimes),
            opt(&self.stream_fexp),
        )?;
        for note in &self.notes {
            write!(f, " | note: {note}")?;
        }
        if let Some(err) = &self.error {
            write!(f, " | {err}")?;
        }
        Ok(())
    }
}
