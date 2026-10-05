//! One request path to the borg daemon for every first-party client.
//!
//! Resolves the address (`hotkey.host:port`), the bearer token
//! (`server.auth-token`) and the timeout (`hotkey.request-timeout`) once, and
//! checks the HTTP status before any body parse: a 401 reads "daemon rejected
//! the request (401)", never "failed to parse response".

use std::time::Duration;

use serde::de::DeserializeOwned;

use super::{HotkeyConfig, client_auth_token};

/// Longest slice of a non-2xx body quoted in an error.
const BODY_PREVIEW_CHARS: usize = 200;

/// Why a daemon call failed. Every variant names the daemon address.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("cannot build an HTTP client for the daemon at {addr}: {reason}")]
    Client { addr: String, reason: String },
    #[error("hotkey.host/hotkey.port form no valid address ({addr}): {reason}")]
    BadAddress { addr: String, reason: String },
    #[error("cannot reach daemon at {addr} for {method} {path}: {source}")]
    Unreachable {
        addr: String,
        method: &'static str,
        path: String,
        source: reqwest::Error,
    },
    #[error("daemon at {addr} rejected the request (401) for {path}; check server.auth-token")]
    Unauthorized { addr: String, path: String },
    #[error("daemon at {addr} answered {path} with HTTP {status}: {body}")]
    Status {
        addr: String,
        path: String,
        status: u16,
        body: String,
    },
    #[error("daemon at {addr} sent an unparseable body for {path}: {source}")]
    Parse {
        addr: String,
        path: String,
        source: reqwest::Error,
    },
}

impl DaemonError {
    /// True when the TCP connection itself failed (daemon not running).
    pub fn is_connect(&self) -> bool {
        matches!(self, Self::Unreachable { source, .. } if source.is_connect())
    }
}

/// A client for one daemon: address, token and timeout resolved at
/// construction. Cheap to reuse across requests.
#[derive(Debug, Clone)]
pub struct DaemonClient {
    http: reqwest::Client,
    addr: String,
    token: Option<String>,
}

impl DaemonClient {
    /// Build from the `hotkey:` block and the configured `server.auth-token`
    /// reference. Every request is bounded by `hotkey.request-timeout`.
    pub fn new(hotkey: &HotkeyConfig, auth_token: Option<&str>) -> Result<Self, DaemonError> {
        let addr = format!("{}:{}", hotkey.host, hotkey.port);
        log::debug!(
            "daemon::client::new: addr={addr} request-timeout={:?} token-configured={}",
            hotkey.request_timeout,
            auth_token.is_some()
        );
        let http = crate::http::client(hotkey.request_timeout).map_err(|e| DaemonError::Client {
            addr: addr.clone(),
            reason: format!("{e:#}"),
        })?;
        Ok(Self {
            http,
            addr,
            token: client_auth_token(auth_token),
        })
    }

    /// `host:port` of the daemon this client talks to.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<reqwest::Url, DaemonError> {
        let mut url =
            reqwest::Url::parse(&format!("http://{}{path}", self.addr)).map_err(|e| DaemonError::BadAddress {
                addr: self.addr.clone(),
                reason: e.to_string(),
            })?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    fn authorize(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => req.bearer_auth(token),
            None => req,
        }
    }

    async fn send(
        &self,
        method: &'static str,
        path: &str,
        req: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, DaemonError> {
        self.authorize(req)
            .send()
            .await
            .map_err(|source| DaemonError::Unreachable {
                addr: self.addr.clone(),
                method,
                path: path.to_string(),
                source,
            })
    }

    /// GET `path` with `query`. `timeout` replaces `hotkey.request-timeout` for
    /// this one request (`sb borg wait` passes `min(request-timeout, time
    /// left)`); `None` keeps the configured one. Returns the response whatever
    /// its status: callers with status-specific meaning inspect it, everyone
    /// else goes through [`Self::json`].
    pub async fn get(
        &self,
        path: &str,
        query: &[(&str, &str)],
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response, DaemonError> {
        log::debug!(
            "daemon::client::get: addr={} path={path} query={query:?} timeout={timeout:?}",
            self.addr
        );
        let mut req = self.http.get(self.url(path, query)?);
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        self.send("GET", path, req).await
    }

    /// POST `body` as JSON to `path`. Same status contract as [`Self::get`].
    pub async fn post_json(&self, path: &str, body: &serde_json::Value) -> Result<reqwest::Response, DaemonError> {
        log::debug!("daemon::client::post_json: addr={} path={path}", self.addr);
        let req = self.http.post(self.url(path, &[])?).json(body);
        self.send("POST", path, req).await
    }

    /// Check the status BEFORE parsing: a non-2xx answer is an error naming
    /// the status and a preview of the body, 401 distinctly.
    pub async fn ensure_success(&self, path: &str, resp: reqwest::Response) -> Result<reqwest::Response, DaemonError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(DaemonError::Unauthorized {
                addr: self.addr.clone(),
                path: path.to_string(),
            });
        }
        let body = resp.text().await.unwrap_or_default();
        Err(DaemonError::Status {
            addr: self.addr.clone(),
            path: path.to_string(),
            status: status.as_u16(),
            body: body.chars().take(BODY_PREVIEW_CHARS).collect(),
        })
    }

    /// Status-check `resp`, then parse its body as `T`. A body that stalls past
    /// the timeout is a [`DaemonError::Parse`], not a hang.
    pub async fn json<T: DeserializeOwned>(&self, path: &str, resp: reqwest::Response) -> Result<T, DaemonError> {
        let resp = self.ensure_success(path, resp).await?;
        resp.json::<T>().await.map_err(|source| DaemonError::Parse {
            addr: self.addr.clone(),
            path: path.to_string(),
            source,
        })
    }
}

#[cfg(test)]
mod tests;
