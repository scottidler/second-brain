use crate::assets;
use crate::backoff::ExponentialBackoff;
use crate::config::{Config, DiscordConfig};
use crate::pipeline;
use crate::router::{extract_url_from_text, format_reply};
use crate::trace;
use crate::types::{ContentKind, IngestMethod, IngestResult};
use eyre::Result;
use serenity::async_trait;
use serenity::model::channel::Message;
use serenity::model::gateway::GatewayIntents;
use serenity::prelude::*;
use std::sync::Arc;

/// Classify a Discord attachment into a ContentKind based on content_type or filename extension.
fn classify_attachment(data: Vec<u8>, filename: String, content_type: Option<&str>) -> Option<ContentKind> {
    // Check content_type first
    if let Some(ct) = content_type {
        if ct.starts_with("image/") {
            return Some(ContentKind::Image { data, filename });
        }
        if ct == "application/pdf" {
            return Some(ContentKind::Pdf { data, filename });
        }
        if ct.starts_with("audio/") {
            return Some(ContentKind::Audio { data, filename });
        }
        if ct.starts_with("application/vnd.")
            || ct == "application/epub+zip"
            || ct == "application/rtf"
            || ct == "application/msword"
        {
            return Some(ContentKind::Document { data, filename });
        }
    }

    // Fall back to extension-based detection
    if assets::is_image_extension(&filename) {
        return Some(ContentKind::Image { data, filename });
    }
    if assets::is_pdf_extension(&filename) {
        return Some(ContentKind::Pdf { data, filename });
    }
    if assets::is_audio_extension(&filename) {
        return Some(ContentKind::Audio { data, filename });
    }
    if assets::is_document_extension(&filename) {
        return Some(ContentKind::Document { data, filename });
    }

    None
}

/// Format a Discord reply with an optional plain-text obsidian:// deep link.
fn format_discord_reply(result: &IngestResult, display_source: &str) -> String {
    let base = format_reply(result, display_source);
    match &result.obsidian_url {
        Some(url) => format!("{base}\n{url}"),
        None => base,
    }
}

struct Handler {
    config: Arc<Config>,
    channel_id: u64,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: serenity::prelude::Context, msg: Message) {
        if msg.channel_id.get() != self.channel_id || msg.author.bot {
            return;
        }

        // Priority 1: File attachment
        if let Some(attachment) = msg.attachments.first() {
            let att_filename = attachment.filename.clone();
            let att_content_type = attachment.content_type.clone();
            let att_url = attachment.url.clone();

            log::info!(
                "Discord: processing attachment '{}' (content_type: {}) from channel {}",
                att_filename,
                att_content_type.as_deref().unwrap_or("unknown"),
                msg.channel_id
            );

            let data = match reqwest::get(&att_url).await {
                Ok(resp) => match resp.bytes().await {
                    Ok(bytes) => bytes.to_vec(),
                    Err(e) => {
                        log::error!("Discord: failed to read attachment bytes: {e}");
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, format!("Failed to download attachment: {e}"))
                            .await;
                        return;
                    }
                },
                Err(e) => {
                    log::error!("Discord: failed to download attachment: {e}");
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, format!("Failed to download attachment: {e}"))
                        .await;
                    return;
                }
            };

            let content = classify_attachment(data, att_filename.clone(), att_content_type.as_deref());

            match content {
                Some(kind) => {
                    let kind_label = match &kind {
                        ContentKind::Image { .. } => "image",
                        ContentKind::Pdf { .. } => "pdf",
                        ContentKind::Audio { .. } => "audio",
                        ContentKind::Document { .. } => "document",
                        _ => "file",
                    };
                    let display_source = format!("[{}: {}]", kind_label, att_filename);
                    let trace_id = trace::generate(IngestMethod::Discord);

                    let _ = msg
                        .channel_id
                        .say(&ctx.http, format!("[{trace_id}] Processing {kind_label}..."))
                        .await;

                    let result = pipeline::process_content(
                        kind,
                        vec![],
                        IngestMethod::Discord,
                        false,
                        &self.config,
                        Some(trace_id),
                    )
                    .await;
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, format_discord_reply(&result, &display_source))
                        .await;
                }
                None => {
                    log::warn!(
                        "Discord: unsupported attachment type '{}' (content_type: {})",
                        att_filename,
                        att_content_type.as_deref().unwrap_or("unknown")
                    );
                    let _ = msg
                        .channel_id
                        .say(
                            &ctx.http,
                            format!(
                                "Unsupported file type: {} (content_type: {})",
                                att_filename,
                                att_content_type.as_deref().unwrap_or("unknown")
                            ),
                        )
                        .await;
                }
            }

            return;
        }

        // Priority 2: URL in text
        // Priority 3: Plain text
        let (content, display_source) = if let Some(url) = extract_url_from_text(&msg.content) {
            (ContentKind::Url(url.clone()), url)
        } else if !msg.content.trim().is_empty() {
            let display = if msg.content.len() > 50 {
                format!("{}...", &msg.content[..50])
            } else {
                msg.content.clone()
            };
            (ContentKind::Text(msg.content.clone()), display)
        } else {
            // Priority 4: Empty -> ignore
            return;
        };

        let trace_id = trace::generate(IngestMethod::Discord);
        let _ = msg
            .channel_id
            .say(&ctx.http, format!("[{trace_id}] Processing..."))
            .await;
        let result = pipeline::process_content(
            content,
            vec![],
            IngestMethod::Discord,
            false,
            &self.config,
            Some(trace_id),
        )
        .await;
        let _ = msg
            .channel_id
            .say(&ctx.http, format_discord_reply(&result, &display_source))
            .await;
    }
}

pub async fn run(token: String, dc_config: DiscordConfig, config: Arc<Config>) -> Result<()> {
    let mut backoff = ExponentialBackoff::new();

    loop {
        log::info!("discord: starting bot");
        let handler = Handler {
            config: config.clone(),
            channel_id: dc_config.channel_id,
        };
        let intents = GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;

        let client = match serenity::Client::builder(&token, intents).event_handler(handler).await {
            Ok(c) => c,
            Err(e) => {
                log::error!("discord: failed to create client: {e}");
                backoff.wait().await;
                continue;
            }
        };

        backoff.reset();

        let mut client = client;
        if let Err(e) = client.start().await {
            log::error!("discord: client error: {e}");
        } else {
            log::warn!("discord: client exited, will restart");
        }

        backoff.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{IngestResult, IngestStatus};

    #[test]
    fn test_format_discord_reply_with_obsidian_url() {
        let result = IngestResult {
            status: IngestStatus::Completed,
            title: Some("Test Article".to_string()),
            tags: vec!["ai".to_string()],
            elapsed_secs: Some(3.5),
            obsidian_url: Some("obsidian://open?vault=obsidian&file=notes%2Ftest-article.md".to_string()),
            ..Default::default()
        };
        let reply = format_discord_reply(&result, "https://example.com");
        assert!(reply.contains("Saved: Test Article"));
        assert!(reply.contains("obsidian://open?vault=obsidian&file=notes%2Ftest-article.md"));
    }

    #[test]
    fn test_format_discord_reply_without_obsidian_url() {
        let result = IngestResult {
            status: IngestStatus::Failed {
                reason: "network error".to_string(),
            },
            ..Default::default()
        };
        let reply = format_discord_reply(&result, "https://example.com");
        assert!(reply.contains("Failed"));
        assert!(!reply.contains("obsidian://"));
    }
}
