use async_trait::async_trait;
use eyre::{Context, ContextCompat, Result, bail};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::config::FabricConfig;
use crate::stages::artifact::{ArtifactStore, sha256_hex};
use crate::types::{FetchMeta, FetchResult};

const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:120.0) Gecko/20100101 Firefox/120.0";

/// Stage 0 network-fetch abstraction. Stage 1 extractors must never call this.
#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<FetchResult>;
}

/// Chain of Stage-0 fetchers tried in order: Jina Reader → Fabric `-u` → browser-UA
/// (reqwest with a realistic Firefox User-Agent piped through markitdown-cli).
/// Each fetcher's failure is logged and the next is attempted.
pub struct MultiFetcher {
    jina: JinaFetcher,
    fabric: FabricFetcher,
    browser: BrowserUaFetcher,
}

impl MultiFetcher {
    pub fn new(fabric_cfg: FabricConfig) -> Self {
        Self {
            jina: JinaFetcher::new(),
            fabric: FabricFetcher::new(fabric_cfg),
            browser: BrowserUaFetcher::new(),
        }
    }
}

#[async_trait]
impl Fetcher for MultiFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchResult> {
        let mut tried: Vec<String> = Vec::new();
        match self.jina.fetch(url).await {
            Ok(mut r) => {
                r.meta.fallbacks_attempted = tried;
                return Ok(r);
            }
            Err(e) => {
                log::warn!("MultiFetcher: jina failed for {url}: {e:#}");
                tried.push("jina".to_string());
            }
        }
        match self.fabric.fetch(url).await {
            Ok(mut r) => {
                r.meta.fallbacks_attempted = tried;
                return Ok(r);
            }
            Err(e) => {
                log::warn!("MultiFetcher: fabric -u failed for {url}: {e:#}");
                tried.push("fabric".to_string());
            }
        }
        match self.browser.fetch(url).await {
            Ok(mut r) => {
                r.meta.fallbacks_attempted = tried;
                Ok(r)
            }
            Err(e) => {
                log::warn!("MultiFetcher: browser-UA failed for {url}: {e:#}");
                bail!("all fetchers failed for {url}: last error: {e:#}")
            }
        }
    }
}

/// Jina Reader (https://r.jina.ai/<url>). Emits markdown; we treat the body as
/// text/markdown. HTTP 451 signals Jina's own IP-based block and is returned
/// as a FetchResult so Gate-1 can inspect it (block-page detection runs from disk).
pub struct JinaFetcher {
    client: reqwest::Client,
}

impl JinaFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for JinaFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Fetcher for JinaFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchResult> {
        let jina_url = format!("https://r.jina.ai/{url}");
        let response = self
            .client
            .get(&jina_url)
            .header("Accept", "text/markdown")
            .send()
            .await
            .context("jina: request failed")?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = response.bytes().await.context("jina: read body")?.to_vec();
        // Gate-1 will reject downstream, but Jina `status` above 400 typically
        // means we should not treat the body as authoritative.
        if !(200..300).contains(&status) && !matches!(status, 451) {
            bail!("jina returned HTTP {status} for {url}");
        }
        let sha256 = sha256_hex(&bytes);
        let meta = FetchMeta {
            source: url.to_string(),
            extractor: "jina".to_string(),
            status,
            content_type,
            bytes: bytes.len() as u64,
            sha256,
            fallbacks_attempted: Vec::new(),
        };
        Ok(FetchResult { bytes, meta })
    }
}

/// `fabric -u <url>` extractor. Executes the fabric binary; stdout is the body.
pub struct FabricFetcher {
    config: FabricConfig,
}

impl FabricFetcher {
    pub fn new(config: FabricConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Fetcher for FabricFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchResult> {
        let binary = vault::fabric::resolve_binary(&self.config.binary);
        let url_owned = url.to_string();
        let output = tokio::task::spawn_blocking(move || -> Result<std::process::Output> {
            let out = std::process::Command::new(&binary)
                .args(["-u", &url_owned])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .context("fabric: spawn failed")?
                .wait_with_output()
                .context("fabric: wait_with_output failed")?;
            Ok(out)
        })
        .await
        .context("fabric: join failed")??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            bail!("fabric -u failed: {stderr}");
        }
        let bytes = output.stdout;
        if bytes.is_empty() {
            bail!("fabric -u returned empty body");
        }
        let sha256 = sha256_hex(&bytes);
        let meta = FetchMeta {
            source: url.to_string(),
            extractor: "fabric-u".to_string(),
            status: 200,
            content_type: Some("text/markdown".to_string()),
            bytes: bytes.len() as u64,
            sha256,
            fallbacks_attempted: Vec::new(),
        };
        Ok(FetchResult { bytes, meta })
    }
}

/// Fetch via reqwest with a realistic browser User-Agent and convert the
/// response body to markdown via markitdown-cli. Recovers URLs that block
/// bot IPs (Jina) but not browser UAs (e.g. XDA Developers on 2026-04-19).
pub struct BrowserUaFetcher {
    client: reqwest::Client,
}

impl BrowserUaFetcher {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(BROWSER_UA)
            .redirect(reqwest::redirect::Policy::limited(5))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|e| {
                log::warn!("browser-ua: falling back to default client: {e:#}");
                reqwest::Client::new()
            });
        Self { client }
    }
}

impl Default for BrowserUaFetcher {
    fn default() -> Self {
        Self::new()
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
            move || -> Result<Vec<u8>> {
                let mut child = std::process::Command::new("markitdown")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .context("spawn markitdown-cli (is it installed?)")?;
                {
                    let stdin = child.stdin.as_mut().context("markitdown: no stdin")?;
                    stdin.write_all(&raw).context("markitdown: write stdin")?;
                }
                let output = child.wait_with_output().context("markitdown: wait")?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    bail!("markitdown failed: {stderr}");
                }
                Ok(output.stdout)
            }
        })
        .await
        .context("markitdown: join failed")?
        .unwrap_or_else(|e| {
            log::warn!("browser-ua: markitdown failed, using raw bytes: {e:#}");
            raw
        });
        let sha256 = sha256_hex(&md);
        let meta = FetchMeta {
            source: url.to_string(),
            extractor: "browser-ua".to_string(),
            status,
            content_type,
            bytes: md.len() as u64,
            sha256,
            fallbacks_attempted: Vec::new(),
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
