use serde::{Deserialize, Serialize};

use super::APP_NAME;

/// Config for the desktop notification sink (a sibling of the Telegram sink).
/// The sink shells out to the user session D-Bus via `notify-rust`. Default
/// `enabled: false` keeps headless borg hosts silent; new machines pick up
/// `enabled: true` via the `sb bootstrap` template.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DesktopConfig {
    /// If false, no DesktopNotifier is constructed and the daemon stays silent
    /// on the desktop. Telegram is unaffected.
    pub enabled: bool,
    /// If set, only construct the notifier on the host with this hostname.
    /// Mirrors the telegram/discord/ntfy host gating so a headless host does
    /// not fight a non-existent D-Bus session.
    pub host: Option<String>,
    /// Toast lifetime hint passed to the notification daemon, in milliseconds.
    pub timeout_ms: u32,
    /// Application name shown by the notification daemon.
    pub appname: String,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: None,
            timeout_ms: 5000,
            appname: APP_NAME.to_string(),
        }
    }
}
