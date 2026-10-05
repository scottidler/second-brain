use super::*;

#[test]
fn test_parse_plain_url() {
    let result = parse_message("https://youtube.com/watch?v=abc123");
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://youtube.com/watch?v=abc123".to_string(),
            tags: vec![],
            force: false,
            note: None,
        })
    );
}

#[test]
fn test_parse_url_with_surrounding_text() {
    // The prose around the URL
    // becomes the capture note (first-URL token removed, whitespace-collapsed).
    let result = parse_message("Check out this video: https://youtube.com/watch?v=abc123");
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://youtube.com/watch?v=abc123".to_string(),
            tags: vec![],
            force: false,
            note: Some("Check out this video:".to_string()),
        })
    );
}

#[test]
fn test_parse_google_discover_format() {
    let result = parse_message("Article Title\nhttps://example.com/article");
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://example.com/article".to_string(),
            tags: vec![],
            force: false,
            note: Some("Article Title".to_string()),
        })
    );
}

#[test]
fn test_parse_json_body() {
    // `force: true` in the body is IGNORED - ntfy's topic-only auth must
    // not let a topic-guesser trigger a force-overwrite.
    let result = parse_message(r#"{"url": "https://example.com", "tags": ["ai", "rust"], "force": true}"#);
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://example.com".to_string(),
            tags: vec!["ai".to_string(), "rust".to_string()],
            force: false,
            note: None,
        })
    );
}

#[test]
fn test_parse_json_body_with_note() {
    // A JSON ntfy body may carry an explicit `note` capture annotation.
    let result = parse_message(r#"{"url": "https://example.com", "note": "fixes borg's linker"}"#);
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://example.com".to_string(),
            tags: vec![],
            force: false,
            note: Some("fixes borg's linker".to_string()),
        })
    );
}

#[test]
fn test_parse_json_body_minimal() {
    let result = parse_message(r#"{"url": "https://example.com"}"#);
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://example.com".to_string(),
            tags: vec![],
            force: false,
            note: None,
        })
    );
}

#[test]
fn test_parse_empty_message() {
    assert_eq!(parse_message(""), None);
    assert_eq!(parse_message("  "), None);
}

#[test]
fn test_parse_no_url_falls_back_to_text() {
    let result = parse_message("just some text without urls");
    assert_eq!(
        result,
        Some(ParsedMessage::Text("just some text without urls".to_string()))
    );
}

#[test]
fn test_parse_invalid_json_falls_through_to_text() {
    let result = parse_message(r#"{"not_valid_json": }"#);
    assert_eq!(result, Some(ParsedMessage::Text(r#"{"not_valid_json": }"#.to_string())));
}

mod stream {
    use super::*;
    use crate::stub::{Behavior, serve};
    use std::sync::Mutex;
    use std::time::Instant;

    /// Run the subscriber against a stub for at most `window`, returning the
    /// number of connections it opened once `want` is reached or the window
    /// closes.
    async fn connections_within(behavior: Behavior, read_timeout: Duration, window: Duration, want: usize) -> usize {
        let (port, seen): (u16, Arc<Mutex<Vec<String>>>) = serve(behavior).await;
        let subscriber = tokio::spawn(run(
            format!("http://127.0.0.1:{port}"),
            "topic".to_string(),
            None,
            read_timeout,
            Arc::new(Config::default()),
            None,
            None,
        ));
        let started = Instant::now();
        while started.elapsed() < window && seen.lock().expect("stub log").len() < want {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!subscriber.is_finished(), "the subscriber must never end on its own");
        subscriber.abort();
        seen.lock().expect("stub log").len()
    }

    #[tokio::test]
    async fn a_stalled_stream_is_reconnected_through_backoff() {
        let read_timeout = Duration::from_millis(300);
        // 3x read-timeout plus the backoff delays (1 s, 2 s), and 500 ms slack.
        let window = 3 * read_timeout + Duration::from_secs(3) + Duration::from_millis(500);
        let n = connections_within(Behavior::HeadersThenStall, read_timeout, window, 3).await;
        assert!(
            n >= 3,
            "expected the first connection plus 2 reconnects within {window:?}, saw {n}"
        );
    }

    #[tokio::test]
    async fn keepalives_inside_the_read_timeout_keep_one_connection() {
        let read_timeout = Duration::from_millis(600);
        let n = connections_within(
            Behavior::NtfyKeepalives {
                every: read_timeout / 3,
            },
            read_timeout,
            3 * read_timeout,
            2,
        )
        .await;
        assert_eq!(n, 1, "a stream with keepalives must not reconnect");
    }

    fn backoff_at_attempt_three() -> ExponentialBackoff {
        let mut backoff = ExponentialBackoff::reconnect();
        for _ in 0..3 {
            backoff.next_delay(None);
        }
        backoff
    }

    #[test]
    fn a_message_on_a_fresh_connection_does_not_reset_the_backoff() {
        let mut backoff = backoff_at_attempt_three();
        settle_backoff(&mut backoff, Instant::now());
        assert_eq!(backoff.attempts(), 3, "a flapping server must keep the backoff growing");
    }

    #[test]
    fn a_message_on_a_healthy_connection_resets_the_backoff() {
        let mut backoff = backoff_at_attempt_three();
        let long_ago = Instant::now()
            .checked_sub(Duration::from_secs(crate::backoff::HEALTHY_RUN_SECS + 1))
            .expect("instant in range");
        settle_backoff(&mut backoff, long_ago);
        assert_eq!(backoff.attempts(), 0);
    }
}
