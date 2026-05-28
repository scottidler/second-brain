//! Glean configuration loaded from `~/.config/sb/glean.yml`.
//!
//! Source of truth for the template ships alongside the other sb
//! templates in `config/templates/glean.yml.example`. `sb bootstrap`
//! writes it write-if-missing. Hardcoded defaults below are the safety
//! net for fresh installs; every field has a sensible default so a
//! missing-config run still works.

use eyre::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

use crate::error::GleanError;

const DEFAULT_PROJECTS_DIR: &str = "~/.claude/projects";
const DEFAULT_CLUSTER_ALGORITHM: &str = "complete-link";
const DEFAULT_CLUSTER_SIMILARITY_THRESHOLD: f32 = 0.78;
const DEFAULT_CLUSTER_MIN_SIZE: usize = 2;
const DEFAULT_FABRIC_BINARY: &str = "fabric";
const DEFAULT_FABRIC_MAX_INPUT_CHARS: usize = 600_000;
const DEFAULT_FABRIC_CLASSIFY_TIMEOUT_SECS: u64 = 240;
const DEFAULT_FABRIC_DISTILL_TIMEOUT_SECS: u64 = 600;
const DEFAULT_FABRIC_DREAM_TIMEOUT_SECS: u64 = 240;
const DEFAULT_FABRIC_CLASSIFY_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_FABRIC_DISTILL_MODEL: &str = "claude-opus-4-7";
const DEFAULT_FABRIC_DREAM_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_HARVEST_INTERVAL_SECS: u64 = 600;
const DEFAULT_DREAM_INTERVAL_SECS: u64 = 86_400;
const DEFAULT_HARVEST_PARALLELISM: usize = 8;
const DEFAULT_DISTILL_PARALLELISM: usize = 4;
const DEFAULT_DREAM_PARALLELISM: usize = 4;
const DEFAULT_DAEMON_DEBOUNCE_SECS: u64 = 30;
const DEFAULT_GLEAN_DIR: &str = "notes/glean";
const DEFAULT_DREAMS_DIR: &str = "notes/glean-dreams";
const DEFAULT_INTERACTION_TURN_BUDGET_CHARS: usize = 800;
const DEFAULT_BUNDLE_BUDGET_CHARS: usize = 500_000;

/// Top-level glean config.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub vault: VaultConfig,
    pub claude: ClaudeConfig,
    pub fabric: FabricConfig,
    pub cluster: ClusterConfig,
    pub daemon: DaemonConfig,
    pub bundle: BundleConfig,
}

impl Config {
    /// Load `~/.config/sb/glean.yml`. Missing file falls back to
    /// defaults so a fresh install (no bootstrap yet) does not block
    /// startup.
    pub fn load() -> Result<Self> {
        let path = vault::paths::config_root().join("glean.yml");
        log::debug!("config::load: path={}", path.display());
        if !path.exists() {
            log::info!("glean config not found at {}; using defaults", path.display());
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path).with_context(|| format!("read glean config {}", path.display()))?;
        let cfg: Self = serde_yaml::from_str(&raw)
            .map_err(GleanError::Yaml)
            .with_context(|| format!("parse glean config {}", path.display()))?;
        Ok(cfg)
    }
}

/// Where the Obsidian vault lives and which subdirs glean writes to.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct VaultConfig {
    #[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]
    pub root_path: PathBuf,
    pub glean_dir: String,
    pub dreams_dir: String,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            root_path: PathBuf::new(),
            glean_dir: DEFAULT_GLEAN_DIR.to_string(),
            dreams_dir: DEFAULT_DREAMS_DIR.to_string(),
        }
    }
}

/// Where Claude Code session JSONLs live.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ClaudeConfig {
    #[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]
    pub projects_dir: PathBuf,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            projects_dir: vault::paths::expand_tilde(DEFAULT_PROJECTS_DIR),
        }
    }
}

/// Fabric invocation knobs.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct FabricConfig {
    pub binary: String,
    pub max_input_chars: usize,
    pub classify_model: String,
    pub distill_model: String,
    pub dream_model: String,
    pub classify_timeout_secs: u64,
    pub distill_timeout_secs: u64,
    pub dream_timeout_secs: u64,
}

impl Default for FabricConfig {
    fn default() -> Self {
        Self {
            binary: DEFAULT_FABRIC_BINARY.to_string(),
            max_input_chars: DEFAULT_FABRIC_MAX_INPUT_CHARS,
            classify_model: DEFAULT_FABRIC_CLASSIFY_MODEL.to_string(),
            distill_model: DEFAULT_FABRIC_DISTILL_MODEL.to_string(),
            dream_model: DEFAULT_FABRIC_DREAM_MODEL.to_string(),
            classify_timeout_secs: DEFAULT_FABRIC_CLASSIFY_TIMEOUT_SECS,
            distill_timeout_secs: DEFAULT_FABRIC_DISTILL_TIMEOUT_SECS,
            dream_timeout_secs: DEFAULT_FABRIC_DREAM_TIMEOUT_SECS,
        }
    }
}

/// Cluster-stage knobs.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ClusterConfig {
    pub algorithm: String,
    pub similarity_threshold: f32,
    pub min_cluster_size: usize,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            algorithm: DEFAULT_CLUSTER_ALGORITHM.to_string(),
            similarity_threshold: DEFAULT_CLUSTER_SIMILARITY_THRESHOLD,
            min_cluster_size: DEFAULT_CLUSTER_MIN_SIZE,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DaemonConfig {
    pub harvest_interval_secs: u64,
    pub dream_interval_secs: u64,
    pub debounce_secs: u64,
    pub harvest_parallelism: usize,
    pub distill_parallelism: usize,
    pub dream_parallelism: usize,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            harvest_interval_secs: DEFAULT_HARVEST_INTERVAL_SECS,
            dream_interval_secs: DEFAULT_DREAM_INTERVAL_SECS,
            debounce_secs: DEFAULT_DAEMON_DEBOUNCE_SECS,
            harvest_parallelism: DEFAULT_HARVEST_PARALLELISM,
            distill_parallelism: DEFAULT_DISTILL_PARALLELISM,
            dream_parallelism: DEFAULT_DREAM_PARALLELISM,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct BundleConfig {
    /// Per-turn tool-result truncation threshold. Blobs over this size
    /// become `<tool-result: N lines, $tool>` placeholders before being
    /// fed to the LLM. Mirrors the borg pipeline's pattern.
    pub interaction_turn_budget_chars: usize,
    /// Soft per-work-item bundle cap for the distill call. If a
    /// work-item's full bundle exceeds this, the cluster step splits
    /// it temporally (median timestamp) and re-distills as two
    /// chunks (see Risks in the design doc).
    pub bundle_budget_chars: usize,
}

impl Default for BundleConfig {
    fn default() -> Self {
        Self {
            interaction_turn_budget_chars: DEFAULT_INTERACTION_TURN_BUDGET_CHARS,
            bundle_budget_chars: DEFAULT_BUNDLE_BUDGET_CHARS,
        }
    }
}
