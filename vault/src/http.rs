//! The one place a `reqwest::Client` is built.
//!
//! A client built anywhere else risks the two failures this module exists to
//! kill: no timeout at all (a silent listener hangs the caller forever), or a
//! failed builder quietly replaced by `Client::new()` with the configured
//! timeout dropped. Construction here returns `Result`, so a failure is the
//! caller's error.
//!
//! Seam: `client` is the request/response constructor. Streaming callers
//! (ntfy) need connect and read timeouts but no total, and callers that add a
//! user agent or redirect policy need the builder itself; both extend this
//! file by adding `builder(Timeouts)` and a `Timeouts` type that `client`
//! then delegates to, without changing `client`'s callers.

use std::time::Duration;

use eyre::{Context, Result};

/// A client whose every request, connect through body, is bounded by `timeout`.
pub fn client(timeout: Duration) -> Result<reqwest::Client> {
    log::debug!("http::client: total timeout={timeout:?}");
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .wrap_err_with(|| format!("cannot build an HTTP client with a {timeout:?} timeout"))
}

#[cfg(test)]
mod tests;
