#![allow(clippy::unwrap_used)]

use super::*;
use std::sync::{Arc, Mutex};

const LIVE: &str = include_str!("../fixtures/dataapi-videos-live.json");
const EMPTY: &str = include_str!("../fixtures/dataapi-videos-empty.json");
const UPCOMING: &str = include_str!("../fixtures/dataapi-videos-upcoming.json");
const QUOTA: &str = include_str!("../fixtures/dataapi-quota-403.json");
const CHANNEL_HANDLE: &str = include_str!("../fixtures/dataapi-channels-handle.json");
const CHANNEL_LEGACY: &str = include_str!("../fixtures/dataapi-channels-legacy.json");
/// Recorded live from desk, 2026-10-16 (implementation audit round 1): the
/// real `videos.list` / `channels.list` bodies for `SDLfjrUyBw4`, unedited.
const RECORDED_VIDEO: &str = include_str!("../fixtures/dataapi-videos-recorded.json");
const RECORDED_CHANNEL: &str = include_str!("../fixtures/dataapi-channels-recorded.json");

const KEY: &str = "SECRET-KEY-do-not-leak-123";

type Requests = Arc<Mutex<Vec<String>>>;

/// Serves `(path prefix, status, body)` routes on a loopback port. Returns the
/// base URL and every raw request head received. An unmatched path is a 404.
async fn spawn_stub(routes: Vec<(&'static str, u16, String)>) -> (String, Requests) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Requests = Arc::new(Mutex::new(Vec::new()));
    let seen_for_task = seen.clone();
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let seen = seen_for_task.clone();
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                seen.lock().unwrap().push(head);
                let (status, body) = routes
                    .iter()
                    .find(|(prefix, _, _)| path.starts_with(prefix))
                    .map(|(_, s, b)| (*s, b.clone()))
                    .unwrap_or((404, "{}".to_string()));
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

fn client(base: &str, key: &str) -> DataApiClient {
    DataApiClient::with_base_url(key.to_string(), base, Duration::from_secs(5)).unwrap()
}

#[test]
fn duration_parses_the_spec_examples() {
    assert_eq!(parse_iso8601_duration("PT8M32S"), Some(512));
    assert_eq!(parse_iso8601_duration("P1DT2H"), Some(93_600));
    assert_eq!(parse_iso8601_duration("P0D"), Some(0));
}

#[test]
fn duration_accepts_any_part_omitted() {
    assert_eq!(parse_iso8601_duration("PT45S"), Some(45));
    assert_eq!(parse_iso8601_duration("PT5M"), Some(300));
    assert_eq!(parse_iso8601_duration("PT1H"), Some(3600));
    assert_eq!(parse_iso8601_duration("PT1H30S"), Some(3630));
    assert_eq!(parse_iso8601_duration("P2D"), Some(172_800));
    assert_eq!(parse_iso8601_duration("PT0S"), Some(0));
}

#[test]
fn duration_rejects_malformed_input() {
    for bad in [
        "", "P", "PT", "8M32S", "PT8M32", "PT32S8M", "P1Y", "P1W", "PT1.5S", "PTS", "P1DT", "pt8m",
    ] {
        assert_eq!(parse_iso8601_duration(bad), None, "{bad:?} must not parse");
    }
}

#[tokio::test]
async fn live_fixture_fills_all_nine_fields() {
    let (base, seen) = spawn_stub(vec![
        ("/videos", 200, LIVE.to_string()),
        ("/channels", 200, CHANNEL_HANDLE.to_string()),
    ])
    .await;
    let m = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap();
    assert_eq!(m.title, "21+ Rust Pro Tips (TOP SECRET)");
    assert_eq!(m.uploader, "Let's Get Rusty");
    assert_eq!(m.duration_secs, 512.0);
    assert_eq!(m.description, "Rust tips and tricks.\nLine two.");
    assert_eq!(m.tags, vec!["rust", "programming"]);
    assert_eq!(m.channel_id.as_deref(), Some("UCSp-OaMpsO8K0KkOqyBl7_w"));
    assert_eq!(m.channel_handle.as_deref(), Some("@letsgetrusty"));
    assert_eq!(
        m.channel_url.as_deref(),
        Some("https://www.youtube.com/channel/UCSp-OaMpsO8K0KkOqyBl7_w")
    );
    assert_eq!(m.published_at.as_deref(), Some("2025-11-21"));

    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for head in requests.iter() {
        assert!(
            head.to_ascii_lowercase()
                .contains(&format!("x-goog-api-key: {}", KEY.to_ascii_lowercase())),
            "key must ride the header"
        );
        let request_line = head.lines().next().unwrap();
        assert!(!request_line.contains(KEY), "key must not be in the URL");
    }
}

#[tokio::test]
async fn published_at_is_converted_to_its_utc_date() {
    let body = LIVE.replace("2025-11-21T23:30:05Z", "2025-11-21T23:30:05-05:00");
    let (base, _) = spawn_stub(vec![
        ("/videos", 200, body),
        ("/channels", 200, CHANNEL_HANDLE.to_string()),
    ])
    .await;
    let m = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap();
    assert_eq!(m.published_at.as_deref(), Some("2025-11-22"));
}

#[tokio::test]
async fn absent_tags_are_empty() {
    let body = LIVE.replace(r#""tags": ["rust", "programming"],"#, "");
    let (base, _) = spawn_stub(vec![
        ("/videos", 200, body),
        ("/channels", 200, CHANNEL_HANDLE.to_string()),
    ])
    .await;
    let m = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap();
    assert!(m.tags.is_empty());
}

#[tokio::test]
async fn legacy_custom_url_without_at_is_no_handle() {
    let (base, _) = spawn_stub(vec![
        ("/videos", 200, LIVE.to_string()),
        ("/channels", 200, CHANNEL_LEGACY.to_string()),
    ])
    .await;
    let m = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap();
    assert_eq!(m.channel_handle, None);
    assert!(m.channel_id.is_some());
}

#[tokio::test]
async fn channels_failure_is_not_fatal_and_warns() {
    vault::capture::install();
    let body = LIVE.replace("53XYcpCgQWE", "CHANFAIL001");
    let (base, _) = spawn_stub(vec![("/videos", 200, body), ("/channels", 500, "{}".to_string())]).await;
    let m = client(&base, KEY).fetch_metadata("CHANFAIL001").await.unwrap();
    assert_eq!(m.channel_handle, None);
    assert_eq!(m.title, "21+ Rust Pro Tips (TOP SECRET)");
    let warns = vault::capture::warns_containing("CHANFAIL001");
    assert_eq!(warns.len(), 1, "{warns:?}");
    assert!(warns[0].contains("channels.list"));
}

#[tokio::test]
async fn empty_items_is_not_found_and_permanent() {
    let (base, _) = spawn_stub(vec![("/videos", 200, EMPTY.to_string())]).await;
    let err = client(&base, KEY).fetch_metadata("gone0000000").await.unwrap_err();
    assert!(
        matches!(err, FetchError::NotFound(ref id) if id == "gone0000000"),
        "{err}"
    );
    assert!(err.is_permanent());
}

/// A 200 whose envelope has no `items` at all is a malformed body, not a
/// gone video: `Response`, non-permanent, so the fallback still runs.
#[tokio::test]
async fn body_without_items_is_response_not_not_found() {
    let (base, _) = spawn_stub(vec![("/videos", 200, "{}".to_string())]).await;
    let err = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap_err();
    assert!(matches!(err, FetchError::Response(_)), "{err}");
    assert!(!err.is_permanent());
}

/// The echo case for channels.list: its error is only logged (WARN), and the
/// log line must not carry the key either.
#[tokio::test]
async fn channels_failure_warn_is_redacted() {
    vault::capture::install();
    let body = LIVE.replace("53XYcpCgQWE", "CHANECHO001");
    let echo = format!(r#"{{"error":{{"message":"API key not valid {KEY}","errors":[{{"reason":"keyInvalid"}}]}}}}"#);
    let (base, _) = spawn_stub(vec![("/videos", 200, body), ("/channels", 400, echo)]).await;
    client(&base, KEY).fetch_metadata("CHANECHO001").await.unwrap();
    let warns = vault::capture::warns_containing("CHANECHO001");
    assert_eq!(warns.len(), 1, "{warns:?}");
    assert!(!warns[0].contains(KEY), "{}", warns[0]);
}

#[tokio::test]
async fn upcoming_is_not_live_and_permanent() {
    let (base, seen) = spawn_stub(vec![("/videos", 200, UPCOMING.to_string())]).await;
    let err = client(&base, KEY).fetch_metadata("UPCOMING123").await.unwrap_err();
    assert!(matches!(err, FetchError::NotLive(_)), "{err}");
    assert!(err.is_permanent());
    assert_eq!(seen.lock().unwrap().len(), 1, "no channels.list for a NotLive video");
}

#[tokio::test]
async fn live_broadcast_is_not_live() {
    let body = UPCOMING.replace("upcoming", "live");
    let (base, _) = spawn_stub(vec![("/videos", 200, body)]).await;
    let err = client(&base, KEY).fetch_metadata("UPCOMING123").await.unwrap_err();
    assert!(matches!(err, FetchError::NotLive(_)), "{err}");
}

#[tokio::test]
async fn zero_duration_on_a_finished_video_is_response() {
    let body = LIVE.replace("PT8M32S", "P0D");
    let (base, _) = spawn_stub(vec![("/videos", 200, body)]).await;
    let err = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap_err();
    assert!(matches!(err, FetchError::Response(_)), "{err}");
    assert!(!err.is_permanent());
}

#[tokio::test]
async fn quota_403_is_quota() {
    let (base, _) = spawn_stub(vec![("/videos", 403, QUOTA.to_string())]).await;
    let err = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap_err();
    assert!(matches!(err, FetchError::Quota(_)), "{err}");
    assert!(err.to_string().contains("quotaExceeded"));
}

#[tokio::test]
async fn non_quota_403_is_auth() {
    let body = QUOTA.replace("quotaExceeded", "forbidden");
    let (base, _) = spawn_stub(vec![("/videos", 403, body)]).await;
    let err = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap_err();
    assert!(matches!(err, FetchError::Auth(_)), "{err}");
}

#[tokio::test]
async fn status_429_5xx_and_garbage_classify() {
    let cases = [(429u16, "quota"), (503, "service"), (418, "response")];
    for (status, want) in cases {
        let (base, _) = spawn_stub(vec![("/videos", status, "not json".to_string())]).await;
        let err = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap_err();
        let got = match err {
            FetchError::Quota(_) => "quota",
            FetchError::Service(_) => "service",
            FetchError::Response(_) => "response",
            ref other => panic!("{status}: unexpected {other}"),
        };
        assert_eq!(got, want, "status {status}");
    }
}

#[tokio::test]
async fn malformed_success_body_is_response() {
    let (base, _) = spawn_stub(vec![("/videos", 200, "<html>".to_string())]).await;
    let err = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap_err();
    assert!(matches!(err, FetchError::Response(_)), "{err}");
}

#[tokio::test]
async fn empty_key_is_auth_before_any_request() {
    let (base, seen) = spawn_stub(vec![("/videos", 200, LIVE.to_string())]).await;
    for key in ["", "   "] {
        let err = client(&base, key).fetch_metadata("53XYcpCgQWE").await.unwrap_err();
        assert!(matches!(err, FetchError::Auth(_)), "{err}");
    }
    assert!(seen.lock().unwrap().is_empty(), "no HTTP request may be made");
}

#[test]
fn unresolvable_key_env_var_is_auth_at_fetch() {
    let config = YoutubeDataApiConfig {
        api_key: "BORG_TEST_YT_DATA_API_KEY_NEVER_SET".to_string(),
        ..YoutubeDataApiConfig::default()
    };
    let client = DataApiClient::from_config(&config).unwrap();
    assert!(client.api_key.is_empty());
}

#[tokio::test]
async fn key_is_absent_from_every_error_display() {
    let cases: Vec<(u16, String)> = vec![
        (200, EMPTY.to_string()),
        (200, UPCOMING.to_string()),
        (200, LIVE.replace("PT8M32S", "P0D")),
        (200, "<html>".to_string()),
        (403, QUOTA.to_string()),
        (403, QUOTA.replace("quotaExceeded", "forbidden")),
        (401, "{}".to_string()),
        (429, "{}".to_string()),
        (503, "{}".to_string()),
        (
            400,
            format!(r#"{{"error":{{"message":"API key not valid {KEY}","errors":[{{"reason":"keyInvalid"}}]}}}}"#),
        ),
    ];
    // Every case, including the last, where the API echoes the key inside its
    // own message: the client redacts before any error leaves it.
    for (status, body) in &cases {
        let (base, _) = spawn_stub(vec![("/videos", *status, body.clone())]).await;
        let err = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap_err();
        assert!(!format!("{err}").contains(KEY), "status {status}: {err}");
        assert!(!format!("{err:?}").contains(KEY), "status {status}: {err:?}");
    }
    let (status, body) = cases.last().unwrap();
    let (base, _) = spawn_stub(vec![("/videos", *status, body.clone())]).await;
    let err = client(&base, KEY).fetch_metadata("53XYcpCgQWE").await.unwrap_err();
    assert!(matches!(err, FetchError::Auth(_)), "{err}");
    assert!(err.to_string().contains(crate::youtube::error::REDACTED), "{err}");

    let empty_key_err = client(&base, "").fetch_metadata("x").await.unwrap_err();
    assert!(!empty_key_err.to_string().contains(KEY));

    // A dead port: the network error text must not carry the key either.
    let dead = client("http://127.0.0.1:1", KEY)
        .fetch_metadata("53XYcpCgQWE")
        .await
        .unwrap_err();
    assert!(matches!(dead, FetchError::Network(_)), "{dead}");
    assert!(!dead.to_string().contains(KEY));
}

/// The recorded live bodies go through the real mappers: the synthesized
/// fixtures' shape matches what the API actually returns (the live body
/// carries more fields, `thumbnails`, `localized`, `defaultLanguage`, and
/// `contentDetails.caption`, which the mappers ignore).
#[tokio::test]
async fn recorded_live_bodies_map_every_field() {
    let (base, _) = spawn_stub(vec![
        ("/videos", 200, RECORDED_VIDEO.to_string()),
        ("/channels", 200, RECORDED_CHANNEL.to_string()),
    ])
    .await;
    let m = client(&base, KEY).fetch_metadata("SDLfjrUyBw4").await.unwrap();
    assert_eq!(m.title, "Dolt: This Makes SQL Feel Like Git");
    assert_eq!(m.uploader, "Better Stack");
    assert_eq!(m.duration_secs, 419.0, "PT6M59S");
    assert!(m.description.starts_with("Dolt is a SQL database that works like Git"));
    assert_eq!(m.tags.len(), 25);
    assert_eq!(m.tags.first().map(String::as_str), Some("dolt"));
    assert_eq!(m.channel_id.as_deref(), Some("UCkVfrGwV-iG9bSsgCbrNPxQ"));
    assert_eq!(
        m.channel_url.as_deref(),
        Some("https://www.youtube.com/channel/UCkVfrGwV-iG9bSsgCbrNPxQ")
    );
    assert_eq!(m.channel_handle.as_deref(), Some("@betterstack"));
    assert_eq!(m.published_at.as_deref(), Some("2026-06-05"));
}
