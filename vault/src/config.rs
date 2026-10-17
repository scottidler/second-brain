use eyre::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde::{Deserializer, Serializer};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::fabric::{ApiKeys, Vendor};

/// Post-deserialization normalization hook run by [`load_config`] on every
/// loaded config, regardless of which path in the fallback chain produced it.
/// Lets a config type derive fields from others so the two can never drift
/// (borg mirrors `llm.api-key` into `fabric.api-key` here). The default impl is
/// a no-op so a type that needs no normalization can `impl Normalize for T {}`.
pub trait Normalize {
    fn normalize(&mut self) {}
}

/// Load a `borg.yml`-shaped config with the chain:
/// 1. Explicit path (if provided)
/// 2. ~/.config/sb/borg.yml
/// 3. Default
///
/// The one borg.yml loading chain: borg's full `Config` and any narrower view
/// struct (e.g. oracle reading just the daemon address) go through here so
/// they can never resolve different files. After loading,
/// [`Normalize::normalize`] runs on every path so derived fields (e.g.
/// `fabric.api-key` mirroring `llm.api-key`) can never be missed.
pub fn load_config<T: DeserializeOwned + Default + Normalize>(config_path: Option<&PathBuf>) -> Result<T> {
    log::debug!("config::load_config: explicit={:?}", config_path);
    let mut config = load_config_inner::<T>(config_path)?;
    // Applied to EVERY load path (explicit --config, primary file,
    // defaults) so a derived field can never be left un-normalized.
    config.normalize();
    Ok(config)
}

fn load_config_inner<T: DeserializeOwned + Default>(config_path: Option<&PathBuf>) -> Result<T> {
    if let Some(path) = config_path {
        return load_from_file(path).context(format!("Failed to load config from {}", path.display()));
    }
    load_first_existing(&implicit_candidates())
}

/// Config files consulted when no explicit path is given. Absolute only: a
/// CWD-relative `./borg.yml` let any repo an agent ran in pick the daemon
/// host AND the `server.auth-token` reference (an env var name or file path),
/// which clients then sent as a bearer token to that host.
fn implicit_candidates() -> Vec<PathBuf> {
    vec![crate::paths::borg_config()]
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

/// borg's `llm:` block (`borg::config::LlmConfig` re-exports it).
///
/// `api-key` names the Anthropic credential var (or file). `api-keys` maps
/// each vendor to its var for the fabric child (`vault::fabric::command`).
/// `api-key` IS the Anthropic entry: after load, `api_keys` always carries
/// `anthropic -> api_key`, so the two can never diverge. Setting both to
/// different vars is a load error, never a silent pick.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "RawLlmConfig", rename_all = "kebab-case")]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub api_keys: ApiKeys,
}

impl Default for LlmConfig {
    fn default() -> Self {
        let api_key = DEFAULT_ANTHROPIC_KEY_VAR.to_string();
        Self {
            provider: "claude".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            api_keys: ApiKeys::anthropic(&api_key),
            api_key,
        }
    }
}

/// The Anthropic credential var when neither `api-key` nor
/// `api-keys.anthropic` is set.
const DEFAULT_ANTHROPIC_KEY_VAR: &str = "ANTHROPIC_API_KEY";

/// The `llm:` block as written, before `api-key` and `api-keys.anthropic`
/// are reconciled into one Anthropic entry.
#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct RawLlmConfig {
    provider: String,
    model: String,
    #[serde(alias = "api_key_env", alias = "api_key")]
    api_key: Option<String>,
    api_keys: ApiKeys,
}

impl Default for RawLlmConfig {
    fn default() -> Self {
        let defaults = LlmConfig::default();
        Self {
            provider: defaults.provider,
            model: defaults.model,
            api_key: None,
            api_keys: ApiKeys::default(),
        }
    }
}

impl TryFrom<RawLlmConfig> for LlmConfig {
    type Error = String;

    fn try_from(raw: RawLlmConfig) -> std::result::Result<Self, Self::Error> {
        let mapped = raw.api_keys.get(Vendor::Anthropic).map(str::to_string);
        let api_key = match (raw.api_key, mapped) {
            (Some(single), Some(mapped)) if single != mapped => {
                return Err(format!(
                    "llm.api-key ({single}) and llm.api-keys.anthropic ({mapped}) name different vars; \
                     set one (api-key is the anthropic entry)"
                ));
            }
            (Some(single), _) => single,
            (None, Some(mapped)) => mapped,
            (None, None) => DEFAULT_ANTHROPIC_KEY_VAR.to_string(),
        };
        let api_keys = raw.api_keys.with(Vendor::Anthropic, api_key.clone());
        Ok(Self {
            provider: raw.provider,
            model: raw.model,
            api_key,
            api_keys,
        })
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

/// Deserialize a humantime string (`15m`, `90s`, `1h 30m`) straight into a
/// `Duration`. An unparseable value is a deserialize error naming `key` and
/// the bad value, so the YAML load itself fails. `key` is explicit because
/// serde_yaml's error path stops at the enclosing map for a field-level
/// `deserialize_with` error (observed: `queue: invalid duration ...`).
pub fn deserialize_humantime<'de, D: Deserializer<'de>>(
    deserializer: D,
    key: &str,
) -> std::result::Result<Duration, D::Error> {
    let raw = String::deserialize(deserializer)?;
    humantime::parse_duration(raw.trim())
        .map_err(|e| serde::de::Error::custom(format!("{key}: invalid duration {raw:?} (humantime, e.g. 15m): {e}")))
}

/// Serialize a `Duration` back to the humantime form [`deserialize_humantime`]
/// reads, so a round-tripped config keeps its shape.
pub fn serialize_humantime<S: Serializer>(value: &Duration, serializer: S) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&humantime::format_duration(*value).to_string())
}

#[cfg(test)]
mod tests;
