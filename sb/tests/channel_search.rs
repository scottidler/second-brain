//! `sb borg channel-search` input refusal end to end: the built `sb` binary as
//! a subprocess with an empty PATH, so a handle that slipped past validation
//! would fail to spawn yt-dlp (a different error) rather than reach YouTube.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Command, Output, Stdio};

fn run_channel_search(args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("borg.yml");
    std::fs::write(&config, "hotkey:\n  host: 127.0.0.1\n  port: 1\n").unwrap();
    let empty_path = home.path().join("bin");
    std::fs::create_dir(&empty_path).unwrap();
    Command::new(env!("CARGO_BIN_EXE_sb"))
        .arg("borg")
        .arg("-c")
        .arg(&config)
        .arg("channel-search")
        .args(args)
        .env("PATH", &empty_path)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn bad_handle_exits_1_naming_the_pattern() {
    let out = run_channel_search(&["letsgetrusty", "tokio", "--format", "json"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("invalid channel handle") && stderr.contains("^@[A-Za-z0-9._-]+$"),
        "stderr: {stderr}"
    );
    assert!(out.stdout.is_empty(), "no result on refusal");
}

#[test]
fn valid_handle_without_yt_dlp_fails_at_the_spawn() {
    // The control for the test above: same setup, valid handle, and the
    // failure moves to the yt-dlp spawn.
    let out = run_channel_search(&["@letsgetrusty", "tokio", "--limit", "3"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("yt-dlp could not run"), "stderr: {stderr}");
}

#[test]
fn limit_out_of_range_is_refused() {
    let out = run_channel_search(&["@letsgetrusty", "tokio", "--limit", "51"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("invalid limit 51"), "stderr: {stderr}");
}
