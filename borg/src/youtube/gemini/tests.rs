#![allow(clippy::unwrap_used)]

use super::*;
use std::sync::{Arc, Mutex};

const HHMMSS: &str = include_str!("../fixtures/gemini-transcript-hhmmss.json");
const CLIP_MMSS: &str = include_str!("../fixtures/gemini-clip-mmss.json");
const OUT_OF_ORDER: &str = include_str!("../fixtures/gemini-out-of-order.json");
const OUT_OF_WINDOW: &str = include_str!("../fixtures/gemini-out-of-window.json");
const NO_TEXT: &str = include_str!("../fixtures/gemini-no-text.json");
const RETRY_400: &str = include_str!("../fixtures/gemini-retry-400.json");

const KEY: &str = "GEMINI-SECRET-do-not-leak-456";

#[derive(Clone)]
enum Reply {
    Respond(u16, String),
    /// Accept the request and never answer.
    Hang,
}

/// Every request received: the raw head and the body.
type Requests = Arc<Mutex<Vec<(String, String)>>>;

/// Serves `replies` in order on a loopback port, one per request; requests
/// past the end get the last reply. Returns the base URL and every request.
async fn spawn_stub(replies: Vec<Reply>) -> (String, Requests) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Requests = Arc::new(Mutex::new(Vec::new()));
    let seen_for_task = seen.clone();
    let replies = Arc::new(replies);
    tokio::spawn(async move {
        let mut served = 0usize;
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let reply = replies[served.min(replies.len() - 1)].clone();
            served += 1;
            let seen = seen_for_task.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let head_end = loop {
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                    match socket.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let length = head
                    .lines()
                    .find_map(|l| {
                        let (name, value) = l.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + length {
                    match socket.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
                seen.lock().unwrap().push((head, body));
                match reply {
                    Reply::Hang => {
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                    }
                    Reply::Respond(status, body) => {
                        let response = format!(
                            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                        let _ = socket.shutdown().await;
                    }
                }
            });
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

fn config(window: Duration, timeout: Duration, budget: Duration) -> YoutubeGeminiConfig {
    YoutubeGeminiConfig {
        window,
        timeout,
        budget,
        ..YoutubeGeminiConfig::default()
    }
}

fn default_config() -> YoutubeGeminiConfig {
    config(
        Duration::from_secs(1800),
        Duration::from_secs(10),
        Duration::from_secs(30),
    )
}

fn client(base: &str, key: &str, config: &YoutubeGeminiConfig) -> GeminiClient {
    GeminiClient::with_base_url(key.to_string(), config, base).unwrap()
}

fn ok(body: &str) -> Reply {
    Reply::Respond(200, body.to_string())
}

/// A completed response whose model text is `text`.
fn response_with(text: &str) -> String {
    json!({
        "status": "completed",
        "model": "gemini-3.8-flash",
        "steps": [{"type": "model_output", "content": [{"type": "text", "text": text}]}],
    })
    .to_string()
}

fn fixture_text(fixture: &str) -> String {
    extract_text(fixture).unwrap().text.unwrap()
}

const FULL: ClipWindow = ClipWindow {
    start_secs: 0,
    end_secs: 1800,
    clipped: true,
};

// --- window planner ---

#[test]
fn planner_splits_at_the_window_length() {
    let windows = plan_windows(3513.0, Duration::from_secs(1800));
    let spans: Vec<(u64, u64)> = windows.iter().map(|w| (w.start_secs, w.end_secs)).collect();
    assert_eq!(spans, vec![(0, 1800), (1800, 3513)]);
    assert!(windows.iter().all(|w| w.clipped));
}

#[test]
fn planner_short_video_is_one_unclipped_window() {
    let windows = plan_windows(512.0, Duration::from_secs(1800));
    assert_eq!(
        windows,
        vec![ClipWindow {
            start_secs: 0,
            end_secs: 512,
            clipped: false
        }]
    );
    let request = build_request("m", "abc", &windows[0]);
    let processing = &request["input"][0]["processing"];
    assert_eq!(processing, &json!({"type": "static"}), "offsets must be omitted");
}

#[test]
fn planner_edges() {
    let w = Duration::from_secs(1800);
    assert_eq!(plan_windows(1800.0, w).len(), 1, "exactly one window long");
    assert!(!plan_windows(1800.0, w)[0].clipped);
    let spans: Vec<(u64, u64)> = plan_windows(3600.0, w)
        .iter()
        .map(|w| (w.start_secs, w.end_secs))
        .collect();
    assert_eq!(spans, vec![(0, 1800), (1800, 3600)]);
    assert_eq!(plan_windows(5400.5, w).last().unwrap().end_secs, 5401);
    assert!(plan_windows(0.0, w).is_empty());
    assert!(plan_windows(-3.0, w).is_empty());
}

// --- request builder ---

#[test]
fn request_is_static_with_second_offsets_and_watch_uri() {
    let window = ClipWindow {
        start_secs: 1800,
        end_secs: 3513,
        clipped: true,
    };
    let request = build_request("gemini-3.8-flash", "BJq73Hecbbo", &window);
    assert_eq!(
        request,
        json!({
            "model": "gemini-3.8-flash",
            "input": [
                {
                    "type": "video",
                    "uri": "https://www.youtube.com/watch?v=BJq73Hecbbo",
                    "processing": {"type": "static", "start_offset": "1800s", "end_offset": "3513s"},
                },
                {"type": "text", "text": TRANSCRIPT_PROMPT},
            ],
        })
    );
}

// --- steps extractor ---

#[test]
fn extractor_joins_model_output_text_only() {
    let text = fixture_text(HHMMSS);
    assert!(text.starts_with("[00:00:00] Welcome back"));
    assert!(!text.contains("Transcribing the video."), "thought steps are not text");

    let body = json!({
        "status": "completed",
        "steps": [
            {"type": "model_output", "content": [
                {"type": "text", "text": "[00:00:01] a"},
                {"type": "image", "data": "zzz"},
                {"type": "text", "text": "[00:00:02] b"},
            ]},
            {"type": "thought", "content": [{"type": "text", "text": "hmm"}]},
            {"type": "model_output", "content": [{"type": "text", "text": "[00:00:03] c"}]},
        ],
    })
    .to_string();
    assert_eq!(
        extract_text(&body).unwrap().text.unwrap(),
        "[00:00:01] a\n[00:00:02] b\n[00:00:03] c"
    );
}

#[test]
fn extractor_reports_the_served_model() {
    let body = HHMMSS.replace("gemini-3.8-flash", "gemini-served-model-probe");
    let output = extract_text(&body).unwrap();
    assert_eq!(output.served_model.as_deref(), Some("gemini-served-model-probe"));
    let body = HHMMSS.replace(r#""model": "gemini-3.8-flash","#, "");
    assert_eq!(extract_text(&body).unwrap().served_model, None);
}

#[test]
fn extractor_status_not_completed_is_response() {
    for status in [r#""status": "failed""#, r#""status": "in_progress""#] {
        let body = HHMMSS.replace(r#""status": "completed""#, status);
        let err = extract_text(&body).unwrap_err();
        assert!(matches!(err, FetchError::Response(_)), "{err}");
    }
    let err = extract_text(r#"{"steps": []}"#).unwrap_err();
    assert!(matches!(err, FetchError::Response(_)), "absent status: {err}");
    let err = extract_text("<html>").unwrap_err();
    assert!(matches!(err, FetchError::Response(_)), "{err}");
}

#[test]
fn extractor_completed_no_text_is_none() {
    assert_eq!(extract_text(NO_TEXT).unwrap().text, None);
    assert_eq!(extract_text(&response_with("  \n ")).unwrap().text, None);
}

// --- line parser ---

#[test]
fn parser_hhmmss_fixture_with_continuation() {
    let segments = parse_window(&fixture_text(HHMMSS), &FULL).unwrap();
    assert_eq!(segments.len(), 5);
    assert_eq!(segments[0], (0.0, "Welcome back to the channel.".to_string()));
    assert_eq!(
        segments[1],
        (
            4.0,
            "Today we look at Rust error handling, starting with the question mark operator.".to_string()
        )
    );
    assert_eq!(segments[4], (510.0, "Thanks for watching.".to_string()));
}

#[test]
fn parser_mmss_normalizes_to_absolute_seconds() {
    let window = ClipWindow {
        start_secs: 3000,
        end_secs: 3300,
        clipped: true,
    };
    let segments = parse_window("[50:00] x", &window).unwrap();
    assert_eq!(segments, vec![(3000.0, "x".to_string())]);

    let segments = parse_window(&fixture_text(CLIP_MMSS), &window).unwrap();
    let times: Vec<f64> = segments.iter().map(|(t, _)| *t).collect();
    assert_eq!(times, vec![3000.0, 3007.0, 3150.0, 3298.0]);
}

#[test]
fn parser_accepts_all_three_shapes() {
    let window = ClipWindow {
        start_secs: 0,
        end_secs: 40_000,
        clipped: true,
    };
    let text = "[05:07] a\n[1:02:03] b\n[10:00:00] c\n[75:00] d";
    let err = parse_window(text, &window).unwrap_err();
    // [75:00] after [10:00:00] goes backwards: one of four lines, over 2%.
    assert!(matches!(err, FetchError::Response(_)), "{err}");
    let segments = parse_window("[05:07] a\n[1:02:03] b\n[75:00] d\n[10:00:00] c", &window).unwrap();
    let times: Vec<f64> = segments.iter().map(|(t, _)| *t).collect();
    assert_eq!(times, vec![307.0, 3723.0, 4500.0, 36_000.0]);
}

#[test]
fn parser_rejects_invalid_fields() {
    for line in ["[00:61:00] x", "[1:60:00] x", "[00:00:60] x", "[12:99] x"] {
        let err = parse_window(line, &FULL).unwrap_err();
        assert!(matches!(err, FetchError::Response(_)), "{line}: {err}");
    }
}

#[test]
fn parser_text_before_first_timestamp_is_response() {
    let err = parse_window("Here is the transcript:\n[00:00:01] hi", &FULL).unwrap_err();
    assert!(matches!(err, FetchError::Response(_)), "{err}");
    assert!(err.to_string().contains("before the first timestamp"));
}

#[test]
fn parser_drops_blank_lines_and_empty_segments() {
    let segments = parse_window("\n[00:00:01]\n\n[00:00:02] two\n   \n", &FULL).unwrap();
    assert_eq!(segments, vec![(2.0, "two".to_string())]);
    let segments = parse_window("[00:00:01]\nfilled later", &FULL).unwrap();
    assert_eq!(segments, vec![(1.0, "filled later".to_string())]);
}

#[test]
fn parser_one_out_of_order_in_100_is_repaired() {
    let text = fixture_text(OUT_OF_ORDER);
    assert_eq!(text.lines().count(), 100);
    let segments = parse_window(&text, &FULL).unwrap();
    assert_eq!(segments.len(), 100);
    assert!(
        segments.windows(2).all(|p| p[0].0 <= p[1].0),
        "no decreasing timestamp after repair"
    );
    assert_eq!(segments[60].0, segments[59].0, "raised to the previous timestamp");
    assert_eq!(segments[60].1, "line 60 out of order");
}

#[test]
fn parser_three_out_of_order_in_100_is_response() {
    let mut lines: Vec<String> = fixture_text(OUT_OF_ORDER).lines().map(str::to_string).collect();
    lines[20] = "[00:00:02] back again".to_string();
    lines[80] = "[00:00:03] and again".to_string();
    let err = parse_window(&lines.join("\n"), &FULL).unwrap_err();
    assert!(matches!(err, FetchError::Response(_)), "{err}");
    assert!(err.to_string().contains("3 of 100"), "{err}");
}

#[test]
fn parser_two_in_100_is_exactly_the_limit_and_ok() {
    let mut lines: Vec<String> = fixture_text(OUT_OF_ORDER).lines().map(str::to_string).collect();
    lines[20] = "[00:00:02] back again".to_string();
    assert!(parse_window(&lines.join("\n"), &FULL).is_ok());
}

#[test]
fn parser_timestamp_outside_window_is_response() {
    let err = parse_window(&fixture_text(OUT_OF_WINDOW), &FULL).unwrap_err();
    assert!(matches!(err, FetchError::Response(_)), "{err}");
    assert!(err.to_string().contains("2400s outside the window"), "{err}");
}

#[test]
fn parser_bounds_allow_five_seconds_of_slack() {
    let window = ClipWindow {
        start_secs: 1800,
        end_secs: 3600,
        clipped: true,
    };
    assert!(parse_window("[00:29:55] a\n[01:00:05] b", &window).is_ok());
    assert!(parse_window("[00:29:54] a", &window).is_err());
    assert!(parse_window("[01:00:06] a", &window).is_err());
}

#[test]
fn plain_text_joins_segment_texts() {
    let transcript = GeminiTranscript {
        segments: vec![(0.0, "one".to_string()), (4.0, "two".to_string())],
    };
    assert_eq!(transcript.plain_text(), "one\ntwo");
}

// --- client against the stub ---

#[tokio::test]
async fn fetch_short_video_is_one_unclipped_request() {
    let (base, seen) = spawn_stub(vec![ok(HHMMSS)]).await;
    let transcript = client(&base, KEY, &default_config())
        .fetch_transcript("abcdefghijk", 512.0, Instant::now())
        .await
        .unwrap();
    assert_eq!(transcript.segments.len(), 5);
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let (head, body) = &requests[0];
    assert!(head.starts_with("POST /interactions "), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains(&format!("x-goog-api-key: {}", KEY.to_ascii_lowercase())),
        "key must ride the header"
    );
    assert!(
        !head.lines().next().unwrap().contains(KEY),
        "key must not be in the URL"
    );
    let sent: serde_json::Value = serde_json::from_str(body).unwrap();
    let window = plan_windows(512.0, Duration::from_secs(1800))[0];
    assert_eq!(
        sent,
        build_request(crate::config::DEFAULT_GEMINI_MODEL, "abcdefghijk", &window)
    );
}

#[tokio::test]
async fn fetch_long_video_runs_windows_sequentially_and_concatenates() {
    let first = response_with("[00:00:00] first half\n[00:29:59] end of first");
    let second = response_with("[30:00] second half\n[58:31] last line");
    let (base, seen) = spawn_stub(vec![Reply::Respond(200, first), Reply::Respond(200, second)]).await;
    let transcript = client(&base, KEY, &default_config())
        .fetch_transcript("abcdefghijk", 3513.0, Instant::now())
        .await
        .unwrap();
    let times: Vec<f64> = transcript.segments.iter().map(|(t, _)| *t).collect();
    assert_eq!(times, vec![0.0, 1799.0, 1800.0, 3511.0]);
    assert_eq!(
        transcript.plain_text(),
        "first half\nend of first\nsecond half\nlast line"
    );
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let offsets: Vec<(String, String)> = requests
        .iter()
        .map(|(_, body)| {
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            let p = &v["input"][0]["processing"];
            (
                p["start_offset"].as_str().unwrap().to_string(),
                p["end_offset"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        offsets,
        vec![
            ("0s".to_string(), "1800s".to_string()),
            ("1800s".to_string(), "3513s".to_string())
        ]
    );
}

#[tokio::test]
async fn retry_400_please_retry_then_200_succeeds_after_two_requests() {
    let (base, seen) = spawn_stub(vec![Reply::Respond(400, RETRY_400.to_string()), ok(HHMMSS)]).await;
    let transcript = client(&base, KEY, &default_config())
        .fetch_transcript("abcdefghijk", 512.0, Instant::now())
        .await
        .unwrap();
    assert_eq!(transcript.segments.len(), 5);
    assert_eq!(seen.lock().unwrap().len(), 2, "exactly two requests");
}

#[tokio::test]
async fn retry_is_once_per_window() {
    let (base, seen) = spawn_stub(vec![Reply::Respond(503, "{}".to_string())]).await;
    let err = client(&base, KEY, &default_config())
        .fetch_transcript("abcdefghijk", 512.0, Instant::now())
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Service(_)), "{err}");
    assert_eq!(seen.lock().unwrap().len(), 2, "one attempt plus one retry");
}

#[tokio::test]
async fn retryable_failures_are_retried() {
    let cases: Vec<(Reply, &str)> = vec![
        (Reply::Respond(500, "{}".to_string()), "5xx"),
        (Reply::Respond(429, "{}".to_string()), "429"),
        (ok(OUT_OF_WINDOW), "parse bounds"),
        (
            ok(&HHMMSS.replace(r#""status": "completed""#, r#""status": "failed""#)),
            "status",
        ),
        (ok("<html>"), "malformed body"),
    ];
    for (first, label) in cases {
        let (base, seen) = spawn_stub(vec![first, ok(HHMMSS)]).await;
        let window_text = client(&base, KEY, &default_config())
            .fetch_transcript("abcdefghijk", 1800.0, Instant::now())
            .await;
        assert!(window_text.is_ok(), "{label}: {window_text:?}");
        assert_eq!(seen.lock().unwrap().len(), 2, "{label}");
    }
}

#[tokio::test]
async fn non_retryable_failures_fail_after_one_request() {
    let cases: Vec<(u16, String, &str)> = vec![
        (400, r#"{"error":{"code":400,"message":"bad request","status":"INVALID_ARGUMENT"}}"#.to_string(), "response"),
        (
            400,
            r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"reason":"API_KEY_INVALID"}]}}"#.to_string(),
            "auth",
        ),
        (403, r#"{"error":{"code":403,"message":"denied","status":"PERMISSION_DENIED"}}"#.to_string(), "auth"),
        (401, "{}".to_string(), "auth"),
        (404, "{}".to_string(), "response"),
    ];
    for (status, body, want) in cases {
        let (base, seen) = spawn_stub(vec![Reply::Respond(status, body), ok(HHMMSS)]).await;
        let err = client(&base, KEY, &default_config())
            .fetch_transcript("abcdefghijk", 512.0, Instant::now())
            .await
            .unwrap_err();
        let got = match err {
            FetchError::Auth(_) => "auth",
            FetchError::Response(_) => "response",
            ref other => panic!("{status}: unexpected {other}"),
        };
        assert_eq!(got, want, "status {status}");
        assert_eq!(seen.lock().unwrap().len(), 1, "status {status}: no retry");
    }
}

#[tokio::test]
async fn completed_no_text_window_is_zero_segments_and_all_empty_is_empty() {
    let (base, seen) = spawn_stub(vec![ok(NO_TEXT)]).await;
    let err = client(&base, KEY, &default_config())
        .fetch_transcript("jTWbGuJcSI8", 7.0, Instant::now())
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Empty), "{err}");
    assert!(!err.is_permanent());
    assert_eq!(seen.lock().unwrap().len(), 1, "a silent window is Ok, not retried");
}

#[tokio::test]
async fn one_silent_window_among_spoken_ones_is_ok() {
    let spoken = response_with("[00:00:01] spoken");
    let (base, _) = spawn_stub(vec![Reply::Respond(200, spoken), ok(NO_TEXT)]).await;
    let transcript = client(&base, KEY, &default_config())
        .fetch_transcript("abcdefghijk", 3513.0, Instant::now())
        .await
        .unwrap();
    assert_eq!(transcript.segments, vec![(1.0, "spoken".to_string())]);
}

#[tokio::test]
async fn stub_that_never_answers_is_timeout_at_budget() {
    let (base, _) = spawn_stub(vec![Reply::Hang]).await;
    let budget = Duration::from_millis(400);
    let config = config(Duration::from_secs(1800), Duration::from_secs(60), budget);
    let started = Instant::now();
    let err = client(&base, KEY, &config)
        .fetch_transcript("abcdefghijk", 512.0, started)
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(matches!(err, FetchError::Timeout), "{err}");
    assert!(!err.is_permanent());
    assert!(elapsed >= budget, "{elapsed:?}");
    assert!(elapsed < budget + Duration::from_secs(2), "{elapsed:?}");
}

#[tokio::test]
async fn budget_is_measured_from_the_callers_start() {
    let (base, seen) = spawn_stub(vec![ok(HHMMSS)]).await;
    let config = config(
        Duration::from_secs(1800),
        Duration::from_secs(10),
        Duration::from_millis(100),
    );
    let started = Instant::now() - Duration::from_millis(200);
    let err = client(&base, KEY, &config)
        .fetch_transcript("abcdefghijk", 512.0, started)
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Timeout), "{err}");
    assert!(seen.lock().unwrap().is_empty(), "no request once the budget is spent");
}

#[tokio::test]
async fn per_call_timeout_is_a_network_error_without_retry() {
    let (base, seen) = spawn_stub(vec![Reply::Hang]).await;
    let config = config(
        Duration::from_secs(1800),
        Duration::from_millis(200),
        Duration::from_secs(30),
    );
    let started = Instant::now();
    let err = client(&base, KEY, &config)
        .fetch_transcript("abcdefghijk", 512.0, started)
        .await
        .unwrap_err();
    assert!(
        matches!(err, FetchError::Network(ref m) if m.contains("per-call timeout")),
        "{err}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn retry_that_would_pass_the_budget_is_timeout() {
    let (base, seen) = spawn_stub(vec![Reply::Respond(503, "{}".to_string()), Reply::Hang]).await;
    let budget = Duration::from_millis(400);
    let config = config(Duration::from_secs(1800), Duration::from_secs(60), budget);
    let err = client(&base, KEY, &config)
        .fetch_transcript("abcdefghijk", 512.0, Instant::now())
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Timeout), "{err}");
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn empty_key_is_auth_before_any_request() {
    let (base, seen) = spawn_stub(vec![ok(HHMMSS)]).await;
    for key in ["", "  "] {
        let err = client(&base, key, &default_config())
            .fetch_transcript("abcdefghijk", 512.0, Instant::now())
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Auth(_)), "{err}");
    }
    assert!(seen.lock().unwrap().is_empty(), "no HTTP request may be made");
}

#[test]
fn unresolvable_key_env_var_is_an_empty_key() {
    let config = YoutubeGeminiConfig {
        api_key: "BORG_TEST_GEMINI_API_KEY_NEVER_SET".to_string(),
        ..YoutubeGeminiConfig::default()
    };
    let client = GeminiClient::from_config(&config).unwrap();
    assert!(client.api_key.is_empty());
}

#[tokio::test]
async fn zero_duration_is_response_before_any_request() {
    let (base, seen) = spawn_stub(vec![ok(HHMMSS)]).await;
    let err = client(&base, KEY, &default_config())
        .fetch_transcript("abcdefghijk", 0.0, Instant::now())
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Response(_)), "{err}");
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn key_is_absent_from_every_error_display() {
    let replies = vec![
        Reply::Respond(400, RETRY_400.to_string()),
        Reply::Respond(401, "{}".to_string()),
        Reply::Respond(403, "{}".to_string()),
        Reply::Respond(429, "{}".to_string()),
        Reply::Respond(503, "{}".to_string()),
        Reply::Respond(200, "<html>".to_string()),
        Reply::Respond(200, OUT_OF_WINDOW.to_string()),
    ];
    for reply in replies {
        let (base, _) = spawn_stub(vec![reply]).await;
        let err = client(&base, KEY, &default_config())
            .fetch_transcript("abcdefghijk", 1800.0, Instant::now())
            .await
            .unwrap_err();
        assert!(!format!("{err}").contains(KEY), "{err}");
        assert!(!format!("{err:?}").contains(KEY), "{err:?}");
    }
    let (base, _) = spawn_stub(vec![Reply::Hang]).await;
    let short = config(
        Duration::from_secs(1800),
        Duration::from_millis(100),
        Duration::from_secs(30),
    );
    let err = client(&base, KEY, &short)
        .fetch_transcript("abcdefghijk", 512.0, Instant::now())
        .await
        .unwrap_err();
    assert!(!format!("{err}").contains(KEY), "{err}");

    let closed = vault::testnet::closed_port();
    let dead = client(&format!("http://127.0.0.1:{}", closed.port()), KEY, &default_config())
        .fetch_transcript("abcdefghijk", 512.0, Instant::now())
        .await
        .unwrap_err();
    assert!(matches!(dead, FetchError::Network(_)), "{dead}");
    assert!(!dead.to_string().contains(KEY));
}
