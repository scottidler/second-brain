use crate::backoff::ExponentialBackoff;
use crate::config::Config;
use crate::intake::{self as intake_log, Kind as IntakeKind};
use crate::notify::{Desktop, Telegram};
use crate::pipeline;
use crate::router::extract_url_from_text;
use crate::trace;
use crate::types::{ContentKind, IngestMethod};
use eyre::Result;
use serde::Deserialize;
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio_stream::StreamExt;
use vault::receipts::FailureStage;

#[derive(Debug, Deserialize)]
struct NtfyEvent {
    id: String,
    event: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct JsonBody {
    url: String,
    #[serde(default)]
    tags: Vec<String>,
    // NOTE: there is intentionally no `force` field. ntfy's only "auth" is the
    // topic name (a shared secret at best); honoring `force: true` from the
    // body would let anyone who guesses the topic force-overwrite vault notes.
    // A `force` key in the JSON is silently ignored (no deny_unknown_fields).
}

#[derive(Debug, PartialEq)]
enum ParsedMessage {
    Url {
        url: String,
        tags: Vec<String>,
        force: bool,
    },
    Text(String),
}

fn parse_message(message: &str) -> Option<ParsedMessage> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }

    // JSON body: {"url": "...", "tags": [...], "force": true}
    if trimmed.starts_with('{')
        && let Ok(body) = serde_json::from_str::<JsonBody>(trimmed)
    {
        return Some(ParsedMessage::Url {
            url: body.url,
            tags: body.tags,
            // `force` is never honored from the ntfy channel (topic-only auth).
            force: false,
        });
    }

    // Plain text: extract first URL, or fall back to text capture
    if let Some(url) = extract_url_from_text(trimmed) {
        Some(ParsedMessage::Url {
            url,
            tags: vec![],
            force: false,
        })
    } else {
        Some(ParsedMessage::Text(trimmed.to_string()))
    }
}

pub async fn run(
    server: String,
    topic: String,
    token: Option<String>,
    config: Arc<Config>,
    telegram: Option<Telegram>,
    desktop: Option<Desktop>,
) -> Result<()> {
    let mut last_event_id: Option<String> = None;
    let mut backoff = ExponentialBackoff::new();

    loop {
        let mut url = format!("{server}/{topic}/json");
        if let Some(ref since) = last_event_id {
            url = format!("{url}?since={since}");
        }

        log::info!("ntfy: connecting to {url}");

        let mut req = reqwest::Client::new().get(&url);
        if let Some(ref token) = token {
            req = req.bearer_auth(token);
        }

        let response = match req.send().await {
            Ok(resp) if resp.status().is_success() => resp,
            Ok(resp) => {
                log::warn!("ntfy: server returned {}", resp.status());
                backoff.wait().await;
                continue;
            }
            Err(e) => {
                log::warn!("ntfy: connection failed: {e}");
                backoff.wait().await;
                continue;
            }
        };

        log::info!("ntfy: connected to {topic}");

        let stream = response.bytes_stream();
        let reader = tokio_util::io::StreamReader::new(stream.map(|r| r.map_err(std::io::Error::other)));
        let mut lines = tokio::io::BufReader::new(reader).lines();

        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }

            let event: NtfyEvent = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(e) => {
                    log::warn!("ntfy: failed to parse event: {e}");
                    continue;
                }
            };

            last_event_id = Some(event.id.clone());

            if event.event != "message" {
                log::debug!("ntfy: skipping event type '{}'", event.event);
                continue;
            }

            backoff.reset();

            // Generate trace at the door so every event - including empty
            // and undeliverable ones - gets a durable record.
            let trace_id = trace::generate(IngestMethod::Ntfy);
            let parsed = parse_message(&event.message);
            let (intake_kind, intake_preview) = match &parsed {
                Some(ParsedMessage::Url { url, .. }) => (IntakeKind::Url, url.clone()),
                Some(ParsedMessage::Text(text)) => (IntakeKind::Text, intake_log::preview_text(text)),
                None => (IntakeKind::Empty, "[empty ntfy message]".to_string()),
            };

            if let Err(e) = intake_log::record_received_with_sidecar(
                &config,
                IngestMethod::Ntfy,
                intake_kind,
                &intake_preview,
                event.message.as_bytes(),
                &trace_id,
            ) {
                log::error!("ntfy: failed to record intake trace={trace_id}: {e:#}");
                continue;
            }

            let Some(parsed) = parsed else {
                log::info!("ntfy: empty message (trace={trace_id})");
                intake_log::record_failure_at_door(
                    IngestMethod::Ntfy,
                    &trace_id,
                    FailureStage::IntakeRejected,
                    "empty ntfy message",
                );
                continue;
            };

            match parsed {
                ParsedMessage::Url { url, tags, force } => {
                    log::info!("ntfy: processing URL {url} (trace={trace_id})");
                    let cfg = config.clone();
                    let tg = telegram.clone();
                    let desk = desktop.clone();
                    let trace_for_spawn = trace_id.clone();
                    tokio::spawn(async move {
                        let prior = if let Some(d) = &desk {
                            d.processing(&trace_for_spawn, "Processing...").await
                        } else {
                            None
                        };
                        if let Some(t) = &tg {
                            let _ = t.processing(&trace_for_spawn, "Processing...", None).await;
                        }
                        let display_source = url.clone();
                        let content = ContentKind::Url(url.clone());
                        let result = pipeline::process_content(
                            content,
                            tags,
                            IngestMethod::Ntfy,
                            force,
                            &cfg,
                            Some(trace_for_spawn),
                        )
                        .await;
                        log::info!("ntfy: pipeline result for {url}: {:?}", result.status);
                        if let Some(t) = tg {
                            t.result(&result, &display_source, None).await;
                        }
                        if let Some(d) = desk {
                            d.result(&result, &display_source, prior).await;
                        }
                    });
                }
                ParsedMessage::Text(text) => {
                    log::info!("ntfy: processing text capture ({} chars, trace={trace_id})", text.len());
                    let cfg = config.clone();
                    let tg = telegram.clone();
                    let desk = desktop.clone();
                    let display = vault::text::truncate_with_ellipsis(&text, 50);
                    let trace_for_spawn = trace_id.clone();
                    tokio::spawn(async move {
                        let prior = if let Some(d) = &desk {
                            d.processing(&trace_for_spawn, "Processing text...").await
                        } else {
                            None
                        };
                        if let Some(t) = &tg {
                            let _ = t.processing(&trace_for_spawn, "Processing text...", None).await;
                        }
                        let content = ContentKind::Text(text);
                        let result = pipeline::process_content(
                            content,
                            vec![],
                            IngestMethod::Ntfy,
                            false,
                            &cfg,
                            Some(trace_for_spawn),
                        )
                        .await;
                        log::info!("ntfy: text capture result: {:?}", result.status);
                        if let Some(t) = tg {
                            t.result(&result, &display, None).await;
                        }
                        if let Some(d) = desk {
                            d.result(&result, &display, prior).await;
                        }
                    });
                }
            }
        }

        log::warn!("ntfy: stream ended, will reconnect");
        backoff.wait().await;
    }
}

#[cfg(test)]
mod tests {
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
            })
        );
    }

    #[test]
    fn test_parse_url_with_surrounding_text() {
        let result = parse_message("Check out this video: https://youtube.com/watch?v=abc123");
        assert_eq!(
            result,
            Some(ParsedMessage::Url {
                url: "https://youtube.com/watch?v=abc123".to_string(),
                tags: vec![],
                force: false,
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
}
