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

    /// File watcher configuration
    #[serde(default)]
    pub watcher: WatcherConfig,

    /// How often (seconds) oracle recomputes `inbound_link_count` for
    /// every note. Modeled on the cortex embed-tick cadence: long enough
    /// that the cost is amortized over many vault edits, short enough
    /// that the cold-note report's structural signal is at worst minutes
    /// stale. Cold reports run weekly; a 10-minute cadence is several
    /// orders of magnitude faster than the consumer.
    #[serde(
        default = "default_inbound_recompute_interval_secs",
        rename = "inbound-recompute-interval-secs"
    )]
    pub inbound_recompute_interval_secs: u64,
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

#[derive(Debug, Clone, Deserialize)]
pub struct WatcherConfig {
    /// Whether to enable the file watcher for live reindexing
    #[serde(default = "default_watcher_enable")]
    pub enable: bool,

    /// Seconds to wait after last event before reindexing
    #[serde(default = "default_debounce_secs", rename = "debounce-secs")]
    pub debounce_secs: u64,

    /// Directory names to ignore
    #[serde(default = "default_ignore_dirs")]
    pub ignore: Vec<String>,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            enable: default_watcher_enable(),
            debounce_secs: default_debounce_secs(),
            ignore: default_ignore_dirs(),
        }
    }
}

fn default_watcher_enable() -> bool {
    true
}

fn default_debounce_secs() -> u64 {
    5
}

fn default_ignore_dirs() -> Vec<String> {
    vec![".git".into(), ".obsidian".into(), "templates".into()]
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

fn default_inbound_recompute_interval_secs() -> u64 {
    600
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
        let primary = vault::paths::oracle_config();
        if primary.exists() {
            return Ok(primary);
        }

        let local = PathBuf::from("oracle.yml");
        if local.exists() {
            return Ok(local);
        }

        Ok(primary)
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
            watcher: WatcherConfig::default(),
            inbound_recompute_interval_secs: default_inbound_recompute_interval_secs(),
        }
    }
}
