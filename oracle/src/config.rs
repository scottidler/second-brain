//! Configuration loading for oracle

use eyre::{Result, WrapErr};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    /// Vault configuration. `root-path` is optional; the unified resolver
    /// (see `vault::paths::resolve_vault_root`) accepts a CLI override, this
    /// config value, or a `.obsidian/`-marked CWD.
    #[serde(default)]
    pub vault: VaultConfig,

    /// Path to the SQLite database
    #[serde(default = "default_db_path", rename = "db-path")]
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

    /// Configurable retrieval pipeline. Composed for any `knowledge_search`
    /// query that arrives without an explicit `mode`. See
    /// `docs/design/2026-06-06-configurable-retrieval-pipeline.md`. An
    /// existing config without a `retrieval:` block loads to the eval-best
    /// built-in default (vector-only, stub-exclude, no rerank/transform).
    #[serde(default)]
    pub retrieval: RetrievalConfig,
}

/// The retrieval pipeline oracle composes when a query arrives with no
/// explicit `mode`. Stage order is fixed:
/// `query-transform -> retrieve -> fuse -> rerank -> exclude -> truncate`.
/// Each method/stage carries an `enabled` flag; one or more retrievers may
/// be enabled and they fuse via (weighted) RRF.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct RetrievalConfig {
    /// Per-retriever enable flags + tuning.
    pub methods: MethodsConfig,
    /// Fusion params, consulted only when more than one retriever is enabled.
    pub fusion: FusionConfig,
    /// Optional cross-encoder rerank stage (off by default).
    pub rerank: RerankConfig,
    /// Optional pre-retrieval query transform stage (off by default).
    pub query_transform: QueryTransformConfig,
    /// Structural result-shape filters applied post-fusion.
    pub exclude: ExcludeConfig,
}

/// The three retrievers and their tuning. Each carries its own `enabled`
/// flag so the operator turns methodologies on and off directly.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct MethodsConfig {
    pub vector: VectorMethod,
    pub bm25: Bm25Method,
    pub graph: GraphMethod,
}

/// Semantic (brute-force cosine) retriever. The eval-best method for this
/// host; on by default.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct VectorMethod {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Candidate depth fed into fusion.
    #[serde(default = "default_top_k")]
    pub top_k: u32,
}

impl Default for VectorMethod {
    fn default() -> Self {
        Self {
            enabled: true,
            top_k: default_top_k(),
        }
    }
}

/// Exact-keyword (FTS5) retriever. Off by default; weighted low when enabled
/// because equal-weight fusion dilutes the stronger vector signal.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Bm25Method {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_top_k")]
    pub top_k: u32,
    /// Fusion weight when enabled (demoted relative to vector's 1.0).
    #[serde(default = "default_bm25_weight")]
    pub weight: f32,
}

impl Default for Bm25Method {
    fn default() -> Self {
        Self {
            enabled: false,
            top_k: default_top_k(),
            weight: default_bm25_weight(),
        }
    }
}

/// Wikilink-expansion retriever. Off by default and weighted 0.0 (no ranking
/// lift on this vault); kept available and eval-testable rather than deleted.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GraphMethod {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_top_k")]
    pub top_k: u32,
    /// Fusion weight when enabled. Defaults to 0.0 (demoted out of the result).
    #[serde(default)]
    pub weight: f32,
    /// Hops to expand from each seed. Falls back to the built-in hop cap.
    #[serde(default = "default_graph_hops")]
    pub hops: u8,
    /// Per-hop score decay.
    #[serde(default = "default_graph_hop_decay")]
    pub hop_decay: f32,
    /// Drop edges below this weight during expansion.
    #[serde(default)]
    pub min_edge_weight: f32,
    /// Edge kinds to traverse.
    #[serde(default = "default_edge_kinds")]
    pub edge_kinds: Vec<String>,
}

impl Default for GraphMethod {
    fn default() -> Self {
        Self {
            enabled: false,
            top_k: default_top_k(),
            weight: 0.0,
            hops: default_graph_hops(),
            hop_decay: default_graph_hop_decay(),
            min_edge_weight: 0.0,
            edge_kinds: default_edge_kinds(),
        }
    }
}

/// Fusion strategy. RRF is the only variant today; the enum keeps the door
/// open without a schema break.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FusionMethod {
    #[default]
    Rrf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FusionConfig {
    #[serde(default)]
    pub method: FusionMethod,
    /// RRF smoothing constant. Falls back to the vault `RRF_K` default.
    #[serde(default = "default_rrf_k")]
    pub k: usize,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            method: FusionMethod::Rrf,
            k: default_rrf_k(),
        }
    }
}

/// Rerank strategy. Cross-encoder is the only variant today.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RerankMethod {
    #[default]
    CrossEncoder,
}

/// Optional cross-encoder rerank stage. OFF by default on this host:
/// cross-encoder inference is the most AVX-sensitive stage.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RerankConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub method: RerankMethod,
    /// Model identifier (small CPU-sane default).
    #[serde(default = "default_rerank_model")]
    pub model: String,
    /// Number of top fused candidates to rerank.
    #[serde(default = "default_rerank_input_k")]
    pub input_k: u32,
    /// Warmup-probe budget; if the projected batch exceeds this, the stage
    /// no-ops for the process (fail-open to fused order).
    #[serde(default = "default_rerank_latency_budget_ms")]
    pub latency_budget_ms: u64,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            method: RerankMethod::CrossEncoder,
            model: default_rerank_model(),
            input_k: default_rerank_input_k(),
            latency_budget_ms: default_rerank_latency_budget_ms(),
        }
    }
}

/// Query transform strategy.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TransformMethod {
    #[default]
    Hyde,
    MultiQuery,
}

/// Optional pre-retrieval query rewrite. OFF by default: adds an LLM
/// round-trip per query and can poison precision.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct QueryTransformConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub method: TransformMethod,
    /// Fabric pattern name (resolved under `~/.config/sb/patterns/`).
    #[serde(default = "default_transform_pattern")]
    pub pattern: String,
    /// Fabric model name; empty = fabric's default model.
    #[serde(default)]
    pub model: String,
    /// multi-query only: number of rewrites to generate.
    #[serde(default = "default_transform_variants")]
    pub variants: u8,
}

impl Default for QueryTransformConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            method: TransformMethod::Hyde,
            pattern: default_transform_pattern(),
            model: String::new(),
            variants: default_transform_variants(),
        }
    }
}

/// Structural result-shape filters, applied post-fusion.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExcludeConfig {
    /// Drop low-content notes (`quality = low` in the `notes` table).
    #[serde(default = "default_true")]
    pub stub: bool,
    /// Drop notes whose retrieved body is shorter than this. 0 = off.
    #[serde(default)]
    pub min_body_chars: usize,
}

impl Default for ExcludeConfig {
    fn default() -> Self {
        Self {
            stub: true,
            min_body_chars: 0,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_top_k() -> u32 {
    50
}

fn default_bm25_weight() -> f32 {
    0.3
}

fn default_graph_hops() -> u8 {
    2
}

fn default_graph_hop_decay() -> f32 {
    0.5
}

fn default_edge_kinds() -> Vec<String> {
    vec!["wikilink".to_string()]
}

fn default_rrf_k() -> usize {
    vault::search::RRF_K
}

fn default_rerank_model() -> String {
    "ms-marco-MiniLM-L6-v2".to_string()
}

fn default_rerank_input_k() -> u32 {
    50
}

fn default_rerank_latency_budget_ms() -> u64 {
    1500
}

fn default_transform_pattern() -> String {
    "hyde".to_string()
}

fn default_transform_variants() -> u8 {
    3
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct VaultConfig {
    /// Vault root. `None` means the runtime requires either a CLI override
    /// or a `.obsidian/`-marked CWD.
    pub root_path: Option<String>,
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

    /// Resolve the vault root via the unified resolver. Oracle has no CLI
    /// override (it's an MCP server); precedence is config > marker-gated CWD.
    pub fn vault_root(&self) -> Result<PathBuf> {
        vault::paths::resolve_vault_root(None, self.vault.root_path.as_deref())
    }

    pub fn db_path(&self) -> PathBuf {
        let expanded = shellexpand::tilde(&self.db_path);
        PathBuf::from(expanded.as_ref())
    }
}

#[cfg(test)]
mod tests;
