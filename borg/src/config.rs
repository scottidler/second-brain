pub use vault::config::resolve_secret;

use eyre::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const APP_NAME: &str = "borg";

/// Load configuration with fallback chain:
/// 1. Explicit path (if provided)
/// 2. ~/.config/sb/borg.yml
/// 3. ./borg.yml
/// 4. Default
pub fn load_config<T: DeserializeOwned + Default>(config_path: Option<&PathBuf>) -> Result<T> {
    if let Some(path) = config_path {
        return load_from_file(path).context(format!("Failed to load config from {}", path.display()));
    }

    let primary_config = vault::paths::borg_config();
    if primary_config.exists() {
        match load_from_file(&primary_config) {
            Ok(config) => return Ok(config),
            Err(e) => {
                log::warn!("Failed to load config from {}: {}", primary_config.display(), e);
            }
        }
    }

    let fallback_config = PathBuf::from(format!("{APP_NAME}.yml"));
    if fallback_config.exists() {
        match load_from_file(&fallback_config) {
            Ok(config) => return Ok(config),
            Err(e) => {
                log::warn!("Failed to load config from {}: {}", fallback_config.display(), e);
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
    pub signal: Option<SignalConfig>,
    pub desktop: Option<DesktopConfig>,
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
    pub extension: ExtensionConfig,
    #[serde(default)]
    pub pipeline: PipelineConfig,
    pub log_level: Option<String>,
}

/// Browser-extension lifecycle settings. The only field today is
/// `origin-patterns`, an optional explicit list of match-pattern hosts the
/// extension is permitted to talk to. When `None` (default), the extension
/// manifest generator merges `DEFAULT_ORIGIN_PATTERNS` (localhost, *.lan,
/// *.local) with `server.host`. Users running borg on Tailscale, ZeroTier, a
/// VPS, or any host not covered by the defaults set this list explicitly.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ExtensionConfig {
    pub origin_patterns: Option<Vec<String>>,
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
    #[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]
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
            .expect("dirs::data_local_dir() returned None (set HOME or XDG_DATA_HOME)")
            .join("sb")
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

impl FrontmatterConfig {
    /// Parse the configured `timezone`, falling back to `America/Los_Angeles`.
    /// The single shared resolver for what used to be seven copy-pasted
    /// `timezone.parse().unwrap_or(LA)` blocks scattered across the pipeline
    /// handlers; an unparseable value WARNs (once at config load via
    /// [`Config::validate`], then silently falls back at the call sites).
    pub fn timezone_tz(&self) -> chrono_tz::Tz {
        self.timezone.parse().unwrap_or(chrono_tz::America::Los_Angeles)
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

/// Signal transport configuration. The presence of this section enables Signal
/// ingest (mirrors `telegram`, `discord`, `ntfy`). `host` is mandatory because
/// Signal-Server fans out Note-to-Self envelopes to every linked device and has
/// no polling-lock equivalent to Telegram's `TerminatedByOtherGetUpdates` -
/// leaving `host` unset on a multi-machine install would silently double-ingest.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SignalConfig {
    /// ACI UUIDs (string form) allowed to send borg peer DMs.
    /// Note-to-Self is structural and never gated by this list.
    #[serde(default)]
    pub allowed_senders: Vec<String>,

    /// Default reply target for cross-method notifications (e.g. an ntfy
    /// ingest acknowledged via Signal). `None` = `SelfSync`; `Some(<ACI UUID>)`
    /// = peer.
    #[serde(default)]
    pub notification_recipient: Option<String>,

    /// Pin Signal ingest to a specific hostname. Mandatory when the `signal:`
    /// block is present; config-load fails if missing or empty.
    pub host: String,

    /// Maximum accepted Note-to-Self envelopes per hour before the rate gate
    /// trips and pauses ingest until the daemon is restarted. Backstops an
    /// upstream `signal-rs` regression in the wire-ACI to `Recipient::SelfSync`
    /// mapping. Peer DMs are not counted.
    #[serde(default = "default_signal_rate_threshold")]
    pub notetoself_rate_threshold_per_hour: u32,
}

fn default_signal_rate_threshold() -> u32 {
    100
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

/// Config for the desktop notification sink (a sibling of the Telegram sink).
/// The sink shells out to the user session D-Bus via `notify-rust`. Default
/// `enabled: false` keeps headless borg hosts silent; new machines pick up
/// `enabled: true` via the `sb bootstrap` template.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DesktopConfig {
    /// If false, no DesktopNotifier is constructed and the daemon stays silent
    /// on the desktop. Telegram is unaffected.
    pub enabled: bool,
    /// If set, only construct the notifier on the host with this hostname.
    /// Mirrors the telegram/discord/ntfy host gating so a headless host does
    /// not fight a non-existent D-Bus session.
    pub host: Option<String>,
    /// Toast lifetime hint passed to the notification daemon, in milliseconds.
    pub timeout_ms: u32,
    /// Application name shown by the notification daemon.
    pub appname: String,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: None,
            timeout_ms: 5000,
            appname: APP_NAME.to_string(),
        }
    }
}

/// Resolve the write-route auth token for a FIRST-PARTY client (reingest /
/// replay / hotkey ingest) to send as a `Bearer` header, mirroring how the
/// server resolves `server.auth-token`. Returns `None` when no token is
/// configured; logs a warning and returns `None` if a reference is set but
/// unresolvable (the request then 401s, surfacing the misconfiguration loudly
/// instead of silently). Without this, enabling `server.auth-token` 401s the
/// CLI hot path.
pub fn resolve_client_auth_token(server: &ServerConfig) -> Option<String> {
    let reference = server.auth_token.as_ref()?;
    match resolve_secret(reference) {
        Ok(t) if !t.is_empty() => Some(t),
        Ok(_) => {
            log::warn!("server.auth-token {reference:?} resolved empty; client will send no token");
            None
        }
        Err(e) => {
            log::warn!("server.auth-token {reference:?} not resolvable for client auth: {e}");
            None
        }
    }
}

/// Check whether a service should run on this host.
/// Returns true if `host` is None/empty (run everywhere) or matches the current hostname.
pub fn is_local_host(host: &Option<String>) -> bool {
    let current = match hostname::get() {
        Ok(c) => Some(c.to_string_lossy().into_owned()),
        Err(e) => {
            log::error!("is_local_host: could not read hostname ({e}); failing closed for any host pin");
            None
        }
    };
    host_matches(host, current.as_deref())
}

/// Pure matcher for [`is_local_host`]. `current` is the resolved hostname, or
/// `None` when it could not be read. Fails CLOSED on an unreadable hostname
/// when a pin is set: a host pin exists to keep a service (e.g. Signal ingest)
/// on exactly one machine, so "I don't know my hostname" must mean "don't
/// run" (NOT "run anyway", which would let an unpinned second machine
/// double-ingest). No pin (None/empty) still runs everywhere.
fn host_matches(host: &Option<String>, current: Option<&str>) -> bool {
    match host {
        None => true,
        Some(h) if h.is_empty() => true,
        Some(h) => match current {
            Some(c) => c.eq_ignore_ascii_case(h),
            None => false,
        },
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// Optional auth token for the HTTP write routes (`/ingest`,
    /// `/ingest/file`, `/note`). This holds a **secret reference** - an
    /// env-var name or a file path - NOT a literal token, mirroring
    /// `telegram.bot-token` and `ntfy.token`. It is resolved at startup via
    /// `vault::config::resolve_secret`. When set, write routes require a
    /// matching `Authorization: Bearer <token>` header. When `None` (the
    /// default), the routes are unauthenticated and behavior is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct VaultConfig {
    /// Vault root. None means the runtime requires either `--vault` on the CLI or
    /// a `.obsidian/` directory in CWD. See `vault::paths::resolve_vault_root`.
    #[serde(default)]
    pub root_path: Option<String>,
    /// Inbox directory. `None` means `<vault_root>/inbox`. A literal value
    /// must be a real path; `~/...` is tilde-expanded at use time via
    /// `Config::inbox_dir`.
    pub inbox_path: Option<String>,
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
            canonical_path: vault::paths::canonical_tags().display().to_string(),
            mapping_path: vault::paths::tag_mapping().display().to_string(),
            reject_concatenated: true,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8181,
            auth_token: None,
        }
    }
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            root_path: None,
            inbox_path: None,
            vault_name: "obsidian".to_string(),
        }
    }
}

impl Config {
    /// Resolve the vault root via the unified resolver. Borg has no CLI override
    /// for the vault path - the daemon takes its vault from config, or
    /// (in marker-gated CWD mode) from a `.obsidian/` directory in CWD.
    pub fn vault_root(&self) -> Result<PathBuf> {
        vault::paths::resolve_vault_root(None, self.vault.root_path.as_deref())
    }

    /// Inbox directory for fresh notes. Explicit `vault.inbox-path` wins
    /// (tilde-expanded); otherwise computed as `<vault_root>/inbox` so the
    /// inbox always tracks the resolved vault — no hardcoded fallback that
    /// can leak writes into a phantom directory.
    pub fn inbox_dir(&self) -> Result<PathBuf> {
        if let Some(s) = self.vault.inbox_path.as_deref() {
            Ok(vault::paths::expand_tilde(s))
        } else {
            Ok(self.vault_root()?.join("inbox"))
        }
    }

    /// Validate cross-cutting invariants on a freshly-loaded Config. Today
    /// this checks Signal's mandatory `host` field; future invariants land
    /// here. Call once at load time from the daemon entry; the doctor
    /// command intentionally skips validation so it can report findings on
    /// misconfigured sections instead of refusing to run.
    pub fn validate(&self) -> Result<()> {
        if let Some(signal) = &self.signal
            && signal.host.trim().is_empty()
        {
            eyre::bail!(
                "signal.host is required when the `signal:` config section is present; \
                 set it to the exact hostname (output of `hostname`) of the machine that should run Signal ingest"
            );
        }
        // Validate the timezone once, at load, so an unparseable value WARNs
        // here instead of silently falling back to LA at every pipeline site.
        if self.frontmatter.timezone.parse::<chrono_tz::Tz>().is_err() {
            log::warn!(
                "frontmatter.timezone '{}' is not a valid IANA zone; falling back to America/Los_Angeles",
                self.frontmatter.timezone
            );
        }
        Ok(())
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
mod tests;
