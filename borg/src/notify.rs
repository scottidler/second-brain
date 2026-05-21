//! Notification sinks. Two structurally identical channels:
//!
//! - [`Telegram`] talks to Telegram via teloxide. Cross-host (the bot delivers
//!   to whichever device is logged in).
//! - [`Desktop`] talks to the local user-session D-Bus via `notify-rust`.
//!   Host-gated; only runs on the machine with the desktop.
//!
//! Both sinks expose `processing(...)` and `result(...)` so call sites stay
//! parallel. Future channels go side-by-side rather than behind a trait -
//! two sinks does not yet justify the abstraction.

use std::time::Duration;

use crate::config::{DesktopConfig, TelegramConfig};
use crate::router::format_reply;
use crate::types::IngestResult;
use notify_rust::{Notification, NotificationHandle, Timeout};
use teloxide::prelude::*;
use teloxide::types::ChatId;

#[cfg(test)]
mod tests;

/// Per the Design Invariant in docs/design/2026-05-21-desktop-notifications.md,
/// every notification call is bounded so a wedged service cannot delay the
/// pipeline. D-Bus default timeout is ~25s; Telegram round trip can be slow on
/// flaky links. 500 ms is comfortably above the warm-bus / warm-HTTPS latency
/// while still being negligible on the 15+ minute YouTube-transcription
/// pipeline timeline.
const NOTIFICATION_CALL_TIMEOUT_MS: u64 = 500;

/// Telegram notification sink. Clone-cheap: `Bot` is an HTTP client wrapper.
#[derive(Clone)]
pub struct Telegram {
    bot: Bot,
    default_chat_id: ChatId,
}

impl Telegram {
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
    /// Appends the Obsidian deep link as plain text. Telegram strips custom
    /// URI schemes from both HTML `<a>` tags and inline keyboard buttons,
    /// so the link is included as a copyable `obsidian://` URL.
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

/// Format an `IngestResult` as an HTML Telegram message.
///
/// Appends the Obsidian deep link as plain text when available. Telegram
/// strips custom URI schemes from both `<a>` tags and inline keyboard
/// buttons, so we include it as a copyable URL.
pub fn format_telegram_reply(result: &IngestResult, display_source: &str) -> String {
    let base = format_reply(result, display_source);
    let escaped = html_escape(&base);
    match &result.obsidian_url {
        Some(url) => format!("{escaped}\n{}", html_escape(url)),
        None => escaped,
    }
}

/// Format an `IngestResult` as a plain-text desktop notification body.
///
/// Identical to [`format_reply`] modulo the appended Obsidian deep link
/// (which the notification daemon renders as plain text - no HTML escape).
/// Kept as a thin wrapper so the desktop body is testably byte-equal to the
/// Telegram body before HTML escaping; any divergence in either channel is
/// caught by `test_format_desktop_body_matches_format_reply` in `notify/tests.rs`.
pub fn format_desktop_body(result: &IngestResult, display_source: &str) -> String {
    let base = format_reply(result, display_source);
    match &result.obsidian_url {
        Some(url) => format!("{base}\n{url}"),
        None => base,
    }
}

/// Desktop notification sink, peer to [`Telegram`]. Renders the same intake /
/// terminal messages onto the local user-session D-Bus via `notify-rust`.
/// The `processing` toast is replaced in place by the terminal toast (the
/// [`NotificationHandle`] from `processing` is threaded through to `result`),
/// so one ingest produces one persistent popup that updates rather than two
/// stacking popups.
#[derive(Clone)]
pub struct Desktop {
    appname: String,
    timeout: Timeout,
}

impl Desktop {
    /// Build from config. Returns `None` when `enabled: false` so call sites
    /// can mirror the [`Telegram::new`] `Option<Self>` pattern.
    pub fn new(cfg: &DesktopConfig) -> Option<Self> {
        if !cfg.enabled {
            log::info!("notify: desktop notifications disabled");
            return None;
        }
        log::info!(
            "notify: desktop notifications enabled (appname={}, timeout_ms={})",
            cfg.appname,
            cfg.timeout_ms
        );
        Some(Self {
            appname: cfg.appname.clone(),
            timeout: Timeout::Milliseconds(cfg.timeout_ms),
        })
    }

    /// Fire the `[trace_id] description` intake popup and return its handle.
    /// The handle is later passed to [`Self::result`] so the terminal popup
    /// REPLACES this one in place (notify-rust's id-based replace pattern).
    /// Returns `None` on D-Bus error or 500 ms timeout; `result(...)` then
    /// falls back to a fresh popup.
    pub async fn processing(&self, trace_id: &str, description: &str) -> Option<NotificationHandle> {
        let body = format!("[{trace_id}] {description}");
        let appname = self.appname.clone();
        let timeout = self.timeout;
        let future = async move {
            Notification::new()
                .appname(&appname)
                .summary("obsidian-borg")
                .body(&body)
                .timeout(timeout)
                .show_async()
                .await
        };
        match tokio::time::timeout(Duration::from_millis(NOTIFICATION_CALL_TIMEOUT_MS), future).await {
            Ok(Ok(handle)) => Some(handle),
            Ok(Err(e)) => {
                log::warn!("notify: failed to send desktop processing popup: {e}");
                None
            }
            Err(_) => {
                log::warn!("notify: desktop processing popup timed out after {NOTIFICATION_CALL_TIMEOUT_MS}ms");
                None
            }
        }
    }

    /// Fire the terminal popup from an `IngestResult`. When `prior` is
    /// `Some(handle)`, the new popup is published with the same id and
    /// replaces the prior popup in place. When `None`, a fresh popup is
    /// created. Wraps the D-Bus call in 500 ms timeout per the Design
    /// Invariant.
    ///
    /// Note: uses `.id(prior.id()) + show_async()` rather than
    /// `prior.update()` because `NotificationHandle::update` is synchronous
    /// in notify-rust v4, which would defeat the timeout wrapper - a sync
    /// call inside `timeout(async { ... })` has no await points and blocks
    /// the worker until D-Bus replies.
    pub async fn result(&self, result: &IngestResult, display_source: &str, prior: Option<NotificationHandle>) {
        let body = format_desktop_body(result, display_source);
        let appname = self.appname.clone();
        let timeout = self.timeout;
        let prior_id = prior.as_ref().map(|h| h.id());
        let future = async move {
            let mut n = Notification::new();
            n.appname(&appname)
                .summary("obsidian-borg")
                .body(&body)
                .timeout(timeout);
            if let Some(id) = prior_id {
                n.id(id);
            }
            n.show_async().await
        };
        match tokio::time::timeout(Duration::from_millis(NOTIFICATION_CALL_TIMEOUT_MS), future).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => log::warn!("notify: failed to send desktop result popup: {e}"),
            Err(_) => log::warn!("notify: desktop result popup timed out after {NOTIFICATION_CALL_TIMEOUT_MS}ms"),
        }
    }
}
