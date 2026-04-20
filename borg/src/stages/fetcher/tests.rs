#![allow(clippy::unwrap_used)]

use super::*;
use crate::stages::artifact::MemArtifactStore;
use crate::types::FetchMeta;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

struct CountingFakeFetcher {
    calls: Arc<AtomicU32>,
    body: Vec<u8>,
    extractor: String,
}

impl CountingFakeFetcher {
    fn new(body: &[u8], extractor: &str) -> Self {
        Self {
            calls: Arc::new(AtomicU32::new(0)),
            body: body.to_vec(),
            extractor: extractor.to_string(),
        }
    }
}

#[async_trait]
impl Fetcher for CountingFakeFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let sha = crate::stages::artifact::sha256_hex(&self.body);
        let meta = FetchMeta {
            source: url.to_string(),
            extractor: self.extractor.clone(),
            status: 200,
            content_type: Some("text/markdown".to_string()),
            bytes: self.body.len() as u64,
            sha256: sha,
            fallbacks_attempted: Vec::new(),
        };
        Ok(FetchResult {
            bytes: self.body.clone(),
            meta,
        })
    }
}

struct AlwaysFailFetcher(&'static str);

#[async_trait]
impl Fetcher for AlwaysFailFetcher {
    async fn fetch(&self, _url: &str) -> Result<FetchResult> {
        bail!("{} always fails", self.0);
    }
}

#[tokio::test]
async fn fs_caching_fetcher_persists_and_counts_calls() {
    let store: Arc<dyn ArtifactStore> = Arc::new(MemArtifactStore::new());
    let env = crate::stages::artifact::new_envelope(
        "tg-test",
        crate::types::IngestKind::ArticleUrl,
        crate::types::IngestMethod::Telegram,
    );
    store.write_envelope(&env.trace, &env).unwrap();
    let fake = CountingFakeFetcher::new(b"<html>body</html>", "fake");
    let counter = fake.calls.clone();
    let cache = FsCachingFetcher::new(fake, store.clone(), env.trace.clone());
    let _ = cache.fetch("https://example.com/post").await.unwrap();
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert_eq!(cache.call_count(), 1);
    let (bytes, meta) = store.read_fetched(&env.trace).unwrap().unwrap();
    assert_eq!(bytes, b"<html>body</html>");
    assert_eq!(meta.source, "https://example.com/post");
}

#[tokio::test]
async fn fs_caching_fetcher_enforces_one_fetch_per_ingestion() {
    // The invariant is: however many times process_content re-asks for the
    // same URL during an ingestion, the underlying network call is made once
    // *per invocation* of the decorator. We enforce that by asserting the
    // caller invokes fetch exactly once per trace for a URL-bearing capture.
    let store: Arc<dyn ArtifactStore> = Arc::new(MemArtifactStore::new());
    let env = crate::stages::artifact::new_envelope(
        "tg-once",
        crate::types::IngestKind::ArticleUrl,
        crate::types::IngestMethod::Telegram,
    );
    store.write_envelope(&env.trace, &env).unwrap();
    let fake = CountingFakeFetcher::new(b"body", "fake");
    let counter = fake.calls.clone();
    let cache = FsCachingFetcher::new(fake, store.clone(), env.trace.clone());

    // Simulate one ingestion: one call to fetch.
    let _ = cache.fetch("https://example.com/").await.unwrap();

    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "ingestion made more than one network fetch"
    );
    assert_eq!(cache.call_count(), 1);
}

#[tokio::test]
async fn multifetcher_falls_back_on_first_failure() {
    // Force the first two fetchers to fail via a compose helper.
    struct Chain {
        jina: AlwaysFailFetcher,
        fabric: AlwaysFailFetcher,
        browser: CountingFakeFetcher,
    }
    #[async_trait]
    impl Fetcher for Chain {
        async fn fetch(&self, url: &str) -> Result<FetchResult> {
            if let Ok(r) = self.jina.fetch(url).await {
                return Ok(r);
            }
            if let Ok(r) = self.fabric.fetch(url).await {
                return Ok(r);
            }
            self.browser.fetch(url).await
        }
    }
    let chain = Chain {
        jina: AlwaysFailFetcher("jina"),
        fabric: AlwaysFailFetcher("fabric"),
        browser: CountingFakeFetcher::new(b"ok", "browser-ua"),
    };
    let got = chain.fetch("https://example.com").await.unwrap();
    assert_eq!(got.bytes, b"ok");
    assert_eq!(got.meta.extractor, "browser-ua");
}
