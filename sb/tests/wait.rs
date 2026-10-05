//! `sb borg wait` end to end: the built `sb` binary as a subprocess against a
//! stub HTTP daemon on 127.0.0.1, pointed there by a temp `borg.yml`
//! (`hotkey.host/port`). Never touches the live daemon. One test per Phase 5
//! success criterion in `docs/design/2026-10-04-ingest-queue-status.md`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BATCH: &str = "20261004-215943-9f01";

/// Hard cap on one subprocess; a hang past this fails the test instead of the suite.
const PROCESS_CAP: Duration = Duration::from_secs(30);

enum Behavior {
    /// Serve these in order (last one repeats): plain `/queue`, then `?batch=`.
    Script {
        plain: Vec<(u16, Value)>,
        batched: Vec<(u16, Value)>,
    },
    /// Accept every connection and never answer.
    Hang,
}

struct Stub {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

fn start_stub(behavior: Behavior) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    std::thread::spawn(move || {
        let mut held: Vec<TcpStream> = Vec::new();
        let (mut plain_n, mut batched_n) = (0usize, 0usize);
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            match &behavior {
                Behavior::Hang => held.push(stream),
                Behavior::Script { plain, batched } => {
                    let target = read_request_target(&mut stream);
                    seen.lock().unwrap().push(target.clone());
                    let (status, body) = if target.contains("batch=") {
                        batched_n += 1;
                        &batched[(batched_n - 1).min(batched.len() - 1)]
                    } else {
                        plain_n += 1;
                        &plain[(plain_n - 1).min(plain.len() - 1)]
                    };
                    let body = body.to_string();
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(reply.as_bytes());
                }
            }
        }
    });
    Stub { port, requests }
}

/// The request target (`/queue?batch=...`) from the request line.
fn read_request_target(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    text.split_whitespace().nth(1).unwrap_or_default().to_string()
}

fn idle() -> Value {
    json!({"state": "idle"})
}

fn summary(total: u64, succeeded: u64, failed: u64, processing: u64, wedged: u64) -> Value {
    json!({
        "id": BATCH,
        "started": "2026-10-04T21:59:43Z",
        "elapsed-secs": 30,
        "total": total,
        "done": succeeded + failed,
        "remaining": processing + wedged,
        "succeeded": succeeded,
        "failed": failed,
        "queued": 0,
        "processing": processing,
        "wedged": wedged,
    })
}

fn item(trace: &str, state: &str) -> Value {
    json!({"trace": trace, "state": state, "source": "https://www.youtube.com/watch?v=x"})
}

fn draining(total: u64, succeeded: u64, processing: u64) -> Value {
    let items: Vec<Value> = (0..processing).map(|i| item(&format!("t{i}"), "processing")).collect();
    json!({"state": "draining", "batch": summary(total, succeeded, 0, processing, 0), "items": items})
}

fn drained(total: u64, failed: u64) -> Value {
    let mut snap = json!({"state": "idle", "batch": summary(total, total - failed, failed, 0, 0)});
    if failed > 0 {
        snap["items"] = json!([item("tfail", "failed")]);
    }
    snap
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    elapsed: Duration,
}

fn write_config(dir: &Path, port: u16) -> std::path::PathBuf {
    let path = dir.join("borg.yml");
    std::fs::write(&path, format!("hotkey:\n  host: 127.0.0.1\n  port: {port}\n")).unwrap();
    path
}

fn run_wait(port: u16, extra: &[&str]) -> Run {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), port);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sb"));
    cmd.arg("borg")
        .arg("-c")
        .arg(&config)
        .arg("wait")
        .args(["--format", "json"])
        .args(extra)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for var in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        cmd.env_remove(var);
    }
    let started = Instant::now();
    let mut child = cmd.spawn().unwrap();
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if started.elapsed() > PROCESS_CAP {
            child.kill().unwrap();
            panic!("sb borg wait still running after {PROCESS_CAP:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let elapsed = started.elapsed();
    let mut stdout = String::new();
    let mut stderr = String::new();
    child.stdout.take().unwrap().read_to_string(&mut stdout).unwrap();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    Run {
        code,
        stdout,
        stderr,
        elapsed,
    }
}

/// stdout is exactly one JSON snapshot line.
fn final_snapshot(run: &Run) -> Value {
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(lines.len(), 1, "expected exactly one stdout line, got {:?}", run.stdout);
    serde_json::from_str(lines[0]).unwrap()
}

fn script(plain: Vec<Value>, batched: Vec<Value>) -> Stub {
    start_stub(Behavior::Script {
        plain: plain.into_iter().map(|v| (200, v)).collect(),
        batched: batched.into_iter().map(|v| (200, v)).collect(),
    })
}

#[test]
fn wait_idle_exits_zero_and_prints_idle() {
    let stub = script(vec![idle()], vec![idle()]);
    let run = run_wait(stub.port, &[]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "{\"state\":\"idle\"}\n");
    assert_eq!(*stub.requests.lock().unwrap(), vec!["/queue".to_string()]);
}

#[test]
fn wait_drains_clean_exits_zero_polling_the_pinned_batch() {
    let stub = script(vec![draining(2, 0, 2)], vec![draining(2, 1, 1), drained(2, 0)]);
    let run = run_wait(stub.port, &[]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let snap = final_snapshot(&run);
    assert_eq!(snap["state"], "idle");
    assert_eq!(snap["batch"]["failed"], 0);
    let requests = stub.requests.lock().unwrap().clone();
    assert_eq!(requests[0], "/queue");
    assert!(requests.len() >= 3, "{requests:?}");
    for target in &requests[1..] {
        assert_eq!(target, &format!("/queue?batch={BATCH}"));
    }
}

#[test]
fn wait_drains_with_last_item_failed_exits_three() {
    let stub = script(vec![draining(2, 1, 1)], vec![drained(2, 1)]);
    let run = run_wait(stub.port, &[]);
    assert_eq!(run.code, Some(3), "stderr: {}", run.stderr);
    let snap = final_snapshot(&run);
    assert_eq!(snap["batch"]["failed"], 1);
    assert_eq!(snap["items"][0]["state"], "failed");
}

#[test]
fn wait_wedged_item_exits_four() {
    let mut wedged = json!({"state": "draining", "batch": summary(2, 1, 0, 0, 1)});
    wedged["items"] = json!([item("tstuck", "wedged")]);
    let stub = script(vec![draining(2, 0, 2)], vec![wedged]);
    let run = run_wait(stub.port, &[]);
    assert_eq!(run.code, Some(4), "stderr: {}", run.stderr);
    assert_eq!(final_snapshot(&run)["items"][0]["state"], "wedged");
}

#[test]
fn wait_never_drains_exits_five_at_timeout_with_last_snapshot() {
    let stub = script(vec![draining(2, 0, 2)], vec![draining(2, 0, 2)]);
    let run = run_wait(stub.port, &["--timeout", "3s"]);
    assert_eq!(run.code, Some(5), "stderr: {}", run.stderr);
    assert_eq!(final_snapshot(&run)["state"], "draining");
    assert!(run.elapsed < Duration::from_secs(8), "took {:?}", run.elapsed);
}

#[test]
fn wait_stub_never_responds_exits_five_not_a_hang() {
    let stub = start_stub(Behavior::Hang);
    let run = run_wait(stub.port, &["--timeout", "3s"]);
    assert_eq!(run.code, Some(5), "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "", "no snapshot was ever received");
    assert!(run.stderr.contains("timeout"), "stderr: {}", run.stderr);
    assert!(run.elapsed < Duration::from_secs(8), "took {:?}", run.elapsed);
}

#[test]
fn wait_http_500_exits_one() {
    let stub = start_stub(Behavior::Script {
        plain: vec![(500, json!({"error": "db open failed"}))],
        batched: vec![(500, json!({"error": "db open failed"}))],
    });
    let run = run_wait(stub.port, &[]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("HTTP 500"), "stderr: {}", run.stderr);
}

#[test]
fn wait_closed_port_exits_one() {
    // Bound but never listening: a connect is refused, and the port stays
    // reserved for the whole test. Binding a listener and dropping it freed the
    // port, and a parallel test's `start_stub` could be handed it and answer
    // "drained" (exit 0).
    let reserved = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
    reserved
        .bind(&std::net::SocketAddr::from(([127, 0, 0, 1], 0)).into())
        .unwrap();
    let port = reserved.local_addr().unwrap().as_socket().unwrap().port();
    let run = run_wait(port, &[]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr.contains(&format!("127.0.0.1:{port}")),
        "stderr: {}",
        run.stderr
    );
}

#[test]
fn wait_bogus_flag_is_clap_exit_two() {
    let run = run_wait(1, &["--bogus"]);
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
}

#[test]
fn wait_new_item_joining_the_pinned_batch_is_waited_for_and_counted() {
    // Plain read: batch of 2. A third click joins it before the first poll;
    // wait must keep going until all 3 finish and report total 3.
    let stub = script(
        vec![draining(2, 0, 2)],
        vec![draining(3, 1, 2), draining(3, 2, 1), drained(3, 0)],
    );
    let run = run_wait(stub.port, &[]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let snap = final_snapshot(&run);
    assert_eq!(snap["state"], "idle");
    assert_eq!(snap["batch"]["total"], 3);
    assert_eq!(stub.requests.lock().unwrap().len(), 4);
}
