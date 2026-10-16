//! Where borg's gemini key check runs, end to end: the built `sb` binary as a
//! subprocess with a temp `borg.yml` (`XDG_CONFIG_HOME`), a temp receipts dir
//! (`XDG_DATA_HOME`) and both key env vars removed.
//!
//! `Config::validate` runs before every `sb borg` subcommand, so it must not
//! read the environment: a shell without the daemon's `borg.env` still runs
//! `sb borg log`. Daemon start (`serve_init`) is where an empty key under
//! `engine: gemini` with no fallback is refused. Implementation audit round 1
//! of docs/design/2026-10-16-youtube-gemini-engine.md.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Hard cap on one subprocess: a daemon that wrongly starts is killed here
/// and the test fails, rather than the suite hanging.
const PROCESS_CAP: Duration = Duration::from_secs(30);

const GEMINI_NO_FALLBACK: &str = "youtube:\n  engine: gemini\n";

fn sb(root: &Path, borg_yml: &str, args: &[&str]) -> Output {
    let config_home = root.join("config");
    std::fs::create_dir_all(config_home.join("sb")).unwrap();
    std::fs::write(config_home.join("sb").join("borg.yml"), borg_yml).unwrap();
    let data_home = root.join("data");
    std::fs::create_dir_all(&data_home).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sb"))
        .args(args)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env_remove("GEMINI_API_KEY")
        .env_remove("YOUTUBE_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sb");
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > PROCESS_CAP {
            child.kill().unwrap();
            panic!("sb {args:?} still running after {PROCESS_CAP:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn borg_log_loads_a_gemini_config_with_empty_keys() {
    let root = tempfile::tempdir().unwrap();
    let out = sb(root.path(), GEMINI_NO_FALLBACK, &["borg", "log", "--limit", "1"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit {:?}, stderr: {stderr}", out.status);
    assert!(!stderr.contains("resolve empty"), "no key error or WARN: {stderr}");
}

#[test]
fn borg_log_with_a_gemini_fallback_does_not_warn_about_keys() {
    let root = tempfile::tempdir().unwrap();
    let yml = "youtube:\n  engine: gemini\n  fallback: yt-dlp\n";
    let out = sb(root.path(), yml, &["borg", "log", "--limit", "1"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit {:?}, stderr: {stderr}", out.status);
    assert!(
        !stderr.contains("resolve empty"),
        "no key WARN off the daemon path: {stderr}"
    );
}

#[test]
fn borg_daemon_start_refuses_a_gemini_config_with_empty_keys() {
    let root = tempfile::tempdir().unwrap();
    let out = sb(root.path(), GEMINI_NO_FALLBACK, &["borg", "daemon", "--start"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("youtube.gemini.api-key (GEMINI_API_KEY)"), "{stderr}");
    assert!(
        stderr.contains("youtube.data-api.api-key (YOUTUBE_API_KEY)"),
        "{stderr}"
    );
}

#[test]
fn structural_youtube_errors_still_fail_every_borg_command() {
    let root = tempfile::tempdir().unwrap();
    let yml = "youtube:\n  engine: gemini\n  fallback: gemini\n";
    let out = sb(root.path(), yml, &["borg", "log", "--limit", "1"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "fallback == engine must fail: {stderr}");
    assert!(stderr.contains("youtube.fallback"), "{stderr}");
}
