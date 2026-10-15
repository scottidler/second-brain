//! Client for the borg daemon's `GET /channels/search`, backing the
//! `channel_search` tool.
//!
//! Same shape as [`crate::queue`]: the daemon address and bearer token come
//! from `borg.yml` through [`crate::queue::BorgView`], and every failure is an
//! error naming the daemon address. Oracle adds the one thing only it can
//! answer: which hits are already notes in the vault (`in-vault`).

use crate::queue::BorgView;
use tracing::debug;
use vault::channel::{ChannelHit, ChannelSearch, ChannelSearchFailure};
use vault::daemon::client::{DaemonClient, DaemonError};
use vault::search::SearchIndex;

/// GET `/channels/search` from the daemon in `view`. A 400/502 carrying a
/// [`ChannelSearchFailure`] body surfaces its `error` (and yt-dlp's stderr
/// tail when present); every other failure is a [`DaemonError`] naming
/// `host:port`.
pub async fn fetch(view: &BorgView, handle: &str, query: &str, limit: Option<u32>) -> eyre::Result<ChannelSearch> {
    let daemon = DaemonClient::new(&view.hotkey, view.server.auth_token.as_deref())?;
    let addr = daemon.addr().to_string();
    debug!("channel::fetch: addr={addr} handle={handle} query={query:?} limit={limit:?}");

    let limit_str = limit.map(|l| l.to_string());
    let mut params: Vec<(&str, &str)> = vec![("handle", handle), ("q", query)];
    if let Some(l) = limit_str.as_deref() {
        params.push(("limit", l));
    }

    let resp = daemon.get("/channels/search", &params, None).await?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        eyre::bail!("daemon at {addr} predates /channels/search; run otto deploy there");
    }
    if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::BAD_GATEWAY {
        let body = resp.text().await.unwrap_or_default();
        match serde_json::from_str::<ChannelSearchFailure>(&body) {
            Ok(f) => {
                let tail = f.stderr_tail.map(|t| format!(": {t}")).unwrap_or_default();
                eyre::bail!(
                    "channel search via daemon at {addr} failed ({}): {}{tail}",
                    status.as_u16(),
                    f.error
                );
            }
            Err(_) => eyre::bail!(
                "channel search via daemon at {addr} failed ({}): {}",
                status.as_u16(),
                body.chars().take(200).collect::<String>()
            ),
        }
    }
    let search = match daemon.json::<ChannelSearch>("/channels/search", resp).await {
        Ok(s) => s,
        Err(e @ DaemonError::Parse { .. }) => {
            eyre::bail!("borg daemon at {addr} returned an unparseable /channels/search body: {e}")
        }
        Err(e) => return Err(e.into()),
    };
    debug!("channel::fetch: addr={addr} ok hits={}", search.hits.len());
    Ok(search)
}

/// Render `search` as the tool's `{handle, query, count, results}` answer,
/// adding `in-vault: <path>` to each hit whose video id is already a YouTube
/// note in `db`. An index error is an error, never a silently unmarked hit.
pub fn render(search: &ChannelSearch, db: &SearchIndex) -> eyre::Result<serde_json::Value> {
    let mut results = Vec::with_capacity(search.hits.len());
    for hit in &search.hits {
        results.push(render_hit(hit, db.youtube_note_path(&hit.id)?));
    }
    Ok(serde_json::json!({
        "handle": search.handle,
        "query": search.query,
        "count": results.len(),
        "results": results,
    }))
}

fn render_hit(hit: &ChannelHit, in_vault: Option<String>) -> serde_json::Value {
    let mut v = serde_json::to_value(hit).expect("ChannelHit serializes");
    if let (Some(path), Some(obj)) = (in_vault, v.as_object_mut()) {
        obj.insert("in-vault".to_string(), serde_json::Value::String(path));
    }
    v
}

#[cfg(test)]
mod tests;
