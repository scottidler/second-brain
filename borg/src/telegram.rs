use crate::assets;
use crate::backoff::ExponentialBackoff;
use crate::config::{Config, TelegramConfig};
use crate::notify;
use crate::pipeline;
use crate::router::extract_url_from_text;
use crate::trace;
use crate::types::{ContentKind, IngestMethod};
use eyre::Result;
use std::sync::Arc;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::requests::Requester;
use teloxide::types::{AllowedUpdate, FileId};

/// Download a file from Telegram by its file_id.
async fn download_telegram_file(bot: &Bot, file_id: &FileId) -> Result<Vec<u8>, teloxide::RequestError> {
    let file = bot.get_file(file_id.clone()).await?;
    let mut buf = Vec::new();
    bot.download_file(&file.path, &mut buf).await?;
    Ok(buf)
}

/// Determine ContentKind for a Telegram document based on MIME type and filename.
fn classify_document(data: Vec<u8>, filename: String, mime_type: Option<&str>) -> Option<ContentKind> {
    // Check MIME type first
    if let Some(mime) = mime_type {
        if mime.starts_with("image/") {
            return Some(ContentKind::Image { data, filename });
        }
        if mime == "application/pdf" {
            return Some(ContentKind::Pdf { data, filename });
        }
        if mime.starts_with("audio/") {
            return Some(ContentKind::Audio { data, filename });
        }
        if mime.starts_with("application/vnd.")
            || mime == "application/epub+zip"
            || mime == "application/rtf"
            || mime == "application/msword"
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

/// Claim the Telegram polling session by issuing a short getUpdates call.
/// This invalidates any lingering long-poll from a previous process so the
/// dispatcher doesn't hit TerminatedByOtherGetUpdates.
async fn claim_polling_session(bot: &Bot) {
    // offset -1 means "give me only the latest update", timeout 0 means return immediately.
    // This forces Telegram to terminate any other active getUpdates connection.
    match bot
        .get_updates()
        .offset(-1)
        .timeout(0)
        .allowed_updates(vec![AllowedUpdate::Message])
        .await
    {
        Ok(updates) => {
            // If we got an update, confirm it so it's not re-delivered
            if let Some(last) = updates.last() {
                let _ = bot
                    .get_updates()
                    .offset(last.id.as_offset())
                    .timeout(0)
                    .allowed_updates(vec![AllowedUpdate::Message])
                    .await;
            }
            log::info!("telegram: claimed polling session");
        }
        Err(e) => {
            log::warn!("telegram: failed to claim polling session: {e}");
        }
    }
}

// html_escape and format_telegram_reply moved to notify module

pub async fn run(
    token: String,
    tg_config: TelegramConfig,
    config: Arc<Config>,
    notifier: Option<notify::Notifier>,
) -> Result<()> {
    let mut backoff = ExponentialBackoff::new();

    loop {
        log::info!("telegram: starting bot dispatcher");
        let bot = teloxide::Bot::new(&token);

        // Pre-flight check: verify we can reach the Telegram API
        match bot.get_me().await {
            Ok(me) => {
                log::info!("telegram: connected as @{}", me.username());
                backoff.reset();
            }
            Err(e) => {
                log::error!("telegram: cannot reach API: {e}");
                backoff.wait().await;
                continue;
            }
        }

        // Steal the polling session from any previous instance before starting
        // the dispatcher. Without this, the first getUpdates from dispatch()
        // races with a lingering long-poll and triggers TerminatedByOtherGetUpdates.
        claim_polling_session(&bot).await;

        let tg = tg_config.clone();
        let cfg = config.clone();
        let nfy = notifier.clone();

        let handler = Update::filter_message().endpoint(move |message: Message, bot: Bot| {
            let config = cfg.clone();
            let allowed = tg.allowed_chat_ids.clone();
            let notifier = nfy.clone();
            async move {
                if !allowed.is_empty() && !allowed.contains(&message.chat.id.0) {
                    return Ok::<(), teloxide::RequestError>(());
                }

                let chat_id = message.chat.id;
                let chat_id_override = Some(chat_id.0);

                // Priority 1: Photo attachment
                if let Some(photos) = message.photo() {
                    let largest = photos
                        .iter()
                        .max_by_key(|p| p.file.size)
                        .expect("photo array is non-empty");
                    let caption = message.caption().unwrap_or("").to_string();
                    log::info!(
                        "Telegram: processing image from chat {} (caption: {})",
                        chat_id,
                        if caption.is_empty() { "<none>" } else { &caption }
                    );

                    let data = match download_telegram_file(&bot, &largest.file.id).await {
                        Ok(d) => d,
                        Err(e) => {
                            log::error!("Failed to download photo: {e}");
                            bot.send_message(chat_id, format!("Failed to download photo: {e}"))
                                .await?;
                            return Ok(());
                        }
                    };

                    let filename = format!("telegram-photo-{}.jpg", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
                    let display_source = format!("[image: {}]", filename);
                    let trace_id = trace::generate(IngestMethod::Telegram);

                    if let Some(ref n) = notifier {
                        let _ = n.processing(&trace_id, "Processing image...", chat_id_override).await;
                    }

                    let content = ContentKind::Image { data, filename };
                    let extra_tags: Vec<String> =
                        if caption.is_empty() { vec![] } else { vec![format!("caption:{caption}")] };

                    let n = notifier.clone();
                    tokio::spawn(async move {
                        let result = pipeline::process_content(
                            content,
                            extra_tags,
                            IngestMethod::Telegram,
                            false,
                            &config,
                            Some(trace_id),
                        )
                        .await;
                        log::debug!("Pipeline result: {:?}", result.status);
                        if let Some(n) = n {
                            n.result(&result, &display_source, chat_id_override).await;
                        }
                    });

                    return Ok(());
                }

                // Priority 2: Voice note
                if let Some(voice) = message.voice() {
                    log::info!(
                        "Telegram: processing voice note from chat {} (duration: {}s)",
                        chat_id,
                        voice.duration
                    );

                    let data = match download_telegram_file(&bot, &voice.file.id).await {
                        Ok(d) => d,
                        Err(e) => {
                            log::error!("Failed to download voice note: {e}");
                            bot.send_message(chat_id, format!("Failed to download voice note: {e}"))
                                .await?;
                            return Ok(());
                        }
                    };

                    let filename = format!("voice-{}.ogg", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
                    let display_source = format!("[voice: {}]", filename);
                    let trace_id = trace::generate(IngestMethod::Telegram);

                    if let Some(ref n) = notifier {
                        let _ = n
                            .processing(&trace_id, "Processing voice note...", chat_id_override)
                            .await;
                    }

                    let content = ContentKind::Audio { data, filename };

                    let n = notifier.clone();
                    tokio::spawn(async move {
                        let result = pipeline::process_content(
                            content,
                            vec![],
                            IngestMethod::Telegram,
                            false,
                            &config,
                            Some(trace_id),
                        )
                        .await;
                        log::debug!("Pipeline result: {:?}", result.status);
                        if let Some(n) = n {
                            n.result(&result, &display_source, chat_id_override).await;
                        }
                    });

                    return Ok(());
                }

                // Priority 3: Audio file
                if let Some(audio) = message.audio() {
                    let original_name = audio.file_name.as_deref().unwrap_or("audio.mp3").to_string();
                    log::info!(
                        "Telegram: processing audio file '{}' from chat {}",
                        original_name,
                        chat_id
                    );

                    let data = match download_telegram_file(&bot, &audio.file.id).await {
                        Ok(d) => d,
                        Err(e) => {
                            log::error!("Failed to download audio file: {e}");
                            bot.send_message(chat_id, format!("Failed to download audio: {e}"))
                                .await?;
                            return Ok(());
                        }
                    };

                    let display_source = format!("[audio: {}]", original_name);
                    let trace_id = trace::generate(IngestMethod::Telegram);

                    if let Some(ref n) = notifier {
                        let _ = n.processing(&trace_id, "Processing audio...", chat_id_override).await;
                    }

                    let content = ContentKind::Audio {
                        data,
                        filename: original_name,
                    };

                    let n = notifier.clone();
                    tokio::spawn(async move {
                        let result = pipeline::process_content(
                            content,
                            vec![],
                            IngestMethod::Telegram,
                            false,
                            &config,
                            Some(trace_id),
                        )
                        .await;
                        log::debug!("Pipeline result: {:?}", result.status);
                        if let Some(n) = n {
                            n.result(&result, &display_source, chat_id_override).await;
                        }
                    });

                    return Ok(());
                }

                // Priority 4: Document attachment
                if let Some(doc) = message.document() {
                    let doc_filename = doc.file_name.as_deref().unwrap_or("document").to_string();
                    let mime_str = doc.mime_type.as_ref().map(|m| m.as_ref().to_string());
                    log::info!(
                        "Telegram: processing document '{}' (MIME: {}) from chat {}",
                        doc_filename,
                        mime_str.as_deref().unwrap_or("unknown"),
                        chat_id
                    );

                    let data = match download_telegram_file(&bot, &doc.file.id).await {
                        Ok(d) => d,
                        Err(e) => {
                            log::error!("Failed to download document: {e}");
                            bot.send_message(chat_id, format!("Failed to download document: {e}"))
                                .await?;
                            return Ok(());
                        }
                    };

                    let content = classify_document(data, doc_filename.clone(), mime_str.as_deref());

                    match content {
                        Some(kind) => {
                            let kind_label = match &kind {
                                ContentKind::Image { .. } => "image",
                                ContentKind::Pdf { .. } => "pdf",
                                ContentKind::Audio { .. } => "audio",
                                ContentKind::Document { .. } => "document",
                                _ => "file",
                            };
                            let display_source = format!("[{}: {}]", kind_label, doc_filename);
                            let caption = message.caption().unwrap_or("").to_string();
                            let extra_tags: Vec<String> =
                                if caption.is_empty() { vec![] } else { vec![format!("caption:{caption}")] };
                            let trace_id = trace::generate(IngestMethod::Telegram);

                            if let Some(ref n) = notifier {
                                let _ = n
                                    .processing(&trace_id, &format!("Processing {kind_label}..."), chat_id_override)
                                    .await;
                            }

                            let n = notifier.clone();
                            tokio::spawn(async move {
                                let result = pipeline::process_content(
                                    kind,
                                    extra_tags,
                                    IngestMethod::Telegram,
                                    false,
                                    &config,
                                    Some(trace_id),
                                )
                                .await;
                                log::debug!("Pipeline result: {:?}", result.status);
                                if let Some(n) = n {
                                    n.result(&result, &display_source, chat_id_override).await;
                                }
                            });
                        }
                        None => {
                            log::warn!(
                                "Telegram: unsupported document type '{}' (MIME: {})",
                                doc_filename,
                                mime_str.as_deref().unwrap_or("unknown")
                            );
                            bot.send_message(
                                chat_id,
                                format!(
                                    "Unsupported file type: {} (MIME: {})",
                                    doc_filename,
                                    mime_str.as_deref().unwrap_or("unknown")
                                ),
                            )
                            .await?;
                        }
                    }

                    return Ok(());
                }

                // Priority 5 & 6: Text messages (URL or plain text)
                let text = message.text().unwrap_or("");
                log::debug!("Telegram message from chat {}: {text}", chat_id);

                let (content, display_source) = if let Some(url) = extract_url_from_text(text) {
                    log::info!("Telegram: processing URL {url} from chat {}", chat_id);
                    (ContentKind::Url(url.clone()), url)
                } else if !text.trim().is_empty() {
                    log::info!("Telegram: processing text from chat {}", chat_id);
                    let display = if text.len() > 50 { format!("{}...", &text[..50]) } else { text.to_string() };
                    (ContentKind::Text(text.to_string()), display)
                } else {
                    log::debug!("Empty message, ignoring");
                    return Ok(());
                };

                let trace_id = trace::generate(IngestMethod::Telegram);
                if let Some(ref n) = notifier {
                    let _ = n.processing(&trace_id, "Processing...", chat_id_override).await;
                }

                let n = notifier.clone();
                tokio::spawn(async move {
                    let result = pipeline::process_content(
                        content,
                        vec![],
                        IngestMethod::Telegram,
                        false,
                        &config,
                        Some(trace_id),
                    )
                    .await;
                    log::debug!("Pipeline result: {:?}", result.status);
                    if let Some(n) = n {
                        n.result(&result, &display_source, chat_id_override).await;
                    }
                });

                Ok(())
            }
        });

        // Catch panics from dispatch (teloxide panics on network errors during init)
        let result = std::panic::AssertUnwindSafe(async {
            Dispatcher::builder(bot, handler)
                .enable_ctrlc_handler()
                .build()
                .dispatch()
                .await;
        });

        match tokio::task::spawn(result).await {
            Ok(()) => {
                log::warn!("telegram: dispatcher exited, will restart");
            }
            Err(e) => {
                log::error!("telegram: dispatcher panicked: {e}");
            }
        }

        backoff.wait().await;
    }
}
