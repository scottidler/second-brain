use super::*;
use crate::config::DesktopConfig;
use crate::router::format_reply;
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
        obsidian_url: Some("obsidian://search?vault=obsidian&query=test-article".to_string()),
        ..Default::default()
    };
    let reply = format_telegram_reply(&result, "https://example.com");
    assert!(reply.contains("Saved: Test Article"));
    assert!(reply.contains("obsidian://search?vault=obsidian&amp;query=test-article"));
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
        obsidian_url: Some("obsidian://search?vault=obsidian&query=test".to_string()),
        ..Default::default()
    };
    let reply = format_telegram_reply(&result, "https://example.com");
    assert!(reply.contains("&lt;html&gt;"));
    assert!(reply.contains("&amp;"));
    assert!(reply.contains("obsidian://search?vault=obsidian&amp;query=test"));
}

#[test]
fn test_telegram_new_with_notification_chat_id() {
    let config = TelegramConfig {
        bot_token: "fake-token".to_string(),
        allowed_chat_ids: vec![111],
        notification_chat_id: Some(222),
        host: None,
    };
    let notifier = Telegram::new("fake-token", &config);
    assert!(notifier.is_some());
    let n = notifier.expect("should be Some");
    assert_eq!(n.default_chat_id, ChatId(222));
}

#[test]
fn test_telegram_new_falls_back_to_allowed_chat_ids() {
    let config = TelegramConfig {
        bot_token: "fake-token".to_string(),
        allowed_chat_ids: vec![333],
        notification_chat_id: None,
        host: None,
    };
    let notifier = Telegram::new("fake-token", &config);
    assert!(notifier.is_some());
    let n = notifier.expect("should be Some");
    assert_eq!(n.default_chat_id, ChatId(333));
}

#[test]
fn test_telegram_new_returns_none_when_no_chat_id() {
    let config = TelegramConfig {
        bot_token: "fake-token".to_string(),
        allowed_chat_ids: vec![],
        notification_chat_id: None,
        host: None,
    };
    let notifier = Telegram::new("fake-token", &config);
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
    let notifier = Telegram::new("fake-token", &config).expect("should be Some");
    assert_eq!(notifier.resolve_chat_id(Some(999)), ChatId(999));
    assert_eq!(notifier.resolve_chat_id(None), ChatId(111));
}

#[test]
fn test_desktop_new_disabled_returns_none() {
    let cfg = DesktopConfig {
        enabled: false,
        ..Default::default()
    };
    assert!(Desktop::new(&cfg).is_none());
}

#[test]
fn test_desktop_new_enabled_returns_some() {
    let cfg = DesktopConfig {
        enabled: true,
        host: None,
        timeout_ms: 5000,
        appname: "borg".to_string(),
    };
    let n = Desktop::new(&cfg).expect("enabled config builds notifier");
    assert_eq!(n.appname, "borg");
    assert_eq!(n.timeout, Timeout::Milliseconds(5000));
}

/// The "processing" placeholder must NEVER expire on its own: it has to
/// outlive the whole pipeline so `result()` can replace it in place by id.
/// A finite placeholder timeout (the old bug: it reused `cfg.timeout_ms`)
/// lets the daemon free the id mid-pipeline, breaking the replace with
/// `Invalid notification ID` on every ingest longer than the timeout. The
/// terminal toast keeps the configured finite timeout - the two must differ.
#[test]
fn test_placeholder_timeout_never_expires_and_differs_from_terminal() {
    let cfg = DesktopConfig {
        enabled: true,
        host: None,
        timeout_ms: 5000,
        appname: "borg".to_string(),
    };
    let n = Desktop::new(&cfg).expect("enabled config builds notifier");
    assert_eq!(PLACEHOLDER_TIMEOUT, Timeout::Never);
    assert_ne!(PLACEHOLDER_TIMEOUT, n.timeout);
}

/// Body must byte-match `format_reply` when no `obsidian_url` is set.
/// Locks in the cross-channel rendering parity invariant from the design doc.
#[test]
fn test_format_desktop_body_matches_format_reply_completed() {
    let result = IngestResult {
        status: IngestStatus::Completed,
        title: Some("Test Article".to_string()),
        tags: vec!["ai".to_string(), "tech".to_string()],
        elapsed_secs: Some(4.56),
        ..Default::default()
    };
    assert_eq!(
        format_desktop_body(&result, "https://example.com"),
        format_reply(&result, "https://example.com")
    );
}

#[test]
fn test_format_desktop_body_matches_format_reply_failed() {
    let result = IngestResult {
        status: IngestStatus::Failed {
            reason: "network error".to_string(),
        },
        elapsed_secs: Some(2.3),
        ..Default::default()
    };
    assert_eq!(
        format_desktop_body(&result, "https://example.com/broken"),
        format_reply(&result, "https://example.com/broken")
    );
}

#[test]
fn test_format_desktop_body_matches_format_reply_duplicate() {
    let result = IngestResult {
        status: IngestStatus::Duplicate {
            original_date: "2026-03-16".to_string(),
        },
        elapsed_secs: Some(0.01),
        trace_id: Some("tg-7f3a2c".to_string()),
        ..Default::default()
    };
    assert_eq!(
        format_desktop_body(&result, "https://example.com"),
        format_reply(&result, "https://example.com")
    );
}

#[test]
fn test_format_desktop_body_matches_format_reply_queued() {
    let result = IngestResult {
        status: IngestStatus::Queued,
        ..Default::default()
    };
    assert_eq!(
        format_desktop_body(&result, "https://example.com"),
        format_reply(&result, "https://example.com")
    );
}

/// When `obsidian_url` is set, format_desktop_body appends it as plain text on
/// a new line - no HTML escape, no markup. This is the one structural divergence
/// from `format_reply` (which knows nothing about obsidian_url) and from
/// `format_telegram_reply` (which HTML-escapes the URL).
#[test]
fn test_format_desktop_body_appends_obsidian_url_unescaped() {
    let result = IngestResult {
        status: IngestStatus::Completed,
        title: Some("Test".to_string()),
        tags: vec![],
        obsidian_url: Some("obsidian://search?vault=obsidian&query=test".to_string()),
        ..Default::default()
    };
    let body = format_desktop_body(&result, "https://example.com");
    assert!(body.contains("Saved: Test"));
    assert!(body.ends_with("obsidian://search?vault=obsidian&query=test"));
    assert!(!body.contains("&amp;"));
}

// ---------------- Signal constructor (recipient resolution) ----------------

fn mk_signal_config(notification_recipient: Option<&str>) -> SignalConfig {
    SignalConfig {
        allowed_senders: vec![],
        notification_recipient: notification_recipient.map(str::to_string),
        host: "test-host".to_string(),
        notetoself_rate_threshold_per_hour: 100,
    }
}

#[test]
fn signal_default_recipient_is_selfsync_when_unset() {
    let cfg = mk_signal_config(None);
    assert_eq!(default_recipient_for(&cfg), Recipient::SelfSync);
}

#[test]
fn signal_default_recipient_is_aci_when_set() {
    let cfg = mk_signal_config(Some("11111111-1111-1111-1111-111111111111"));
    assert_eq!(
        default_recipient_for(&cfg),
        Recipient::Aci("11111111-1111-1111-1111-111111111111".to_string())
    );
}

#[test]
fn real_notifications_disabled_under_cargo_test() {
    // Regression for 2026-05-24: `test_ingest_connection_refused` fired a
    // real "cannot reach obsidian-borg" libnotify toast at the operator's
    // desktop because send_notification was unconditional. The gate must
    // return true any time cargo test is the harness so no future test
    // can leak a real notification to D-Bus / Telegram / Signal.
    assert!(
        real_notifications_disabled(),
        "real_notifications_disabled() must return true under cfg!(test)"
    );
}
