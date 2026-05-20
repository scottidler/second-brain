use clap::{Args, Subcommand};
use eyre::{Context, Result};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::LazyLock;

use cortex::opts;

static AFTER_HELP: LazyLock<String> = LazyLock::new(after_help_text);

fn after_help_text() -> String {
    let fabric_status = check_tool("fabric", &["--version"]);
    let log_path = crate::logger::log_path("cortex");

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

#[derive(Args)]
#[command(after_help = AFTER_HELP.as_str())]
pub struct CortexCli {
    /// Path to config file
    #[arg(short = 'c', long)]
    pub config: Option<PathBuf>,

    /// Vault root directory (default: CWD)
    #[arg(short = 'r', long = "vault")]
    pub vault: Option<PathBuf>,

    /// Log level: trace, debug, info, warn, error
    #[arg(short, long)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Classify inbox notes by domain and promote to notes/
    Classify(ClassifyArgs),
    /// Validate vault against rules
    Lint(LintArgs),
    /// Scan for and create wikilinks
    Link(LinkArgs),
    /// Generate intelligence (daily/weekly notes)
    Intel(IntelArgs),
    /// Vault state fingerprinting
    State(StateArgs),
    /// Watch mode - run actions on change
    Daemon(DaemonArgs),
    /// Schema evolution and vault structure migration
    Migrate(MigrateArgs),
    /// Sweep tags: consolidate to canonical vocabulary
    Sweep(SweepArgs),
    /// Distill legacy notes into the structured L2 contract (backfill)
    Summarize(SummarizeArgs),
    /// Embed note summaries (and Phase B transcripts) into the search DB
    Embed(EmbedArgs),
}

#[derive(Args)]
pub struct ClassifyArgs {
    #[arg(long)]
    pub apply: bool,
    #[arg(long)]
    pub path: Option<String>,
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub review_only: bool,
    #[arg(long)]
    pub reclassify_domain: Option<String>,
}
impl From<ClassifyArgs> for opts::ClassifyOpts {
    fn from(a: ClassifyArgs) -> Self {
        Self {
            apply: a.apply,
            path: a.path,
            force: a.force,
            review_only: a.review_only,
            reclassify_domain: a.reclassify_domain,
        }
    }
}

#[derive(Args)]
pub struct LintArgs {
    #[arg(long)]
    pub apply: bool,
    #[arg(long, default_value = "human")]
    pub format: String,
    #[arg(long)]
    pub rule: Vec<String>,
    #[arg(long)]
    pub path: Option<String>,
}
impl From<LintArgs> for opts::LintOpts {
    fn from(a: LintArgs) -> Self {
        Self {
            apply: a.apply,
            format: a.format,
            rule: a.rule,
            path: a.path,
        }
    }
}

#[derive(Args)]
pub struct LinkArgs {
    #[arg(long)]
    pub apply: bool,
    #[arg(long, default_value = "all")]
    pub scan: String,
}
impl From<LinkArgs> for opts::LinkOpts {
    fn from(a: LinkArgs) -> Self {
        Self {
            apply: a.apply,
            scan: a.scan,
        }
    }
}

#[derive(Args)]
pub struct IntelArgs {
    #[arg(long)]
    pub daily: bool,
    #[arg(long)]
    pub weekly: bool,
    #[arg(long)]
    pub output: Option<PathBuf>,
}
impl From<IntelArgs> for opts::IntelOpts {
    fn from(a: IntelArgs) -> Self {
        Self {
            daily: a.daily,
            weekly: a.weekly,
            output: a.output,
        }
    }
}

#[derive(Args)]
pub struct StateArgs {
    #[arg(long)]
    pub refresh: bool,
    #[arg(long)]
    pub diff: bool,
}
impl From<StateArgs> for opts::StateOpts {
    fn from(a: StateArgs) -> Self {
        Self {
            refresh: a.refresh,
            diff: a.diff,
        }
    }
}

#[derive(Args)]
pub struct DaemonArgs {
    #[arg(long)]
    pub install: bool,
    #[arg(long)]
    pub uninstall: bool,
    #[arg(long)]
    pub start: bool,
    #[arg(long)]
    pub stop: bool,
    #[arg(long)]
    pub status: bool,
}
impl From<DaemonArgs> for opts::DaemonOpts {
    fn from(a: DaemonArgs) -> Self {
        Self {
            install: a.install,
            uninstall: a.uninstall,
            start: a.start,
            stop: a.stop,
            status: a.status,
        }
    }
}

#[derive(Args)]
pub struct MigrateArgs {
    #[arg(long)]
    pub apply: bool,
    #[arg(long)]
    pub plan: Option<PathBuf>,
}
impl From<MigrateArgs> for opts::MigrateOpts {
    fn from(a: MigrateArgs) -> Self {
        Self {
            apply: a.apply,
            plan: a.plan,
        }
    }
}

#[derive(Args)]
pub struct SweepArgs {
    #[arg(long)]
    pub migrate: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub proposals: bool,
    #[arg(long)]
    pub cold: bool,
}
impl From<SweepArgs> for opts::SweepOpts {
    fn from(a: SweepArgs) -> Self {
        Self {
            migrate: a.migrate,
            dry_run: a.dry_run,
            proposals: a.proposals,
            cold: a.cold,
        }
    }
}

#[derive(Args)]
pub struct EmbedArgs {
    #[arg(long)]
    pub backfill: bool,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long, default_value_t = cortex::embed::DEFAULT_BATCH_SIZE)]
    pub batch_size: usize,
    #[arg(long)]
    pub prefetch_model: bool,
    #[arg(long, hide = true)]
    pub use_mock: bool,
}
impl From<EmbedArgs> for opts::EmbedOpts {
    fn from(a: EmbedArgs) -> Self {
        Self {
            backfill: a.backfill,
            kind: a.kind,
            model: a.model,
            batch_size: a.batch_size,
            prefetch_model: a.prefetch_model,
            use_mock: a.use_mock,
        }
    }
}

#[derive(Args)]
pub struct SummarizeArgs {
    #[arg(long)]
    pub backfill: bool,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub domain: Option<String>,
    #[arg(long)]
    pub extractor: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
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
impl From<SummarizeArgs> for opts::SummarizeOpts {
    fn from(a: SummarizeArgs) -> Self {
        Self {
            backfill: a.backfill,
            since: a.since,
            domain: a.domain,
            extractor: a.extractor,
            dry_run: a.dry_run,
            resume: a.resume,
        }
    }
}

impl CortexCli {
    pub async fn run(self) -> Result<()> {
        let config = cortex::config::Config::load(self.config.as_ref()).context("failed to load configuration")?;
        let vault_root = config.vault_root(self.vault.as_ref());
        log::info!("cortex starting (version={})", env!("GIT_DESCRIBE"));
        log::info!("resolved vault root: {}", vault_root.display());

        match self.command {
            Command::Classify(a) => {
                cortex::run_classify(&vault_root, &config, &a.into())?;
            }
            Command::Lint(a) => {
                cortex::run_lint(&vault_root, &config, &a.into())?;
            }
            Command::Link(a) => {
                cortex::run_link(&vault_root, &config, &a.into())?;
            }
            Command::Intel(a) => {
                cortex::run_intel(&vault_root, &config, &a.into())?;
            }
            Command::State(a) => {
                cortex::run_state(&vault_root, &config, &a.into())?;
            }
            Command::Daemon(a) => {
                cortex::daemon::run_daemon(&vault_root, &config, &a.into()).await?;
            }
            Command::Migrate(a) => {
                cortex::run_migrate(&vault_root, &config, &a.into())?;
            }
            Command::Sweep(a) => {
                cortex::run_sweep(&vault_root, &config, &a.into())?;
            }
            Command::Summarize(a) => {
                let summary = cortex::run_summarize(&vault_root, &config, &a.into()).await?;
                log::info!(
                    "summarize complete: attempted={} distilled={} skipped={} failed={}",
                    summary.attempted,
                    summary.distilled,
                    summary.skipped,
                    summary.failed,
                );
            }
            Command::Embed(a) => {
                let stats = cortex::embed::run_embed(&vault_root, &config, &a.into())?;
                log::info!(
                    "embed complete: scanned={} embedded={} skipped_empty={} failed={}",
                    stats.scanned,
                    stats.embedded,
                    stats.skipped_empty,
                    stats.failed,
                );
            }
        }
        Ok(())
    }
}
