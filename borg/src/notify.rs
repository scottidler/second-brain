//! Notification sinks. Three structurally identical channels:
//!
//! - [`Telegram`] talks to Telegram via teloxide. Cross-host (the bot delivers
//!   to whichever device is logged in).
//! - [`Signal`] talks to Signal via signal-rs's in-process `Client`.
//!   Host-pinned to the same machine that runs the Signal receive loop.
//! - [`Desktop`] talks to the local user-session D-Bus via `notify-rust`.
//!   Host-gated; only runs on the machine with the desktop.
//!
//! All three sinks expose `processing(...)` and `result(...)` so call sites
//! stay parallel. Future channels go side-by-side rather than behind a trait
//! - three sinks does not yet justify the abstraction.

use std::sync::Arc;
use std::time::Duration;

use crate::config::{DesktopConfig, SignalConfig, TelegramConfig};
use crate::router::format_reply;
use crate::types::IngestResult;
use notify_rust::{Notification, NotificationHandle, Timeout};
use signal_rs::{Client as SignalClient, Recipient};
use teloxide::prelude::*;
use teloxide::types::ChatId;

#[cfg(test)]
mod tests;

// Per-channel notification timeouts. The Design Invariant in
// docs/design/2026-05-21-desktop-notifications.md ships ONE shared 500 ms
// timeout. Empirically (2026-05-21 on desk) Telegram bot HTTPS round trips
// from a residential ISP consistently exceeded 500 ms even on the warm path,
// which dropped every Telegram message. D-Bus on the same box stays under
// 500 ms comfortably, so the desktop timeout keeps its sharper wedge-
// detection bound and Telegram gets a wider one that still cannot meaningfully
// delay the 15+ minute video-transcription pipeline.
const DESKTOP_TIMEOUT_MS: u64 = 500;
const TELEGRAM_TIMEOUT_MS: u64 = 3000;
const SIGNAL_TIMEOUT_MS: u64 = 3000;

/// Display timeout for the desktop "processing" placeholder. It is
/// deliberately `Never` (not the configured `timeout_ms`): the placeholder
/// must outlive the whole pipeline so `Desktop::result` can replace it in
/// place by id. The configured `timeout_ms` governs the TERMINAL toast only.
const PLACEHOLDER_TIMEOUT: Timeout = Timeout::Never;

/// Hard gate against test-suite leakage into the user's real notification
/// systems (desktop libnotify, Telegram, Signal). On 2026-05-24 the
/// `test_ingest_connection_refused` unit test shipped a real "cannot reach
/// obsidian-borg" toast to the operator's desktop because `send_notification`
/// called libnotify unconditionally. Every D-Bus / Bot / Client send in
/// production code paths must consult this gate first; tests that need to
/// assert the rendered text use the pure `format_*` helpers, not the live
/// sinks.
pub(crate) fn real_notifications_disabled() -> bool {
    // `cfg!(test)` catches borg's own `cargo test`/`cargo test -p borg`.
    // `CARGO_TARGET_TMPDIR` is set by cargo when running integration tests
    // for any crate. `NEXTEST_RUN_ID` is set by `cargo nextest`. The
    // explicit env override is for shell debugging when an operator wants
    // to exercise the binary without producing real toasts.
    cfg!(test)
        || std::env::var_os("CARGO_TARGET_TMPDIR").is_some()
        || std::env::var_os("NEXTEST_RUN_ID").is_some()
        || std::env::var_os("BORG_DISABLE_DESKTOP_NOTIFY").is_some()
}

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
    /// On failure or `TELEGRAM_TIMEOUT_MS` timeout (per Design Invariant 2),
    /// logs a warning and returns `Err(())`.
    pub async fn processing(&self, trace_id: &str, description: &str, override_chat_id: Option<i64>) -> Result<(), ()> {
        if real_notifications_disabled() {
            log::debug!("notify::Telegram::processing: suppressed under test (trace={trace_id})");
            return Ok(());
        }
        let chat_id = self.resolve_chat_id(override_chat_id);
        let text = format!("[{trace_id}] {description}");
        let bot = self.bot.clone();
        let future = async move { bot.send_message(chat_id, text).await };

        match tokio::time::timeout(Duration::from_millis(TELEGRAM_TIMEOUT_MS), future).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => {
                log::warn!("notify: failed to send processing message: {e}");
                Err(())
            }
            Err(_) => {
                log::warn!("notify: telegram processing message timed out after {TELEGRAM_TIMEOUT_MS}ms");
                Err(())
            }
        }
    }

    /// Send the full result message (Saved/Duplicate/Failed) with HTML formatting.
    ///
    /// Appends the Obsidian deep link as plain text. Telegram strips custom
    /// URI schemes from both HTML `<a>` tags and inline keyboard buttons,
    /// so the link is included as a copyable `obsidian://` URL. Wraps the
    /// HTTPS call in a `TELEGRAM_TIMEOUT_MS` `tokio::time::timeout` per Design
    /// Invariant 2.
    pub async fn result(&self, result: &IngestResult, display_source: &str, override_chat_id: Option<i64>) {
        if real_notifications_disabled() {
            log::debug!("notify::Telegram::result: suppressed under test (display={display_source})");
            return;
        }
        let chat_id = self.resolve_chat_id(override_chat_id);
        let reply = format_telegram_reply(result, display_source);
        let bot = self.bot.clone();
        let future = async move {
            bot.send_message(chat_id, reply)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await
        };

        match tokio::time::timeout(Duration::from_millis(TELEGRAM_TIMEOUT_MS), future).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => log::warn!("notify: failed to send result message: {e}"),
            Err(_) => log::warn!("notify: telegram result message timed out after {TELEGRAM_TIMEOUT_MS}ms"),
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
        if real_notifications_disabled() {
            log::debug!("notify::Desktop::processing: suppressed under test (trace={trace_id})");
            return None;
        }
        let body = format!("[{trace_id}] {description}");
        let appname = self.appname.clone();
        // The placeholder must outlive the entire pipeline (tens of seconds to
        // minutes) so `result()` can replace it in place by id. A finite
        // display timeout lets the daemon expire it mid-pipeline and free the
        // id, which breaks the replace with `Invalid notification ID` on every
        // ingest longer than the timeout (i.e. essentially every video).
        // `cfg.timeout_ms` governs the TERMINAL toast only (see `Desktop::show`);
        // the placeholder persists until it is replaced.
        let future = async move {
            Notification::new()
                .appname(&appname)
                .summary("obsidian-borg")
                .body(&body)
                .timeout(PLACEHOLDER_TIMEOUT)
                .show_async()
                .await
        };
        match tokio::time::timeout(Duration::from_millis(DESKTOP_TIMEOUT_MS), future).await {
            Ok(Ok(handle)) => Some(handle),
            Ok(Err(e)) => {
                log::warn!("notify: failed to send desktop processing popup: {e}");
                None
            }
            Err(_) => {
                log::warn!("notify: desktop processing popup timed out after {DESKTOP_TIMEOUT_MS}ms");
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
    /// The placeholder (shown with `Timeout::Never`, see [`Self::processing`])
    /// is updated IN PLACE via [`NotificationHandle::update`], which re-sends
    /// over the placeholder's ORIGINAL D-Bus connection. That connection
    /// identity is load-bearing: GNOME rejects a `replaces_id` arriving on a
    /// different connection than the one that created it with
    /// `InvalidArgs: Invalid notification ID`. The previous approach (a fresh
    /// `Notification` + `.id()` + `show_async()`, which opens a NEW connection)
    /// therefore failed the replace on every ingest - reproduced against
    /// notify-rust 4.17 + GNOME; see
    /// `docs/design/2026-06-10-desktop-notification-replace-timeout.md`.
    /// `update()` is synchronous (`zbus::block_on`), so it runs on the blocking
    /// pool under the same 500 ms backstop. We restore the finite terminal
    /// timeout first so the "done" toast auto-dismisses (the placeholder's was
    /// `Never`). If the update fails (daemon restart, user dismissed the
    /// placeholder) we fall back to a fresh popup so the user still sees a
    /// terminal result.
    pub async fn result(&self, result: &IngestResult, display_source: &str, prior: Option<NotificationHandle>) {
        let body = format_desktop_body(result, display_source);

        if let Some(mut handle) = prior {
            if real_notifications_disabled() {
                log::debug!("notify::Desktop::result: in-place update suppressed under test");
                return;
            }
            let body_for_update = body.clone();
            let terminal_timeout = self.timeout;
            let updated = tokio::time::timeout(
                Duration::from_millis(DESKTOP_TIMEOUT_MS),
                tokio::task::spawn_blocking(move || {
                    // Mutate the original notification (via DerefMut) and
                    // re-send over its own connection. Restore the finite
                    // timeout so the terminal toast dismisses rather than
                    // persisting like the `Never` placeholder.
                    handle.body(&body_for_update);
                    handle.timeout(terminal_timeout);
                    handle.update()
                }),
            )
            .await;
            if matches!(updated, Ok(Ok(Ok(())))) {
                return;
            }
            log::debug!("notify: in-place update failed, falling back to fresh result popup");
            let _ = self.show(&body, "desktop result popup (fresh)").await;
            return;
        }

        let _ = self.show(&body, "desktop result popup").await;
    }

    /// Show a fresh result/terminal popup. Wraps the D-Bus call in the 500 ms
    /// desktop timeout. Returns `Ok(())` on success, `Err(())` on any failure
    /// (D-Bus error or timeout) with a `warn!` already logged; `label`
    /// distinguishes log lines. In-place replacement of the placeholder is done
    /// in [`Self::result`] via `update()`, never here - this only ever creates
    /// a new notification, so any failure is a genuine WARN.
    async fn show(&self, body: &str, label: &'static str) -> Result<(), ()> {
        if real_notifications_disabled() {
            log::debug!("notify::Desktop::show: suppressed under test ({label})");
            return Ok(());
        }
        let appname = self.appname.clone();
        let timeout = self.timeout;
        let body = body.to_string();
        let future = async move {
            Notification::new()
                .appname(&appname)
                .summary("obsidian-borg")
                .body(&body)
                .timeout(timeout)
                .show_async()
                .await
        };
        match tokio::time::timeout(Duration::from_millis(DESKTOP_TIMEOUT_MS), future).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => {
                log::warn!("notify: failed to send {label}: {e}");
                Err(())
            }
            Err(_) => {
                log::warn!("notify: {label} timed out after {DESKTOP_TIMEOUT_MS}ms");
                Err(())
            }
        }
    }
}

/// Signal notification sink, peer to [`Telegram`] and [`Desktop`]. Sends
/// Processing / Saved / Duplicate / Failed acks back to the inbound source
/// (Note-to-Self for the privileged Note-to-Self path, peer ACI for an
/// allowed-sender DM). Cross-method notifications (e.g. an ntfy ingest
/// acknowledged via Signal) route to `default_recipient`, populated from
/// `SignalConfig::notification_recipient`.
///
/// Clones cheap because the underlying `Client` is held by `Arc` and the
/// recipient is a small enum.
#[derive(Clone)]
pub struct Signal {
    client: Arc<SignalClient>,
    default_recipient: Recipient,
}

/// Resolve the default reply target for cross-method notifications. Extracted
/// so it can be exercised in tests without constructing a live `SignalClient`
/// (which requires a fully linked state directory).
pub(crate) fn default_recipient_for(signal_config: &SignalConfig) -> Recipient {
    match &signal_config.notification_recipient {
        None => Recipient::SelfSync,
        Some(aci) => Recipient::Aci(aci.clone()),
    }
}

impl Signal {
    /// Build from a shared client and config. Returns `Option<Self>` to match
    /// the [`Telegram::new`] shape - the constructor always succeeds today
    /// (SelfSync is always a valid default) but the Option lets future
    /// config-validation failures land without a signature change.
    pub fn new(client: Arc<SignalClient>, signal_config: &SignalConfig) -> Option<Self> {
        let default_recipient = default_recipient_for(signal_config);
        log::info!("notify: Signal notifications enabled (default={:?})", default_recipient);
        Some(Self {
            client,
            default_recipient,
        })
    }

    fn resolve_recipient(&self, override_recipient: Option<&Recipient>) -> Recipient {
        override_recipient
            .cloned()
            .unwrap_or_else(|| self.default_recipient.clone())
    }

    /// Send `[trace_id] description` ack to the resolved recipient. Returns
    /// `Ok(())` on success so callers can await delivery before starting the
    /// pipeline (preserves ordering with the pipeline's first downstream
    /// step). Logs a warn and returns `Err(())` on failure or timeout.
    pub async fn processing(
        &self,
        trace_id: &str,
        description: &str,
        override_recipient: Option<&Recipient>,
    ) -> Result<(), ()> {
        if real_notifications_disabled() {
            log::debug!("notify::Signal::processing: suppressed under test (trace={trace_id})");
            return Ok(());
        }
        let recipient = self.resolve_recipient(override_recipient);
        let body = format!("[{trace_id}] {description}");
        let client = Arc::clone(&self.client);
        let future = async move { client.send(recipient, &body).await };
        match tokio::time::timeout(Duration::from_millis(SIGNAL_TIMEOUT_MS), future).await {
            Ok(Ok(_ts)) => Ok(()),
            Ok(Err(e)) => {
                log::warn!("notify: failed to send Signal processing message: {e}");
                Err(())
            }
            Err(_) => {
                log::warn!("notify: Signal processing message timed out after {SIGNAL_TIMEOUT_MS}ms");
                Err(())
            }
        }
    }

    /// Send the full result message (Saved / Duplicate / Failed) to the
    /// resolved recipient. Body is the same plain-text rendering [`Desktop`]
    /// uses (no HTML, since Signal renders plain text in the thread). Wraps
    /// the send in `SIGNAL_TIMEOUT_MS`.
    pub async fn result(&self, result: &IngestResult, display_source: &str, override_recipient: Option<&Recipient>) {
        let body = format_signal_body(result, display_source, None);
        self.send_body(body, override_recipient, "result").await;
    }

    /// Partial-attachment ack. Used when the inbound envelope carried more
    /// than one attachment - the pipeline processes the first, this method
    /// renders a body that names the partial outcome so the operator does
    /// not think the rest of the attachments were silently consumed.
    pub async fn result_partial(
        &self,
        result: &IngestResult,
        display_source: &str,
        dropped_count: usize,
        override_recipient: Option<&Recipient>,
    ) {
        let body = format_signal_body(result, display_source, Some(dropped_count));
        self.send_body(body, override_recipient, "result (partial)").await;
    }

    async fn send_body(&self, body: String, override_recipient: Option<&Recipient>, label: &'static str) {
        if real_notifications_disabled() {
            log::debug!("notify::Signal::{label}: suppressed under test");
            return;
        }
        let recipient = self.resolve_recipient(override_recipient);
        let client = Arc::clone(&self.client);
        let future = async move { client.send(recipient, &body).await };
        match tokio::time::timeout(Duration::from_millis(SIGNAL_TIMEOUT_MS), future).await {
            Ok(Ok(_ts)) => {}
            Ok(Err(e)) => log::warn!("notify: failed to send Signal {label} message: {e}"),
            Err(_) => log::warn!("notify: Signal {label} message timed out after {SIGNAL_TIMEOUT_MS}ms"),
        }
    }
}

/// Render a Signal-bound ack body. Same plain-text shape as the desktop sink
/// for consistency, plus an optional partial-attachment line when the
/// dispatch path was `ClassifyOutcome::PartialMultiAttachment`.
fn format_signal_body(result: &IngestResult, display_source: &str, dropped_count: Option<usize>) -> String {
    let base = format_reply(result, display_source);
    let with_link = match &result.obsidian_url {
        Some(url) => format!("{base}\n{url}"),
        None => base,
    };
    match dropped_count {
        None => with_link,
        Some(dropped) => {
            let total = dropped + 1;
            format!("{with_link}\nSaved 1 of {total} attachments; multi-attachment support is v2")
        }
    }
}
