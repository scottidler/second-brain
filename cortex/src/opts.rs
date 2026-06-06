use std::path::PathBuf;

/// CLI options for the classify command
#[derive(Debug, Clone)]
pub struct ClassifyOpts {
    /// Move notes (default: dry-run showing planned moves)
    pub apply: bool,

    /// Process specific files
    pub path: Option<String>,

    /// Reclassify notes that already have cortex-classified: true
    pub force: bool,

    /// Only process notes with cortex-needs-review: true
    pub review_only: bool,

    /// Reclassify all notes with this domain (e.g., --reclassify-domain resources)
    pub reclassify_domain: Option<String>,
}

/// Output shape for `cortex lint`. Clap rejects unknown values at parse
/// time (e.g. `--format yaml` errors with "possible values: human, json").
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LintFormat {
    Human,
    Json,
}

#[derive(Debug, Clone)]
pub struct LintOpts {
    /// Auto-fix what's fixable (default: report only)
    pub apply: bool,

    /// Output format: human (default), json
    pub format: LintFormat,

    /// Run only specific rule(s): naming, frontmatter, tags, scope, broken-links, duplicates, quality, auto-tag
    pub rule: Vec<String>,

    /// Lint only files matching glob pattern
    pub path: Option<String>,
}

/// What `cortex link` should scan for. `All` means "use whatever the config's
/// `actions.linking.scan-for` says"; the other variants override the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ScanScope {
    People,
    Projects,
    Concepts,
    Metadata,
    All,
}

impl ScanScope {
    /// Translate the enum into the strings the linker reads out of
    /// `config.actions.linking.scan_for` (matches the legacy contract:
    /// the linker checks `contains("people")` / `contains("projects")` /
    /// `contains("concepts")` / `contains("all")`).
    ///
    /// `Metadata` is accepted for symmetry but produces no wikilink fixes:
    /// creator/source/domain relationships are materialized as graph `edges`
    /// by `sb cortex graph`, never written into note frontmatter (the design's
    /// "no metadata linking into frontmatter" rule). The linker has no
    /// `metadata` branch, so this scope is a deliberate no-op for `cortex link`.
    pub fn as_config_scan_for(self) -> Vec<String> {
        match self {
            ScanScope::People => vec!["people".to_string()],
            ScanScope::Projects => vec!["projects".to_string()],
            ScanScope::Concepts => vec!["concepts".to_string()],
            ScanScope::Metadata => vec!["metadata".to_string()],
            ScanScope::All => vec!["all".to_string()],
        }
    }
}

#[derive(Debug, Clone)]
pub struct LinkOpts {
    /// Insert wikilinks into notes (default: report only)
    pub apply: bool,

    /// What to scan for: people, projects, concepts, all (default).
    /// `All` means "fall through to the config value".
    pub scan: ScanScope,
}

#[derive(Debug, Clone)]
pub struct IntelOpts {
    /// Which artifact to generate.
    pub mode: crate::intel::IntelMode,

    /// Write to specific path (default: vault daily note).
    pub output: Option<PathBuf>,

    /// Treat this date as "today" instead of the system clock. Lets a past
    /// day's digest be regenerated (backfill). `None` = real today.
    pub as_of: Option<chrono::NaiveDate>,
}

#[derive(Debug, Clone)]
pub struct StateOpts {
    /// Recompute and cache vault manifest
    pub refresh: bool,

    /// Show what changed since last cached manifest
    pub diff: bool,
}

#[derive(Debug, Clone)]
pub struct DaemonOpts {
    /// Install systemd user service
    pub install: bool,

    /// Remove systemd user service
    pub uninstall: bool,

    /// Start watching (used by systemd ExecStart)
    pub start: bool,

    /// Stop watching
    pub stop: bool,

    /// Show daemon status
    pub status: bool,
}

#[derive(Debug, Clone)]
pub struct MigrateOpts {
    /// Apply migrations (default: report only)
    pub apply: bool,

    /// Path to migration plan YAML
    pub plan: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SweepOpts {
    /// Run full tag migration (rewrite all notes to canonical tags)
    pub migrate: bool,

    /// Preview changes without modifying files
    pub dry_run: bool,

    /// Scan for non-canonical tags and generate proposals
    pub proposals: bool,

    /// Produce the cold-note review report at
    /// `system/views/cold-notes.md`. Reads the materialized signals;
    /// does not modify any note files. Mutually exclusive with
    /// `--migrate` and `--proposals`.
    pub cold: bool,
}

#[derive(Debug, Clone)]
pub struct EmbedOpts {
    /// One-shot pass over every note that is missing or stale. Alias
    /// for the default behavior; explicit for the first run on a new
    /// install.
    pub backfill: bool,

    /// Restrict the pass to a single embedding kind. Accepts
    /// `summary` (Phase A default) or `transcript-chunk` (Phase B).
    pub kind: Option<String>,

    /// Override the active model. Omit to use whatever
    /// `embedding_config.active_model` holds in the DB.
    pub model: Option<String>,

    /// Tune memory vs throughput. The transaction-discipline test
    /// asserts the write transaction wall-clock stays under 200 ms at
    /// the default of 64.
    pub batch_size: usize,

    /// Download the model weights to the fastembed cache and exit
    /// without embedding anything. Use this on install machines that
    /// have network during install but may be offline at oracle's
    /// first-query time.
    pub prefetch_model: bool,

    /// Use the deterministic MockEmbedder. Test-only.
    pub use_mock: bool,
}

#[derive(Debug, Clone)]
pub struct GraphOpts {
    /// Force a full rebuild of the `edges` table (clear-then-rebuild every
    /// note), bypassing the incremental watermarks. Implied on the first run
    /// after a daemon restart (no persisted `last_run_at`).
    pub backfill: bool,
}

#[derive(Debug, Clone)]
pub struct SummarizeOpts {
    /// Backfill legacy notes into the structured L2 (`Distilled`) contract.
    /// Required: this is the only mode `cortex summarize` ships today.
    pub backfill: bool,

    /// Only re-distill notes whose `date:` is within the last <duration>.
    /// Accepts a number suffixed with `d` / `w` / `mo` (e.g. `30d`, `2w`,
    /// `3mo`). Omit to scan the entire vault.
    pub since: Option<String>,

    /// Only re-distill notes whose `domain:` frontmatter matches <name>.
    pub domain: Option<String>,

    /// Force re-distill against a specific extractor id (e.g.
    /// `distill-article-v2`), bypassing the `distilled: true` skip-guard.
    /// Use this to regenerate notes against a newer pattern version.
    pub extractor: Option<String>,

    /// Print what would be rewritten without touching any files.
    pub dry_run: bool,

    /// Resume from the saved checkpoint (default: true). Pass
    /// `--resume false` (or `--resume=false`) to start a fresh pass.
    pub resume: bool,
}
