//! Client for the borg daemon's `GET /queue`, backing the `ingest_queue` tool.
//!
//! The daemon address and bearer token come from `borg.yml` through the same
//! loader borg uses (`vault::config::load_config`), read into a view struct
//! that holds only what a client needs. Every failure is an error naming the
//! daemon address; nothing here can turn a failed read into an idle answer.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tracing::debug;
use vault::config::{Normalize, load_config};
use vault::daemon::{HotkeyConfig, client_auth_token};

/// Per-request ceiling covering connect, headers and body.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The slice of `borg.yml` a daemon client reads. Not `deny_unknown_fields`:
/// the rest of the file belongs to borg.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct BorgView {
    pub hotkey: HotkeyConfig,
    pub server: ServerView,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ServerView {
    pub auth_token: Option<String>,
}

impl Normalize for BorgView {}

/// Load the daemon-address view through the shared `borg.yml` chain.
pub fn load_view(config_path: Option<&PathBuf>) -> eyre::Result<BorgView> {
    debug!("queue::load_view: explicit={:?}", config_path);
    load_config::<BorgView>(config_path)
}

/// GET `/queue` from the daemon in `view`, returning its JSON verbatim.
/// Any failure (connect, timeout, non-2xx, bad body) is an error naming
/// `host:port`.
pub async fn fetch(view: &BorgView, timeout: Duration) -> eyre::Result<serde_json::Value> {
    let addr = format!("{}:{}", view.hotkey.host, view.hotkey.port);
    let url = format!("http://{addr}/queue");
    debug!("queue::fetch: addr={addr} timeout={timeout:?}");

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| eyre::eyre!("cannot build HTTP client for borg daemon at {addr}: {e}"))?;
    let mut req = client.get(&url);
    if let Some(token) = client_auth_token(view.server.auth_token.as_deref()) {
        req = req.bearer_auth(token);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| eyre::eyre!("borg daemon at {addr} unreachable: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        eyre::bail!("daemon at {addr} predates /queue; run otto deploy there");
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let preview: String = body.chars().take(200).collect();
        eyre::bail!("borg daemon at {addr} answered GET /queue with HTTP {status}: {preview}");
    }
    let value = resp
        .json::<serde_json::Value>()
        .await
        .map_err(|e| eyre::eyre!("borg daemon at {addr} returned an unparseable /queue body: {e}"))?;
    debug!("queue::fetch: addr={addr} ok state={:?}", value.get("state"));
    Ok(value)
}

#[cfg(test)]
mod tests;
