use eyre::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The CWD-relative last-resort `borg.yml` consulted by [`load_config`] when
/// neither an explicit path nor `~/.config/sb/borg.yml` exists.
const BORG_FALLBACK_CONFIG: &str = "borg.yml";

/// Post-deserialization normalization hook run by [`load_config`] on every
/// loaded config, regardless of which path in the fallback chain produced it.
/// Lets a config type derive fields from others so the two can never drift
/// (borg mirrors `llm.api-key` into `fabric.api-key` here). The default impl is
/// a no-op so a type that needs no normalization can `impl Normalize for T {}`.
pub trait Normalize {
    fn normalize(&mut self) {}
}

/// Load a `borg.yml`-shaped config with the fallback chain:
/// 1. Explicit path (if provided)
/// 2. ~/.config/sb/borg.yml
/// 3. ./borg.yml
/// 4. Default
///
/// The one borg.yml loading chain: borg's full `Config` and any narrower view
/// struct (e.g. oracle reading just the daemon address) go through here so
/// they can never resolve different files. After loading,
/// [`Normalize::normalize`] runs on every path so derived fields (e.g.
/// `fabric.api-key` mirroring `llm.api-key`) can never be missed.
pub fn load_config<T: DeserializeOwned + Default + Normalize>(config_path: Option<&PathBuf>) -> Result<T> {
    log::debug!("config::load_config: explicit={:?}", config_path);
    let mut config = load_config_inner::<T>(config_path)?;
    // Applied to EVERY load path (explicit --config, primary, fallback,
    // defaults) so a derived field can never be left un-normalized.
    config.normalize();
    Ok(config)
}

fn load_config_inner<T: DeserializeOwned + Default>(config_path: Option<&PathBuf>) -> Result<T> {
    if let Some(path) = config_path {
        return load_from_file(path).context(format!("Failed to load config from {}", path.display()));
    }
    load_first_existing(&[crate::paths::borg_config(), PathBuf::from(BORG_FALLBACK_CONFIG)])
}

/// The first candidate that exists is THE config: a parse error there is a
/// loud error, never a silent fall-through to the next candidate or to
/// defaults (a typo in borg.yml once ran the daemon on default settings).
/// Defaults apply only when no candidate exists.
fn load_first_existing<T: DeserializeOwned + Default>(candidates: &[PathBuf]) -> Result<T> {
    log::debug!("config::load_first_existing: candidates={:?}", candidates);
    match candidates.iter().find(|p| p.exists()) {
        Some(path) => load_from_file(path).context(format!("Failed to load config from {}", path.display())),
        None => {
            log::info!("No config file found, using defaults");
            Ok(T::default())
        }
    }
}

fn load_from_file<T: DeserializeOwned, P: AsRef<Path>>(path: P) -> Result<T> {
    let content = fs::read_to_string(&path).context("Failed to read config file")?;
    let config: T = serde_yaml::from_str(&content).context("Failed to parse config file")?;
    log::info!("Loaded config from: {}", path.as_ref().display());
    Ok(config)
}

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
