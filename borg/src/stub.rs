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
    /// A `/ingest` daemon: answers `Queued` with a fresh trace id, except a
    /// request whose body contains `drop_marker`, which is dropped unanswered
    /// (a transport failure on the client's POST).
    IngestDropping { drop_marker: String },
    /// An ntfy subscription: chunked headers, an `open` event, then a
    /// `keepalive` event every `every`, for 30 s.
    NtfyKeepalives { every: Duration },
    /// A bulk-capable `/ingest` daemon: `/health` lists `features`, `/ingest`
    /// answers `Queued` with a fresh trace id, and `POST /batch/{id}/seal`
    /// answers `seal_status` (e.g. `200 OK`, `409 Conflict`).
    Bulk {
        features: Vec<String>,
        seal_status: &'static str,
    },
    /// A daemon with the distill routes: `/health`, `POST /distill`,
    /// `GET /trace/{id}`, `GET /distill/{id}` and `GET /distill/{id}/file/..`.
    Distill(DistillStub),
}

/// What the distill stub reports and serves.
#[derive(Clone)]
pub(crate) struct DistillStub {
    /// `/health` `features`.
    pub features: Vec<String>,
    pub trace_id: String,
    /// `received`, `succeeded` or `failed`: what `/trace` and the manifest say.
    pub state: &'static str,
    /// The scratch tree the manifest lists and the file route serves.
    pub tree: std::path::PathBuf,
    /// Manifest entries with no file behind them (a hostile path, say).
    pub extra_manifest_paths: Vec<String>,
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
                    Behavior::Json(body) => json_reply("200 OK", body).into_bytes(),
                    Behavior::RequireToken { token, body } => {
                        if request
                            .to_lowercase()
                            .contains(&format!("authorization: bearer {}", token.to_lowercase()))
                        {
                            json_reply("200 OK", body).into_bytes()
                        } else {
                            "HTTP/1.1 401 Unauthorized\r\ncontent-length: 13\r\nconnection: close\r\n\r\nunauthorized\n"
                                .as_bytes()
                                .to_vec()
                        }
                    }
                    Behavior::IngestDropping { drop_marker } => {
                        if request.contains(drop_marker.as_str()) {
                            return;
                        }
                        let n = log.lock().expect("stub log").len();
                        json_reply(
                            "200 OK",
                            &serde_json::json!({
                                "status": "Queued", "note_path": null, "title": null, "tags": [],
                                "trace_id": format!("stub-trace-{n}"),
                            })
                            .to_string(),
                        )
                        .into_bytes()
                    }
                    Behavior::NtfyKeepalives { every } => {
                        stream_ntfy_keepalives(&mut sock, *every).await;
                        return;
                    }
                    Behavior::Distill(stub) => distill_reply(stub, &request),
                    Behavior::Bulk { features, seal_status } => {
                        let n = log.lock().expect("stub log").len();
                        bulk_reply(features, seal_status, &request, n).into_bytes()
                    }
                    Behavior::Unauthorized => {
                        "HTTP/1.1 401 Unauthorized\r\ncontent-length: 13\r\nconnection: close\r\n\r\nunauthorized\n"
                            .as_bytes()
                            .to_vec()
                    }
                };
                let _ = sock.write_all(&reply).await;
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

fn bulk_reply(features: &[String], seal_status: &str, request: &str, n: usize) -> String {
    let first_line = request.lines().next().unwrap_or_default();
    if first_line.starts_with("GET /health") {
        return json_reply(
            "200 OK",
            &serde_json::json!({ "status": "ok", "service": "stub", "version": "0.0.0", "features": features })
                .to_string(),
        );
    }
    if first_line.starts_with("POST /batch/") {
        return json_reply(seal_status, r#"{"batch":"stub"}"#);
    }
    json_reply(
        "200 OK",
        &serde_json::json!({
            "status": "Queued", "note_path": null, "title": null, "tags": [],
            "trace_id": format!("stub-trace-{n}"),
        })
        .to_string(),
    )
}

fn json_reply(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn bytes_reply(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut reply = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    reply.extend_from_slice(body);
    reply
}

fn distill_reply(stub: &DistillStub, request: &str) -> Vec<u8> {
    let line = request.lines().next().unwrap_or_default();
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    let not_found = || {
        bytes_reply(
            "404 Not Found",
            "application/json",
            br#"{"error":"stub: no such route"}"#,
        )
    };
    let json = |value: serde_json::Value| json_reply("200 OK", &value.to_string()).into_bytes();
    let trace = &stub.trace_id;
    match (method, path) {
        ("GET", "/health") => json(serde_json::json!({
            "status": "ok", "service": "stub", "version": "v0.0.0", "features": stub.features,
        })),
        ("POST", "/distill") => json(serde_json::json!({
            "status": "Queued", "note_path": null, "title": null, "tags": [], "trace_id": trace,
        })),
        ("GET", p) if p == format!("/trace/{trace}") => json(serde_json::json!({
            "found": true, "trace_id": trace, "status": stub.state,
            "failure_stage": (stub.state == "failed").then_some("publish-failed"),
            "failure_reason": (stub.state == "failed").then_some("stub failure"),
            "note_path": null, "source-durable": false,
        })),
        ("GET", p) if p == format!("/distill/{trace}") => {
            let mut files: Vec<serde_json::Value> = crate::scratch::regular_files(&stub.tree)
                .expect("stub tree walk")
                .into_iter()
                .map(|(_, f)| serde_json::json!({"path": f.path, "size": f.size}))
                .collect();
            files.extend(
                stub.extra_manifest_paths
                    .iter()
                    .map(|p| serde_json::json!({"path": p, "size": 1})),
            );
            json(serde_json::json!({
                "trace-id": trace, "status": stub.state,
                "failure-stage": (stub.state == "failed").then_some("publish-failed"),
                "failure-reason": (stub.state == "failed").then_some("stub failure"),
                "files": files,
            }))
        }
        ("GET", p) if p.starts_with(&format!("/distill/{trace}/file/")) => {
            let raw = &p[format!("/distill/{trace}/file/").len()..];
            let decoded = urlencoding::decode(raw).map(|c| c.into_owned()).unwrap_or_default();
            match std::fs::read(stub.tree.join(&decoded)) {
                Ok(bytes) => bytes_reply("200 OK", "application/octet-stream", &bytes),
                Err(_) => not_found(),
            }
        }
        _ => not_found(),
    }
}
