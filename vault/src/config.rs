use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Shared scan configuration - what directories to ignore during vault scanning.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub ignore: Vec<String>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        // `quarantine` is where audit `--fix duplicate` parks set-aside notes
        // (`system/quarantine/<source-key>/...`); they keep their original
        // frontmatter, so without this exclusion every consumer of
        // `scan_vault` would index them as live knowledge.
        Self {
            ignore: [
                ".git",
                ".obsidian",
                ".cortex",
                "assets",
                "attachments",
                "quarantine",
                ".claude",
                "templates",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

/// Shared LLM configuration used by both borg and cortex.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    #[serde(alias = "api_key_env", alias = "api_key", alias = "api-key")]
    pub api_key: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "claude".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            api_key: "ANTHROPIC_API_KEY".to_string(),
        }
    }
}

/// Resolve a secret value: if the value is a path to an existing file, read its contents;
/// otherwise treat it as an environment variable name and resolve from env.
pub fn resolve_secret(value: &str) -> Result<String> {
    let expanded = shellexpand::tilde(value);
    let path = Path::new(expanded.as_ref());
    if path.exists() {
        Ok(fs::read_to_string(path)?.trim().to_string())
    } else {
        std::env::var(value).context(format!("secret '{value}' is not a file and env var is not set"))
    }
}

#[cfg(test)]
mod tests;
