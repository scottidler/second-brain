use serde::{Deserialize, Deserializer, Serialize};
use std::time::Duration;

use super::{DEFAULT_NTFY_READ_TIMEOUT, deserialize_humantime, serialize_humantime};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct NtfyConfig {
    pub topic: String,
    #[serde(default = "default_ntfy_server")]
    pub server: String,
    pub token: Option<String>,
    /// If set, only run the ntfy subscriber on the host with this hostname.
    #[serde(default)]
    pub host: Option<String>,
    /// Longest silence tolerated on the subscription (humantime) before the
    /// stream counts as dead and is reconnected; also bounds the connect.
    /// Must exceed the server's keepalive interval (ntfy.sh: 45 s).
    #[serde(
        default = "default_ntfy_read_timeout",
        deserialize_with = "deserialize_read_timeout",
        serialize_with = "serialize_humantime"
    )]
    pub read_timeout: Duration,
}

fn default_ntfy_server() -> String {
    "https://ntfy.sh".to_string()
}

fn default_ntfy_read_timeout() -> Duration {
    DEFAULT_NTFY_READ_TIMEOUT
}

fn deserialize_read_timeout<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Duration, D::Error> {
    deserialize_humantime(deserializer, "read-timeout")
}
