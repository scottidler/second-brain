use serde::{Deserialize, Serialize};

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
}

fn default_ntfy_server() -> String {
    "https://ntfy.sh".to_string()
}
