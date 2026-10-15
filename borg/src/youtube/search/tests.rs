use super::*;

/// Captured `yt-dlp --flat-playlist -J --playlist-end 10
/// "https://www.youtube.com/@letsgetrusty/search?query=tokio"` (2026-10-15,
/// yt-dlp 2026.08.19), trimmed to the fields the parser reads. Entry 3 is a
/// playlist (`playlist?list=`), the case the filter exists for.
const FLAT_SEARCH_FIXTURE: &[u8] = include_bytes!("../fixtures/flat-search-letsgetrusty-tokio.json");

fn fixture_path() -> String {
    format!(
        "{}/src/youtube/fixtures/flat-search-letsgetrusty-tokio.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A program that cannot exist: spawning it fails, so reaching the spawn is
/// observable as `Spawn`.
fn missing_program() -> Command {
    Command::new("/nonexistent/yt-dlp-sentinel")
}

/// `sh -c <script>` as the "yt-dlp": the search arguments land in `$1..` and
/// are ignored by the script.
fn sh(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", script, "yt-dlp"]);
    cmd
}

#[test]
fn bad_handle_is_rejected_before_yt_dlp_runs() {
    for handle in [
        "letsgetrusty",
        "@",
        "@lets/get",
        "@lets get",
        "@lets;rm -rf",
        "@lets?query=x",
        "@lets&x=1",
        "@caf\u{e9}",
        "",
    ] {
        let err = run_search(missing_program(), handle, "tokio", 10, SEARCH_TIMEOUT).expect_err(handle);
        assert!(
            matches!(err, ChannelSearchError::InvalidHandle { .. }),
            "{handle:?} must be refused before spawn, got {err:?}"
        );
        assert!(err.is_invalid_input());
    }
    // The control: a valid handle does reach the spawn, so the refusal above
    // happened before it, not instead of a failed spawn.
    let err = run_search(missing_program(), "@letsgetrusty", "tokio", 10, SEARCH_TIMEOUT).expect_err("spawn fails");
    assert!(matches!(err, ChannelSearchError::Spawn { .. }), "got {err:?}");
    assert!(!err.is_invalid_input());
}

#[test]
fn validate_handle_accepts_the_handle_alphabet() {
    for handle in ["@letsgetrusty", "@a.b_c-1", "@NoBoilerplate"] {
        validate_handle(handle).unwrap_or_else(|e| panic!("{handle}: {e}"));
    }
}

#[test]
fn empty_query_and_out_of_range_limit_are_refused_before_spawn() {
    let empty = run_search(missing_program(), "@x", "  ", 10, SEARCH_TIMEOUT).expect_err("empty");
    assert!(matches!(empty, ChannelSearchError::EmptyQuery), "got {empty:?}");
    for limit in [0, MAX_LIMIT + 1] {
        let err = run_search(missing_program(), "@x", "tokio", limit, SEARCH_TIMEOUT).expect_err("limit");
        assert!(
            matches!(err, ChannelSearchError::InvalidLimit { limit: l } if l == limit),
            "got {err:?}"
        );
    }
}

#[test]
fn search_url_percent_encodes_the_query() {
    assert_eq!(
        search_url("@letsgetrusty", "async & await #1/2?"),
        "https://www.youtube.com/@letsgetrusty/search?query=async%20%26%20await%20%231%2F2%3F"
    );
    assert_eq!(
        search_url("@x", "tokio"),
        "https://www.youtube.com/@x/search?query=tokio"
    );
}

#[test]
fn search_command_passes_the_url_as_one_argument() {
    let url = search_url("@letsgetrusty", "tokio select; rm -rf /");
    let cmd = search_command(Command::new("yt-dlp"), &url, 7);
    assert_eq!(cmd.get_program(), "yt-dlp");
    let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
    assert_eq!(
        args,
        ["--flat-playlist", "-J", "--playlist-end", "7", url.as_str()]
            .map(std::ffi::OsStr::new)
            .to_vec()
    );
}

#[test]
fn parse_hits_keeps_only_watch_entries_in_order() {
    let hits = parse_hits(FLAT_SEARCH_FIXTURE).expect("fixture parses");
    assert_eq!(hits.len(), 9, "10 entries, 1 playlist dropped");
    assert!(hits.iter().all(|h| h.url.contains("watch?v=")));
    assert!(!hits.iter().any(|h| h.url.contains("playlist?list=")));
    let first = &hits[0];
    assert_eq!(first.id, "FUg1y-yv6cs");
    assert_eq!(first.title, "How Rust engineered the perfect async runtime");
    assert_eq!(first.url, "https://www.youtube.com/watch?v=FUg1y-yv6cs");
    assert_eq!(first.duration_secs, Some(488));
    assert_eq!(first.view_count, Some(68792));
    assert_eq!(
        hits[2].id, "K8LNPYNvT-U",
        "order is preserved across the dropped playlist"
    );
}

#[test]
fn parse_hits_fails_loudly_on_a_broken_body() {
    for (body, needle) in [
        (&b"not json"[..], "did not parse"),
        (br#"{"_type":"playlist"}"#, "no `entries` array"),
        (
            br#"{"entries":[{"url":"https://www.youtube.com/watch?v=abc","id":"abc"}]}"#,
            "has no `title`",
        ),
    ] {
        let err = parse_hits(body).expect_err("must fail");
        assert!(matches!(err, ChannelSearchError::Parse { .. }), "got {err:?}");
        assert!(err.to_string().contains(needle), "{err} lacks {needle}");
    }
    let empty = parse_hits(br#"{"entries":[]}"#).expect("empty search parses");
    assert!(empty.is_empty(), "no matches is zero hits, not an error");
}

#[test]
fn run_search_parses_stdout_of_a_successful_run() {
    let mut cmd = sh(r#"cat "$FIXTURE""#);
    cmd.env("FIXTURE", fixture_path());
    let hits = run_search(cmd, "@letsgetrusty", "tokio", 10, SEARCH_TIMEOUT).expect("hits");
    assert_eq!(hits.len(), 9);
}

#[test]
fn run_search_failure_carries_the_stderr_tail() {
    let cmd =
        sh("echo 'WARNING: noise' >&2; echo 'ERROR: [youtube:tab] @nope: This channel does not exist.' >&2; exit 1");
    let err = run_search(cmd, "@nope", "tokio", 10, SEARCH_TIMEOUT).expect_err("fails");
    let tail = err.stderr_tail().expect("an exited failure carries stderr");
    assert!(tail.ends_with("This channel does not exist."), "tail: {tail}");
    assert!(tail.contains("WARNING: noise"));
    assert!(matches!(err, ChannelSearchError::Failed { .. }), "got {err:?}");
    assert!(!err.is_invalid_input());
}

#[test]
fn run_search_times_out_with_no_stderr_tail() {
    let err =
        run_search(sh("sleep 5"), "@letsgetrusty", "tokio", 10, Duration::from_millis(200)).expect_err("times out");
    assert!(matches!(err, ChannelSearchError::TimedOut { .. }), "got {err:?}");
    assert!(err.stderr_tail().is_none());
    assert!(err.to_string().contains("timed out"));
}

#[test]
fn stderr_tail_keeps_the_last_lines_and_caps_chars() {
    let many: String = (1..=15).map(|i| format!("line {i}\n\n")).collect();
    let tail = stderr_tail(many.as_bytes());
    assert_eq!(tail.lines().count(), STDERR_TAIL_LINES);
    assert!(tail.starts_with("line 6") && tail.ends_with("line 15"), "tail: {tail}");

    let long = "x".repeat(STDERR_TAIL_MAX_CHARS + 50) + "END";
    let capped = stderr_tail(long.as_bytes());
    assert_eq!(capped.chars().count(), STDERR_TAIL_MAX_CHARS);
    assert!(capped.ends_with("END"));

    assert_eq!(stderr_tail(b""), "");
}

/// Live: hits YouTube through the real yt-dlp. Run by hand with
/// `cargo test -p borg --lib search_channel_live -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "network: runs the real yt-dlp against YouTube"]
async fn search_channel_live_letsgetrusty_tokio() {
    let hits = search_channel("@letsgetrusty", "tokio", 10).await.expect("live search");
    for hit in &hits {
        println!("{}\t{}\t{}", hit.id, hit.url, hit.title);
    }
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h.url.contains("watch?v=")));
}
