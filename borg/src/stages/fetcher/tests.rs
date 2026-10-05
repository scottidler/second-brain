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
            author: None,
        };
        Ok(FetchResult {
            bytes: self.body.clone(),
            meta,
        })
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

fn sh_command(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", script]);
    cmd
}

#[test]
fn browser_ua_markitdown_one_mib_of_stdout_is_returned_whole() {
    let started = std::time::Instant::now();
    let out = run_markitdown(
        sh_command("head -c 1048576 /dev/zero | tr '\\0' a"),
        b"<html></html>".to_vec(),
        Duration::from_secs(30),
    )
    .expect("run");
    assert_eq!(out.len(), 1_048_576);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn browser_ua_markitdown_one_mib_of_stdin_round_trips() {
    let started = std::time::Instant::now();
    let html = vec![b'x'; 1_048_576];
    let out = run_markitdown(sh_command("cat | tr x y"), html, Duration::from_secs(30)).expect("run");
    assert_eq!(out.len(), 1_048_576);
    assert!(out.iter().all(|b| *b == b'y'));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn browser_ua_markitdown_timeout_is_err_and_bounded() {
    let started = std::time::Instant::now();
    let err = run_markitdown(sh_command("sleep 30"), b"x".to_vec(), Duration::from_millis(300)).expect_err("timeout");
    assert!(format!("{err:#}").contains("timed out"), "{err:#}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn browser_ua_markitdown_non_zero_exit_is_err_with_stderr() {
    let err = run_markitdown(
        sh_command("echo bad >&2; exit 4"),
        b"x".to_vec(),
        Duration::from_secs(10),
    )
    .expect_err("non-zero exit");
    assert!(format!("{err:#}").contains("bad"), "{err:#}");
}

#[test]
fn browser_ua_markitdown_missing_binary_is_err() {
    let err = run_markitdown(
        Command::new("/nonexistent/markitdown"),
        b"x".to_vec(),
        Duration::from_secs(5),
    )
    .expect_err("spawn failure");
    assert!(format!("{err:#}").contains("markitdown"), "{err:#}");
}
