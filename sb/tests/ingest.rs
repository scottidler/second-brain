//! `sb borg ingest <targets>...` end to end: the built `sb` binary as a
//! subprocess against a counting stub daemon on 127.0.0.1, pointed there by a
//! temp `borg.yml`. Never touches the live daemon. One test per success criterion in
//! `docs/design/2026-10-27-multi-target-ingest.md`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// A request body containing this makes the stub drop the connection unanswered.
const DROP_MARKER: &str = "fail.example";

struct Stub {
    port: u16,
    bodies: Arc<Mutex<Vec<String>>>,
}

fn start_stub() -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let body = read_body(&mut stream);
            let n = {
                let mut seen = seen.lock().unwrap();
                seen.push(body.clone());
                seen.len()
            };
            if body.contains(DROP_MARKER) {
                continue;
            }
            let reply =
                format!(r#"{{"status":"Queued","note_path":null,"title":null,"tags":[],"trace_id":"stub-trace-{n}"}}"#);
            let http = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = stream.write_all(http.as_bytes());
        }
    });
    Stub { port, bodies }
}

fn read_body(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = stream.read(&mut chunk).unwrap();
        if n == 0 {
            return String::new();
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .map(|v| v.trim().parse().unwrap())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = stream.read(&mut chunk).unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    String::from_utf8_lossy(&buf[head_end..]).to_string()
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_ingest(port: u16, args: &[&str]) -> Run {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("borg.yml");
    std::fs::write(
        &config,
        format!("hotkey:\n  host: 127.0.0.1\n  port: {port}\n  request-timeout: 5s\n"),
    )
    .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sb"));
    cmd.arg("borg")
        .arg("-c")
        .arg(&config)
        .arg("ingest")
        .args(args)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("BORG_DISABLE_NOTIFICATIONS", "1")
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
    let out = cmd.output().unwrap();
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

#[test]
fn a_nonexistent_path_exits_2_with_zero_posts() {
    let stub = start_stub();
    let run = run_ingest(stub.port, &["https://a.example", "/no/such/urls.txt"]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("/no/such/urls.txt"), "{}", run.stderr);
    assert!(stub.bodies.lock().unwrap().is_empty(), "nothing submitted");
}

#[test]
fn a_note_with_a_file_target_exits_2_with_zero_posts() {
    let stub = start_stub();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.pdf");
    std::fs::write(&file, b"x").unwrap();
    let run = run_ingest(
        stub.port,
        &["https://a.example", "--file", file.to_str().unwrap(), "--note", "why"],
    );
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("--note"), "{}", run.stderr);
    assert!(stub.bodies.lock().unwrap().is_empty(), "nothing submitted");
}

#[test]
fn a_tag_that_swallowed_a_target_exits_2_with_zero_posts() {
    let stub = start_stub();
    let run = run_ingest(stub.port, &["https://a.example", "-t", "ai", "https://b.example"]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("https://b.example"), "{}", run.stderr);
    assert!(stub.bodies.lock().unwrap().is_empty(), "nothing submitted");
}

#[test]
fn clipboard_conflicts_with_targets_and_exits_2() {
    let stub = start_stub();
    let run = run_ingest(stub.port, &["--clipboard", "https://a.example"]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(stub.bodies.lock().unwrap().is_empty());
}

#[test]
fn no_targets_and_no_clipboard_exits_2() {
    let stub = start_stub();
    let run = run_ingest(stub.port, &[]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
}

#[test]
fn three_urls_exit_0_with_one_queued_line_each() {
    let stub = start_stub();
    let run = run_ingest(
        stub.port,
        &["https://a.example", "https://b.example", "https://c.example"],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "https://a.example -> queued stub-trace-1",
            "https://b.example -> queued stub-trace-2",
            "https://c.example -> queued stub-trace-3",
        ]
    );
}

#[test]
fn one_failed_post_of_three_exits_1_and_still_submits_the_rest() {
    let stub = start_stub();
    let run = run_ingest(
        stub.port,
        &["https://a.example", "https://fail.example", "https://c.example"],
    );
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert_eq!(stub.bodies.lock().unwrap().len(), 3, "every URL attempted");
    assert!(run.stdout.contains("https://a.example -> queued"), "{}", run.stdout);
    assert!(run.stdout.contains("https://c.example -> queued"), "{}", run.stdout);
    assert!(run.stderr.contains("https://fail.example -> failed:"), "{}", run.stderr);
}

#[test]
fn a_url_list_file_submits_every_url_in_order() {
    let stub = start_stub();
    let dir = tempfile::tempdir().unwrap();
    let list = dir.path().join("urls.txt");
    std::fs::write(&list, "# scour\nhttps://b.example\n\nhttps://c.example\n").unwrap();
    let run = run_ingest(stub.port, &["https://a.example", list.to_str().unwrap()]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "https://a.example -> queued stub-trace-1",
            "https://b.example -> queued stub-trace-2",
            "https://c.example -> queued stub-trace-3",
        ]
    );
}

#[test]
fn a_malformed_last_list_line_exits_2_with_zero_posts() {
    let stub = start_stub();
    let dir = tempfile::tempdir().unwrap();
    let list = dir.path().join("urls.txt");
    std::fs::write(&list, "https://a.example\nhttps://b.example\nhttps://exa mple.com\n").unwrap();
    let run = run_ingest(stub.port, &[list.to_str().unwrap()]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains(":3: https://exa mple.com"), "{}", run.stderr);
    assert!(stub.bodies.lock().unwrap().is_empty(), "nothing submitted");
}

#[test]
fn the_removed_from_flag_exits_2() {
    let stub = start_stub();
    let run = run_ingest(stub.port, &["--from", "urls.txt"]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(stub.bodies.lock().unwrap().is_empty());
}
