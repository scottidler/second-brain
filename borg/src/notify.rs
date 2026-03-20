use crate::config::TelegramConfig;
use crate::router::format_reply;
use crate::types::IngestResult;
use teloxide::prelude::*;
use teloxide::types::ChatId;

/// Shared notification service that sends feedback via Telegram.
/// Clone-cheap: `Bot` is an HTTP client wrapper.
#[derive(Clone)]
pub struct Notifier {
    bot: Bot,
    default_chat_id: ChatId,
}

impl Notifier {
    /// Build from a resolved bot token and Telegram config.
    ///
    /// Chat ID resolution order:
    ///   1. `tg_config.notification_chat_id` (explicit)
    ///   2. `tg_config.allowed_chat_ids[0]` (implicit fallback)
    ///   3. `None` - no destination available, notifier disabled
    pub fn new(token: &str, tg_config: &TelegramConfig) -> Option<Self> {
        let chat_id = tg_config
            .notification_chat_id
            .or_else(|| tg_config.allowed_chat_ids.first().copied());

        let chat_id = match chat_id {
            Some(id) => {
                log::info!("notify: Telegram notifications enabled (chat_id: {id})");
                ChatId(id)
            }
            None => {
                log::warn!("notify: no notification-chat-id or allowed-chat-ids configured, notifications disabled");
                return None;
            }
        };

        let bot = Bot::new(token);
        Some(Self {
            bot,
            default_chat_id: chat_id,
        })
    }

    /// Resolve the target chat ID: use override if provided, else default.
    fn resolve_chat_id(&self, override_chat_id: Option<i64>) -> ChatId {
        override_chat_id.map(ChatId).unwrap_or(self.default_chat_id)
    }

    /// Send `[trace_id] Processing...` message.
    ///
    /// Returns `Ok(())` on success so callers can await delivery before
    /// starting the pipeline (preserves message ordering).
    /// On failure, logs a warning and returns `Err(())`.
    pub async fn processing(&self, trace_id: &str, description: &str, override_chat_id: Option<i64>) -> Result<(), ()> {
        let chat_id = self.resolve_chat_id(override_chat_id);
        let text = format!("[{trace_id}] {description}");

        match self.bot.send_message(chat_id, text).await {
            Ok(_) => Ok(()),
            Err(e) => {
                log::warn!("notify: failed to send processing message: {e}");
                Err(())
            }
        }
    }

    /// Send the full result message (Saved/Duplicate/Failed) with HTML formatting.
    ///
    /// Fire-and-forget: logs on failure, never errors the caller.
    pub async fn result(&self, result: &IngestResult, display_source: &str, override_chat_id: Option<i64>) {
        let chat_id = self.resolve_chat_id(override_chat_id);
        let reply = format_telegram_reply(result, display_source);

        if let Err(e) = self
            .bot
            .send_message(chat_id, reply)
            .parse_mode(teloxide::types::ParseMode::Html)
            .await
        {
            log::warn!("notify: failed to send result message: {e}");
        }
    }
}

/// Escape HTML special characters for Telegram messages.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Format an `IngestResult` as an HTML Telegram message with an optional
/// clickable "Open in Obsidian" deep link.
pub fn format_telegram_reply(result: &IngestResult, display_source: &str) -> String {
    let base = format_reply(result, display_source);
    let escaped = html_escape(&base);

    match &result.obsidian_url {
        Some(url) => format!("{escaped}\n<a href=\"{url}\">Open in Obsidian</a>"),
        None => escaped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{IngestResult, IngestStatus};

    #[test]
    fn test_html_escape_special_chars() {
        assert_eq!(
            html_escape("<script>alert('xss')</script>"),
            "&lt;script&gt;alert('xss')&lt;/script&gt;"
        );
        assert_eq!(html_escape("AT&T"), "AT&amp;T");
        assert_eq!(html_escape("no special chars"), "no special chars");
    }

    #[test]
    fn test_html_escape_mixed() {
        assert_eq!(html_escape("a < b & c > d"), "a &lt; b &amp; c &gt; d");
    }

    #[test]
    fn test_format_telegram_reply_with_obsidian_url() {
        let result = IngestResult {
            status: IngestStatus::Completed,
            title: Some("Test Article".to_string()),
            tags: vec!["ai".to_string()],
            elapsed_secs: Some(3.5),
            domain: Some("tech".to_string()),
            obsidian_url: Some("obsidian://open?vault=obsidian&file=notes%2Ftest-article.md".to_string()),
            ..Default::default()
        };
        let reply = format_telegram_reply(&result, "https://example.com");
        assert!(reply.contains("Saved: Test Article"));
        assert!(
            reply.contains(
                "<a href=\"obsidian://open?vault=obsidian&file=notes%2Ftest-article.md\">Open in Obsidian</a>"
            )
        );
    }

    #[test]
    fn test_format_telegram_reply_without_obsidian_url() {
        let result = IngestResult {
            status: IngestStatus::Failed {
                reason: "network error".to_string(),
            },
            ..Default::default()
        };
        let reply = format_telegram_reply(&result, "https://example.com");
        assert!(reply.contains("Failed"));
        assert!(!reply.contains("Open in Obsidian"));
    }

    #[test]
    fn test_format_telegram_reply_escapes_html_in_title() {
        let result = IngestResult {
            status: IngestStatus::Completed,
            title: Some("Title with <html> & stuff".to_string()),
            tags: vec![],
            obsidian_url: Some("obsidian://open?vault=obsidian&file=test.md".to_string()),
            ..Default::default()
        };
        let reply = format_telegram_reply(&result, "https://example.com");
        assert!(reply.contains("&lt;html&gt;"));
        assert!(reply.contains("&amp;"));
        assert!(reply.contains("<a href="));
    }

    #[test]
    fn test_notifier_new_with_notification_chat_id() {
        let config = TelegramConfig {
            bot_token: "fake-token".to_string(),
            allowed_chat_ids: vec![111],
            notification_chat_id: Some(222),
            host: None,
        };
        let notifier = Notifier::new("fake-token", &config);
        assert!(notifier.is_some());
        let n = notifier.expect("should be Some");
        assert_eq!(n.default_chat_id, ChatId(222));
    }

    #[test]
    fn test_notifier_new_falls_back_to_allowed_chat_ids() {
        let config = TelegramConfig {
            bot_token: "fake-token".to_string(),
            allowed_chat_ids: vec![333],
            notification_chat_id: None,
            host: None,
        };
        let notifier = Notifier::new("fake-token", &config);
        assert!(notifier.is_some());
        let n = notifier.expect("should be Some");
        assert_eq!(n.default_chat_id, ChatId(333));
    }

    #[test]
    fn test_notifier_new_returns_none_when_no_chat_id() {
        let config = TelegramConfig {
            bot_token: "fake-token".to_string(),
            allowed_chat_ids: vec![],
            notification_chat_id: None,
            host: None,
        };
        let notifier = Notifier::new("fake-token", &config);
        assert!(notifier.is_none());
    }

    #[test]
    fn test_resolve_chat_id_override() {
        let config = TelegramConfig {
            bot_token: "fake-token".to_string(),
            allowed_chat_ids: vec![111],
            notification_chat_id: None,
            host: None,
        };
        let notifier = Notifier::new("fake-token", &config).expect("should be Some");
        assert_eq!(notifier.resolve_chat_id(Some(999)), ChatId(999));
        assert_eq!(notifier.resolve_chat_id(None), ChatId(111));
    }
}
