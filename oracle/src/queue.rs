//! Client for the borg daemon's `GET /queue`, backing the `ingest_queue` tool.
//!
//! The daemon address, bearer token and vocabulary path come from `borg.yml` through the same
//! loader borg uses (`vault::config::load_config`), read into a view struct
//! that holds only what a client needs. Every failure is an error naming the
//! daemon address; nothing here can turn a failed read into an idle answer.

use serde::Deserialize;
use std::path::PathBuf;
use tracing::debug;
use vault::config::{Normalize, load_config};
use vault::daemon::HotkeyConfig;
use vault::daemon::client::{DaemonClient, DaemonError};

/// The slice of `borg.yml` oracle reads. Not `deny_unknown_fields`:
/// the rest of the file belongs to borg.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct BorgView {
    pub hotkey: HotkeyConfig,
    pub server: ServerView,
    pub tags: TagsView,
}

/// `tags.canonical-path`, the one vocabulary key oracle shares with borg.
/// Kept raw: `vocab` tilde-expands it at the point of use.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct TagsView {
    pub canonical_path: Option<String>,
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

/// GET `/queue` from the daemon in `view`, returning its JSON verbatim, bounded
/// by `hotkey.request-timeout`. Any failure (connect, timeout, non-2xx, bad
/// body) is an error naming `host:port`.
pub async fn fetch(view: &BorgView) -> eyre::Result<serde_json::Value> {
    let daemon = DaemonClient::new(&view.hotkey, view.server.auth_token.as_deref())?;
    let addr = daemon.addr().to_string();
    debug!("queue::fetch: addr={addr} timeout={:?}", view.hotkey.request_timeout);

    let resp = daemon.get("/queue", &[], None).await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eyre::bail!("daemon at {addr} predates /queue; run otto deploy there");
    }
    let value = match daemon.json::<serde_json::Value>("/queue", resp).await {
        Ok(v) => v,
        Err(e @ DaemonError::Parse { .. }) => {
            eyre::bail!("borg daemon at {addr} returned an unparseable /queue body: {e}")
        }
        Err(e) => return Err(e.into()),
    };
    debug!("queue::fetch: addr={addr} ok state={:?}", value.get("state"));
    Ok(value)
}

#[cfg(test)]
mod tests;
