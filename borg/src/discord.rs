use crate::assets;
use crate::backoff::ExponentialBackoff;
use crate::config::{Config, DiscordConfig};
use crate::intake::{self as intake_log, Kind as IntakeKind};
use crate::notify::Desktop;
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
use vault::receipts::FailureStage;

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
    desktop: Option<Desktop>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: serenity::prelude::Context, msg: Message) {
        // Bot-author messages are never user input - they're our own
        // replies. Skip without producing an intake row.
        if msg.author.bot {
            return;
        }

        // Generate trace at the door so disallowed-channel messages and
        // empty messages still get a durable record (a failed receipts row).
        let trace_id = trace::generate(IngestMethod::Discord);

        let (intake_kind, intake_preview) = if let Some(att) = msg.attachments.first() {
            let mime = att.content_type.as_deref();
            let kind = match mime {
                Some(m) if m.starts_with("image/") => IntakeKind::Photo,
                Some(m) if m.starts_with("audio/") => IntakeKind::Audio,
                Some("application/pdf") => IntakeKind::Document,
                _ => {
                    if assets::is_image_extension(&att.filename) {
                        IntakeKind::Photo
                    } else if assets::is_audio_extension(&att.filename) {
                        IntakeKind::Audio
                    } else if assets::is_pdf_extension(&att.filename) || assets::is_document_extension(&att.filename) {
                        IntakeKind::Document
                    } else {
                        IntakeKind::Unknown
                    }
                }
            };
            let preview = intake_log::binary_descriptor(kind, &att.filename, att.size as usize, mime);
            (kind, preview)
        } else if let Some(url) = extract_url_from_text(&msg.content) {
            (IntakeKind::Url, url)
        } else if msg.content.trim().is_empty() {
            (IntakeKind::Empty, "[empty discord message]".to_string())
        } else {
            (IntakeKind::Text, intake_log::preview_text(&msg.content))
        };

        if let Err(e) = intake_log::record_received_with_sidecar(
            &self.config,
            IngestMethod::Discord,
            intake_kind,
            &intake_preview,
            msg.content.as_bytes(),
            &trace_id,
        ) {
            log::error!("discord: failed to record intake trace={trace_id}: {e:#}");
            let _ = msg
                .channel_id
                .say(&ctx.http, format!("[{trace_id}] borg failed to record input: {e}"))
                .await;
            return;
        }

        if msg.channel_id.get() != self.channel_id {
            log::info!(
                "discord: rejecting disallowed channel {} (trace={trace_id})",
                msg.channel_id
            );
            intake_log::record_failure_at_door(
                IngestMethod::Discord,
                &trace_id,
                FailureStage::IntakeRejected,
                &format!("channel {} not configured", msg.channel_id),
            );
            return;
        }

        if intake_kind == IntakeKind::Empty {
            log::info!("discord: rejecting empty message (trace={trace_id})");
            intake_log::record_failure_at_door(
                IngestMethod::Discord,
                &trace_id,
                FailureStage::IntakeRejected,
                "empty discord message",
            );
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
                        intake_log::record_failure_at_door(
                            IngestMethod::Discord,
                            &trace_id,
                            FailureStage::FetchFailed,
                            &format!("attachment read failed: {e}"),
                        );
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, format!("Failed to download attachment: {e}"))
                            .await;
                        return;
                    }
                },
                Err(e) => {
                    log::error!("Discord: failed to download attachment: {e}");
                    intake_log::record_failure_at_door(
                        IngestMethod::Discord,
                        &trace_id,
                        FailureStage::FetchFailed,
                        &format!("attachment download failed: {e}"),
                    );
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
                    let processing_text = format!("Processing {kind_label}...");

                    let prior = if let Some(d) = &self.desktop {
                        d.processing(&trace_id, &processing_text).await
                    } else {
                        None
                    };
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, format!("[{trace_id}] {processing_text}"))
                        .await;

                    // Detach: run the pipeline off the serenity event loop so a
                    // slow ingest can't stall the gateway heartbeat. Mirrors
                    // telegram's spawned dispatch.
                    let config = self.config.clone();
                    let desktop = self.desktop.clone();
                    let http = ctx.http.clone();
                    let channel_id = msg.channel_id;
                    let trace_id = trace_id.clone();
                    tokio::spawn(async move {
                        let result = pipeline::process_content(
                            kind,
                            vec![],
                            IngestMethod::Discord,
                            false,
                            &config,
                            Some(trace_id.clone()),
                            None,
                        )
                        .await;
                        let _ = channel_id
                            .say(&http, format_discord_reply(&result, &display_source))
                            .await;
                        if let Some(d) = &desktop {
                            d.result(&result, &display_source, prior).await;
                        }
                    });
                }
                None => {
                    log::warn!(
                        "Discord: unsupported attachment type '{}' (content_type: {})",
                        att_filename,
                        att_content_type.as_deref().unwrap_or("unknown")
                    );
                    intake_log::record_failure_at_door(
                        IngestMethod::Discord,
                        &trace_id,
                        FailureStage::IntakeRejected,
                        &format!(
                            "unsupported attachment type: {} (content_type: {})",
                            att_filename,
                            att_content_type.as_deref().unwrap_or("unknown")
                        ),
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
        // Priority 3: Plain text. Empty was already recorded as a failed receipt above.
        let (content, display_source) = if let Some((content, url)) = crate::router::url_content_from_text(&msg.content)
        {
            (content, url)
        } else {
            let display = vault::text::truncate_with_ellipsis(&msg.content, 50);
            (ContentKind::Text(msg.content.clone()), display)
        };

        let prior = if let Some(d) = &self.desktop {
            d.processing(&trace_id, "Processing...").await
        } else {
            None
        };
        let _ = msg
            .channel_id
            .say(&ctx.http, format!("[{trace_id}] Processing..."))
            .await;
        // Detach: same rationale as the attachment path above.
        let config = self.config.clone();
        let desktop = self.desktop.clone();
        let http = ctx.http.clone();
        let channel_id = msg.channel_id;
        let trace_id = trace_id.clone();
        tokio::spawn(async move {
            let result = pipeline::process_content(
                content,
                vec![],
                IngestMethod::Discord,
                false,
                &config,
                Some(trace_id.clone()),
                None,
            )
            .await;
            let _ = channel_id
                .say(&http, format_discord_reply(&result, &display_source))
                .await;
            if let Some(d) = &desktop {
                d.result(&result, &display_source, prior).await;
            }
        });
    }
}

pub async fn run(token: String, dc_config: DiscordConfig, config: Arc<Config>, desktop: Option<Desktop>) -> Result<()> {
    log::debug!(
        "discord::run: host={:?} channel_id={:?} desktop={}",
        dc_config.host,
        dc_config.channel_id,
        desktop.is_some()
    );
    let mut backoff = ExponentialBackoff::new();

    loop {
        log::info!("discord: starting bot");
        let handler = Handler {
            config: config.clone(),
            channel_id: dc_config.channel_id,
            desktop: desktop.clone(),
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

        let connected_at = std::time::Instant::now();

        let mut client = client;
        if let Err(e) = client.start().await {
            log::error!("discord: client error: {e}");
        } else {
            log::warn!("discord: client exited, will restart");
        }

        // Reset only after a sustained-healthy run; a fast drop keeps the
        // backoff growing rather than hot-looping at the base delay.
        backoff.reset_if_healthy(connected_at);
        backoff.wait().await;
    }
}

#[cfg(test)]
mod tests;
