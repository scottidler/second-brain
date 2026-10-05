use super::*;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One-shot stub daemon: answers a single request with `status` + `body`,
/// handing back the raw request text so tests can assert on headers.
async fn stub(status: &str, body: &str) -> (u16, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        let mut buf = vec![0u8; 4096];
        let n = sock.read(&mut buf).await.expect("read");
        sock.write_all(response.as_bytes()).await.expect("write");
        String::from_utf8_lossy(&buf[..n]).to_string()
    });
    (port, handle)
}

fn view_for(port: u16) -> BorgView {
    BorgView {
        hotkey: HotkeyConfig {
            host: "127.0.0.1".to_string(),
            port,
            ..HotkeyConfig::default()
        },
        server: ServerView::default(),
        tags: TagsView::default(),
    }
}

#[tokio::test]
async fn fetch_returns_daemon_json_verbatim() {
    let body = r#"{"state":"draining","batch":{"id":"abc","total":2}}"#;
    let (port, req) = stub("200 OK", body).await;
    let v = fetch(&view_for(port)).await.expect("fetch");
    assert_eq!(v, serde_json::from_str::<serde_json::Value>(body).unwrap());
    assert!(req.await.unwrap().starts_with("GET /queue "));
}

#[tokio::test]
async fn fetch_sends_bearer_when_token_configured() {
    // SAFETY: unique var name, set before any read of it in this process.
    unsafe { std::env::set_var("ORACLE_QUEUE_TEST_TOKEN", "s3cret") };
    let (port, req) = stub("200 OK", r#"{"state":"idle"}"#).await;
    let mut view = view_for(port);
    view.server.auth_token = Some("ORACLE_QUEUE_TEST_TOKEN".to_string());
    fetch(&view).await.expect("fetch");
    assert!(
        req.await
            .unwrap()
            .to_lowercase()
            .contains("authorization: bearer s3cret")
    );
}

#[tokio::test]
async fn closed_port_is_an_error_naming_the_address_not_idle() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let err = fetch(&view_for(port)).await.expect_err("must fail");
    let msg = err.to_string();
    assert!(msg.contains(&format!("127.0.0.1:{port}")), "{msg}");
    assert!(!msg.contains("idle"), "{msg}");
}

#[tokio::test]
async fn http_500_is_an_error_naming_the_address() {
    let (port, _req) = stub("500 Internal Server Error", r#"{"error":"db down"}"#).await;
    let msg = fetch(&view_for(port)).await.unwrap_err().to_string();
    assert!(msg.contains(&format!("127.0.0.1:{port}")), "{msg}");
    assert!(msg.contains("500"), "{msg}");
    assert!(msg.contains("db down"), "{msg}");
}

#[tokio::test]
async fn http_404_says_daemon_predates_queue() {
    let (port, _req) = stub("404 Not Found", "").await;
    let msg = fetch(&view_for(port)).await.unwrap_err().to_string();
    assert!(msg.contains("predates /queue"), "{msg}");
    assert!(msg.contains(&format!("127.0.0.1:{port}")), "{msg}");
}

#[tokio::test]
async fn garbage_body_is_an_error_naming_the_address() {
    let (port, _req) = stub("200 OK", "not json").await;
    let msg = fetch(&view_for(port)).await.unwrap_err().to_string();
    assert!(msg.contains("unparseable"), "{msg}");
    assert!(msg.contains(&format!("127.0.0.1:{port}")), "{msg}");
}

#[tokio::test]
async fn silent_daemon_hits_the_request_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _hold = tokio::spawn(async move {
        let (_sock, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let mut view = view_for(port);
    view.hotkey.request_timeout = Duration::from_millis(300);
    let started = Instant::now();
    let msg = fetch(&view).await.unwrap_err().to_string();
    assert!(
        started.elapsed() < Duration::from_millis(600),
        "took {:?}",
        started.elapsed()
    );
    assert!(msg.contains(&format!("127.0.0.1:{port}")), "{msg}");
}

#[tokio::test]
async fn headers_then_a_stalled_body_hits_the_request_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _hold = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let _ = sock.read(&mut buf).await;
        let _ = sock
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{")
            .await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let mut view = view_for(port);
    view.hotkey.request_timeout = Duration::from_millis(300);
    let started = Instant::now();
    let msg = fetch(&view).await.unwrap_err().to_string();
    assert!(
        started.elapsed() < Duration::from_millis(600),
        "took {:?}",
        started.elapsed()
    );
    assert!(msg.contains(&format!("127.0.0.1:{port}")), "{msg}");
}

#[tokio::test]
async fn a_401_with_a_non_json_body_names_the_401_not_a_parse_error() {
    let (port, _req) = stub("401 Unauthorized", "unauthorized").await;
    let msg = fetch(&view_for(port)).await.unwrap_err().to_string();
    assert!(msg.contains("(401)"), "{msg}");
    assert!(!msg.contains("unparseable"), "{msg}");
}

#[test]
fn load_view_reads_hotkey_and_auth_token_ignoring_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("borg.yml");
    std::fs::write(
        &path,
        "hotkey:\n  host: desk.example\n  port: 9191\nserver:\n  host: 0.0.0.0\n  auth-token: TOK\nllm:\n  model: x\n",
    )
    .unwrap();
    let view = load_view(Some(&path)).expect("load");
    assert_eq!(view.hotkey.host, "desk.example");
    assert_eq!(view.hotkey.port, 9191);
    assert_eq!(view.server.auth_token.as_deref(), Some("TOK"));
}

#[test]
fn load_view_errors_on_a_missing_explicit_file() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load_view(Some(&dir.path().join("nope.yml"))).is_err());
}
