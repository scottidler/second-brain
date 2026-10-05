//! Test-only stub daemon: one TCP listener on `127.0.0.1:0` whose behavior the
//! test picks, so every daemon-client entry point is exercised against a
//! silent listener, a stalled body, a 401, or a token-requiring daemon.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone)]
pub(crate) enum Behavior {
    /// Reads the request, never answers.
    Silent,
    /// Sends headers promising a body, then stalls.
    HeadersThenStall,
    /// Answers 200 with this JSON body.
    Json(String),
    /// Answers 401 unless the request carries `Authorization: Bearer <token>`,
    /// then answers 200 with the JSON body.
    RequireToken { token: String, body: String },
    /// Answers 401 with a non-JSON body.
    Unauthorized,
    /// An ntfy subscription: chunked headers, an `open` event, then a
    /// `keepalive` event every `every`, for 30 s.
    NtfyKeepalives { every: Duration },
}

/// Starts the stub; returns its port and the raw text of every request seen.
pub(crate) async fn serve(behavior: Behavior) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let port = listener.local_addr().expect("stub addr").port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = Arc::clone(&log);
            let behavior = behavior.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                log.lock().expect("stub log").push(request.clone());
                let reply = match &behavior {
                    Behavior::Silent => {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return;
                    }
                    Behavior::HeadersThenStall => {
                        let _ = sock
                            .write_all(
                                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{",
                            )
                            .await;
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return;
                    }
                    Behavior::Json(body) => json_reply("200 OK", body),
                    Behavior::RequireToken { token, body } => {
                        if request
                            .to_lowercase()
                            .contains(&format!("authorization: bearer {}", token.to_lowercase()))
                        {
                            json_reply("200 OK", body)
                        } else {
                            "HTTP/1.1 401 Unauthorized\r\ncontent-length: 13\r\nconnection: close\r\n\r\nunauthorized\n"
                                .to_string()
                        }
                    }
                    Behavior::NtfyKeepalives { every } => {
                        stream_ntfy_keepalives(&mut sock, *every).await;
                        return;
                    }
                    Behavior::Unauthorized => {
                        "HTTP/1.1 401 Unauthorized\r\ncontent-length: 13\r\nconnection: close\r\n\r\nunauthorized\n"
                            .to_string()
                    }
                };
                let _ = sock.write_all(reply.as_bytes()).await;
            });
        }
    });
    (port, seen)
}

async fn stream_ntfy_keepalives(sock: &mut tokio::net::TcpStream, every: Duration) {
    let headers = "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ntransfer-encoding: chunked\r\n\r\n";
    if sock.write_all(headers.as_bytes()).await.is_err() {
        return;
    }
    let started = std::time::Instant::now();
    let mut n = 0u32;
    while started.elapsed() < Duration::from_secs(30) {
        let event = if n == 0 { "open" } else { "keepalive" };
        let line = format!("{{\"id\":\"k{n}\",\"event\":\"{event}\"}}\n");
        let chunk = format!("{:x}\r\n{line}\r\n", line.len());
        if sock.write_all(chunk.as_bytes()).await.is_err() {
            return;
        }
        n += 1;
        tokio::time::sleep(every).await;
    }
}

fn json_reply(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}
