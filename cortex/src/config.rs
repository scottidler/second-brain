use eyre::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub vault: VaultConfig,
    #[serde(rename = "log-level")]
    pub log_level: String,
    pub schema: SchemaConfig,
    pub actions: ActionsConfig,
    pub state: StateConfig,
    pub daemon: DaemonConfig,
    pub migrations: Vec<MigrationConfig>,
    pub llm: LlmConfig,
    pub sweep: SweepConfig,
    pub backfill: BackfillConfig,
    pub fabric: FabricConfig,
    pub embed: EmbedConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            vault: VaultConfig::default(),
            log_level: "info".to_string(),
            schema: SchemaConfig::default(),
            actions: ActionsConfig::default(),
            state: StateConfig::default(),
            daemon: DaemonConfig::default(),
            migrations: Vec::new(),
            llm: LlmConfig::default(),
            sweep: SweepConfig::default(),
            backfill: BackfillConfig::default(),
            fabric: FabricConfig::default(),
            embed: EmbedConfig::default(),
        }
    }
}

/// Knobs for `cortex embed` and the daemon's embed tick.
///
/// `workers` controls the internal replica pool inside the Candle
/// backend. The default of 0 means "let the backend pick its own
/// platform-aware default" (currently `min(8, available_parallelism)`).
/// A non-zero value pins an explicit count; the backend clamps it to
/// its own `[1, MAX_WORKERS]` range.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct EmbedConfig {
    pub workers: usize,
}

/// Phase-7 backfill knobs. `max-concurrent` defaults to 2 so a one-pass
/// `--since 30d` over the inbox doesn't hammer Fabric harder than borg's
/// own pipeline.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct BackfillConfig {
    #[serde(rename = "max-concurrent")]
    pub max_concurrent: u32,
    /// Filename of the resume checkpoint inside `state.cache-dir`.
    #[serde(rename = "checkpoint-file")]
    pub checkpoint_file: String,
}

impl Default for BackfillConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 2,
            checkpoint_file: "backfill-state.json".to_string(),
        }
    }
}

/// Subset of borg's FabricConfig that the distillers crate needs. Mirrored
/// here (rather than imported) so cortex stays decoupled from borg.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct FabricConfig {
    pub binary: String,
    pub model: String,
    #[serde(rename = "max-content-chars")]
    pub max_content_chars: usize,
    #[serde(rename = "timeout-secs")]
    pub timeout_secs: u64,
}

impl Default for FabricConfig {
    fn default() -> Self {
        Self {
            binary: "fabric".to_string(),
            model: String::new(),
            max_content_chars: 32_000,
            timeout_secs: 120,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SchemaConfig {
    pub domains: Vec<String>,
    pub types: Vec<String>,
    pub origins: Vec<String>,
    pub statuses: Vec<String>,
    pub methods: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    #[serde(rename = "root-path")]
    pub root_path: Option<String>,
    pub ignore: Vec<String>,
    pub exclude: Vec<String>,
    pub include: Vec<String>,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            root_path: None,
            ignore: vec![
                ".git".to_string(),
                ".obsidian".to_string(),
                ".cortex".to_string(),
                "assets".to_string(),
                "attachments".to_string(),
            ],
            exclude: Vec::new(),
            include: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ActionsConfig {
    pub classify: crate::classify::ClassifyConfig,
    pub naming: NamingConfig,
    pub frontmatter: FrontmatterConfig,
    pub tags: TagsConfig,
    pub scope: ScopeConfig,
    pub linking: LinkingConfig,
    pub intel: IntelConfig,
    pub duplicates: DuplicatesConfig,
    #[serde(rename = "broken-links")]
    pub broken_links: BrokenLinksConfig,
    pub quality: QualityConfig,
    #[serde(rename = "auto-tag")]
    pub auto_tag: AutoTagConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct NamingConfig {
    pub style: String,
    #[serde(rename = "max-length")]
    pub max_length: u32,
    #[serde(rename = "exempt-patterns")]
    pub exempt_patterns: Vec<String>,
}

impl Default for NamingConfig {
    fn default() -> Self {
        Self {
            style: "lowercase-hyphenated".to_string(),
            max_length: 80,
            exempt_patterns: vec![r"^[\p{Emoji}].*/$".to_string()],
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct FrontmatterConfig {
    pub required: Vec<String>,
    pub exempt: HashMap<String, Vec<String>>,
    #[serde(rename = "path-exempt")]
    pub path_exempt: HashMap<String, Vec<String>>,
    #[serde(rename = "type-fields")]
    pub type_fields: HashMap<String, Vec<String>>,
    #[serde(rename = "auto-title")]
    pub auto_title: bool,
}

impl Default for FrontmatterConfig {
    fn default() -> Self {
        Self {
            required: vec![
                "title".to_string(),
                "date".to_string(),
                "type".to_string(),
                "tags".to_string(),
            ],
            exempt: HashMap::new(),
            path_exempt: HashMap::new(),
            type_fields: HashMap::new(),
            auto_title: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct TagsConfig {
    pub style: String,
    pub canonical: Vec<String>,
    pub aliases: HashMap<String, String>,
}

impl Default for TagsConfig {
    fn default() -> Self {
        Self {
            style: "lowercase-hyphenated".to_string(),
            canonical: Vec::new(),
            aliases: HashMap::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct SweepConfig {
    pub canonical_path: String,
    pub mapping_path: String,
    pub proposals_path: String,
    pub sweep_interval: String,
    pub proposal_threshold: usize,
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            canonical_path: "~/.config/second-brain/canonical-tags.yml".to_string(),
            mapping_path: "~/.config/second-brain/tag-mapping.yml".to_string(),
            proposals_path: "~/.config/second-brain/tag-proposals.yml".to_string(),
            sweep_interval: "1h".to_string(),
            proposal_threshold: 3,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ScopeConfig {
    pub rules: Vec<ScopeRule>,
}

#[derive(Debug, Deserialize)]
pub struct ScopeRule {
    #[serde(rename = "match")]
    pub match_criteria: ScopeMatch,
    pub set: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
pub struct ScopeMatch {
    pub tags: Option<Vec<String>>,
    #[serde(rename = "source-contains")]
    pub source_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LinkingConfig {
    #[serde(rename = "scan-for")]
    pub scan_for: Vec<String>,
    pub entities: LinkingEntities,
    pub targets: LinkingTargets,
    #[serde(rename = "min-word-length")]
    pub min_word_length: usize,
}

impl Default for LinkingConfig {
    fn default() -> Self {
        Self {
            scan_for: vec!["people".to_string(), "projects".to_string(), "concepts".to_string()],
            entities: LinkingEntities::default(),
            targets: LinkingTargets::default(),
            min_word_length: 5,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct LinkingEntities {
    pub people: Vec<String>,
    pub projects: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct LinkingTargets {
    pub types: LinkingFilter,
    pub paths: LinkingFilter,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct LinkingFilter {
    pub exclude: Vec<String>,
    pub include: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct IntelConfig {
    #[serde(rename = "daily-note")]
    pub daily_note: bool,
    #[serde(rename = "weekly-review")]
    pub weekly_review: bool,
    #[serde(rename = "fabric-patterns")]
    pub fabric_patterns: Vec<String>,
    #[serde(rename = "output-path")]
    pub output_path: String,
    #[serde(rename = "on-new-note")]
    pub on_new_note: Option<String>,
    #[serde(rename = "batch-weekly")]
    pub batch_weekly: Option<String>,
    #[serde(rename = "max-input-tokens")]
    pub max_input_tokens: usize,
    #[serde(rename = "fabric-timeout-secs")]
    pub fabric_timeout_secs: u64,
    /// Model override for daily digest LLM call. Falls back to top-level llm.model if None.
    pub model: Option<String>,
    #[serde(rename = "max-output-tokens")]
    pub max_output_tokens: u32,
    #[serde(rename = "llm-timeout-secs")]
    pub llm_timeout_secs: u64,
}

impl Default for IntelConfig {
    fn default() -> Self {
        Self {
            daily_note: true,
            weekly_review: true,
            fabric_patterns: vec!["extract_wisdom".to_string(), "summarize".to_string()],
            output_path: "notes/ai".to_string(),
            on_new_note: Some("extract_wisdom".to_string()),
            batch_weekly: Some("weekly_digest".to_string()),
            max_input_tokens: 50000,
            fabric_timeout_secs: 120,
            model: Some("claude-opus-4-20250514".to_string()),
            max_output_tokens: 1024,
            llm_timeout_secs: 120,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DuplicatesConfig {
    pub threshold: f64,
    #[serde(rename = "same-type-only")]
    pub same_type_only: bool,
    pub exclude: Vec<String>,
}

impl Default for DuplicatesConfig {
    fn default() -> Self {
        Self {
            threshold: 0.85,
            same_type_only: false,
            exclude: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct AutoTagConfig {
    pub enabled: bool,
    #[serde(rename = "min-tags-threshold")]
    pub min_tags_threshold: usize,
    #[serde(rename = "canonical-tags")]
    pub canonical_tags: Vec<String>,
    #[serde(rename = "fabric-pattern")]
    pub fabric_pattern: Option<String>,
    #[serde(rename = "auto-derive-top-n")]
    pub auto_derive_top_n: usize,
    #[serde(rename = "max-input-tokens")]
    pub max_input_tokens: usize,
    #[serde(rename = "fabric-timeout-secs")]
    pub fabric_timeout_secs: u64,
}

impl Default for AutoTagConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_tags_threshold: 3,
            canonical_tags: Vec::new(),
            fabric_pattern: None,
            auto_derive_top_n: 50,
            max_input_tokens: 50000,
            fabric_timeout_secs: 120,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct QualityConfig {
    #[serde(rename = "min-word-count")]
    pub min_word_count: usize,
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self { min_word_count: 50 }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct BrokenLinksConfig {
    #[serde(rename = "check-wikilinks")]
    pub check_wikilinks: bool,
    #[serde(rename = "check-urls")]
    pub check_urls: bool,
}

impl Default for BrokenLinksConfig {
    fn default() -> Self {
        Self {
            check_wikilinks: true,
            check_urls: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct StateConfig {
    #[serde(rename = "cache-dir")]
    pub cache_dir: String,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            cache_dir: ".cortex".to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub actions: HashMap<String, DaemonAction>,
    #[serde(rename = "debounce-secs")]
    pub debounce_secs: u64,
    pub watch: String,
    #[serde(rename = "poll-interval")]
    pub poll_interval: u64,
    #[serde(rename = "daily-at")]
    pub daily_at: Option<String>,
    #[serde(rename = "weekly-at")]
    pub weekly_at: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct DaemonAction {
    pub enable: bool,
}

impl DaemonConfig {
    /// Get the list of enabled action names.
    pub fn enabled_actions(&self) -> Vec<&str> {
        self.actions.keys().map(|s| s.as_str()).collect()
    }

    /// Check whether a given action is enabled.
    pub fn is_enabled(&self, action: &str) -> bool {
        self.actions.get(action).is_some_and(|a| a.enable)
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let mut actions = HashMap::new();
        actions.insert("lint".to_string(), DaemonAction { enable: false });
        actions.insert("broken-links".to_string(), DaemonAction { enable: false });
        Self {
            actions,
            debounce_secs: 5,
            watch: "notify".to_string(),
            poll_interval: 300,
            daily_at: None,
            weekly_at: None,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct MigrationConfig {
    pub name: String,
    #[serde(default)]
    pub moves: Vec<MigrationMove>,
    #[serde(rename = "field-renames", default)]
    pub field_renames: HashMap<String, String>,
    #[serde(rename = "field-drops", default)]
    pub field_drops: Vec<String>,
    #[serde(rename = "value-renames", default)]
    pub value_renames: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub struct MigrationMove {
    pub from: String,
    pub to: String,
    #[serde(rename = "set-frontmatter")]
    pub set_frontmatter: Option<HashMap<String, serde_yaml::Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    #[serde(rename = "api-key")]
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

impl Config {
    /// Load configuration with fallback chain:
    /// 1. Explicit --config flag
    /// 2. ~/.config/obsidian-cortex/obsidian-cortex.yml
    /// 3. Defaults
    pub fn load(config_path: Option<&PathBuf>) -> Result<Self> {
        if let Some(path) = config_path {
            return Self::load_from_file(path).context(format!("Failed to load config from {}", path.display()));
        }

        if let Some(config_dir) = dirs::config_dir() {
            let primary = config_dir.join("cortex").join("cortex.yml");
            if primary.exists() {
                match Self::load_from_file(&primary) {
                    Ok(config) => return Ok(config),
                    Err(e) => {
                        log::warn!(
                            "failed to load config, falling back to defaults: {}: {e}",
                            primary.display()
                        );
                    }
                }
            }
        }

        log::info!("no config file found, using defaults");
        Ok(Self::default())
    }

    fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref()).context("failed to read config file")?;
        let config: Self = serde_yaml::from_str(&content).context("failed to parse config file")?;
        log::info!("loaded config: {}", path.as_ref().display());
        Ok(config)
    }

    /// Resolve the vault root path from CLI flag, config, or CWD.
    pub fn vault_root(&self, cli_vault: Option<&PathBuf>) -> PathBuf {
        if let Some(vault) = cli_vault {
            return vault.clone();
        }
        if let Some(ref root_path) = self.vault.root_path {
            let expanded = shellexpand::tilde(root_path);
            return PathBuf::from(expanded.as_ref());
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    /// Path to oracle's search database. Cortex's embed loop reads from
    /// and writes to the same DB file oracle's indexer maintains, so
    /// both processes see the same notes + embeddings on every query.
    pub fn oracle_db_path(&self) -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("~/.local/share"))
            .join("oracle")
            .join("oracle.db")
    }
}
