//! Configuration loading for oracle

use eyre::{Result, WrapErr};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use vault::search::KindOffsets;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Vault configuration. `root-path` is optional; the unified resolver
    /// (see `vault::paths::resolve_vault_root`) accepts a CLI override, this
    /// config value, or a `.obsidian/`-marked CWD.
    #[serde(default)]
    pub vault: VaultConfig,

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
    /// built-in default (vector-only, stub-exclude, rerank on, no transform).
    #[serde(default)]
    pub retrieval: RetrievalConfig,

    /// Inference backend and threads for query embedding: `backend` is
    /// `auto | candle | onnx` (case-insensitive; resolved against the
    /// detected CPU and the pinned model on first use), `threads` 0 is the
    /// backend default (Candle 8 replicas capped at the CPU count, ONNX 16
    /// intra-op threads capped the same way).
    #[serde(default)]
    pub inference: vault::embedding::InferenceConfig,

    /// Credentials for oracle's own fabric calls (the eval relevance judge).
    #[serde(default)]
    pub llm: LlmConfig,
}

/// oracle's `llm:` block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LlmConfig {
    /// `vendor -> env var (or file) holding its key` (`anthropic`, `gemini`,
    /// `openai`), exported under fabric's names on the judge's fabric child.
    /// Empty leaves fabric on its own `.env`.
    #[serde(default)]
    pub api_keys: vault::fabric::ApiKeys,
}

impl Default for Config {
    /// Manual impl so `inbound_recompute_interval_secs` defaults to the serde
    /// default fn, NOT u64's `0`. A derived `Default` yielded `0`, and
    /// `tokio::time::interval(Duration::from_secs(0))` panics - in a
    /// never-awaited JoinHandle - on any host with no `oracle.yml` (so
    /// `Config::default()` is what loads). The manual impl keeps the derived
    /// path and the empty-YAML deserialize path in agreement.
    fn default() -> Self {
        Self {
            vault: VaultConfig::default(),
            logging: LogConfig::default(),
            watcher: WatcherConfig::default(),
            inbound_recompute_interval_secs: default_inbound_recompute_interval_secs(),
            retrieval: RetrievalConfig::default(),
            inference: vault::embedding::InferenceConfig::default(),
            llm: LlmConfig::default(),
        }
    }
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
    /// Cross-encoder rerank stage (on by default; the warmup probe fails open).
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
    /// Distance added to a row of each kind before the per-note min-reduce.
    /// All zero (the default) is the plain min-reduce.
    #[serde(default)]
    pub kind_offsets: KindOffsets,
}

impl Default for VectorMethod {
    fn default() -> Self {
        Self {
            enabled: true,
            top_k: default_top_k(),
            kind_offsets: KindOffsets::default(),
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
    /// How expanded notes are scored: `bfs` (the hop-limited expansion) or
    /// `ppr` (personalized PageRank). `hops` and `hop-decay` apply to `bfs`
    /// only; the `ppr-*` keys apply to `ppr` only.
    #[serde(default)]
    pub algorithm: GraphAlgorithm,
    /// PPR teleport probability back to the seeds, in (0, 1).
    #[serde(default = "default_ppr_alpha")]
    pub ppr_alpha: f64,
    /// PPR residual threshold per unit of weighted degree.
    #[serde(default = "default_ppr_epsilon")]
    pub ppr_epsilon: f64,
    /// PPR work cap per query; reaching it returns the current estimate.
    #[serde(default = "default_ppr_max_pushes")]
    pub ppr_max_pushes: usize,
}

impl GraphMethod {
    /// The vault-side PPR parameters this method configures.
    pub fn ppr_params(&self) -> vault::search::PprParams {
        vault::search::PprParams {
            alpha: self.ppr_alpha,
            epsilon: self.ppr_epsilon,
            max_pushes: self.ppr_max_pushes,
        }
    }
}

/// The graph retriever's scorer.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GraphAlgorithm {
    /// Breadth-first expansion up to `hops`, each neighbor scored by
    /// `sum(seed weight * path weight * hop-decay^(hop-1))`.
    #[default]
    Bfs,
    /// Forward-push personalized PageRank from the reciprocal-rank-weighted
    /// seeds.
    Ppr,
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
            algorithm: GraphAlgorithm::Bfs,
            ppr_alpha: default_ppr_alpha(),
            ppr_epsilon: default_ppr_epsilon(),
            ppr_max_pushes: default_ppr_max_pushes(),
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

/// Cross-encoder rerank stage. ON by default: the paired eval cleared the
/// ship rule. It is the most CPU-sensitive stage (~150-300 ms/pair on Candle
/// without AVX2+FMA, as on `talos`; ONNX is faster where the CPU allows it),
/// so the warmup probe no-ops the stage on a host that cannot meet the budget.
/// Absent, empty and partial blocks all resolve enabled; only an explicit
/// `enabled: false` turns it off.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct RerankConfig {
    #[serde(default = "default_rerank_enabled")]
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
            enabled: default_rerank_enabled(),
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

fn default_ppr_alpha() -> f64 {
    0.15
}

fn default_ppr_epsilon() -> f64 {
    1e-4
}

fn default_ppr_max_pushes() -> usize {
    20_000
}

fn default_rrf_k() -> usize {
    vault::search::RRF_K
}

fn default_rerank_enabled() -> bool {
    true
}

fn default_rerank_model() -> String {
    vault::search::RERANK_MODEL_ID.to_string()
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
    /// Whether to enable the file watcher for live reindexing. Named `enabled`
    /// for consistency with the retrieval method flags; `enable` stays a serde
    /// alias so existing oracle.yml files keep parsing.
    #[serde(default = "default_watcher_enable", alias = "enable")]
    pub enabled: bool,

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
            enabled: default_watcher_enable(),
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
    vec![".git".into(), ".obsidian".into(), "templates".into(), ".claude".into()]
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_inbound_recompute_interval_secs() -> u64 {
    600
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = if let Some(p) = path { p.to_path_buf() } else { Self::find_config_file() };

        if config_path.exists() {
            tracing::info!("oracle: loading config from {}", config_path.display());
            let contents = std::fs::read_to_string(&config_path).wrap_err("Failed to read config file")?;
            let config: Config = serde_yaml::from_str(&contents).wrap_err("Failed to parse config file")?;
            Ok(config)
        } else {
            tracing::info!(
                "oracle: no config at {}; using built-in defaults",
                config_path.display()
            );
            Ok(Self::default())
        }
    }

    /// The single config location: `~/.config/sb/oracle.yml`. The previous
    /// `./oracle.yml` CWD fallback was dropped - it let any project directory
    /// the MCP server happened to launch from silently reconfigure retrieval
    /// (the same "no silent CWD" rule the vault-root resolver follows).
    fn find_config_file() -> PathBuf {
        vault::paths::oracle_config()
    }

    /// Resolve the vault root via the unified resolver. Oracle has no CLI
    /// override (it's an MCP server); precedence is config > marker-gated CWD.
    pub fn vault_root(&self) -> Result<PathBuf> {
        vault::paths::resolve_vault_root(None, self.vault.root_path.as_deref())
    }

    /// Resolve oracle's SQLite DB path via the shared source of truth in
    /// `vault::paths`. The path is fixed and owned by `vault::paths`; it is
    /// not configurable, so cortex (the embeddings writer) and oracle (the
    /// reader) can never desync on the file they open.
    pub fn db_path(&self) -> PathBuf {
        vault::paths::oracle_db_path()
    }
}

#[cfg(test)]
mod tests;
