use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

/// Generate the after_help text with tool dependency checks and log path.
pub fn after_help_text() -> String {
    let fabric_status = check_tool("fabric", &["--version"]);
    let log_path = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("cortex")
        .join("logs")
        .join("cortex.log");

    format!(
        "REQUIRED TOOLS:\n{fabric_status}\n\nLogs are written to: {log_path}",
        log_path = log_path.display()
    )
}

fn check_tool(name: &str, version_args: &[&str]) -> String {
    match ProcessCommand::new(name).args(version_args).output() {
        Ok(output) if output.status.success() => {
            let ver = String::from_utf8_lossy(&output.stdout)
                .trim()
                .lines()
                .next()
                .unwrap_or("unknown")
                .to_string();
            format!("  \u{2705} {name:<10} {ver}")
        }
        _ => format!("  \u{274c} {name:<10} NOT FOUND"),
    }
}

#[derive(Parser)]
#[command(
    name = "cortex",
    about = "Vault governance and intelligence companion for Obsidian",
    version = env!("GIT_DESCRIBE"),
)]
pub struct Cli {
    /// Path to config file
    #[arg(short = 'c', long)]
    pub config: Option<PathBuf>,

    /// Vault root directory (default: CWD)
    #[arg(short = 'r', long = "vault")]
    pub vault: Option<PathBuf>,

    /// Enable verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// Log level: trace, debug, info, warn, error
    /// Resolution: --log-level > LOG_LEVEL env > config > info
    #[arg(short, long)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Classify inbox notes by domain and promote to notes/
    Classify(crate::classify::ClassifyOpts),
    /// Validate vault against rules
    Lint(LintOpts),
    /// Scan for and create wikilinks
    Link(LinkOpts),
    /// Generate intelligence (daily/weekly notes)
    Intel(IntelOpts),
    /// Vault state fingerprinting
    State(StateOpts),
    /// Watch mode - run actions on change
    Daemon(DaemonOpts),
    /// Schema evolution and vault structure migration
    Migrate(MigrateOpts),
    /// Sweep tags: consolidate to canonical vocabulary
    Sweep(SweepOpts),
    /// Distill legacy notes into the structured L2 contract (backfill)
    Summarize(SummarizeOpts),
    /// Embed note summaries (and Phase B transcripts) into the search DB
    Embed(EmbedOpts),
}

#[derive(Parser)]
pub struct LintOpts {
    /// Auto-fix what's fixable (default: report only)
    #[arg(long)]
    pub apply: bool,

    /// Output format: human (default), json
    #[arg(long, default_value = "human")]
    pub format: String,

    /// Run only specific rule(s): naming, frontmatter, tags, scope, broken-links, duplicates, quality, auto-tag
    #[arg(long)]
    pub rule: Vec<String>,

    /// Lint only files matching glob pattern
    #[arg(long)]
    pub path: Option<String>,
}

#[derive(Parser)]
pub struct LinkOpts {
    /// Insert wikilinks into notes (default: report only)
    #[arg(long)]
    pub apply: bool,

    /// What to scan for: people, projects, concepts, all (default)
    #[arg(long, default_value = "all")]
    pub scan: String,
}

#[derive(Parser)]
pub struct IntelOpts {
    /// Generate today's daily digest
    #[arg(long)]
    pub daily: bool,

    /// Generate weekly review
    #[arg(long)]
    pub weekly: bool,

    /// Write to specific path (default: vault daily note)
    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Parser)]
pub struct StateOpts {
    /// Recompute and cache vault manifest
    #[arg(long)]
    pub refresh: bool,

    /// Show what changed since last cached manifest
    #[arg(long)]
    pub diff: bool,
}

#[derive(Parser)]
pub struct DaemonOpts {
    /// Install systemd user service
    #[arg(long)]
    pub install: bool,

    /// Remove systemd user service
    #[arg(long)]
    pub uninstall: bool,

    /// Start watching (used by systemd ExecStart)
    #[arg(long)]
    pub start: bool,

    /// Stop watching
    #[arg(long)]
    pub stop: bool,

    /// Show daemon status
    #[arg(long)]
    pub status: bool,
}

#[derive(Parser)]
pub struct MigrateOpts {
    /// Apply migrations (default: report only)
    #[arg(long)]
    pub apply: bool,

    /// Path to migration plan YAML
    #[arg(long)]
    pub plan: Option<PathBuf>,
}

#[derive(Parser)]
pub struct SweepOpts {
    /// Run full tag migration (rewrite all notes to canonical tags)
    #[arg(long)]
    pub migrate: bool,

    /// Preview changes without modifying files
    #[arg(long)]
    pub dry_run: bool,

    /// Scan for non-canonical tags and generate proposals
    #[arg(long)]
    pub proposals: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct EmbedOpts {
    /// One-shot pass over every note that is missing or stale. Alias
    /// for the default behavior; explicit for the first run on a new
    /// install.
    #[arg(long)]
    pub backfill: bool,

    /// Restrict the pass to a single embedding kind. Accepts
    /// `summary` (Phase A default) or `transcript-chunk` (Phase B).
    #[arg(long)]
    pub kind: Option<String>,

    /// Override the active model. Omit to use whatever
    /// `embedding_config.active_model` holds in the DB.
    #[arg(long)]
    pub model: Option<String>,

    /// Tune memory vs throughput. The transaction-discipline test
    /// asserts the write transaction wall-clock stays under 200 ms at
    /// the default of 64.
    #[arg(long, default_value_t = crate::embed::DEFAULT_BATCH_SIZE)]
    pub batch_size: usize,

    /// Download the model weights to the fastembed cache and exit
    /// without embedding anything. Use this on install machines that
    /// have network during install but may be offline at oracle's
    /// first-query time.
    #[arg(long)]
    pub prefetch_model: bool,

    /// Use the deterministic MockEmbedder. Test-only.
    #[arg(long, hide = true)]
    pub use_mock: bool,
}

#[derive(Parser)]
pub struct SummarizeOpts {
    /// Backfill legacy notes into the structured L2 (`Distilled`) contract.
    /// Required: this is the only mode `cortex summarize` ships today.
    #[arg(long)]
    pub backfill: bool,

    /// Only re-distill notes whose `date:` is within the last <duration>.
    /// Accepts a number suffixed with `d` / `w` / `mo` (e.g. `30d`, `2w`,
    /// `3mo`). Omit to scan the entire vault.
    #[arg(long)]
    pub since: Option<String>,

    /// Only re-distill notes whose `domain:` frontmatter matches <name>.
    #[arg(long)]
    pub domain: Option<String>,

    /// Force re-distill against a specific extractor id (e.g.
    /// `distill-article-v2`), bypassing the `distilled: true` skip-guard.
    /// Use this to regenerate notes against a newer pattern version.
    #[arg(long)]
    pub extractor: Option<String>,

    /// Print what would be rewritten without touching any files.
    #[arg(long)]
    pub dry_run: bool,

    /// Resume from the saved checkpoint (default: true). Pass
    /// `--resume false` (or `--resume=false`) to start a fresh pass.
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        value_name = "BOOL",
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    pub resume: bool,
}
