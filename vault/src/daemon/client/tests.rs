use super::*;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Copy)]
enum Behavior {
    /// Reads the request, never answers.
    Silent,
    /// Sends headers promising a body, then stalls.
    HeadersThenStall,
    /// Waits, then answers 200 with `{"ok":true}`.
    AnswerAfter(Duration),
    /// Answers `401` with a non-JSON body.
    Unauthorized,
    /// Answers `503` with the body `busy`.
    Unavailable,
}

/// Binds 127.0.0.1:0; returns the port and every raw request seen.
async fn stub(behavior: Behavior) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                log.lock()
                    .expect("lock")
                    .push(String::from_utf8_lossy(&buf[..n]).to_string());
                match behavior {
                    Behavior::Silent => tokio::time::sleep(Duration::from_secs(30)).await,
                    Behavior::HeadersThenStall => {
                        let _ = sock
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{",
                            )
                            .await;
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                    Behavior::AnswerAfter(d) => {
                        tokio::time::sleep(d).await;
                        let _ = sock
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}",
                            )
                            .await;
                    }
                    Behavior::Unauthorized => {
                        let _ = sock
                            .write_all(
                                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\nConnection: close\r\n\r\nnope, no way",
                            )
                            .await;
                    }
                    Behavior::Unavailable => {
                        let _ = sock
                            .write_all(
                                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbusy",
                            )
                            .await;
                    }
                }
            });
        }
    });
    (port, seen)
}

fn hotkey(port: u16, timeout: Duration) -> HotkeyConfig {
    HotkeyConfig {
        host: "127.0.0.1".to_string(),
        port,
        request_timeout: timeout,
        ..HotkeyConfig::default()
    }
}

#[tokio::test]
async fn silent_listener_is_an_error_within_twice_the_timeout() {
    let (port, _) = stub(Behavior::Silent).await;
    let timeout = Duration::from_millis(400);
    let client = DaemonClient::new(&hotkey(port, timeout), None).expect("client");
    let started = Instant::now();
    let err = client.get("/queue", &[], None).await.expect_err("must time out");
    assert!(started.elapsed() < timeout * 2, "took {:?}", started.elapsed());
    assert!(matches!(err, DaemonError::Unreachable { .. }), "{err}");
}

#[tokio::test]
async fn a_body_that_stalls_after_headers_is_an_error_within_twice_the_timeout() {
    let (port, _) = stub(Behavior::HeadersThenStall).await;
    let timeout = Duration::from_millis(400);
    let client = DaemonClient::new(&hotkey(port, timeout), None).expect("client");
    let started = Instant::now();
    let resp = client.get("/queue", &[], None).await.expect("headers arrive");
    let err = client
        .json::<serde_json::Value>("/queue", resp)
        .await
        .expect_err("body must time out");
    assert!(started.elapsed() < timeout * 2, "took {:?}", started.elapsed());
    assert!(matches!(err, DaemonError::Parse { .. }), "{err}");
}

#[tokio::test]
async fn configured_request_timeout_is_the_timeout_used() {
    let one_second = Duration::from_secs(1);
    let (slow, _) = stub(Behavior::AnswerAfter(Duration::from_secs(2))).await;
    let client = DaemonClient::new(&hotkey(slow, one_second), None).expect("client");
    assert!(
        client.get("/queue", &[], None).await.is_err(),
        "a 2 s stall must fail a 1 s timeout"
    );

    let (quick, _) = stub(Behavior::AnswerAfter(Duration::from_millis(500))).await;
    let client = DaemonClient::new(&hotkey(quick, one_second), None).expect("client");
    let resp = client.get("/queue", &[], None).await.expect("0.5 s delay succeeds");
    let value: serde_json::Value = client.json("/queue", resp).await.expect("json");
    assert_eq!(value["ok"], true);
}

#[tokio::test]
async fn a_per_request_timeout_tightens_the_configured_one() {
    let (port, _) = stub(Behavior::AnswerAfter(Duration::from_secs(2))).await;
    let client = DaemonClient::new(&hotkey(port, Duration::from_secs(10)), None).expect("client");
    let started = Instant::now();
    let result = client.get("/queue", &[], Some(Duration::from_millis(300))).await;
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn a_401_with_a_non_json_body_names_the_401_not_a_parse_failure() {
    let (port, _) = stub(Behavior::Unauthorized).await;
    let client = DaemonClient::new(&hotkey(port, Duration::from_secs(5)), None).expect("client");
    let resp = client.get("/queue", &[], None).await.expect("answered");
    let err = client
        .json::<serde_json::Value>("/queue", resp)
        .await
        .expect_err("401 is an error");
    let msg = err.to_string();
    assert!(matches!(err, DaemonError::Unauthorized { .. }), "{msg}");
    assert!(msg.contains("rejected the request (401)"), "{msg}");
    assert!(!msg.to_lowercase().contains("parse"), "{msg}");
}

#[tokio::test]
async fn a_non_success_status_quotes_the_status_and_a_body_preview() {
    let (port, _) = stub(Behavior::Unavailable).await;
    let client = DaemonClient::new(&hotkey(port, Duration::from_secs(5)), None).expect("client");
    let resp = client.get("/x", &[], None).await.expect("answered");
    let err = client.json::<serde_json::Value>("/x", resp).await.expect_err("503");
    assert!(
        matches!(&err, DaemonError::Status { status: 503, body, .. } if body == "busy"),
        "{err}"
    );
}

#[tokio::test]
async fn the_bearer_token_is_sent_when_configured() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, "s3cret\n").expect("write");
    let (port, seen) = stub(Behavior::AnswerAfter(Duration::ZERO)).await;
    let client = DaemonClient::new(
        &hotkey(port, Duration::from_secs(5)),
        Some(token_file.to_str().expect("utf8")),
    )
    .expect("client");
    client
        .post_json("/ingest", &serde_json::json!({"url": "x"}))
        .await
        .expect("answered");
    let req = seen.lock().expect("lock")[0].to_lowercase();
    assert!(req.starts_with("post /ingest"), "{req}");
    assert!(req.contains("authorization: bearer s3cret"), "{req}");
}

#[tokio::test]
async fn query_pairs_are_encoded_into_the_url() {
    let (port, seen) = stub(Behavior::AnswerAfter(Duration::ZERO)).await;
    let client = DaemonClient::new(&hotkey(port, Duration::from_secs(5)), None).expect("client");
    client
        .get("/queue", &[("batch", "a b&c")], None)
        .await
        .expect("answered");
    let req = seen.lock().expect("lock")[0].clone();
    assert!(req.starts_with("GET /queue?batch=a+b%26c "), "{req}");
}

#[tokio::test]
async fn connection_refused_reports_is_connect_and_the_address() {
    let client = DaemonClient::new(&hotkey(1, Duration::from_secs(2)), None).expect("client");
    let err = client.get("/queue", &[], None).await.expect_err("refused");
    assert!(err.is_connect(), "{err}");
    assert!(err.to_string().contains("127.0.0.1:1"), "{err}");
}

#[test]
fn an_unparseable_host_is_a_bad_address() {
    let hk = HotkeyConfig {
        host: "bad host".to_string(),
        ..HotkeyConfig::default()
    };
    let client = DaemonClient::new(&hk, None).expect("client");
    assert!(matches!(client.url("/queue", &[]), Err(DaemonError::BadAddress { .. })));
}
