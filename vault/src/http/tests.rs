use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn client_enforces_its_total_timeout_on_a_silent_listener() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
        let _ = sock.write_all(b"").await;
    });
    let c = client(Duration::from_millis(300)).expect("client");
    let started = std::time::Instant::now();
    let err = c
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .expect_err("must time out");
    assert!(err.is_timeout(), "expected a timeout, got {err}");
    assert!(started.elapsed() < Duration::from_secs(2));
}
