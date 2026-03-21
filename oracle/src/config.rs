//! Configuration loading for oracle

use eyre::{Result, WrapErr};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Path to the Obsidian vault root
    #[serde(default = "default_vault_root")]
    pub vault_root: String,

    /// Path to the SQLite database
    #[serde(default = "default_db_path")]
    pub db_path: String,

    /// Logging configuration
    #[serde(default)]
    pub logging: LogConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,

    pub file: Option<String>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: None,
        }
    }
}

fn default_vault_root() -> String {
    "~/repos/scottidler/obsidian".to_string()
}

fn default_db_path() -> String {
    "~/.local/share/oracle/oracle.db".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = if let Some(p) = path { p.to_path_buf() } else { Self::find_config_file()? };

        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path).wrap_err("Failed to read config file")?;
            let config: Config = serde_yaml::from_str(&contents).wrap_err("Failed to parse config file")?;
            Ok(config)
        } else {
            Ok(Self::default())
        }
    }

    fn find_config_file() -> Result<PathBuf> {
        if let Some(config_dir) = dirs::config_dir() {
            let path = config_dir.join("oracle").join("oracle.yml");
            if path.exists() {
                return Ok(path);
            }
        }

        let local = PathBuf::from("oracle.yml");
        if local.exists() {
            return Ok(local);
        }

        Ok(dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("oracle")
            .join("oracle.yml"))
    }

    pub fn vault_root(&self) -> PathBuf {
        let expanded = shellexpand::tilde(&self.vault_root);
        PathBuf::from(expanded.as_ref())
    }

    pub fn db_path(&self) -> PathBuf {
        let expanded = shellexpand::tilde(&self.db_path);
        PathBuf::from(expanded.as_ref())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            vault_root: default_vault_root(),
            db_path: default_db_path(),
            logging: LogConfig::default(),
        }
    }
}
