//! The one place a `reqwest::Client` is built.
//!
//! A client built anywhere else risks the two failures this module exists to
//! kill: no timeout at all (a silent listener hangs the caller forever), or a
//! failed builder quietly replaced by a default client with the configured
//! timeout dropped. Construction here returns `Result`, so a failure is the
//! caller's error. `clippy.toml` bans `reqwest::Client::new`,
//! `reqwest::Client::builder` and `reqwest::ClientBuilder::new` everywhere
//! else, tests included.
//!
//! [`client`] is the request/response constructor. Callers that add a user
//! agent or a redirect policy take [`builder`] and call `.build()`
//! themselves; a streaming caller (ntfy) passes [`Timeouts::Stream`], which
//! bounds each read instead of the whole response.

use std::time::Duration;

use eyre::{Context, Result};

/// How a client's requests are bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeouts {
    /// Every request, connect through the last body byte, is bounded by this.
    Total(Duration),
    /// A long-lived stream: connect is bounded, then each read is bounded,
    /// and there is no total. A total timeout would cut a healthy stream.
    Stream { connect: Duration, read: Duration },
}

impl Timeouts {
    /// The request/response bound.
    pub fn total(timeout: Duration) -> Self {
        Self::Total(timeout)
    }
}

/// A builder carrying `timeouts`. Callers add a user agent or redirect policy
/// and call `.build()`; its error is theirs to propagate.
#[allow(clippy::disallowed_methods)]
pub fn builder(timeouts: Timeouts) -> reqwest::ClientBuilder {
    log::debug!("http::builder: timeouts={timeouts:?}");
    let builder = reqwest::Client::builder();
    match timeouts {
        Timeouts::Total(timeout) => builder.timeout(timeout),
        Timeouts::Stream { connect, read } => builder.connect_timeout(connect).read_timeout(read),
    }
}

/// A client whose every request, connect through body, is bounded by `timeout`.
pub fn client(timeout: Duration) -> Result<reqwest::Client> {
    builder(Timeouts::total(timeout))
        .build()
        .wrap_err_with(|| format!("cannot build an HTTP client with a {timeout:?} timeout"))
}

#[cfg(test)]
mod tests;
