//! Runtime config for the facet subsystem.
//!
//! Loaded from `~/.config/sb/facet.yml` (or an explicit path); falls back
//! to the type-level defaults. All path fields run through
//! [`vault::paths::deserialize_tilde_pathbuf`] at deserialise time so a
//! literal `~/...` in YAML is expanded once, not at every consumer.

use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Top-level facet configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// Root containing per-cwd JSONL transcript directories.
    /// Default: `~/.claude/projects`.
    #[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]
    pub claude_projects_root: PathBuf,

    /// Daemon harvest interval. Default 24h.
    pub harvest_interval_secs: u64,

    /// Daemon narrate-pass interval (Session Arc + Cross-Session Arc
    /// discovery, opus synthesis, rejection-gate). 0 disables.
    /// Default weekly.
    pub narrate_interval_secs: u64,

    /// Daemon dream-pass interval (semantic-duplicate / cross-ref /
    /// stale-spectrum / narrative-candidate proposals). 0 disables.
    /// Default 24h (cheap, no LLM calls).
    pub dream_interval_secs: u64,

    /// Paths (tilde-expanded, prefix-match) that ARE eligible for harvest.
    /// Empty means "all roots not in exclude_cwds".
    #[serde(default, deserialize_with = "vault::paths::deserialize_tilde_pathbuf_vec")]
    pub include_cwds: Vec<PathBuf>,

    /// Paths (tilde-expanded, prefix-match) NEVER harvested. Wins on overlap
    /// with include_cwds.
    #[serde(default, deserialize_with = "vault::paths::deserialize_tilde_pathbuf_vec")]
    pub exclude_cwds: Vec<PathBuf>,

    /// LLM tiering, models, and per-tick / per-day budget caps.
    pub llm: LlmConfig,

    /// Concurrency caps - scan rayon threads, in-flight LLM cap, per-tick
    /// session cap. See 2026-05-12 borg pipeline concurrency-cap design
    /// for the underlying incident.
    pub concurrency: ConcurrencyConfig,

    /// Inactivity threshold: a work-item is marked `dormant` after this
    /// many days with no contribution.
    pub dormancy: DormancyConfig,

    /// Vault output layout.
    pub vault: VaultLayout,

    /// Optional notification sinks.
    pub notify: NotifyConfig,

    /// Extraction-stage tuning - quote excerpt cap, per-call max input
    /// token estimate (used to split oversized cluster_assignments rows).
    pub extract: ExtractConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            claude_projects_root: shellexpand_path("~/.claude/projects"),
            harvest_interval_secs: 86_400,
            narrate_interval_secs: 604_800,
            dream_interval_secs: 86_400,
            include_cwds: vec![],
            exclude_cwds: vec![
                shellexpand_path("~/repos/tatari-tv"),
                shellexpand_path("~/repos/scottidler/obsidian"),
            ],
            llm: LlmConfig::default(),
            concurrency: ConcurrencyConfig::default(),
            dormancy: DormancyConfig::default(),
            vault: VaultLayout::default(),
            notify: NotifyConfig::default(),
            extract: ExtractConfig::default(),
        }
    }
}

fn shellexpand_path(s: &str) -> PathBuf {
    vault::paths::expand_tilde(s)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct LlmConfig {
    pub cluster_model: String,
    pub extract_model: String,
    pub spectra_model: String,
    pub per_tick_budget_usd: f64,
    pub per_day_budget_usd: f64,
    pub fabric_binary: String,
    /// Per-call timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            cluster_model: "claude-haiku-4-5".to_string(),
            extract_model: "claude-sonnet-4-6".to_string(),
            spectra_model: "claude-opus-4-7".to_string(),
            per_tick_budget_usd: 5.0,
            per_day_budget_usd: 20.0,
            fabric_binary: "fabric".to_string(),
            timeout_secs: 180,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ConcurrencyConfig {
    pub max_sessions_per_tick: usize,
    pub max_llm_inflight: usize,
    pub parse_rayon_threads: usize,
}

impl Default for ConcurrencyConfig {
    fn default() -> Self {
        Self {
            max_sessions_per_tick: 16,
            max_llm_inflight: 4,
            parse_rayon_threads: 4,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DormancyConfig {
    pub inactive_days: u32,
}

impl Default for DormancyConfig {
    fn default() -> Self {
        Self { inactive_days: 14 }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct VaultLayout {
    pub prisms_dir: String,
    pub spectra_dir: String,
    pub dreams_dir: String,
    pub quarantine_dir: String,
    pub archive_dir: String,
}

impl Default for VaultLayout {
    fn default() -> Self {
        Self {
            prisms_dir: "notes/facet/prisms".to_string(),
            spectra_dir: "notes/facet/spectra".to_string(),
            dreams_dir: "notes/facet/dreams".to_string(),
            quarantine_dir: "notes/facet/quarantine".to_string(),
            archive_dir: "notes/facet/archive".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct NotifyConfig {
    pub on_new_workitem: bool,
    pub on_budget_exhausted: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ExtractConfig {
    /// Verbatim quote cap, char count.
    pub quote_max_chars: usize,
    /// Per-call upper bound for cluster_assignments input length. A row
    /// exceeding this is split at a turn boundary across multiple
    /// extract calls.
    pub max_input_tokens: usize,
}

impl Default for ExtractConfig {
    fn default() -> Self {
        Self {
            quote_max_chars: 800,
            max_input_tokens: 60_000,
        }
    }
}

impl Config {
    /// Load from explicit path; fall back to `~/.config/sb/facet.yml`; fall
    /// back to `Config::default()`. Matches the borg/oracle load_config
    /// shape.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        log::debug!("Config::load: path={:?}", path);
        if let Some(p) = path {
            return Self::load_from(p).with_context(|| format!("loading facet config from {}", p.display()));
        }
        let primary = vault::paths::facet_config();
        if primary.exists() {
            return Self::load_from(&primary)
                .with_context(|| format!("loading facet config from {}", primary.display()));
        }
        log::info!("facet: no config file found; using defaults");
        Ok(Self::default())
    }

    fn load_from(path: &Path) -> Result<Self> {
        let body = fs::read_to_string(path).context("read facet config")?;
        let cfg: Self = serde_yaml::from_str(&body).context("parse facet config")?;
        log::info!("facet: loaded config from {}", path.display());
        Ok(cfg)
    }

    /// Returns true if `cwd` should be harvested under the current
    /// include/exclude lists. Excludes win on overlap; an empty include
    /// list means "no positive restriction".
    pub fn is_cwd_eligible(&self, cwd: &Path) -> bool {
        let cwd_s = cwd.to_string_lossy();
        for ex in &self.exclude_cwds {
            let ex_s = ex.to_string_lossy();
            if cwd_s.starts_with(ex_s.as_ref()) {
                return false;
            }
        }
        if self.include_cwds.is_empty() {
            return true;
        }
        for inc in &self.include_cwds {
            let inc_s = inc.to_string_lossy();
            if cwd_s.starts_with(inc_s.as_ref()) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests;
