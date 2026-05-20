use clap::{Args, Subcommand};
use colored::Colorize;
use eyre::{Context, Result};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::LazyLock;

use borg::opts;

static HELP_TEXT: LazyLock<String> = LazyLock::new(get_tool_validation_help);

#[derive(Args)]
#[command(after_help = HELP_TEXT.as_str())]
pub struct BorgCli {
    /// Path to config file
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Log level: trace, debug, info, warn, error
    #[arg(short, long)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage the daemon (install, start, stop, status, etc.)
    Daemon(DaemonArgs),
    /// Send a URL to the running daemon for ingestion
    Ingest {
        url: Option<String>,
        #[arg(long)]
        clipboard: bool,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(short, long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
        #[arg(long)]
        force: bool,
    },
    /// Quick text capture - create a note from text
    Note {
        text: Option<String>,
        #[arg(long)]
        clipboard: bool,
        #[arg(short, long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
    },
    /// Install/uninstall a keyboard shortcut to ingest URLs from clipboard
    Hotkey(HotkeyArgs),
    /// Sign the browser extension for Firefox (AMO)
    Sign,
    /// Migrate vault frontmatter to current schema
    Migrate {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        apply: bool,
    },
    /// Audit ledger and vault for misclassified or broken entries
    Audit {
        #[arg(long)]
        fix: bool,
        #[arg(long)]
        invariant: bool,
        #[arg(long, default_value_t = 1800)]
        bound_secs: u64,
    },
    /// Inspect the intake log
    Intake(IntakeCliArgs),
    /// Inspect and replay the dead letter queue
    Dlq(DlqCliArgs),
    /// Reingest existing entries through the current pipeline
    Reingest {
        #[arg(long)]
        all: bool,
        #[arg(long, value_name = "TYPE")]
        r#type: Option<String>,
        #[arg(long)]
        domain: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        before: Option<String>,
        #[arg(long)]
        after: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Replay the ingestion pipeline for staged traces or vault notes
    Replay(ReplayCliArgs),
    /// Manage staging retention
    Retention(RetentionCliArgs),
    /// Re-ingest every vault note whose body matches the failed-fetch signature
    ReingestFailed {
        #[arg(long)]
        dry_run: bool,
    },
    /// Manage the Gate-0 domain blocklist
    Blocklist(BlocklistCliArgs),
    /// Backfill the `ingested:` frontmatter field on assisted notes
    BackfillIngested {
        #[arg(long)]
        dry_run: bool,
    },
    /// Manage the Borg Dashboard view
    Dashboard(DashboardCliArgs),
}

#[derive(Args)]
pub struct HotkeyArgs {
    #[arg(long)]
    pub install: bool,
    #[arg(long)]
    pub uninstall: bool,
    #[arg(long, default_value = "localhost")]
    pub host: String,
    #[arg(long, default_value_t = 8181)]
    pub port: u16,
    #[arg(long, default_value = "<Ctrl><Shift>b")]
    pub key: String,
}
impl From<HotkeyArgs> for opts::HotkeyOpts {
    fn from(a: HotkeyArgs) -> Self {
        Self {
            install: a.install,
            uninstall: a.uninstall,
            host: a.host,
            port: a.port,
            key: a.key,
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
    pub reinstall: bool,
    #[arg(long)]
    pub start: bool,
    #[arg(long)]
    pub stop: bool,
    #[arg(long)]
    pub restart: bool,
    #[arg(long)]
    pub status: bool,
}
impl From<DaemonArgs> for opts::DaemonOpts {
    fn from(a: DaemonArgs) -> Self {
        Self {
            install: a.install,
            uninstall: a.uninstall,
            reinstall: a.reinstall,
            start: a.start,
            stop: a.stop,
            restart: a.restart,
            status: a.status,
        }
    }
}

#[derive(Args)]
pub struct DashboardCliArgs {
    #[command(subcommand)]
    pub action: DashboardAction,
}
#[derive(Subcommand)]
pub enum DashboardAction {
    /// Rewrite the dashboard with the current canonical template
    Refresh,
}

#[derive(Args)]
pub struct IntakeCliArgs {
    #[command(subcommand)]
    pub action: IntakeAction,
}
#[derive(Subcommand)]
pub enum IntakeAction {
    /// List recent intake rows
    List {
        #[arg(long)]
        method: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show the intake row + raw-input sidecar for a single trace
    Show { trace_id: String },
}

#[derive(Args)]
pub struct DlqCliArgs {
    #[command(subcommand)]
    pub action: DlqAction,
}
#[derive(Subcommand)]
pub enum DlqAction {
    /// List DLQ rows
    List {
        #[arg(long)]
        method: Option<String>,
        #[arg(long)]
        stage: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show one DLQ row + its intake row + the raw-input sidecar
    Show { trace_id: String },
    /// Mark a DLQ row as resolved
    Archive {
        trace_id: Option<String>,
        #[arg(long, default_value = "resolved")]
        status: String,
        #[arg(long)]
        resolved: bool,
    },
    /// Replay a DLQ entry
    Replay { trace_id: String },
}

#[derive(Args)]
pub struct BlocklistCliArgs {
    #[command(subcommand)]
    pub action: BlocklistAction,
}
#[derive(Subcommand)]
pub enum BlocklistAction {
    /// List every blocklisted domain
    List,
    /// Remove a single domain from the blocklist
    Remove { domain: String },
    /// Remove every entry from the blocklist
    Clear,
}

#[derive(Args)]
pub struct ReplayCliArgs {
    pub trace_id: Option<String>,
    #[arg(long, default_value_t = 0)]
    pub from_stage: u8,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub rejected: bool,
    #[arg(long)]
    pub bootstrap_from_vault: bool,
    #[arg(long)]
    pub note: Option<PathBuf>,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct RetentionCliArgs {
    #[command(subcommand)]
    pub action: RetentionAction,
}
#[derive(Subcommand)]
pub enum RetentionAction {
    /// Sweep aged-off trace directories
    Sweep {
        #[arg(long)]
        dry_run: bool,
    },
    /// Report trace counts and disk usage
    Status,
}

impl BorgCli {
    pub async fn run(self) -> Result<()> {
        let config = borg::config::load_config(self.config.as_ref()).context("Failed to load configuration")?;

        borg::startup::init_permits(&config).context("Failed to initialize pipeline permits")?;
        borg::startup::log_ffmpeg_thread_caps(&config);

        let verbose = false; // root --verbose moved to sb level; not surfaced here

        match self.command {
            None => {
                use clap::CommandFactory;
                let mut cmd = crate::cli::Cli::command();
                cmd.print_help()?;
                println!();
                Ok(())
            }
            Some(Command::Daemon(a)) => {
                let opts: borg::opts::DaemonOpts = a.into();
                if opts.start {
                    let (startup, handle) = borg::serve_init(config).await?;
                    print_server_banner(&startup);
                    handle.wait().await
                } else {
                    let outcome = borg::daemon(config, verbose, opts).await?;
                    for line in &outcome.lines {
                        println!("{line}");
                    }
                    Ok(())
                }
            }
            Some(Command::Ingest {
                url,
                clipboard,
                file,
                tags,
                force,
            }) => {
                let outcome = if let Some(file_path) = file {
                    borg::ingest_file(config, file_path, tags, force).await?
                } else {
                    let resolved_url = borg::resolve_ingest_url(url, clipboard)?;
                    let method = if clipboard {
                        borg::types::IngestMethod::Clipboard
                    } else {
                        borg::types::IngestMethod::Cli
                    };
                    borg::ingest(config, resolved_url, tags, force, clipboard, method).await?
                };
                print_ingest_outcome(&outcome)
            }
            Some(Command::Note { text, clipboard, tags }) => {
                let resolved_text = borg::resolve_note_text(text, clipboard)?;
                let outcome = borg::note(config, resolved_text, tags).await?;
                print_ingest_outcome(&outcome)
            }
            Some(Command::Hotkey(a)) => {
                let outcome = borg::hotkey(a.into(), &config).await?;
                match outcome {
                    borg::HotkeyOutcome::Installed {
                        key,
                        host,
                        port,
                        post_install,
                    } => {
                        if let Some(msg) = post_install {
                            println!("{msg}");
                        } else {
                            println!("Hotkey installed: {key} -> obsidian-borg ingest --clipboard");
                            println!("Daemon target: http://{host}:{port}/ingest (from config)");
                        }
                    }
                    borg::HotkeyOutcome::Uninstalled => {
                        println!("Hotkey uninstalled.");
                    }
                    borg::HotkeyOutcome::NoAction => {
                        eprintln!("No hotkey action specified. See: sb borg hotkey --help");
                    }
                }
                Ok(())
            }
            Some(Command::Sign) => {
                let result = borg::sign(&config).await?;
                println!(
                    "Signing extension v{} in {}",
                    result.version,
                    result.extension_dir.display()
                );
                println!(
                    "Extension signed successfully. Check {}/web-ext-artifacts/",
                    result.extension_dir.display()
                );
                Ok(())
            }
            Some(Command::Migrate { dry_run: _, apply }) => borg::migrate::run(&config, apply).await,
            Some(Command::Audit {
                fix,
                invariant,
                bound_secs,
            }) => {
                if invariant {
                    borg::triage::orphan_audit(&config, bound_secs).await
                } else {
                    borg::audit::run(&config, fix)
                }
            }
            Some(Command::Intake(args)) => match args.action {
                IntakeAction::List { method, since, limit } => {
                    borg::triage::intake_rows(&config, method, since, limit).await
                }
                IntakeAction::Show { trace_id } => borg::triage::intake_row(&config, &trace_id).await,
            },
            Some(Command::Dlq(args)) => match args.action {
                DlqAction::List {
                    method,
                    stage,
                    status,
                    limit,
                } => borg::triage::dlq_rows(&config, method, stage, status, limit).await,
                DlqAction::Show { trace_id } => borg::triage::dlq_row(&config, &trace_id).await,
                DlqAction::Archive {
                    trace_id,
                    status,
                    resolved,
                } => borg::triage::dlq_archive(&config, trace_id, &status, resolved).await,
                DlqAction::Replay { trace_id } => borg::triage::dlq_replay(&config, &trace_id).await,
            },
            Some(Command::Reingest {
                all,
                r#type,
                domain,
                source,
                before,
                after,
                dry_run,
            }) => {
                borg::reingest(
                    config,
                    all,
                    r#type,
                    domain,
                    source,
                    before,
                    after,
                    dry_run,
                    |event| match event {
                        borg::ReingestEvent::NoMatches => println!("No matching entries found."),
                        borg::ReingestEvent::Matched { count, dry_run } => println!(
                            "{} {} entries{}",
                            if *dry_run { "Would reingest" } else { "Reingesting" },
                            count,
                            if *dry_run { " (dry run)" } else { "" }
                        ),
                        borg::ReingestEvent::ItemStart {
                            index,
                            total,
                            date,
                            slug,
                            source,
                        } => println!("  [{}/{}] {} - {} ({})", index + 1, total, date, slug, source),
                        borg::ReingestEvent::ItemReplaced { title } => {
                            println!("    -> Replaced: \"{title}\"")
                        }
                        borg::ReingestEvent::ItemFailed { reason } => {
                            eprintln!("    -> Failed: {reason}")
                        }
                        borg::ReingestEvent::ItemOther(s) => println!("    -> {s}"),
                        borg::ReingestEvent::ItemError(e) => eprintln!("    -> Error: {e}"),
                        borg::ReingestEvent::Complete { dry_run } => {
                            if !*dry_run {
                                println!("Reingest complete.");
                            }
                        }
                    },
                )
                .await
            }
            Some(Command::Replay(args)) => {
                let opts = borg::replay::ReplayOptions {
                    trace_id: args.trace_id,
                    from_stage: args.from_stage,
                    since: args.since,
                    rejected: args.rejected,
                    bootstrap_from_vault: args.bootstrap_from_vault,
                    note: args.note,
                    dry_run: args.dry_run,
                };
                borg::replay::run(config, opts).await
            }
            Some(Command::Retention(args)) => match args.action {
                RetentionAction::Sweep { dry_run } => {
                    let result = borg::retention::sweep(&config, dry_run)?;
                    let action = if dry_run { "Would delete" } else { "Deleted" };
                    println!(
                        "Scanned {} traces, kept {}, {} {} (freed {} bytes)",
                        result.scanned,
                        result.kept,
                        action.to_ascii_lowercase(),
                        result.deleted.len(),
                        result.bytes_freed
                    );
                    for name in &result.deleted {
                        println!("  {action}: {name}");
                    }
                    Ok(())
                }
                RetentionAction::Status => {
                    let report = borg::retention::status(&config)?;
                    println!("Staging root: {}", report.root.display());
                    println!("Traces:       {}", report.traces);
                    println!("Rejected:     {}", report.rejected);
                    println!("Disk usage:   {} bytes", report.total_bytes);
                    Ok(())
                }
            },
            Some(Command::ReingestFailed { dry_run }) => borg::migrate::reingest_failed(&config, dry_run).await,
            Some(Command::BackfillIngested { dry_run }) => borg::backfill::ingested(&config, dry_run),
            Some(Command::Dashboard(args)) => match args.action {
                DashboardAction::Refresh => borg::dashboard::refresh(&borg::dashboard::dashboard_path(&config)),
            },
            Some(Command::Blocklist(args)) => match args.action {
                BlocklistAction::List => {
                    let rows = borg::blocklist::entries()?;
                    if rows.is_empty() {
                        println!("(blocklist empty)");
                    } else {
                        for (domain, entry) in &rows {
                            println!(
                                "{domain:30} retriable-after={} hits={} reason={}",
                                entry.retriable_after, entry.hits, entry.reason
                            );
                        }
                    }
                    Ok(())
                }
                BlocklistAction::Remove { domain } => {
                    let removed = borg::blocklist::remove(&domain)?;
                    if removed {
                        println!("removed: {domain}");
                    } else {
                        println!("not blocklisted: {domain}");
                    }
                    Ok(())
                }
                BlocklistAction::Clear => {
                    borg::blocklist::clear()?;
                    println!("blocklist cleared");
                    Ok(())
                }
            },
        }
    }
}

/// Render the daemon startup banner from the typed snapshot. Lines that
/// previously went to stdout/stderr go through here so the lib stays
/// stdout-clean.
fn print_server_banner(s: &borg::ServerStartup) {
    use borg::SubsystemStatus;

    let arrow = "-->".to_string();
    match &s.telegram_notifier {
        SubsystemStatus::Active => println!("{} telegram notifier active", arrow.green()),
        SubsystemStatus::SkippedNoToken => {
            eprintln!("{} telegram notifier skipped (token not available)", arrow.yellow())
        }
        _ => {}
    }

    println!("{} http server on {}", arrow.green(), s.addr.to_string().cyan());

    match &s.telegram_bot {
        SubsystemStatus::Active => println!("{} telegram bot active", arrow.green()),
        SubsystemStatus::SkippedHostMismatch => {
            eprintln!("{} telegram bot skipped (host mismatch)", arrow.yellow())
        }
        SubsystemStatus::SkippedNoToken => {
            eprintln!("{} telegram bot skipped (token not available)", arrow.yellow())
        }
        _ => {}
    }

    match &s.discord {
        SubsystemStatus::Active => println!("{} discord bot active", arrow.green()),
        SubsystemStatus::SkippedHostMismatch => {
            eprintln!("{} discord bot skipped (host mismatch)", arrow.yellow())
        }
        SubsystemStatus::SkippedNoToken => {
            eprintln!("{} discord bot skipped (token not available)", arrow.yellow())
        }
        _ => {}
    }

    match &s.ntfy {
        SubsystemStatus::ActiveWithDetail(detail) => {
            println!("{} ntfy subscriber active ({})", arrow.green(), detail)
        }
        SubsystemStatus::Active => println!("{} ntfy subscriber active", arrow.green()),
        SubsystemStatus::SkippedHostMismatch => {
            eprintln!("{} ntfy subscriber skipped (host mismatch)", arrow.yellow())
        }
        _ => {}
    }

    if matches!(s.watchdog, SubsystemStatus::Active) {
        println!("{} watchdog active", arrow.green());
    }
}

/// Format and emit a borg `IngestOutcome`. Failed outcomes write to stderr
/// and exit with code 1 to preserve the prior shell contract.
fn print_ingest_outcome(outcome: &borg::IngestOutcome) -> Result<()> {
    match outcome {
        borg::IngestOutcome::Captured { title, path } => {
            println!("Captured: \"{title}\" -> {path}");
        }
        borg::IngestOutcome::Duplicate { original_date } => {
            println!("Duplicate: already ingested on {original_date}");
        }
        borg::IngestOutcome::Queued => {
            println!("Queued for processing.");
        }
        borg::IngestOutcome::Failed { reason } => {
            eprintln!("Error: {reason}");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn get_tool_validation_help() -> String {
    #[allow(clippy::type_complexity)]
    let tools: &[(&str, &str, &str, &[(&str, &str, &str)])] = &[
        ("yt-dlp", "--version", "2023.0.0", &[("ffmpeg", "-version", "")]),
        ("fabric", "--version", "1.0.0", &[]),
        ("markitdown", "--version", "", &[]),
    ];

    struct ToolEntry {
        icon: String,
        name: String,
        version: String,
        prefix: String,
    }
    let mut entries: Vec<ToolEntry> = Vec::new();
    for (tool, version_arg, min_version, deps) in tools {
        let status = check_tool_version(tool, version_arg, min_version);
        entries.push(ToolEntry {
            icon: status.status_icon,
            name: tool.to_string(),
            version: status.version,
            prefix: "  ".to_string(),
        });
        for (i, (dep, dep_ver_arg, dep_min_ver)) in deps.iter().enumerate() {
            let dep_status = check_tool_version(dep, dep_ver_arg, dep_min_ver);
            let connector = if i == deps.len() - 1 { "└──" } else { "├──" };
            entries.push(ToolEntry {
                icon: dep_status.status_icon,
                name: dep.to_string(),
                version: dep_status.version,
                prefix: format!("  {connector} "),
            });
        }
    }

    let max_left_len = entries
        .iter()
        .map(|e| e.prefix.chars().count() + 2 + 1 + e.name.len())
        .max()
        .unwrap_or(0);
    let max_ver_len = entries.iter().map(|e| e.version.len()).max().unwrap_or(0);

    let mut help = String::from("REQUIRED TOOLS:\n");
    for entry in &entries {
        let left_len = entry.prefix.chars().count() + 2 + 1 + entry.name.len();
        let padding = max_left_len - left_len;
        help.push_str(&format!(
            "{}{} {}{}  {:>width$}\n",
            entry.prefix,
            entry.icon,
            entry.name,
            " ".repeat(padding),
            entry.version,
            width = max_ver_len,
        ));
    }

    help.push_str(&format!(
        "\nLogs are written to: {}",
        crate::logger::log_path("borg").display()
    ));
    help
}

struct ToolStatus {
    version: String,
    status_icon: String,
}

fn check_tool_version(tool: &str, version_arg: &str, min_version: &str) -> ToolStatus {
    match ProcessCommand::new(tool).arg(version_arg).output() {
        Ok(output) if output.status.success() => {
            let version_output = String::from_utf8_lossy(&output.stdout);
            let version = extract_version(&version_output);
            let meets_requirement = if min_version.is_empty() {
                true
            } else {
                version_compare(&version, min_version)
            };
            ToolStatus {
                version: if version.is_empty() || version == "unknown" {
                    "installed".to_string()
                } else {
                    version
                },
                status_icon: if meets_requirement { "\u{2705}" } else { "\u{26a0}\u{fe0f}" }.to_string(),
            }
        }
        _ => ToolStatus {
            version: "not found".to_string(),
            status_icon: "\u{274c}".to_string(),
        },
    }
}

fn extract_version(output: &str) -> String {
    if let Some(line) = output.lines().next() {
        for word in line.split_whitespace() {
            let trimmed = word.trim_start_matches('v');
            if trimmed.contains('.') && trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return trimmed.to_string();
            }
        }
        let trimmed = line.trim();
        if trimmed.contains('.') && trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return trimmed.to_string();
        }
    }
    "unknown".to_string()
}

fn version_compare(version: &str, min_version: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.split('.').map(|part| part.parse().unwrap_or(0)).collect() };
    let v1 = parse(version);
    let v2 = parse(min_version);
    for (a, b) in v1.iter().zip(v2.iter()) {
        if a > b {
            return true;
        }
        if a < b {
            return false;
        }
    }
    v1.len() >= v2.len()
}
