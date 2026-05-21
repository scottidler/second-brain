use clap::{Args, Subcommand};
use colored::Colorize;
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
    #[arg(long, value_enum, default_value_t = opts::LintFormat::Human)]
    pub format: opts::LintFormat,
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
    #[arg(long, value_enum, default_value_t = opts::ScanScope::All)]
    pub scan: opts::ScanScope,
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
        let mode = if a.weekly {
            cortex::intel::IntelMode::Weekly
        } else {
            cortex::intel::IntelMode::Daily
        };
        Self { mode, output: a.output }
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
        // Resolve the vault root lazily-ish: status/install/uninstall verbs don't
        // touch the vault, so a missing root_path shouldn't block them.
        let vault_root = if matches!(&self.command, Command::Daemon(_)) {
            config
                .vault_root(self.vault.as_ref())
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
        } else {
            config.vault_root(self.vault.as_ref())?
        };
        log::debug!("cortex starting (version={})", env!("GIT_DESCRIBE"));
        log::debug!("resolved vault root: {}", vault_root.display());

        match self.command {
            Command::Classify(a) => {
                let apply = a.apply;
                let report = cortex::classify::run(&vault_root, &config, &a.into())?;
                if apply {
                    println!("Classified {} note(s).", report.applied);
                    for line in report.format_human(true) {
                        println!("{line}");
                    }
                } else {
                    for line in report.format_human(false) {
                        println!("{line}");
                    }
                }
            }
            Command::Lint(a) => {
                let opts: cortex::opts::LintOpts = a.into();
                let report = cortex::lint(&vault_root, &config, &opts)?;
                if opts.format == cortex::opts::LintFormat::Json {
                    println!("{}", report.format_json()?);
                } else {
                    for line in report.format_human(opts.apply) {
                        println!("{line}");
                    }
                }
            }
            Command::Link(a) => {
                let apply = a.apply;
                let report = cortex::link(&vault_root, &config, &a.into())?;
                if apply {
                    println!("Inserted wikilinks in {} file(s).", report.applied);
                } else {
                    for line in report.format_human(false) {
                        println!("{line}");
                    }
                }
            }
            Command::Intel(a) => {
                let report = cortex::intel::run(&vault_root, &config, &a.into())?;
                print_intel_report(&report);
            }
            Command::State(a) => {
                let report = cortex::state::run(&vault_root, &config, &a.into())?;
                print_state_report(&report);
            }
            Command::Daemon(a) => {
                let outcome = cortex::daemon::run(&vault_root, &config, &a.into()).await?;
                for line in &outcome.lines {
                    println!("{line}");
                }
            }
            Command::Migrate(a) => {
                let apply = a.apply;
                let report = cortex::migrate::run(&vault_root, &config, &a.into())?;
                if apply {
                    println!("Migrated {} file(s).", report.applied);
                } else {
                    for line in report.format_human(false) {
                        println!("{line}");
                    }
                }
            }
            Command::Sweep(a) => {
                let report = cortex::sweep::run(&vault_root, &config, &a.into())?;
                print_sweep_report(&report);
            }
            Command::Summarize(a) => {
                let summary = cortex::summarize::run(&vault_root, &config, &a.into()).await?;
                for line in &summary.would_distill {
                    println!("{line}");
                }
                log::info!(
                    "summarize complete: attempted={} distilled={} skipped={} failed={}",
                    summary.attempted,
                    summary.distilled,
                    summary.skipped,
                    summary.failed,
                );
            }
            Command::Embed(a) => {
                let opts_struct: cortex::opts::EmbedOpts = a.into();
                if opts_struct.prefetch_model {
                    let resolved = cortex::embed::prefetch(opts_struct.model.as_deref())?;
                    println!("Prefetched embedding model {resolved}.");
                } else {
                    let stats = cortex::embed::run(&vault_root, &config, &opts_struct)?;
                    println!(
                        "embed complete: scanned={} embedded={} skipped_empty={} failed={}",
                        stats.scanned, stats.embedded, stats.skipped_empty, stats.failed,
                    );
                }
            }
        }
        Ok(())
    }
}

fn print_state_report(r: &cortex::state::StateReport) {
    if let Some(diff) = &r.diff {
        if diff.has_changes() {
            if !diff.added.is_empty() {
                println!("{}", "Added:".green().bold());
                for p in &diff.added {
                    println!("  + {}", p.display());
                }
            }
            if !diff.removed.is_empty() {
                println!("{}", "Removed:".red().bold());
                for p in &diff.removed {
                    println!("  - {}", p.display());
                }
            }
            if !diff.modified.is_empty() {
                println!("{}", "Modified:".yellow().bold());
                for p in &diff.modified {
                    println!("  ~ {}", p.display());
                }
            }
            println!(
                "\n{}: {} added, {} removed, {} modified",
                "Summary".bold(),
                diff.added.len(),
                diff.removed.len(),
                diff.modified.len()
            );
        } else {
            println!("{}", "No changes since last scan.".green());
        }
    } else if r.diff_requested {
        println!("{}", "No previous manifest found. Run with --refresh first.".yellow());
    }

    if let Some(count) = r.refreshed_count {
        println!("{} manifest saved ({} files)", "Refreshed:".green().bold(), count);
    }

    if r.diff.is_none() && r.refreshed_count.is_none() && !r.diff_requested {
        match &r.current {
            Some(snap) => {
                println!(
                    "Last scan: {} ({} files)",
                    snap.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
                    snap.file_count
                );
            }
            None => {
                println!(
                    "{}",
                    "No manifest found. Run `sb cortex state --refresh` to create one.".yellow()
                );
            }
        }
    }
}

fn print_sweep_report(r: &cortex::sweep::SweepReport) {
    use cortex::sweep::SweepMode;
    match &r.mode {
        SweepMode::Cold {
            scanned,
            surfaced,
            pinned_excluded,
        } => {
            println!("Cold sweep: scanned={scanned} surfaced={surfaced} pinned_excluded={pinned_excluded}");
            return;
        }
        SweepMode::WouldMigrate { count } => {
            println!("Dry run: would modify {count} note(s).");
        }
        SweepMode::Migrated { count } => {
            println!("Migrated tags in {count} note(s).");
        }
        SweepMode::Proposals => {}
    }
    if let Some(proposals) = &r.proposals {
        if proposals.is_empty() {
            println!("No new tag proposals.");
        } else {
            println!("Found {} tag(s) needing review:", proposals.len());
            for proposal in proposals {
                println!("  {} (on {} notes)", proposal.tag, proposal.frequency);
            }
            if let Some(path) = &r.proposals_path {
                println!("Proposals written to {path}");
            }
        }
    }
}

fn print_intel_report(r: &cortex::intel::IntelReport) {
    let label = match r.mode {
        cortex::intel::IntelMode::Daily => "daily digest",
        cortex::intel::IntelMode::Weekly => "weekly review",
    };
    println!("Generated {label}: {}", r.output_path.display());
}

#[cfg(test)]
mod tests;
