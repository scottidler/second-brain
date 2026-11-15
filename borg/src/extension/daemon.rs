use eyre::{Result, bail, eyre};
use url::Url;

use crate::config::Config;

const DEFAULT_ENDPOINT: &str = "http://localhost:8181";
const DEFAULT_ATTEMPT_TIMEOUT_MS: u64 = 3000;

/// The ordered daemon endpoints the popup will try, validated.
///
/// Unset -> `[http://localhost:8181]`. Every entry must be `http`, carry a
/// host, and have no path beyond `/`, no query, no fragment and no userinfo.
/// An empty list is an error, never a silent default. Render with
/// [`endpoint_base`] to get the form without the trailing `/`.
pub fn resolved_endpoints(config: &Config) -> Result<Vec<Url>> {
    log::debug!("extension::daemon::resolved_endpoints");
    let Some(raw) = &config.extension.endpoints else {
        return Ok(vec![
            Url::parse(DEFAULT_ENDPOINT).expect("DEFAULT_ENDPOINT is a valid URL"),
        ]);
    };
    if raw.is_empty() {
        bail!("extension.endpoints is empty; list at least one endpoint or remove the key");
    }
    raw.iter().map(|entry| parse_endpoint(entry)).collect()
}

/// Per-attempt timeout in milliseconds. Unset -> 3000; 0 is an error.
pub fn attempt_timeout_ms(config: &Config) -> Result<u64> {
    match config.extension.attempt_timeout_ms {
        None => Ok(DEFAULT_ATTEMPT_TIMEOUT_MS),
        Some(0) => bail!("extension.attempt-timeout-ms must be greater than 0"),
        Some(ms) => Ok(ms),
    }
}

/// An endpoint as the popup concatenates it with a path: no trailing `/`.
pub fn endpoint_base(endpoint: &Url) -> String {
    endpoint.as_str().trim_end_matches('/').to_string()
}

fn parse_endpoint(entry: &str) -> Result<Url> {
    let url = Url::parse(entry).map_err(|e| eyre!("extension.endpoints: {entry:?} is not a valid URL: {e}"))?;
    if url.scheme() != "http" {
        bail!(
            "extension.endpoints: {entry:?} must use http (host_permissions and the CSP are http-only), got {}",
            url.scheme()
        );
    }
    if url.host_str().is_none_or(str::is_empty) {
        bail!("extension.endpoints: {entry:?} has no host");
    }
    if url.path() != "/" {
        bail!("extension.endpoints: {entry:?} has a path; only a bare origin is allowed");
    }
    if url.query().is_some() {
        bail!("extension.endpoints: {entry:?} has a query string");
    }
    if url.fragment().is_some() {
        bail!("extension.endpoints: {entry:?} has a fragment");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("extension.endpoints: {entry:?} carries credentials; they would ship to AMO inside the .xpi");
    }
    Ok(url)
}

#[cfg(test)]
mod tests;
