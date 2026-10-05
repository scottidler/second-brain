use async_trait::async_trait;
use eyre::{Context, Result, bail};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use vault::process::{self, Outcome};

use crate::stages::artifact::{ArtifactStore, sha256_hex};
use crate::types::{FetchMeta, FetchResult};

const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:120.0) Gecko/20100101 Firefox/120.0";

/// Stage 0 network-fetch abstraction. Stage 1 extractors must never call this.
#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<FetchResult>;
}

/// Fetch via reqwest with a realistic browser User-Agent and convert the
/// response body to markdown via markitdown. Recovers URLs that block
/// bot IPs (Jina) but not browser UAs (e.g. XDA Developers on 2026-04-19).
pub struct BrowserUaFetcher {
    client: reqwest::Client,
    /// Bound on the markitdown subprocess (`pipeline.browser-ua-timeout`).
    markitdown_timeout: Duration,
}

impl BrowserUaFetcher {
    pub fn new(markitdown_timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(BROWSER_UA)
            .redirect(reqwest::redirect::Policy::limited(5))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|e| {
                log::warn!("browser-ua: falling back to default client: {e:#}");
                reqwest::Client::new()
            });
        Self {
            client,
            markitdown_timeout,
        }
    }
}

/// The markitdown invocation that converts fetched HTML (fed on stdin) to markdown.
fn markitdown_stdin_command() -> Command {
    Command::new("markitdown")
}

/// Run markitdown over `html` on stdin. A timeout, a spawn failure, and a
/// non-zero exit are each `Err`; the caller falls back to the raw bytes.
fn run_markitdown(cmd: Command, html: Vec<u8>, timeout: Duration) -> Result<Vec<u8>> {
    match process::run(cmd, Some(html), timeout, "markitdown (browser-ua)")
        .context("spawn markitdown-cli (is it installed?)")?
    {
        Outcome::TimedOut { after } => bail!("markitdown timed out after {after:?}"),
        Outcome::Exited { status, stderr, .. } if !status.success() => {
            bail!("markitdown failed ({status}): {}", String::from_utf8_lossy(&stderr))
        }
        Outcome::Exited { stdout, .. } => Ok(stdout),
    }
}

#[async_trait]
impl Fetcher for BrowserUaFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchResult> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("browser-ua: request failed")?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let raw = response.bytes().await.context("browser-ua: read body")?.to_vec();
        if !(200..300).contains(&status) {
            bail!("browser-ua: HTTP {status} for {url}");
        }
        // Pipe the raw HTML through markitdown-cli to get markdown.
        let md = tokio::task::spawn_blocking({
            let raw = raw.clone();
            let timeout = self.markitdown_timeout;
            move || run_markitdown(markitdown_stdin_command(), raw, timeout)
        })
        .await
        .context("markitdown: join failed")?
        .unwrap_or_else(|e| {
            log::warn!("browser-ua: markitdown failed, using raw bytes: {e:#}");
            raw.clone()
        });
        // Surface the byline from the SAME fetch before the HTML is gone -
        // `byline::extract` runs on the raw HTML this fetcher already cleared
        // the bot-wall for, so no extra GET is needed (see the design doc).
        let author = crate::byline::extract(&String::from_utf8_lossy(&raw));
        let sha256 = sha256_hex(&md);
        let meta = FetchMeta {
            source: url.to_string(),
            extractor: "browser-ua".to_string(),
            status,
            content_type,
            bytes: md.len() as u64,
            sha256,
            fallbacks_attempted: Vec::new(),
            author,
        };
        Ok(FetchResult { bytes: md, meta })
    }
}

/// Wraps a `Fetcher` and persists every successful response to an
/// `ArtifactStore` keyed by `trace_id` as a side effect. Critical for the
/// one-fetch-per-ingestion invariant during double-write.
pub struct FsCachingFetcher<F: Fetcher> {
    inner: F,
    store: Arc<dyn ArtifactStore>,
    trace_id: String,
    calls: AtomicU32,
}

impl<F: Fetcher> FsCachingFetcher<F> {
    pub fn new(inner: F, store: Arc<dyn ArtifactStore>, trace_id: String) -> Self {
        Self {
            inner,
            store,
            trace_id,
            calls: AtomicU32::new(0),
        }
    }

    /// Number of times `fetch` was invoked on this decorator. Drives the
    /// one-fetch-per-ingestion integration test.
    pub fn call_count(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl<F: Fetcher> Fetcher for FsCachingFetcher<F> {
    async fn fetch(&self, url: &str) -> Result<FetchResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let result = self.inner.fetch(url).await?;
        self.store
            .write_fetched(&self.trace_id, &result.bytes, &result.meta)
            .context("cache fetched bytes")?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
