//! Test-only stubs for the engine: one loopback HTTP server that plays the
//! Data API and Gemini, and a canned yt-dlp. Shared by the engine tests and
//! the pipeline tests that drive `process_youtube`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::{Config, YoutubeEngine};
use crate::youtube::spawn;

pub(crate) const VIDEO_ID: &str = "53XYcpCgQWE";
pub(crate) const URL: &str = "https://www.youtube.com/watch?v=53XYcpCgQWE";
pub(crate) const DATAAPI_LIVE: &str = include_str!("../fixtures/dataapi-videos-live.json");
pub(crate) const DATAAPI_EMPTY: &str = include_str!("../fixtures/dataapi-videos-empty.json");
pub(crate) const DATAAPI_CHANNEL: &str = include_str!("../fixtures/dataapi-channels-handle.json");
pub(crate) const GEMINI_OK: &str = include_str!("../fixtures/gemini-transcript-hhmmss.json");
pub(crate) const DUMP_JSON: &str = include_str!("../fixtures/dump-json-53xycpcgqwe.json");
pub(crate) const GEMINI_5XX: &str = r#"{"error": {"code": 503, "message": "overloaded", "status": "UNAVAILABLE"}}"#;

/// The VTT the canned yt-dlp hands back for `--write-auto-sub`.
pub(crate) const VTT: &str = "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nfrom the yt-dlp arm\n\n00:00:04.000 --> 00:00:06.000\nsecond caption line\n";

/// Path prefix -> replies served in order; requests past the end get the last.
pub(crate) type Routes = Vec<(&'static str, Vec<(u16, String)>)>;

/// Requests received per route prefix.
#[derive(Clone, Default)]
pub(crate) struct Hits(Arc<Mutex<HashMap<&'static str, usize>>>);

impl Hits {
    pub(crate) fn get(&self, prefix: &str) -> usize {
        self.0.lock().unwrap().get(prefix).copied().unwrap_or(0)
    }
}

/// Serve `routes` on a loopback port. Returns the base URL and the hit counts.
/// An unmatched path is a 404.
pub(crate) async fn serve(routes: Routes) -> (String, Hits) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Hits::default();
    let hits_for_task = hits.clone();
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let hits = hits_for_task.clone();
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let head_end = loop {
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                    match socket.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let length = head
                    .lines()
                    .find_map(|l| {
                        let (name, value) = l.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + length {
                    match socket.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let (status, body) = match routes.iter().find(|(prefix, _)| path.starts_with(prefix)) {
                    Some((prefix, replies)) => {
                        let mut counts = hits.0.lock().unwrap();
                        let n = counts.entry(*prefix).or_insert(0);
                        let reply = replies[(*n).min(replies.len() - 1)].clone();
                        *n += 1;
                        reply
                    }
                    None => (404, "{}".to_string()),
                };
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}"), hits)
}

/// Data API routes for a live video, plus `gemini` replies on `/interactions`.
pub(crate) fn routes(videos: &str, gemini: Vec<(u16, String)>) -> Routes {
    vec![
        ("/videos", vec![(200, videos.to_string())]),
        ("/channels", vec![(200, DATAAPI_CHANNEL.to_string())]),
        ("/interactions", gemini),
    ]
}

/// A config that reaches nothing real: keys from files under `dir`, fabric
/// pointed at a binary that does not exist, staging under `dir`, millisecond
/// gemini deadlines.
pub(crate) fn config(dir: &Path, engine: YoutubeEngine, fallback: Option<YoutubeEngine>) -> Config {
    let key_file = |name: &str| -> String {
        let path = dir.join(name);
        std::fs::write(&path, "stub-key").unwrap();
        path.to_string_lossy().to_string()
    };
    let mut config = Config::default();
    // `Config::default()` carries no link rules (serde fills them on load);
    // without them a YouTube URL would not classify as YouTube.
    config.links = crate::config::default_links();
    config.youtube.engine = engine;
    config.youtube.fallback = fallback;
    config.youtube.data_api.api_key = key_file("youtube.key");
    config.youtube.gemini.api_key = key_file("gemini.key");
    config.youtube.gemini.timeout = std::time::Duration::from_secs(5);
    config.youtube.gemini.budget = std::time::Duration::from_secs(10);
    config.youtube.slides.enabled = false;
    config.fabric.binary = dir.join("no-such-fabric").to_string_lossy().to_string();
    config.staging.enabled = false;
    config.staging.root = dir.join("stages");
    config
}

/// A canned yt-dlp: `--dump-json` prints the recorded metadata,
/// `--write-auto-sub` writes [`VTT`] under `dir` and prints its path, and
/// anything else (the slides mp4 download, audio) exits 1.
pub(crate) fn yt_dlp(dir: &Path) -> spawn::Stub {
    let dir: PathBuf = dir.to_path_buf();
    Arc::new(move |argv: &[String]| {
        let has = |flag: &str| argv.iter().any(|a| a == flag);
        if has("--dump-json") {
            return Ok(spawn::exit(0, DUMP_JSON, ""));
        }
        if has("--write-auto-sub") {
            let path = dir.join(format!("{VIDEO_ID}.en-orig.vtt"));
            std::fs::write(&path, VTT)?;
            let printed = serde_json::json!({"en-orig": {"filepath": path}}).to_string();
            return Ok(spawn::exit(0, &printed, ""));
        }
        Ok(spawn::exit(1, "", "ERROR: stub yt-dlp does not download"))
    })
}
