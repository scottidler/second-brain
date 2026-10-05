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

/// Answers one request with chunked headers, then one `.\n` chunk every
/// `every` for `chunks` chunks (or never, when `chunks` is 0), then ends.
async fn chunked_listener(every: Duration, chunks: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        sock.write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n")
            .await
            .expect("headers");
        if chunks == 0 {
            tokio::time::sleep(Duration::from_secs(30)).await;
            return;
        }
        for _ in 0..chunks {
            tokio::time::sleep(every).await;
            sock.write_all(b"2\r\n.\n\r\n").await.expect("chunk");
        }
        sock.write_all(b"0\r\n\r\n").await.expect("end");
    });
    port
}

fn stream_timeouts(read: Duration) -> Timeouts {
    Timeouts::Stream {
        connect: Duration::from_secs(5),
        read,
    }
}

#[tokio::test]
async fn a_stream_client_has_no_total_timeout() {
    // 8 chunks 100 ms apart: 800 ms of body, far past the 300 ms read bound,
    // but no single read waits longer than 100 ms.
    let port = chunked_listener(Duration::from_millis(100), 8).await;
    let c = builder(stream_timeouts(Duration::from_millis(300)))
        .build()
        .expect("client");
    let mut resp = c.get(format!("http://127.0.0.1:{port}/")).send().await.expect("send");
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.expect("a steady stream must not time out") {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body, b".\n".repeat(8));
}

#[tokio::test]
async fn a_stream_client_times_out_a_stalled_read() {
    let port = chunked_listener(Duration::ZERO, 0).await;
    let c = builder(stream_timeouts(Duration::from_millis(300)))
        .build()
        .expect("client");
    let started = std::time::Instant::now();
    let mut resp = c
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .expect("headers arrive");
    let err = resp
        .chunk()
        .await
        .expect_err("a stalled body must hit the read timeout");
    assert!(err.is_timeout(), "expected a timeout, got {err}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_total_client_cuts_a_steady_stream_at_its_total() {
    // The contrast that makes `Stream` necessary: the same steady stream under
    // a 300 ms total is cut off.
    let port = chunked_listener(Duration::from_millis(100), 8).await;
    let c = client(Duration::from_millis(300)).expect("client");
    let mut resp = c.get(format!("http://127.0.0.1:{port}/")).send().await.expect("send");
    let mut result = Ok(());
    loop {
        match resp.chunk().await {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    let err = result.expect_err("a total timeout must cut the stream");
    assert!(err.is_timeout(), "expected a timeout, got {err}");
}
