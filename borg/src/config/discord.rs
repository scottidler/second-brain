use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct DiscordConfig {
    #[serde(alias = "bot_token_env", alias = "bot_token")]
    pub bot_token: String,
    #[serde(alias = "channel_id")]
    pub channel_id: u64,
    /// If set, only run the Discord bot on the host with this hostname.
    #[serde(default)]
    pub host: Option<String>,
}
