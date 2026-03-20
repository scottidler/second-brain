use crate::backoff::ExponentialBackoff;
use crate::config::Config;
use crate::notify::Notifier;
use crate::pipeline;
use crate::router::extract_url_from_text;
use crate::trace;
use crate::types::{ContentKind, IngestMethod};
use eyre::Result;
use serde::Deserialize;
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio_stream::StreamExt;

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
    #[serde(default)]
    force: bool,
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
            force: body.force,
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
    notifier: Option<Notifier>,
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

            let Some(parsed) = parse_message(&event.message) else {
                log::info!("ntfy: empty message, skipping");
                continue;
            };

            match parsed {
                ParsedMessage::Url { url, tags, force } => {
                    log::info!("ntfy: processing URL {url}");
                    let cfg = config.clone();
                    let n = notifier.clone();
                    let trace_id = trace::generate(IngestMethod::Ntfy);
                    if let Some(ref n) = notifier {
                        let _ = n.processing(&trace_id, "Processing...", None).await;
                    }
                    tokio::spawn(async move {
                        let display_source = url.clone();
                        let content = ContentKind::Url(url.clone());
                        let result =
                            pipeline::process_content(content, tags, IngestMethod::Ntfy, force, &cfg, Some(trace_id))
                                .await;
                        log::info!("ntfy: pipeline result for {url}: {:?}", result.status);
                        if let Some(n) = n {
                            n.result(&result, &display_source, None).await;
                        }
                    });
                }
                ParsedMessage::Text(text) => {
                    log::info!("ntfy: processing text capture ({} chars)", text.len());
                    let cfg = config.clone();
                    let n = notifier.clone();
                    let trace_id = trace::generate(IngestMethod::Ntfy);
                    let display = if text.len() > 50 { format!("{}...", &text[..50]) } else { text.clone() };
                    if let Some(ref n) = notifier {
                        let _ = n.processing(&trace_id, "Processing text...", None).await;
                    }
                    tokio::spawn(async move {
                        let content = ContentKind::Text(text);
                        let result =
                            pipeline::process_content(content, vec![], IngestMethod::Ntfy, false, &cfg, Some(trace_id))
                                .await;
                        log::info!("ntfy: text capture result: {:?}", result.status);
                        if let Some(n) = n {
                            n.result(&result, &display, None).await;
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
        let result = parse_message(r#"{"url": "https://example.com", "tags": ["ai", "rust"], "force": true}"#);
        assert_eq!(
            result,
            Some(ParsedMessage::Url {
                url: "https://example.com".to_string(),
                tags: vec!["ai".to_string(), "rust".to_string()],
                force: true,
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
