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
    pub graph: GraphConfig,
    pub entities: EntitiesConfig,
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
            graph: GraphConfig::default(),
            entities: EntitiesConfig::default(),
        }
    }
}

/// Knobs for `cortex entities --discover` (Phase 4 of graph-augmented-memory):
/// an off-hot-path LLM pass that proposes new glossary entries into
/// `entity-proposals.yml`. Bounded by `max_per_run` (notes per pass) so a
/// backlog never fans unbounded LLM calls; daemon cadence defaults to daily.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct EntitiesConfig {
    /// Fabric pattern that extracts entity names from a note body.
    pub fabric_pattern: String,
    /// Max ingested notes processed per discovery run (bounds LLM cost).
    pub max_per_run: usize,
    /// Truncate each note body to this many tokens before extraction.
    pub max_input_tokens: usize,
    /// Per-call fabric timeout (seconds).
    pub fabric_timeout_secs: u64,
    /// Daemon cadence (seconds) for the discovery tick. Default 86_400 (daily):
    /// vocabulary grows slowly and the pass is LLM-bound.
    pub discover_interval_secs: u64,
}

impl Default for EntitiesConfig {
    fn default() -> Self {
        Self {
            fabric_pattern: "extract-entities".to_string(),
            max_per_run: 50,
            max_input_tokens: 4_000,
            fabric_timeout_secs: 120,
            discover_interval_secs: 86_400,
        }
    }
}

/// Knobs for the `cortex graph` deterministic-edge pass and its daemon tick.
///
/// All edge-shape constants (kNN `k`, cosine threshold, per-kind weights,
/// fan-out cap) are tunable here rather than hardcoded so the design's
/// "calibrate against the labeled query set" open questions are a config edit.
/// Defaults are the design doc's suggested starting values.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct GraphConfig {
    /// Daemon cadence (seconds) for the graph tick. Runs after the embed tick
    /// so semantic edges see fresh vectors. Default 900 (15 min).
    pub graph_interval_secs: u64,
    /// Daemon cadence (seconds) for the typed-`fact` backfill tick. The
    /// deterministic `graph_interval` tick is deterministic-only by design; this
    /// is the in-process schedule on which the LLM fact layer (triple extraction
    /// and consolidation) refreshes. Runs in-process so it cannot collide with
    /// the embed tick on the shared embed lock the way a separate-process timer
    /// would. Default 604800 (weekly), matching the cold-note sweep cadence.
    pub fact_interval_secs: u64,
    /// Top-k semantic neighbors retained per note.
    pub semantic_k: usize,
    /// Minimum cosine similarity for a semantic edge.
    pub min_cosine: f32,
    /// Tags/creators/sources/domains held by more than this many notes are
    /// skipped for pairwise edge emission (routed through hub notes in
    /// Phase 3 instead) so blanket buckets do not explode the table.
    pub fanout_cap: usize,
    /// Fixed weight for shared-creator edges.
    pub creator_weight: f32,
    /// Fixed weight for shared-source-host edges.
    pub source_weight: f32,
    /// Fixed weight for shared-domain edges.
    pub domain_weight: f32,
    // --- Phase 5 (MemGraphRAG) ---
    /// Fabric pattern that extracts subject-predicate-object triples.
    pub fact_pattern: String,
    /// Weight assigned to typed `fact` edges.
    pub fact_weight: f32,
    /// Max ingested notes processed for triple extraction per `--backfill`.
    pub fact_max_per_run: usize,
    /// Truncate each note body to this many tokens before triple extraction.
    pub fact_max_input_tokens: usize,
    /// Per-call fabric timeout (seconds) for triple extraction.
    pub fact_timeout_secs: u64,
    /// Predicates that are single-valued/functional: a subject may have only
    /// one object. Conflicting objects across notes are flagged (never silently
    /// overwritten). Everything else is multi-valued and accumulates.
    pub functional_predicates: Vec<String>,
    /// Predicates dropped by the noise-removal consolidation agent (too generic
    /// to carry retrieval value).
    pub noise_predicates: Vec<String>,
    /// Minimum cosine for a cluster-bridge edge from an isolated note to its
    /// nearest semantic neighbor.
    pub bridge_min_cosine: f32,
    /// Wikilink targets the graph pass refuses to turn into an edge, matched
    /// case-insensitively on the RAW `[[target]]` before resolution. The
    /// auto-linker's blunt `min-word-length` gate case-insensitively rewrites
    /// common English words into links to short-titled hubs (`every` alone
    /// minted 569 false `wikilink` edges into `entities/every.md`), and landed
    /// bodies are never retracted, so the graph layer has to refuse them at
    /// build time or a backfill reinstates them forever.
    ///
    /// This lives under `graph:` deliberately: the graph builder must not read
    /// the auto-linker's `actions.linking.*` namespace. Defaults EMPTY — code
    /// never silently suppresses a link; the shipped `cortex.yml.example` seeds
    /// the two measured offenders.
    pub wikilink_stopwords: Vec<String>,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            graph_interval_secs: 900,
            fact_interval_secs: 604_800,
            semantic_k: 10,
            min_cosine: 0.6,
            fanout_cap: 100,
            creator_weight: 0.2,
            source_weight: 0.15,
            domain_weight: 0.1,
            fact_pattern: "extract-triples".to_string(),
            fact_weight: 0.5,
            fact_max_per_run: 50,
            fact_max_input_tokens: 4_000,
            fact_timeout_secs: 120,
            functional_predicates: [
                "born-in",
                "released-on",
                "released-by",
                "created-by",
                "founded-by",
                "part-of",
                "authored-by",
                "licensed-under",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            noise_predicates: ["is", "has", "relates-to", "related-to", "about"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            bridge_min_cosine: 0.5,
            wikilink_stopwords: Vec::new(),
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
///
/// `max_chunks_per_call` bounds the input size of any single
/// `embed_batch` invocation, regardless of how many notes the read
/// phase pulled. Without this cap, a backlog of transcript-eligible
/// notes can flatten to thousands of chunks and trigger an 8-replica
/// rayon fan-out that allocates tens of GB. See
/// docs/design/2026-05-19-cortex-embed-memory-bounding.md.
#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct EmbedConfig {
    pub workers: usize,
    pub max_chunks_per_call: usize,
    /// Daemon cadence (seconds) between embed ticks. CLAUDE.md documents the
    /// embed cadence as configurable (default 10 min); the previous
    /// `daemon_cadence` ignored its config argument and hardcoded the value.
    pub cadence_secs: u64,
    /// Which embedding kinds the default (no-`--kind`) `cortex embed`/`--backfill`
    /// pass and the daemon embed tick generate. See [`EmbedKindsConfig`].
    pub kinds: EmbedKindsConfig,
    /// Root of borg's per-trace staging directories. cortex reads the staged
    /// `distilled.yml` (read-only) at `<staging-root>/<trace>/distilled.yml` as
    /// the transcript-embedding source for Video/Article notes
    /// (2026-07-07-distillation-output-restore Phase 5): those notes no longer
    /// carry a `## Transcript` body section, so the verbatim text is read from
    /// staging via the `notes.trace` join. Defaults to borg's own staging
    /// default (`vault::paths::borg_stages_dir()`); an operator who overrode
    /// borg's `staging.root` must point this at the same directory. borg remains
    /// the sole staging writer; cortex only reads. Tilde-expanded at load.
    #[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]
    pub staging_root: PathBuf,
}

impl Default for EmbedConfig {
    fn default() -> Self {
        Self {
            workers: 0,
            max_chunks_per_call: crate::embed::DEFAULT_MAX_CHUNKS_PER_CALL,
            cadence_secs: crate::embed::DEFAULT_CADENCE_SECS,
            kinds: EmbedKindsConfig::default(),
            staging_root: vault::paths::borg_stages_dir(),
        }
    }
}

/// Per-kind on/off toggles for the embed loop's default kind set. This is the
/// "methodology selection is legitimate config" carve-out (`general.md`): the
/// daemon embed tick has no per-invocation CLI surface, so which kinds it
/// generates must be config. Mirrors the per-method `enabled` flags in
/// `oracle.yml`'s retrieval pipeline.
///
/// `claim` is default-OFF pending the kind-weighted-pooling contingency: the
/// v0.9.0 claim-embedding rollout regressed retrieval (the live `sb oracle eval`
/// gate failed 2026-07-05, nDCG 0.8795 -> 0.8471 with recall down too), and the
/// daemon tick embedded claims unconditionally so the `--drop-kind claim`
/// rollback was non-sticky. Gating it here keeps the claim-free baseline
/// restored across daemon ticks. The explicit `sb cortex embed --kind claim`
/// override still forces claim embedding (CLI > config) for the future
/// guard-first experiment.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct EmbedKindsConfig {
    pub summary: bool,
    pub transcript_chunk: bool,
    pub claim: bool,
}

impl Default for EmbedKindsConfig {
    fn default() -> Self {
        Self {
            summary: true,
            transcript_chunk: true,
            claim: false,
        }
    }
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
    /// NAME of the env var (or a file path) holding the Anthropic credential
    /// the fabric child needs under the literal name `ANTHROPIC_API_KEY`. NOT a
    /// standalone yml knob: [`Config::load`] overwrites it with `llm.api-key` at
    /// load so the two can never diverge (single source). The serde default only
    /// covers a bare `FabricConfig` deserialized outside a full `Config`.
    #[serde(rename = "api-key", alias = "api_key_env", alias = "api_key")]
    pub api_key: String,
}

impl Default for FabricConfig {
    fn default() -> Self {
        Self {
            binary: "fabric".to_string(),
            model: String::new(),
            max_content_chars: 32_000,
            timeout_secs: 120,
            api_key: "ANTHROPIC_API_KEY".to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct SchemaConfig {
    pub domains: Vec<String>,
    pub types: Vec<String>,
    pub origins: Vec<String>,
    pub statuses: Vec<String>,
    pub methods: Vec<String>,
}

impl Default for SchemaConfig {
    /// Built from the `vault::schema` enums (the single source of truth) so
    /// the validation vocabulary can never drift from the real Domain /
    /// NoteType / Origin / Status / Method variants. The previous derived
    /// `Default` produced EMPTY vecs (validated nothing), and the historically
    /// hand-typed defaults had already drifted (missing reddit/image/pdf/...).
    /// Config still overrides these.
    fn default() -> Self {
        use vault::schema::{Domain, Method, NoteType, Origin, Status};
        Self {
            domains: Domain::all().iter().map(|d| d.as_str().to_string()).collect(),
            types: NoteType::all().iter().map(|t| t.as_str().to_string()).collect(),
            origins: Origin::all().iter().map(|o| o.as_str().to_string()).collect(),
            statuses: Status::all().iter().map(|s| s.as_str().to_string()).collect(),
            methods: Method::all().iter().map(|m| m.as_str().to_string()).collect(),
        }
    }
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
    pub association: AssociationConfig,
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
    #[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]
    pub canonical_path: PathBuf,
    #[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]
    pub mapping_path: PathBuf,
    #[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]
    pub proposals_path: PathBuf,
    pub sweep_interval: String,
    pub proposal_threshold: usize,
    pub cold: ColdConfig,
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            canonical_path: vault::paths::canonical_tags(),
            mapping_path: vault::paths::tag_mapping(),
            proposals_path: vault::paths::tag_proposals(),
            sweep_interval: "1h".to_string(),
            proposal_threshold: 3,
            cold: ColdConfig::default(),
        }
    }
}

/// Knobs for `cortex sweep --cold`. The defaults are calibrated for a
/// vault entering its first audit moment with no prior signal state:
/// 180 days is long enough that short-term reference notes do not
/// surface before they have a chance to accrue signals, short enough
/// that the long tail surfaces within a year; 500 rows is the largest
/// list a reviewer can chew through in one sitting.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ColdConfig {
    pub older_than_days: u32,
    pub limit: u32,
}

impl Default for ColdConfig {
    fn default() -> Self {
        Self {
            older_than_days: 180,
            limit: 500,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LinkingConfig {
    #[serde(rename = "scan-for")]
    pub scan_for: Vec<String>,
    pub entities: LinkingEntities,
    pub targets: LinkingTargets,
    #[serde(rename = "min-word-length")]
    pub min_word_length: usize,
    /// Alias surface form → canonical concept slug. When `find_mention` matches
    /// a surface form, the linker emits a piped wikilink `[[slug|surface]]`.
    /// Loaded from `glossary.yml` (Phase 2 of graph-augmented-memory).
    #[serde(default)]
    pub aliases: HashMap<String, String>,
}

impl Default for LinkingConfig {
    fn default() -> Self {
        Self {
            scan_for: vec!["people".to_string(), "projects".to_string(), "concepts".to_string()],
            entities: LinkingEntities::default(),
            targets: LinkingTargets::default(),
            min_word_length: 5,
            aliases: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LinkingEntities {
    pub people: Vec<String>,
    pub projects: Vec<String>,
    /// Concept glossary: kebab-case slugs (mirroring `canonical-tags.yml`),
    /// loaded from `glossary.yml`. Each is linked at first body mention as
    /// `[[slug]]` (Phase 2 of graph-augmented-memory).
    pub concepts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LinkingTargets {
    pub types: LinkingFilter,
    pub paths: LinkingFilter,
}

#[derive(Debug, Clone, Deserialize, Default)]
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
            // None = fall back to the shared `llm.model`. A hardcoded pin here
            // silently overrode `llm.model` for the digest call.
            model: None,
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

/// Which similarity signal `cortex associate` uses to decide merge vs.
/// cross-link for a same-slug pair. Selecting the active methodology is
/// legitimate config per the `general.md` carve-out (this picks the
/// system's retrieval methodology, not whether a governance rule runs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SimilaritySource {
    /// Embedding cosine only (via `vault::search::cosine_between`); a pair
    /// with no summary embedding on either side is uncomputable.
    Embedding,
    /// Claim TF-IDF cosine only (`duplicates::cosine_similarity` over claim
    /// text).
    Claim,
    /// Embedding cosine primary, claim TF-IDF fallback when a note has no
    /// `kind=summary` embedding. The default: harvest session notes are
    /// summary-embedded in practice (probed 2026-07-24, 213 rows live), so
    /// embedding is the strong signal and claim only covers the gap.
    #[default]
    Both,
}

/// Knobs for `cortex associate` (`sb cortex associate`): groups harvest
/// session notes sharing a content-derived `slug:` (borg's harvest naming,
/// v0.12.2) and, per pairwise similarity, merges or cross-links them
/// (2026-07-24 cortex-association-sweep design). `deny_unknown_fields` so a
/// typo'd key fails the loader loud (see `Config::load_inner`) instead of
/// silently running with a default threshold.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AssociationConfig {
    /// Merge iff pairwise similarity >= this (mirrors `duplicates.threshold`).
    pub threshold: f64,
    /// Which similarity signal to compute.
    pub similarity_source: SimilaritySource,
    /// Skip a note (and its whole group - quiescence is whole-group) modified
    /// within this many seconds, so an actively-edited note is never merged
    /// mid-edit.
    pub min_quiescence_secs: u64,
    /// Glob paths excluded from association entirely.
    pub exclude: Vec<String>,
    /// Daemon cadence (seconds) for the association tick (Phase 5). A NEW
    /// periodic interval arm, modeled on `graph_interval_secs` /
    /// `discover_interval_secs` - not part of the doc's Data Model YAML
    /// example, added here because the Architecture section requires "own
    /// cadence config" and every sibling periodic action keys its cadence off
    /// its own config struct, never a bare literal. Default 3600 (hourly): a
    /// merge is destructive-ish (soft-retire) so it runs far less often than
    /// the read-mostly embed/graph ticks, matching the design's "e.g. hourly"
    /// suggestion.
    pub interval_secs: u64,
}

impl Default for AssociationConfig {
    fn default() -> Self {
        Self {
            threshold: 0.85,
            similarity_source: SimilaritySource::default(),
            min_quiescence_secs: 600,
            exclude: Vec::new(),
            interval_secs: 3_600,
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
    /// Cadence (seconds) for the cold-note sweep tick. Default 604_800
    /// (1 week): the cold report is a review surface, not a watchdog;
    /// reviewing more often than weekly out-paces the user.
    #[serde(rename = "cold-interval-secs")]
    pub cold_interval_secs: u64,
    /// Rayon thread-pool cap emitted into the installed systemd unit as
    /// `Environment="RAYON_NUM_THREADS=N"` (candle's gemm degree reads the
    /// same var, so this also bounds embedding fan-out). 0 (the default)
    /// omits the line entirely, letting rayon pick its own platform default.
    /// A cap is a per-host tuning knob (this fleet runs it at 8, ~nproc/4 on
    /// a 32-core box), never a value baked into Rust source.
    #[serde(rename = "rayon-threads")]
    pub rayon_threads: usize,
    /// Optional secret/environment bootstrap for the installed systemd unit.
    /// `None` (the default) omits both the `ExecStartPre` and
    /// `EnvironmentFile` directives, so a host with no secret bootstrap
    /// still gets a valid, complete unit. See [`EnvBootstrapConfig`].
    #[serde(rename = "env-bootstrap")]
    pub env_bootstrap: Option<EnvBootstrapConfig>,
}

/// Secret/environment bootstrap for the installed systemd unit: `command`'s
/// stdout is captured into `env_file` via
/// `ExecStartPre=/bin/sh -c '<command> > <env_file>'`, then the unit loads it
/// with `EnvironmentFile=-<env_file>` (the leading `-` makes a missing file
/// non-fatal). Lets a `sb cortex daemon --install` re-run reproduce a live
/// unit's `manifest age decrypt ... > /run/user/<uid>/cortex.env` secret
/// bootstrap without baking any UID or secrets path into Rust source - the
/// defect this phase closes (2026-07-05 cortex-daemon-oscillation-loop design
/// doc, Problem Statement defect 5).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct EnvBootstrapConfig {
    /// Shell command whose stdout is redirected into `env_file`.
    pub command: String,
    /// Destination path for the captured environment, e.g.
    /// `/run/user/1000/cortex.env`. Tilde-expanded at load time.
    #[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]
    pub env_file: PathBuf,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct DaemonAction {
    pub enable: bool,
}

impl DaemonConfig {
    /// All CONFIGURED action names (every key under `daemon.actions`),
    /// regardless of each action's `enable` flag. Use `is_enabled(name)` to
    /// test whether a specific action is actually on. (Was misnamed
    /// `enabled_actions`, which implied it filtered by `enable` - it never did.)
    pub fn configured_actions(&self) -> Vec<&str> {
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
            cold_interval_secs: 604_800,
            rayon_threads: 0,
            env_bootstrap: None,
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
    /// 2. ~/.config/sb/cortex.yml
    /// 3. Defaults
    pub fn load(config_path: Option<&PathBuf>) -> Result<Self> {
        let mut config = Self::load_inner(config_path)?;
        // Fabric (the third-party Go binary) reads its credential from the env
        // var named literally `ANTHROPIC_API_KEY`. `llm.api-key` is the single
        // source of truth for which var/file holds it; mirror it into
        // `fabric.api-key` here (not a second yml knob) so the two can never
        // diverge. The fabric-spawn boundary translates this NAME to
        // `ANTHROPIC_API_KEY` on the child process only. Applied to every load
        // path (explicit --config, primary, defaults).
        config.fabric.api_key = config.llm.api_key.clone();
        Ok(config)
    }

    fn load_inner(config_path: Option<&PathBuf>) -> Result<Self> {
        if let Some(path) = config_path {
            return Self::load_from_file(path).context(format!("Failed to load config from {}", path.display()));
        }

        let primary = vault::paths::cortex_config();
        if primary.exists() {
            // Fail-closed (2026-07-24 cortex-association-sweep design, panel
            // finding 8): a PRESENT config that fails to parse used to warn and
            // silently fall back to defaults, so a typo'd key ran the daemon on
            // defaults with zero visible signal. A present-but-unparseable file
            // is now a hard error; only a MISSING file defaults.
            return Self::load_from_file(&primary).context(format!("failed to load config from {}", primary.display()));
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

    /// Resolve the vault root via the unified resolver. Precedence: CLI > config > marker-gated CWD.
    pub fn vault_root(&self, cli_vault: Option<&PathBuf>) -> Result<PathBuf> {
        vault::paths::resolve_vault_root(cli_vault.map(|p| p.as_path()), self.vault.root_path.as_deref())
    }

    /// Path to oracle's search database. Cortex's embed loop reads from
    /// and writes to the same DB file oracle's indexer maintains, so
    /// both processes see the same notes + embeddings on every query.
    /// Resolved via `vault::paths::oracle_db_path` so cortex and oracle
    /// share one source of truth and can never desync.
    pub fn oracle_db_path(&self) -> PathBuf {
        vault::paths::oracle_db_path()
    }
}

#[cfg(test)]
mod tests;
