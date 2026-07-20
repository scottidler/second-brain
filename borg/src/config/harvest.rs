use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Absolute, tilde-expanded default location for the clyde binary. The
/// harvest systemd timer (Phase 8) runs with a stripped PATH, so a bare
/// `"clyde"` would not resolve there - only an absolute path survives that
/// environment. Expanded here directly (not through serde) because
/// `Default::default()` never runs the deserializer; mirrors
/// `StagingConfig::default`'s call to `vault::paths::borg_stages_dir()`.
const DEFAULT_CLYDE_BINARY: &str = "~/.cargo/bin/clyde";

/// First-run backfill bound (design doc: Architecture > Watermark). When no
/// watermark cursor exists yet, `sb borg harvest` scans back this far via
/// `clyde session export --since <this>` instead of inhaling the whole
/// catalog unasked on a fresh install. A human-time span in the same shape
/// `borg::receipts::parse_since` already accepts (`7d`, `24h`, `2w`); Phase 3's
/// export reader owns parsing it.
const DEFAULT_INITIAL_SINCE: &str = "7d";

/// Inter-session gap that splits two sessions sharing `(cwd, git-branch)`
/// into separate threads instead of merging them into one note (design doc:
/// Selection > Thread boundary rules). A human-time span, same shape as
/// `initial-since`.
const DEFAULT_THREAD_WINDOW: &str = "2h";

/// Selection floor on `n-msgs` (design doc: Selection signals): a session
/// with fewer messages than this is a one-shot, not substantive enough to
/// earn a note. Tuned (Phase 3) against the real 2026-07-02 catalog slice:
/// one-shots cluster at <=3 messages (the canonical `"what"` reject is 3),
/// while every substantive engineering thread is >=29, so a floor of 6 sits
/// well inside that empty gap with margin. (Phase 2's starter value of 4 also
/// separated the fixtures, but left less headroom against 4-5 message
/// near-one-shots the "not a one-shot" intent excludes.)
const DEFAULT_MIN_MSGS: usize = 6;

/// Head+tail windowing cap in tokens fed to the distiller per thread (design
/// doc: Distillation > Input), independent of the selection thresholds above.
const DEFAULT_TOKEN_CAP: usize = 12_000;

/// Nightly harvest, off-peak (design doc: not real-time, nightly reflection).
const DEFAULT_SCHEDULE: &str = "*-*-* 03:00:00";

/// What the nightly timer runs (design doc: API Design > Config). `DryRun`
/// lists selections/rejections and writes nothing; `Live` publishes notes.
/// Defaults to `DryRun` so a fresh install never lands notes unattended
/// before the first-week soak (design doc: Rollout Plan) - flipping to
/// `Live` is a deliberate config edit, never the out-of-the-box behavior.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HarvestMode {
    #[default]
    DryRun,
    Live,
}

impl HarvestMode {
    /// Resolve whether a `sb borg harvest` invocation runs dry (writes nothing)
    /// or live, from the two mutually-exclusive CLI overrides and this config
    /// default. Precedence, fail-safe first: an explicit `--dry-run` always
    /// wins (never publish when the operator asked not to), then an explicit
    /// `--live` overrides the config default, then the config `mode` decides
    /// (`DryRun` out of the box, per Rollout Plan). `--dry-run`/`--live` are
    /// clap `conflicts_with`, so both-true never reaches here; if it ever did,
    /// dry-run still wins.
    pub fn resolve_dry_run(self, cli_dry_run: bool, cli_live: bool) -> bool {
        if cli_dry_run {
            return true;
        }
        if cli_live {
            return false;
        }
        matches!(self, HarvestMode::DryRun)
    }
}

/// Config for `sb borg harvest`
/// (design doc: `docs/design/2026-07-17-harvest-clyde-sessions.md`). Every
/// tunable the harvest loop needs lives here so the timer unit (Phase 8)
/// bakes in nothing but `OnCalendar` - the one value that IS the timer.
///
/// Omitting the whole `harvest:` section (or any individual key within it)
/// keeps these defaults, mirroring the `distill:`/`pipeline:` pattern: the
/// container-level `#[serde(default)]` fills any missing field from this
/// struct's hand-written `Default` impl, field by field.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct HarvestConfig {
    /// Absolute path to the clyde binary. Tilde-expanded at load
    /// (`vault::paths::deserialize_tilde_pathbuf`) because the timer's
    /// stripped PATH (Phase 8) means a bare `"clyde"` cannot resolve.
    #[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]
    pub clyde_binary: PathBuf,
    /// First-run backfill bound: how far back to scan when no watermark
    /// cursor exists yet (`clyde session export --since <this>`). A
    /// deliberate deep backfill is `sb borg harvest --since 90d`, which
    /// overrides this on a per-invocation basis.
    pub initial_since: String,
    /// What the nightly timer runs. On-demand `sb borg harvest` runs still
    /// honor an explicit `--dry-run` flag regardless of this setting.
    pub mode: HarvestMode,
    /// Selection floor: a session with fewer than this many messages is a
    /// one-shot, not worth a note.
    pub min_msgs: usize,
    /// Regex patterns matched against a session's title/first-prompt; a
    /// match excludes the candidate before scoring (e.g. auto-fired security
    /// reviews, bare "sure"/empty prompts, navigational lookups). A plain
    /// YAML list - never comma-delimited (`rules/cli.md`).
    pub exclude_patterns: Vec<String>,
    /// Inter-session gap that splits two same-`(cwd, git-branch)` sessions
    /// into separate threads. A human-time span (`2h`, `90m`).
    pub thread_window: String,
    /// Hard cap on tokens fed to the distiller per thread (head+tail
    /// windowing for very long threads).
    pub token_cap: usize,
    /// Model override for the harvest distillation pass. Empty string (the
    /// default) inherits `llm.model`, mirroring `vision.model` and
    /// `youtube.slides.content_filter.model`.
    pub model: String,
    /// systemd `OnCalendar` expression for the nightly timer (Phase 8). This
    /// is the ONE value baked into the `.timer` unit - every behavioral
    /// tunable stays in this config, read by the service's `sb borg harvest`
    /// ExecStart at fire time. A standard systemd calendar spec (`daily`,
    /// `*-*-* 03:00:00`, `Mon *-*-* 06:00:00`).
    pub schedule: String,
}

impl Default for HarvestConfig {
    fn default() -> Self {
        Self {
            clyde_binary: vault::paths::expand_tilde(DEFAULT_CLYDE_BINARY),
            initial_since: DEFAULT_INITIAL_SINCE.to_string(),
            mode: HarvestMode::default(),
            min_msgs: DEFAULT_MIN_MSGS,
            exclude_patterns: Vec::new(),
            thread_window: DEFAULT_THREAD_WINDOW.to_string(),
            token_cap: DEFAULT_TOKEN_CAP,
            model: String::new(),
            schedule: DEFAULT_SCHEDULE.to_string(),
        }
    }
}
