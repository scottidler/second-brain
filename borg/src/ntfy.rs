use crate::backoff::ExponentialBackoff;
use crate::config::Config;
use crate::intake::{self as intake_log, Kind as IntakeKind};
use crate::notify::{Desktop, Telegram};
use crate::router::{extract_capture_note, extract_url_from_text};
use crate::trace;
use crate::types::{ContentKind, IngestMethod};
use eyre::{Context, Result};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncBufReadExt;
use tokio_stream::StreamExt;
use vault::http::Timeouts;
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
    /// Optional operator capture annotation, threaded to the note's
    /// `## Why Captured`. Absent in existing ntfy senders -> `None`.
    #[serde(default)]
    note: Option<String>,
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
        /// Operator capture annotation: the JSON `note`, or the prose
        /// surrounding the URL in a plain-text message (first-URL token removed).
        note: Option<String>,
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
            note: body.note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()),
        });
    }

    // Plain text: extract first URL, or fall back to text capture
    if let Some(url) = extract_url_from_text(trimmed) {
        let note = extract_capture_note(trimmed, &url);
        Some(ParsedMessage::Url {
            url,
            tags: vec![],
            force: false,
            note,
        })
    } else {
        Some(ParsedMessage::Text(trimmed.to_string()))
    }
}

/// Subscribe to `{server}/{topic}/json` forever. The client is built once:
/// connect and each read are bounded by `read_timeout` (`ntfy.read-timeout`),
/// with no total, so a healthy stream lives as long as the server keeps
/// sending keepalives and a silent one is dropped and reconnected through
/// backoff. Only a client that cannot be built ends the subscriber.
pub async fn run(
    server: String,
    topic: String,
    token: Option<String>,
    read_timeout: Duration,
    config: Arc<Config>,
    telegram: Option<Telegram>,
    desktop: Option<Desktop>,
) -> Result<()> {
    log::debug!("ntfy::run: server={server} topic={topic} read_timeout={read_timeout:?}");
    let client = vault::http::builder(Timeouts::Stream {
        connect: read_timeout,
        read: read_timeout,
    })
    .build()
    .context("ntfy: cannot build the HTTP client")?;
    let mut last_event_id: Option<String> = None;
    let mut backoff = ExponentialBackoff::reconnect();

    loop {
        let mut url = format!("{server}/{topic}/json");
        if let Some(ref since) = last_event_id {
            url = format!("{url}?since={since}");
        }

        log::info!("ntfy: connecting to {url}");

        let mut req = client.get(&url);
        if let Some(ref token) = token {
            req = req.bearer_auth(token);
        }

        let response = match req.send().await {
            Ok(resp) if resp.status().is_success() => resp,
            Ok(resp) => {
                log::warn!("ntfy: server returned {}", resp.status());
                backoff.wait("reconnecting").await;
                continue;
            }
            Err(e) => {
                log::warn!("ntfy: connection failed: {e}");
                backoff.wait("reconnecting").await;
                continue;
            }
        };

        log::info!("ntfy: connected to {topic}");
        let connected_at = Instant::now();

        let stream = response.bytes_stream();
        let reader = tokio_util::io::StreamReader::new(stream.map(|r| r.map_err(std::io::Error::other)));
        let mut lines = tokio::io::BufReader::new(reader).lines();

        loop {
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => {
                    log::warn!("ntfy: stream ended, will reconnect");
                    break;
                }
                Err(e) => {
                    log::warn!("ntfy: read failed ({e}), will reconnect");
                    break;
                }
            };
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

            settle_backoff(&mut backoff, connected_at);

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
                ParsedMessage::Url { url, tags, force, note } => {
                    log::info!("ntfy: processing URL {url} (trace={trace_id})");
                    let cfg = config.clone();
                    let tg = telegram.clone();
                    let desk = desktop.clone();
                    let trace_for_spawn = trace_id.clone();
                    tokio::spawn(async move {
                        let display_source = url.clone();
                        let result = crate::dispatch::dispatch_ingest(
                            ContentKind::Url { url: url.clone(), note },
                            tags,
                            IngestMethod::Ntfy,
                            force,
                            &cfg,
                            trace_for_spawn,
                            &display_source,
                            "Processing...",
                            desk,
                            tg,
                            None,
                        )
                        .await;
                        log::info!("ntfy: pipeline result for {url}: {:?}", result.status);
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
                        let result = crate::dispatch::dispatch_ingest(
                            ContentKind::Text(text),
                            vec![],
                            IngestMethod::Ntfy,
                            false,
                            &cfg,
                            trace_for_spawn,
                            &display,
                            "Processing text...",
                            desk,
                            tg,
                            None,
                        )
                        .await;
                        log::info!("ntfy: text capture result: {:?}", result.status);
                    });
                }
            }
        }

        settle_backoff(&mut backoff, connected_at);
        backoff.wait("reconnecting").await;
    }
}

/// A stream earns a backoff reset only by staying up `HEALTHY_RUN_SECS`; a
/// server that accepts, sends one message and drops keeps the backoff growing.
fn settle_backoff(backoff: &mut ExponentialBackoff, connected_at: Instant) {
    backoff.reset_if_healthy(connected_at);
}

#[cfg(test)]
mod tests;
