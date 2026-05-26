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
            ignore: [".git", ".obsidian", ".cortex", "assets", "attachments", "quarantine"]
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
mod tests {
    use super::*;

    #[test]
    fn test_scan_config_default() {
        let config = ScanConfig::default();
        assert!(config.ignore.contains(&".git".to_string()));
        assert!(config.ignore.contains(&".obsidian".to_string()));
        // Audit quarantine must be excluded so oracle/cortex don't index
        // set-aside-pending-review notes as live knowledge.
        assert!(config.ignore.contains(&"quarantine".to_string()));
    }

    #[test]
    fn test_llm_config_default() {
        let config = LlmConfig::default();
        assert_eq!(config.provider, "claude");
        assert_eq!(config.model, "claude-sonnet-4-6");
    }

    #[test]
    fn test_resolve_secret_from_file() {
        let dir = std::env::temp_dir().join("vault-test-secret");
        fs::create_dir_all(&dir).expect("create dir");
        let file = dir.join("test-token");
        fs::write(&file, "  my-secret-value\n").expect("write");
        let result = resolve_secret(file.to_str().expect("path")).expect("resolve");
        assert_eq!(result, "my-secret-value");
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn test_resolve_secret_from_env() {
        let key = "VAULT_TEST_SECRET_42";
        unsafe { std::env::set_var(key, "env-secret-value") };
        let result = resolve_secret(key).expect("resolve");
        assert_eq!(result, "env-secret-value");
        unsafe { std::env::remove_var(key) };
    }

    #[test]
    fn test_resolve_secret_missing() {
        let result = resolve_secret("NONEXISTENT_VAR_VAULT_TEST_999");
        assert!(result.is_err());
    }
}
