pub use vault::config::resolve_secret;

use eyre::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const APP_NAME: &str = "borg";

/// Load configuration with fallback chain:
/// 1. Explicit path (if provided)
/// 2. ~/.config/borg/borg.yml
/// 3. ./borg.yml
/// 4. Default
pub fn load_config<T: DeserializeOwned + Default>(config_path: Option<&PathBuf>) -> Result<T> {
    if let Some(path) = config_path {
        return load_from_file(path).context(format!("Failed to load config from {}", path.display()));
    }

    if let Some(config_dir) = dirs::config_dir() {
        let primary_config = config_dir.join(APP_NAME).join(format!("{APP_NAME}.yml"));
        if primary_config.exists() {
            match load_from_file(&primary_config) {
                Ok(config) => return Ok(config),
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to load config from {}: {}",
                        primary_config.display(),
                        e
                    );
                }
            }
        }
    }

    let fallback_config = PathBuf::from(format!("{APP_NAME}.yml"));
    if fallback_config.exists() {
        match load_from_file(&fallback_config) {
            Ok(config) => return Ok(config),
            Err(e) => {
                eprintln!(
                    "Warning: Failed to load config from {}: {}",
                    fallback_config.display(),
                    e
                );
            }
        }
    }

    log::info!("No config file found, using defaults");
    Ok(T::default())
}

fn load_from_file<T: DeserializeOwned, P: AsRef<Path>>(path: P) -> Result<T> {
    let content = fs::read_to_string(&path).context("Failed to read config file")?;
    let config: T = serde_yaml::from_str(&content).context("Failed to parse config file")?;
    log::info!("Loaded config from: {}", path.as_ref().display());
    Ok(config)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CanonicalRule {
    pub name: String,
    #[serde(rename = "match")]
    pub match_regex: String,
    pub canonical: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct CanonicalConfig {
    pub rules: Vec<CanonicalRule>,
}

impl Default for CanonicalConfig {
    fn default() -> Self {
        Self {
            rules: default_canonicalization_rules(),
        }
    }
}

pub fn default_canonicalization_rules() -> Vec<CanonicalRule> {
    vec![
        // YouTube Shorts — normalize domain only, keep /shorts/ path
        CanonicalRule {
            name: "youtube-shorts-mobile".to_string(),
            match_regex: r"https?://m\.youtube\.com/shorts/(?P<id>[a-zA-Z0-9_-]+)".to_string(),
            canonical: "https://www.youtube.com/shorts/{id}".to_string(),
        },
        // YouTube watch — normalize all domain variants to www.youtube.com
        CanonicalRule {
            name: "youtube-shortlink".to_string(),
            match_regex: r"https?://youtu\.be/(?P<id>[a-zA-Z0-9_-]+)".to_string(),
            canonical: "https://www.youtube.com/watch?v={id}".to_string(),
        },
        CanonicalRule {
            name: "youtube-mobile".to_string(),
            match_regex: r"https?://m\.youtube\.com/watch\?v=(?P<id>[a-zA-Z0-9_-]+)".to_string(),
            canonical: "https://www.youtube.com/watch?v={id}".to_string(),
        },
        CanonicalRule {
            name: "youtube-music".to_string(),
            match_regex: r"https?://music\.youtube\.com/watch\?v=(?P<id>[a-zA-Z0-9_-]+)".to_string(),
            canonical: "https://www.youtube.com/watch?v={id}".to_string(),
        },
        CanonicalRule {
            name: "youtube-nocookie".to_string(),
            match_regex: r"https?://www\.youtube-nocookie\.com/embed/(?P<id>[a-zA-Z0-9_-]+)".to_string(),
            canonical: "https://www.youtube.com/watch?v={id}".to_string(),
        },
        // Twitter/X — normalize to x.com
        CanonicalRule {
            name: "twitter-to-x".to_string(),
            match_regex: r"https?://twitter\.com/(?P<path>.*)".to_string(),
            canonical: "https://x.com/{path}".to_string(),
        },
        CanonicalRule {
            name: "mobile-twitter".to_string(),
            match_regex: r"https?://mobile\.twitter\.com/(?P<path>.*)".to_string(),
            canonical: "https://x.com/{path}".to_string(),
        },
    ]
}

/// Merge user-provided rules with built-in defaults.
/// Config rules with the same name replace the built-in; new names are appended.
pub fn merge_canonicalization_rules(config_rules: &[CanonicalRule]) -> Vec<CanonicalRule> {
    let defaults = default_canonicalization_rules();
    if config_rules.is_empty() {
        return defaults;
    }

    let mut merged: Vec<CanonicalRule> = Vec::new();
    for default in &defaults {
        if let Some(override_rule) = config_rules.iter().find(|r| r.name == default.name) {
            merged.push(override_rule.clone());
        } else {
            merged.push(default.clone());
        }
    }
    // Append config rules that don't match any built-in name
    for rule in config_rules {
        if !defaults.iter().any(|d| d.name == rule.name) {
            merged.push(rule.clone());
        }
    }
    merged
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub server: ServerConfig,
    pub vault: VaultConfig,
    pub transcriber: TranscriberConfig,
    pub groq: GroqConfig,
    pub llm: LlmConfig,
    pub telegram: Option<TelegramConfig>,
    pub discord: Option<DiscordConfig>,
    pub ntfy: Option<NtfyConfig>,
    #[serde(default = "default_links")]
    pub links: Vec<LinkConfig>,
    pub fabric: FabricConfig,
    pub frontmatter: FrontmatterConfig,
    pub hotkey: HotkeyConfig,
    pub canonicalization: CanonicalConfig,
    pub migration: MigrationConfig,
    pub text_capture: TextCaptureConfig,
    pub vision: VisionConfig,
    pub tags: TagsConfig,
    pub staging: StagingConfig,
    pub youtube: YoutubeConfig,
    #[serde(default)]
    pub pipeline: PipelineConfig,
    pub log_level: Option<String>,
}

/// Bounded-wait configuration for the ingestion pipeline. All timeouts are
/// in seconds. Each per-call timeout is a backstop for an external tool or
/// network call that could otherwise hang indefinitely. `hard_timeout_secs`
/// wraps the whole `process_url_inner` future as a final backstop for any
/// unbounded path the per-call timeouts miss; if it fires, that is a signal
/// to investigate the underlying hang, not a feature.
const DEFAULT_MAX_CONCURRENT_TRACES: usize = 8;
const DEFAULT_MAX_CONCURRENT_HEAVY_TRACES: usize = 4;

/// Per-subprocess fetch timeouts. Each external tool needs its own bound
/// because their expected latencies differ by orders of magnitude:
/// `fabric -u` and `markitdown` are URL scrapes that should complete in
/// under a minute; `fabric -y --transcript` hits a captions API whose
/// payload can be larger; LLM completions via `fabric -p <pattern>`
/// (governed by `fabric.timeout_secs`, default 600s) genuinely can need
/// minutes. Conflating them under a single value lets a hung URL fetch
/// burn the LLM budget; separating them caps the blast radius.
const DEFAULT_FABRIC_URL_TIMEOUT_SECS: u64 = 60;
const DEFAULT_FABRIC_TRANSCRIPT_TIMEOUT_SECS: u64 = 120;
const DEFAULT_MARKITDOWN_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PipelineConfig {
    pub hard_timeout_secs: u64,
    pub subtitle_fetch_timeout_secs: u64,
    pub yt_dlp_timeout_secs: u64,
    pub ocr_timeout_secs: u64,
    pub jina_timeout_secs: u64,
    pub fabric_url_timeout_secs: u64,
    pub fabric_transcript_timeout_secs: u64,
    pub markitdown_timeout_secs: u64,
    pub max_concurrent_traces: usize,
    pub max_concurrent_heavy_traces: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            hard_timeout_secs: 1800,
            subtitle_fetch_timeout_secs: 30,
            yt_dlp_timeout_secs: 600,
            ocr_timeout_secs: 60,
            jina_timeout_secs: 60,
            fabric_url_timeout_secs: DEFAULT_FABRIC_URL_TIMEOUT_SECS,
            fabric_transcript_timeout_secs: DEFAULT_FABRIC_TRANSCRIPT_TIMEOUT_SECS,
            markitdown_timeout_secs: DEFAULT_MARKITDOWN_TIMEOUT_SECS,
            max_concurrent_traces: DEFAULT_MAX_CONCURRENT_TRACES,
            max_concurrent_heavy_traces: DEFAULT_MAX_CONCURRENT_HEAVY_TRACES,
        }
    }
}

/// Numerator-only fraction of `nproc` used for default ffmpeg thread counts.
/// On a 32-core box this resolves to 4 threads per ffmpeg invocation;
/// `MIN_FFMPEG_THREADS` enforces a floor for smaller hosts.
const DEFAULT_FFMPEG_THREAD_DENOM: usize = 8;

/// Minimum threads per ffmpeg invocation regardless of host size. ffmpeg's
/// `-threads 1` is meaningfully slower than `-threads 2` for long videos at
/// negligible CPU savings, so 2 is the floor.
const MIN_FFMPEG_THREADS: usize = 2;

/// Thread-count knob accepting either a literal integer or an `nproc`-derived
/// expression. Defaults expressed as fractions of `nproc` so the same config
/// behaves sensibly across host sizes; resolved at call time.
///
/// Accepted YAML forms:
/// - Integer (e.g. `4`) -> a literal thread count.
/// - String `"nproc"` -> all cores.
/// - String `"nproc/N"` for positive integer `N` -> `nproc / N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadCount(Spec);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spec {
    Absolute(usize),
    NprocOver { denom: usize },
}

impl ThreadCount {
    pub fn absolute(n: usize) -> Self {
        Self(Spec::Absolute(n))
    }

    pub fn nproc_over(denom: usize) -> Self {
        Self(Spec::NprocOver { denom })
    }

    /// Resolve the symbolic value against the host's available parallelism.
    /// Always returns at least `MIN_FFMPEG_THREADS`.
    pub fn resolve(self) -> usize {
        let nproc = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        let raw = match self.0 {
            Spec::Absolute(n) => n,
            Spec::NprocOver { denom } => nproc.saturating_div(denom.max(1)),
        };
        raw.max(MIN_FFMPEG_THREADS)
    }

    /// Symbolic form (inverse of the parser) for logs and config dumps.
    pub fn symbolic(self) -> String {
        match self.0 {
            Spec::Absolute(n) => n.to_string(),
            Spec::NprocOver { denom: 1 } => "nproc".to_string(),
            Spec::NprocOver { denom } => format!("nproc/{denom}"),
        }
    }
}

impl Serialize for ThreadCount {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0 {
            Spec::Absolute(n) => serializer.serialize_u64(n as u64),
            Spec::NprocOver { denom: 1 } => serializer.serialize_str("nproc"),
            Spec::NprocOver { denom } => serializer.serialize_str(&format!("nproc/{denom}")),
        }
    }
}

impl<'de> Deserialize<'de> for ThreadCount {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = ThreadCount;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a positive integer, the string \"nproc\", or \"nproc/N\" for positive integer N")
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                if v < 1 {
                    return Err(E::custom(format!("thread count must be >= 1, got {v}")));
                }
                Ok(ThreadCount::absolute(v as usize))
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                if v < 1 {
                    return Err(E::custom("thread count must be >= 1, got 0"));
                }
                Ok(ThreadCount::absolute(v as usize))
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                let trimmed = v.trim();
                if trimmed == "nproc" {
                    return Ok(ThreadCount::nproc_over(1));
                }
                if let Some(denom_str) = trimmed.strip_prefix("nproc/") {
                    let denom: usize = denom_str
                        .parse()
                        .map_err(|_| E::custom(format!("expected positive integer denominator in {trimmed:?}")))?;
                    if denom == 0 {
                        return Err(E::custom("denominator must be >= 1"));
                    }
                    return Ok(ThreadCount::nproc_over(denom));
                }
                Err(E::custom(format!(
                    "expected integer, \"nproc\", or \"nproc/N\", got {trimmed:?}"
                )))
            }
        }
        deserializer.deserialize_any(V)
    }
}

fn default_ffmpeg_threads() -> ThreadCount {
    ThreadCount::nproc_over(DEFAULT_FFMPEG_THREAD_DENOM)
}

fn default_ffmpeg_filter_threads() -> ThreadCount {
    ThreadCount::nproc_over(DEFAULT_FFMPEG_THREAD_DENOM)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct YoutubeConfig {
    pub slides: YoutubeSlidesConfig,
    /// ffmpeg `-threads` value for decoder/encoder threads. Applied to direct
    /// ffmpeg invocations (slides extraction) and spliced into yt-dlp's
    /// `--postprocessor-args` for the audio path.
    #[serde(default = "default_ffmpeg_threads")]
    pub ffmpeg_threads: ThreadCount,
    /// ffmpeg `-filter_threads` value for filter-graph threading (mpdecimate).
    /// Applied only to direct ffmpeg invocations that use a filter graph.
    #[serde(default = "default_ffmpeg_filter_threads")]
    pub ffmpeg_filter_threads: ThreadCount,
}

impl Default for YoutubeConfig {
    fn default() -> Self {
        Self {
            slides: YoutubeSlidesConfig::default(),
            ffmpeg_threads: default_ffmpeg_threads(),
            ffmpeg_filter_threads: default_ffmpeg_filter_threads(),
        }
    }
}

impl YoutubeConfig {
    /// Argv tokens for a direct `Command::new("ffmpeg")` invocation that runs
    /// a filter graph. Returns `["-threads", "<n>", "-filter_threads", "<m>"]`
    /// with values resolved against the host's `nproc`.
    pub fn ffmpeg_thread_args(&self) -> [String; 4] {
        [
            "-threads".to_string(),
            self.ffmpeg_threads.resolve().to_string(),
            "-filter_threads".to_string(),
            self.ffmpeg_filter_threads.resolve().to_string(),
        ]
    }

    /// Thread count to splice into yt-dlp's `--postprocessor-args` string.
    /// The audio path has no filter graph, so `-filter_threads` is omitted.
    pub fn yt_dlp_postprocessor_threads(&self) -> usize {
        self.ffmpeg_threads.resolve()
    }
}

/// Frame-aware YouTube ingestion config (see docs/design/2026-04-29-frame-aware-youtube-ingestion.md).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct YoutubeSlidesConfig {
    /// Master switch. When false, frame extraction and slide segmentation are skipped
    /// entirely; the pipeline reverts to pre-frame-aware behavior.
    pub enabled: bool,
    /// Hard cap on extracted frames per video (per-budget tier may be lower).
    pub max_frames: u32,
    /// Hard ceiling on the source-resampling fps fed to mpdecimate. The
    /// auto-fps table picks an effective fps based on duration; this caps it.
    pub max_fps: f32,
    /// mpdecimate `hi` threshold (default 64*32 = 2048).
    pub mpdecimate_hi: u32,
    /// mpdecimate `lo` threshold (default 64*16 = 1024).
    pub mpdecimate_lo: u32,
    /// mpdecimate `frac` threshold (fraction of 8x8 blocks that must change).
    pub mpdecimate_frac: f32,
    /// Hamming-distance threshold for clustering frames into a single slide.
    pub phash_hamming_threshold: u32,
    /// Slide clusters shorter than this many seconds are dropped as transition artifacts.
    pub transition_min_seconds: f32,
    /// Width to downscale frames to before writing JPEGs (height auto-preserves aspect).
    pub frame_resolution_px: u32,
    /// Per-slide vision-API captioning. `false` in Phase 1 (Tesseract OCR only).
    pub vision_per_slide: bool,
    /// Thresholds that drive Stage 1's note-shape proposal.
    pub slide_thresholds: SlideThresholds,
}

impl Default for YoutubeSlidesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_frames: 100,
            max_fps: 2.0,
            mpdecimate_hi: 64 * 32,
            mpdecimate_lo: 64 * 16,
            mpdecimate_frac: 0.33,
            phash_hamming_threshold: 6,
            transition_min_seconds: 5.0,
            frame_resolution_px: 512,
            vision_per_slide: false,
            slide_thresholds: SlideThresholds::default(),
        }
    }
}

/// Thresholds that drive Stage 1's note-shape proposal.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct SlideThresholds {
    /// `compression_ratio >= this` => text-only (motion / animated content).
    pub text_only_max_ratio: f32,
    /// `compression_ratio < this` => slide-section (slide-heavy content).
    pub slide_section_max_ratio: f32,
    /// Below this unique-slide count, fall back to text-only regardless of ratio
    /// (handles talking-head / static-camera videos with low ratio but few slides).
    pub min_unique_slides: u32,
}

impl Default for SlideThresholds {
    fn default() -> Self {
        Self {
            text_only_max_ratio: 0.50,
            slide_section_max_ratio: 0.10,
            min_unique_slides: 4,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct StagingConfig {
    /// Master switch. When `false` the staged pipeline is dormant (no stage
    /// artifacts are written, no gates fire). Flipped `true` in Phase 2 rollout
    /// once the artifact store + Stage 0 plumbing is live.
    pub enabled: bool,
    /// Root directory for staging artifacts. Per-trace directories hang off this.
    pub root: PathBuf,
    /// Retention window for successful traces. A directory without a
    /// `rejection.yml` older than this is deleted by the retention sweep.
    pub retention_days: u32,
    /// Retention window for rejected traces (presence of `rejection.yml`).
    /// Intentionally longer than `retention_days` so failures have a bigger
    /// investigation window.
    pub rejected_retention_days: u32,
    /// On-disk layout. `PerTrace` is the recommended default (see design doc
    /// Storage Organization Options). `PerStage` exists as a config knob for
    /// users who want stage-level views.
    pub layout: StagingLayout,
    /// Soft cap on total staging disk usage in GB; used by the retention
    /// sweep to emit a warning alert past `size_alert_threshold_pct`.
    pub max_size_gb: u32,
    /// Percentage of `max_size_gb` that triggers a disk-usage alert.
    pub size_alert_threshold_pct: u8,
    /// Write-through mode: the legacy single-shot pipeline still publishes
    /// notes, but any URL fetch it performs is intercepted and also persisted
    /// to the artifact store. Enables Stage 1/2 to read from disk offline
    /// while preserving a one-fetch-per-ingestion invariant.
    pub double_write: bool,
}

impl Default for StagingConfig {
    fn default() -> Self {
        let root = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from(".local/share"))
            .join("borg")
            .join("stages");
        Self {
            enabled: false,
            root,
            retention_days: 60,
            rejected_retention_days: 90,
            layout: StagingLayout::default(),
            max_size_gb: 20,
            size_alert_threshold_pct: 80,
            double_write: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum StagingLayout {
    #[default]
    PerTrace,
    PerStage,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct VisionConfig {
    pub enabled: bool,
    pub model: String,
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: String::new(), // empty = use llm.model
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct MigrationConfig {
    pub field_renames: std::collections::HashMap<String, String>,
    pub value_renames: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    pub field_transforms: std::collections::HashMap<String, String>,
    pub title_fallback: bool,
    pub seed_borg_log: bool,
    pub skip_folders: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct TextCaptureConfig {}

fn default_links() -> Vec<LinkConfig> {
    vec![
        LinkConfig {
            name: "shorts".to_string(),
            regex: r"https?://(?:www\.)?youtube\.com/shorts/([a-zA-Z0-9_-]+)".to_string(),
            resolution: "480p".to_string(),
        },
        LinkConfig {
            name: "youtube".to_string(),
            regex:
                r"https?://(?:www\.)?(youtube\.com/watch\?v=|youtu\.be/|music\.youtube\.com/watch\?v=)([a-zA-Z0-9_-]+)"
                    .to_string(),
            resolution: "FWVGA".to_string(),
        },
        LinkConfig {
            name: "github".to_string(),
            regex: r"https?://github\.com/[^/]+/[^/]+/?(\?[^ ]*)?$".to_string(),
            resolution: "FWVGA".to_string(),
        },
        LinkConfig {
            name: "social".to_string(),
            regex: r"https?://x\.com/[^/]+/status/\d+".to_string(),
            resolution: "FWVGA".to_string(),
        },
        LinkConfig {
            name: "reddit".to_string(),
            regex: r"https?://(?:www\.)?reddit\.com/r/[^/]+/comments/".to_string(),
            resolution: "FWVGA".to_string(),
        },
        LinkConfig {
            name: "default".to_string(),
            regex: r".*".to_string(),
            resolution: "FWVGA".to_string(),
        },
    ]
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LinkConfig {
    pub name: String,
    pub regex: String,
    #[serde(default = "default_resolution")]
    pub resolution: String,
}

fn default_resolution() -> String {
    "FWVGA".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct FabricConfig {
    pub binary: String,
    pub model: String,
    pub summarize_pattern_youtube: String,
    pub summarize_pattern_article: String,
    pub condense_pattern: String,
    pub tag_pattern: String,
    pub max_content_chars: usize,
    /// Per-call timeout in seconds for `fabric -p <pattern>` LLM completions.
    /// URL scrapes (`fabric -u`, `markitdown`) and YouTube transcript fetches
    /// (`fabric -y`) use their own pipeline-level timeouts so a stuck fetch
    /// cannot consume the LLM budget - see `PipelineConfig`.
    pub timeout_secs: u64,
}

impl Default for FabricConfig {
    fn default() -> Self {
        Self {
            binary: "fabric".to_string(),
            model: String::new(),
            summarize_pattern_youtube: "obsidian-note.md".to_string(),
            summarize_pattern_article: "obsidian-note.md".to_string(),
            condense_pattern: "condense.md".to_string(),
            tag_pattern: "create_tags".to_string(),
            max_content_chars: 100000,
            timeout_secs: 600,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct FrontmatterConfig {
    #[serde(default)]
    pub default_tags: Vec<String>,
    #[serde(default, alias = "default-author")]
    pub default_creator: String,
    pub timezone: String,
}

impl Default for FrontmatterConfig {
    fn default() -> Self {
        Self {
            default_tags: vec![],
            default_creator: String::new(),
            timezone: "America/Los_Angeles".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TelegramConfig {
    #[serde(alias = "bot_token_env", alias = "bot_token")]
    pub bot_token: String,
    #[serde(default, alias = "allowed_chat_ids")]
    pub allowed_chat_ids: Vec<i64>,
    /// Default chat ID for cross-method notifications.
    /// If not set, falls back to first allowed_chat_ids entry.
    #[serde(default, alias = "notification_chat_id")]
    pub notification_chat_id: Option<i64>,
    /// If set, only run the Telegram poller on the host with this hostname.
    #[serde(default)]
    pub host: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct DiscordConfig {
    #[serde(alias = "bot_token_env", alias = "bot_token")]
    pub bot_token: String,
    #[serde(alias = "channel_id")]
    pub channel_id: u64,
    /// If set, only run the Discord bot on the host with this hostname.
    #[serde(default)]
    pub host: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct NtfyConfig {
    pub topic: String,
    #[serde(default = "default_ntfy_server")]
    pub server: String,
    pub token: Option<String>,
    /// If set, only run the ntfy subscriber on the host with this hostname.
    #[serde(default)]
    pub host: Option<String>,
}

fn default_ntfy_server() -> String {
    "https://ntfy.sh".to_string()
}

/// Check whether a service should run on this host.
/// Returns true if `host` is None/empty (run everywhere) or matches the current hostname.
pub fn is_local_host(host: &Option<String>) -> bool {
    match host {
        None => true,
        Some(h) if h.is_empty() => true,
        Some(h) => {
            let Ok(current) = hostname::get() else {
                return true; // if we can't determine hostname, run anyway
            };
            current.to_string_lossy().eq_ignore_ascii_case(h)
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct VaultConfig {
    pub root_path: String,
    pub inbox_path: String,
    pub vault_name: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct TranscriberConfig {
    pub url: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct GroqConfig {
    #[serde(alias = "api_key_env", alias = "api_key")]
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    #[serde(alias = "api_key_env", alias = "api_key")]
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct HotkeyConfig {
    pub host: String,
    pub port: u16,
    pub key: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 8181,
            key: "<Ctrl><Shift>b".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct TagsConfig {
    pub canonical_path: String,
    pub mapping_path: String,
    pub reject_concatenated: bool,
}

impl Default for TagsConfig {
    fn default() -> Self {
        Self {
            canonical_path: "~/.config/second-brain/canonical-tags.yml".to_string(),
            mapping_path: "~/.config/second-brain/tag-mapping.yml".to_string(),
            reject_concatenated: true,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
        }
    }
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            root_path: "~/obsidian-vault".to_string(),
            inbox_path: "~/obsidian-vault/inbox".to_string(),
            vault_name: "obsidian".to_string(),
        }
    }
}

impl Default for TranscriberConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:8090".to_string(),
            timeout_secs: 120,
        }
    }
}

impl Default for GroqConfig {
    fn default() -> Self {
        Self {
            api_key: "GROQ_API_KEY".to_string(),
            model: "whisper-large-v3".to_string(),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, Default, PartialEq)]
    struct TestConfig {
        #[serde(default)]
        name: String,
    }

    #[test]
    fn test_load_config_returns_default_when_no_file() {
        let config: TestConfig = load_config(None).expect("should succeed");
        assert_eq!(config, TestConfig::default());
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.transcriber.url, "http://localhost:8090");
        assert_eq!(config.groq.model, "whisper-large-v3");
        assert_eq!(config.llm.provider, "claude");
    }

    #[test]
    fn test_pipeline_defaults_include_concurrency_caps() {
        let config = Config::default();
        assert_eq!(config.pipeline.max_concurrent_traces, DEFAULT_MAX_CONCURRENT_TRACES);
        assert_eq!(
            config.pipeline.max_concurrent_heavy_traces,
            DEFAULT_MAX_CONCURRENT_HEAVY_TRACES
        );
    }

    #[test]
    fn test_pipeline_concurrency_caps_yaml_override() {
        let yaml = r#"
pipeline:
  max-concurrent-traces: 12
  max-concurrent-heavy-traces: 6
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("should parse");
        assert_eq!(config.pipeline.max_concurrent_traces, 12);
        assert_eq!(config.pipeline.max_concurrent_heavy_traces, 6);
    }

    #[test]
    fn test_thread_count_parse_integer() {
        let tc: ThreadCount = serde_yaml::from_str("4").expect("parse 4");
        assert_eq!(tc, ThreadCount::absolute(4));
    }

    #[test]
    fn test_thread_count_parse_nproc() {
        let tc: ThreadCount = serde_yaml::from_str(r#""nproc""#).expect("parse nproc");
        assert_eq!(tc, ThreadCount::nproc_over(1));
    }

    #[test]
    fn test_thread_count_parse_nproc_over_n() {
        let tc: ThreadCount = serde_yaml::from_str(r#""nproc/8""#).expect("parse nproc/8");
        assert_eq!(tc, ThreadCount::nproc_over(8));
    }

    #[test]
    fn test_thread_count_rejects_invalid() {
        for bad in [
            "\"nproc/0\"",
            "\"-1\"",
            "\"4cores\"",
            "\"nproc/abc\"",
            "\"\"",
            "0",
            "-3",
        ] {
            let result: std::result::Result<ThreadCount, _> = serde_yaml::from_str(bad);
            assert!(result.is_err(), "expected error parsing {bad:?}, got {result:?}");
        }
    }

    #[test]
    fn test_thread_count_resolve_floors_at_min() {
        assert_eq!(ThreadCount::absolute(1).resolve(), MIN_FFMPEG_THREADS);
        assert_eq!(ThreadCount::nproc_over(999).resolve(), MIN_FFMPEG_THREADS);
    }

    #[test]
    fn test_thread_count_roundtrip_integer() {
        let tc = ThreadCount::absolute(4);
        let yaml = serde_yaml::to_string(&tc).expect("serialize");
        let reparsed: ThreadCount = serde_yaml::from_str(&yaml).expect("reparse");
        assert_eq!(reparsed, tc);
    }

    #[test]
    fn test_thread_count_roundtrip_nproc() {
        let tc = ThreadCount::nproc_over(1);
        let yaml = serde_yaml::to_string(&tc).expect("serialize");
        let reparsed: ThreadCount = serde_yaml::from_str(&yaml).expect("reparse");
        assert_eq!(reparsed, tc);
    }

    #[test]
    fn test_thread_count_roundtrip_nproc_over_n() {
        let tc = ThreadCount::nproc_over(8);
        let yaml = serde_yaml::to_string(&tc).expect("serialize");
        let reparsed: ThreadCount = serde_yaml::from_str(&yaml).expect("reparse");
        assert_eq!(reparsed, tc);
        assert!(
            yaml.contains("nproc/8"),
            "expected serialized form to contain 'nproc/8', got {yaml:?}"
        );
    }

    #[test]
    fn test_thread_count_symbolic_forms() {
        assert_eq!(ThreadCount::absolute(4).symbolic(), "4");
        assert_eq!(ThreadCount::nproc_over(1).symbolic(), "nproc");
        assert_eq!(ThreadCount::nproc_over(8).symbolic(), "nproc/8");
    }

    #[test]
    fn test_youtube_config_default_uses_nproc_over_default_denom() {
        let cfg = YoutubeConfig::default();
        assert_eq!(cfg.ffmpeg_threads, ThreadCount::nproc_over(DEFAULT_FFMPEG_THREAD_DENOM));
        assert_eq!(
            cfg.ffmpeg_filter_threads,
            ThreadCount::nproc_over(DEFAULT_FFMPEG_THREAD_DENOM)
        );
    }

    #[test]
    fn test_youtube_config_serde_default_matches_struct_default() {
        let from_yaml: YoutubeConfig = serde_yaml::from_str("{}").expect("parse empty");
        let from_default = YoutubeConfig::default();
        assert_eq!(from_yaml.ffmpeg_threads, from_default.ffmpeg_threads);
        assert_eq!(from_yaml.ffmpeg_filter_threads, from_default.ffmpeg_filter_threads);
    }

    #[test]
    fn test_youtube_config_yaml_override() {
        let yaml = r#"
youtube:
  ffmpeg-threads: 4
  ffmpeg-filter-threads: "nproc/4"
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("should parse");
        assert_eq!(config.youtube.ffmpeg_threads, ThreadCount::absolute(4));
        assert_eq!(config.youtube.ffmpeg_filter_threads, ThreadCount::nproc_over(4));
    }

    #[test]
    fn test_youtube_config_ffmpeg_thread_args_shape() {
        let cfg = YoutubeConfig {
            slides: YoutubeSlidesConfig::default(),
            ffmpeg_threads: ThreadCount::absolute(3),
            ffmpeg_filter_threads: ThreadCount::absolute(5),
        };
        let args = cfg.ffmpeg_thread_args();
        assert_eq!(args[0], "-threads");
        assert_eq!(args[1], "3");
        assert_eq!(args[2], "-filter_threads");
        assert_eq!(args[3], "5");
    }

    #[test]
    fn test_youtube_config_yt_dlp_postprocessor_threads_matches_ffmpeg_threads() {
        let cfg = YoutubeConfig {
            slides: YoutubeSlidesConfig::default(),
            ffmpeg_threads: ThreadCount::absolute(3),
            ffmpeg_filter_threads: ThreadCount::absolute(5),
        };
        assert_eq!(cfg.yt_dlp_postprocessor_threads(), 3);
    }

    #[test]
    fn test_config_deserialize() {
        let yaml = r#"
server:
  host: "127.0.0.1"
  port: 9090
vault:
  inbox-path: "/tmp/vault/inbox"
transcriber:
  url: "http://192.168.1.100:8090"
  timeout-secs: 60
groq:
  model: "whisper-large-v3-turbo"
llm:
  provider: "ollama"
  model: "llama3"
log-level: debug
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("should parse");
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 9090);
        assert_eq!(config.vault.inbox_path, "/tmp/vault/inbox");
        assert_eq!(config.transcriber.url, "http://192.168.1.100:8090");
        assert_eq!(config.transcriber.timeout_secs, 60);
        assert_eq!(config.groq.model, "whisper-large-v3-turbo");
        assert_eq!(config.llm.provider, "ollama");
        assert_eq!(config.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn test_config_without_bot_sections() {
        let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8080
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("should parse");
        assert!(config.telegram.is_none());
        assert!(config.discord.is_none());
        assert!(config.ntfy.is_none());
    }

    #[test]
    fn test_config_with_ntfy_section() {
        let yaml = r#"
ntfy:
  topic: "obsidian-borg-abc123"
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("should parse");
        let ntfy = config.ntfy.expect("ntfy should be Some");
        assert_eq!(ntfy.topic, "obsidian-borg-abc123");
        assert_eq!(ntfy.server, "https://ntfy.sh");
        assert!(ntfy.token.is_none());
    }

    #[test]
    fn test_config_with_ntfy_full() {
        let yaml = r#"
ntfy:
  topic: "my-topic"
  server: "https://ntfy.example.com"
  token: "~/.config/ntfy/token"
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("should parse");
        let ntfy = config.ntfy.expect("ntfy should be Some");
        assert_eq!(ntfy.topic, "my-topic");
        assert_eq!(ntfy.server, "https://ntfy.example.com");
        assert_eq!(ntfy.token, Some("~/.config/ntfy/token".to_string()));
    }

    #[test]
    fn test_config_with_telegram_section() {
        let yaml = r#"
telegram:
  bot-token: TELEGRAM_BOT_TOKEN
  allowed-chat-ids: [123456, 789012]
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("should parse");
        let tg = config.telegram.expect("telegram should be Some");
        assert_eq!(tg.bot_token, "TELEGRAM_BOT_TOKEN");
        assert_eq!(tg.allowed_chat_ids, vec![123456, 789012]);
    }

    #[test]
    fn test_config_with_telegram_no_allowed_ids() {
        let yaml = r#"
telegram:
  bot-token: TELEGRAM_BOT_TOKEN
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("should parse");
        let tg = config.telegram.expect("telegram should be Some");
        assert!(tg.allowed_chat_ids.is_empty());
    }

    #[test]
    fn test_config_with_discord_section() {
        let yaml = r#"
discord:
  bot-token: DISCORD_BOT_TOKEN
  channel-id: 1234567890
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("should parse");
        let dc = config.discord.expect("discord should be Some");
        assert_eq!(dc.bot_token, "DISCORD_BOT_TOKEN");
        assert_eq!(dc.channel_id, 1234567890);
    }

    #[test]
    fn test_config_with_both_bots() {
        let yaml = r#"
telegram:
  bot-token: TG_TOKEN
discord:
  bot-token: DC_TOKEN
  channel-id: 999
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("should parse");
        assert!(config.telegram.is_some());
        assert!(config.discord.is_some());
    }

    #[test]
    fn test_default_canonicalization_rules() {
        let rules = default_canonicalization_rules();
        assert!(!rules.is_empty());
        assert_eq!(rules[0].name, "youtube-shorts-mobile");
    }

    #[test]
    fn test_merge_canonicalization_rules_empty_config() {
        let merged = merge_canonicalization_rules(&[]);
        assert_eq!(merged.len(), default_canonicalization_rules().len());
    }

    #[test]
    fn test_merge_canonicalization_rules_override() {
        let overrides = vec![CanonicalRule {
            name: "youtube-shortlink".to_string(),
            match_regex: "custom".to_string(),
            canonical: "custom".to_string(),
        }];
        let merged = merge_canonicalization_rules(&overrides);
        let rule = merged.iter().find(|r| r.name == "youtube-shortlink").expect("found");
        assert_eq!(rule.match_regex, "custom");
    }

    #[test]
    fn test_merge_canonicalization_rules_append() {
        let custom = vec![CanonicalRule {
            name: "old-reddit".to_string(),
            match_regex: "r".to_string(),
            canonical: "c".to_string(),
        }];
        let merged = merge_canonicalization_rules(&custom);
        assert_eq!(merged.len(), default_canonicalization_rules().len() + 1);
        assert_eq!(merged.last().expect("last").name, "old-reddit");
    }

    #[test]
    fn test_config_with_canonicalization() {
        let yaml = r#"
canonicalization:
  rules:
    - name: old-reddit
      match: 'https?://old\.reddit\.com/(?P<path>.*)'
      canonical: "https://www.reddit.com/{path}"
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("should parse");
        assert_eq!(config.canonicalization.rules.len(), 1);
        assert_eq!(config.canonicalization.rules[0].name, "old-reddit");
    }

    #[test]
    fn test_resolve_secret_from_file() {
        let dir = std::env::temp_dir().join("obsidian-borg-test-secret");
        fs::create_dir_all(&dir).expect("create dir");
        let file = dir.join("test-token");
        fs::write(&file, "  my-secret-value\n").expect("write");
        let result = resolve_secret(file.to_str().expect("path")).expect("resolve");
        assert_eq!(result, "my-secret-value");
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn test_resolve_secret_from_env() {
        let key = "OBSIDIAN_BORG_TEST_SECRET_42";
        // SAFETY: single-threaded test, no other threads reading this env var
        unsafe { std::env::set_var(key, "env-secret-value") };
        let result = resolve_secret(key).expect("resolve");
        assert_eq!(result, "env-secret-value");
        unsafe { std::env::remove_var(key) };
    }

    #[test]
    fn test_resolve_secret_missing() {
        let result = resolve_secret("NONEXISTENT_VAR_OBSBORG_TEST_999");
        assert!(result.is_err());
    }

    #[test]
    fn test_pipeline_config_defaults_for_split_fetch_timeouts() {
        // Per docs/design/2026-05-18-fabric-pattern-resolve-and-distill-dlq.md
        // the article-fetch path got split into per-subprocess timeouts so a
        // stuck `fabric -u` can no longer eat the LLM completion budget.
        let p = PipelineConfig::default();
        assert_eq!(p.fabric_url_timeout_secs, 60, "fabric -u: URL scrape, 60s ceiling");
        assert_eq!(
            p.fabric_transcript_timeout_secs, 120,
            "fabric -y: transcript fetch, 120s ceiling"
        );
        assert_eq!(p.markitdown_timeout_secs, 60, "markitdown fallback: 60s ceiling");
    }

    #[test]
    fn test_pipeline_config_split_fetch_timeouts_from_yaml() {
        // YAML keys are kebab-case (rename_all = "kebab-case"); user-supplied
        // values must override the defaults. This locks the public surface
        // for the operator-tunable timeouts.
        let yaml = "\
fabric-url-timeout-secs: 45
fabric-transcript-timeout-secs: 90
markitdown-timeout-secs: 30
";
        let p: PipelineConfig = serde_yaml::from_str(yaml).expect("parse pipeline yaml");
        assert_eq!(p.fabric_url_timeout_secs, 45);
        assert_eq!(p.fabric_transcript_timeout_secs, 90);
        assert_eq!(p.markitdown_timeout_secs, 30);
        // Other defaults are preserved (serde(default) on PipelineConfig).
        assert_eq!(p.hard_timeout_secs, 1800);
        assert_eq!(p.jina_timeout_secs, 60);
    }

    #[test]
    fn test_pipeline_config_split_fetch_timeouts_independent_of_fabric_pattern_timeout() {
        // `fabric.timeout_secs` (LLM pattern completion, currently 600s default)
        // must not be tied to any of the three new subprocess timeouts. This
        // test pins the invariant: changing one cannot change another.
        let p = PipelineConfig::default();
        let f = FabricConfig::default();
        assert_ne!(p.fabric_url_timeout_secs, f.timeout_secs);
        assert_ne!(p.fabric_transcript_timeout_secs, f.timeout_secs);
        assert_ne!(p.markitdown_timeout_secs, f.timeout_secs);
    }
}
