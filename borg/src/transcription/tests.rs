#![allow(clippy::unwrap_used)]

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[test]
fn test_client_construction() {
    let client = TranscriptionClient::new(
        "http://localhost:8090",
        Some("test-key".to_string()),
        "whisper-large-v3",
        120,
    );
    assert_eq!(client.transcriber_url, "http://localhost:8090");
    assert_eq!(client.groq_model, "whisper-large-v3");
    assert_eq!(client.timeout, Duration::from_secs(120));
    assert!(client.groq_url.contains("api.groq.com"));
}

#[test]
fn test_client_without_groq_key() {
    let client = TranscriptionClient::new("http://localhost:8090", None, "whisper-large-v3", 120);
    assert!(client.groq_api_key.is_none());
}

#[test]
fn test_retry_backoff_uses_server_retry_after() {
    let d = retry_backoff(0, Some(Duration::from_secs(3)));
    assert_eq!(d, Duration::from_secs(3));
}

#[test]
fn test_retry_backoff_caps_server_retry_after() {
    // Server requests an unreasonable wait; we cap at GROQ_BACKOFF_CAP.
    let d = retry_backoff(0, Some(Duration::from_secs(3600)));
    assert_eq!(d, GROQ_BACKOFF_CAP);
}

#[test]
fn test_retry_backoff_exponential_when_no_server_hint() {
    assert_eq!(retry_backoff(0, None), Duration::from_secs(1));
    assert_eq!(retry_backoff(1, None), Duration::from_secs(2));
    assert_eq!(retry_backoff(2, None), Duration::from_secs(4));
}

/// A tiny in-process HTTP server that lets us script status codes for the
/// successive Groq POSTs. Returns `(url, request_count)` - the count is the
/// authoritative attempt counter for the test.
async fn spawn_mock_groq(responses: Vec<(u16, Option<u64>)>) -> (String, Arc<AtomicU32>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}/transcribe");
    let count = Arc::new(AtomicU32::new(0));
    let count_for_handler = count.clone();
    let plan = Arc::new(responses);

    tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let count = count_for_handler.clone();
            let plan = plan.clone();
            tokio::spawn(async move {
                // Drain the request: read until we've seen the headers, then
                // pull the body bytes per Content-Length. Multipart bodies
                // can be large; we read in chunks until done.
                let mut buf = Vec::with_capacity(4096);
                let mut tmp = [0u8; 4096];
                let mut headers_seen = false;
                let mut content_length: usize = 0;
                let mut header_end_idx: usize = 0;
                while !headers_seen {
                    match socket.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            if let Some(idx) = find_subseq(&buf, b"\r\n\r\n") {
                                header_end_idx = idx + 4;
                                let headers = std::str::from_utf8(&buf[..idx]).unwrap_or("");
                                for line in headers.lines() {
                                    if line.to_ascii_lowercase().starts_with("content-length:")
                                        && let Some(v) = line.split(':').nth(1)
                                    {
                                        content_length = v.trim().parse().unwrap_or(0);
                                    }
                                }
                                headers_seen = true;
                            }
                        }
                    }
                }
                while buf.len() - header_end_idx < content_length {
                    match socket.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }

                let attempt = count.fetch_add(1, Ordering::SeqCst) as usize;
                let (status, retry_after) = plan
                    .get(attempt)
                    .copied()
                    .unwrap_or_else(|| plan.last().copied().unwrap_or((200, None)));

                let body = if status == 200 {
                    "{\"text\": \"transcribed\", \"language\": \"en\", \"duration\": 1.0}".to_string()
                } else {
                    format!("error status {status}")
                };
                let mut response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Length: {len}\r\nContent-Type: application/json\r\n",
                    len = body.len(),
                );
                if let Some(ra) = retry_after {
                    response.push_str(&format!("Retry-After: {ra}\r\n"));
                }
                response.push_str("Connection: close\r\n\r\n");
                response.push_str(&body);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (url, count)
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[tokio::test]
async fn test_try_groq_retries_on_429_and_eventually_succeeds() {
    // First attempt: 429 with Retry-After: 1; second attempt: 200 OK.
    let (url, count) = spawn_mock_groq(vec![(429, Some(1)), (200, None)]).await;
    let client = TranscriptionClient::with_groq_url("http://unused", &url, Some("k".to_string()), "m", 30);
    let resp = client
        .try_groq(b"audio bytes", &AudioFormat::Mp3, &None)
        .await
        .expect("should succeed after retry");
    assert_eq!(resp.text, "transcribed");
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_try_groq_gives_up_after_max_retries() {
    // Always 503; we should exhaust GROQ_MAX_RETRIES + 1 attempts then error.
    let (url, count) = spawn_mock_groq(vec![(503, None)]).await;
    let client = TranscriptionClient::with_groq_url("http://unused", &url, Some("k".to_string()), "m", 30);
    let result = client.try_groq(b"audio bytes", &AudioFormat::Mp3, &None).await;
    assert!(result.is_err());
    assert_eq!(count.load(Ordering::SeqCst), GROQ_MAX_RETRIES + 1);
}

#[tokio::test]
async fn test_try_groq_does_not_retry_on_4xx_other_than_429() {
    // 400 Bad Request - not retriable, should fail on first attempt.
    let (url, count) = spawn_mock_groq(vec![(400, None)]).await;
    let client = TranscriptionClient::with_groq_url("http://unused", &url, Some("k".to_string()), "m", 30);
    let result = client.try_groq(b"audio bytes", &AudioFormat::Mp3, &None).await;
    assert!(result.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
