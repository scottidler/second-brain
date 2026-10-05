//! The client-side address of the borg daemon and the bearer token a
//! first-party client sends it.
//!
//! Lives in vault (not borg) because more than one crate is a daemon client:
//! borg's own CLI paths (reingest, replay, `sb borg queue`) and oracle's
//! `ingest_queue` tool all read the same `hotkey:` block and `server.auth-token`
//! from `borg.yml`, so there is exactly one address type and one resolver.

use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};

use crate::config::{deserialize_humantime, resolve_secret, serialize_humantime};

#[cfg(feature = "http")]
pub mod client;

/// Per-request ceiling for a first-party daemon call when
/// `hotkey.request-timeout` is not configured.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Where first-party clients reach the borg daemon (`hotkey.host` /
/// `hotkey.port` in `borg.yml`; live: `desk.lan:8181`). The `hotkey` name
/// predates its use as the general client-side daemon address.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct HotkeyConfig {
    pub host: String,
    pub port: u16,
    pub key: String,
    /// Ceiling on one daemon request: connect, headers and body together.
    /// Humantime in `borg.yml` (`hotkey.request-timeout: 10s`).
    #[serde(
        deserialize_with = "deserialize_request_timeout",
        serialize_with = "serialize_humantime"
    )]
    pub request_timeout: Duration,
}

fn deserialize_request_timeout<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    deserialize_humantime(deserializer, "request-timeout")
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 8181,
            key: "<Ctrl><Shift>b".to_string(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

/// Resolve the daemon auth token for a FIRST-PARTY client to send as a
/// `Bearer` header, mirroring how the server resolves `server.auth-token`.
/// `auth_token` is the configured secret REFERENCE (env-var name or file
/// path), not the token. Returns `None` when no reference is configured; logs
/// a warning and returns `None` if a reference is set but unresolvable or
/// empty (the request then 401s, surfacing the misconfiguration loudly
/// instead of silently).
pub fn client_auth_token(auth_token: Option<&str>) -> Option<String> {
    let reference = auth_token?;
    match resolve_secret(reference) {
        Ok(t) if !t.is_empty() => Some(t),
        Ok(_) => {
            log::warn!("server.auth-token {reference:?} resolved empty; client will send no token");
            None
        }
        Err(e) => {
            log::warn!("server.auth-token {reference:?} not resolvable for client auth: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests;
