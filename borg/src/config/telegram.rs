use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TelegramConfig {
    #[serde(alias = "bot_token_env", alias = "bot_token")]
    pub bot_token: String,
    #[serde(default, alias = "allowed_chat_ids")]
    pub allowed_chat_ids: Vec<i64>,
    /// Default chat ID for cross-method notifications.
    /// If not set, falls back to first allowed_chat_ids entry.
    #[serde(default, alias = "notification_chat_id")]
    pub notification_chat_id: Option<i64>,
    /// If set, only run the Telegram poller on the host with this hostname.
    #[serde(default)]
    pub host: Option<String>,
}
