use clap::{Args, Subcommand};
use colored::Colorize;
use eyre::{Context, Result};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::LazyLock;

use borg::opts;

pub mod extension;

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
        #[arg(short, long, num_args = 0..)]
        tags: Option<Vec<String>>,
        #[arg(long)]
        force: bool,
    },
    /// Quick text capture - create a note from text
    Note {
        text: Option<String>,
        #[arg(long)]
        clipboard: bool,
        #[arg(short, long, num_args = 0..)]
        tags: Option<Vec<String>>,
    },
    /// Install/uninstall a keyboard shortcut to ingest URLs from clipboard
    Hotkey(HotkeyArgs),
    /// Manage the Firefox browser extension (generate, validate, sign, install)
    Extension(extension::ExtensionCli),
    /// Migrate vault frontmatter to current schema. Without `--apply` this is a
    /// dry run (reports what would change); `--apply` is the gate that writes.
    Migrate {
        #[arg(long)]
        apply: bool,
    },
    /// Audit ledger and vault for misclassified or broken entries
    Audit {
        /// Apply fixes. With no value, fixes every class. With one or more
        /// kinds (space-separated), fixes only those classes. Case-insensitive.
        #[arg(long, num_args = 0.., value_name = "KINDS", ignore_case = true)]
        fix: Option<Vec<borg::audit::FindingKind>>,
    },
    /// Query the receipts log (durable record of every input borg ever saw)
    Log(LogCliArgs),
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
}

#[derive(Args)]
pub struct HotkeyArgs {
    /// Install the desktop hotkey that ingests the clipboard URL
    #[arg(long)]
    pub install: bool,
    /// Remove the installed hotkey
    #[arg(long)]
    pub uninstall: bool,
    /// Daemon host the hotkey POSTs the captured URL to
    #[arg(long, default_value = "localhost")]
    pub host: String,
    /// Daemon port the hotkey POSTs to
    #[arg(long, default_value_t = 8181)]
    pub port: u16,
    /// Key binding to register (desktop-environment syntax)
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
    /// Write the systemd user unit for the borg daemon
    #[arg(long)]
    pub install: bool,
    /// Remove the systemd user unit
    #[arg(long)]
    pub uninstall: bool,
    /// Uninstall then install the systemd user unit
    #[arg(long)]
    pub reinstall: bool,
    /// Run the daemon in the foreground (the no-flag default)
    #[arg(long)]
    pub start: bool,
    /// Stop the running daemon
    #[arg(long)]
    pub stop: bool,
    /// Restart the running daemon
    #[arg(long)]
    pub restart: bool,
    /// Show the daemon's systemd status
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
pub struct LogCliArgs {
    /// Filter by receipt status (received | succeeded | failed | crashed).
    #[arg(long)]
    pub status: Option<String>,
    /// Filter by method (http | telegram | discord | ntfy | cli | clipboard).
    #[arg(long)]
    pub method: Option<String>,
    /// Filter failed rows by failure_stage.
    #[arg(long)]
    pub stage: Option<String>,
    /// Lower bound on received_at (inclusive). Accepts a relative duration
    /// (5m, 2h, 7d), an ISO-8601 datetime (2026-06-04T05:18:59Z), or a date
    /// (2026-06-04).
    #[arg(long)]
    pub since: Option<String>,
    /// SQL LIKE pattern matched against raw_input (e.g. `%youtube.com%`).
    #[arg(long)]
    pub source: Option<String>,
    /// Show only degraded publishes (notes written from a distill fallback).
    #[arg(long)]
    pub degraded: bool,
    /// Cap the number of rows returned.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Show a single trace's full detail instead of the list view.
    #[arg(long)]
    pub trace: Option<String>,
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
        let config: borg::config::Config =
            borg::config::load_config(self.config.as_ref()).context("Failed to load configuration")?;
        config.validate().context("borg config validation failed")?;

        borg::startup::init_permits(&config).context("Failed to initialize pipeline permits")?;
        borg::startup::log_ffmpeg_thread_caps(&config);

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
                    let (startup, handle) = borg::serve_init(config, env!("GIT_DESCRIBE").to_string()).await?;
                    print_server_banner(&startup);
                    handle.wait().await
                } else {
                    let outcome = borg::daemon(config, opts).await?;
                    print_daemon_outcome(&outcome);
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
                    borg::ingest(config, resolved_url, tags, force, method).await?
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
            Some(Command::Extension(cli)) => extension::run(cli, config),
            Some(Command::Migrate { apply }) => {
                let report = borg::migrate::run(&config, apply).await?;
                print_migrate_report(&report);
                Ok(())
            }
            Some(Command::Audit { fix }) => {
                let mut report = borg::audit::scan(&config)?;
                print_audit_summary(&report, fix.is_some());
                if let Some(kinds) = fix
                    && !report.no_ledger
                {
                    report.fixed_count = borg::audit::apply_fixes(&report, &kinds, |event| {
                        print_audit_event(event);
                    });
                }
                Ok(())
            }
            Some(Command::Log(args)) => {
                if let Some(trace_id) = args.trace {
                    let row = borg::triage::receipts_show(&trace_id)?;
                    print_receipt_detail(&row);
                } else {
                    let filter = borg::triage::ReceiptLogFilter {
                        status: args.status,
                        method: args.method,
                        stage: args.stage,
                        since: args.since,
                        source: args.source,
                        degraded: args.degraded,
                        limit: args.limit,
                    };
                    let rows = borg::triage::receipts_log(filter)?;
                    print_receipt_rows(&rows);
                }
                Ok(())
            }
            Some(Command::Reingest {
                all,
                r#type,
                domain,
                source,
                before,
                after,
                dry_run,
            }) => {
                let _report =
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
                    .await?;
                Ok(())
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
                let report = borg::replay::run(config, opts, |event| {
                    print_replay_event(event);
                })
                .await?;
                let _ = report;
                Ok(())
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
            Some(Command::ReingestFailed { dry_run }) => {
                let report = borg::migrate::reingest_failed(&config, dry_run, |event| {
                    print_reingest_failed_event(event);
                })
                .await?;
                print_reingest_failed_report(&report);
                Ok(())
            }
            Some(Command::BackfillIngested { dry_run }) => {
                let report = borg::backfill::ingested(&config, dry_run)?;
                let (count_label, count) = if dry_run {
                    ("would backfill", report.would_backfill)
                } else {
                    ("backfilled", report.backfilled)
                };
                println!(
                    "backfill-ingested complete:\n  scanned: {}\n  {}: {} (precise from receipts: {})\n  skipped (already had ingested:): {}\n  skipped (origin != assisted): {}\n  skipped (recent mtime): {}\n  skipped (no date: field): {}",
                    report.scanned,
                    count_label,
                    count,
                    report.precise,
                    report.skipped_already_had,
                    report.skipped_origin,
                    report.skipped_recent_mtime,
                    report.skipped_no_date,
                );
                Ok(())
            }
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

/// Emit each pre-rendered line from the lib boundary. Used by every borg
/// sub-verb that returns `Vec<String>` so sb owns stdout uniformly.
fn print_daemon_outcome(outcome: &borg::DaemonOutcome) {
    use borg::DaemonOutcome;
    match outcome {
        DaemonOutcome::Installed { unit_path } => {
            println!("Wrote {}", unit_path.display());
            println!("Service installed and started.");
        }
        DaemonOutcome::Uninstalled { unit_path } => {
            println!("Removed {}", unit_path.display());
            println!("Service uninstalled.");
        }
        DaemonOutcome::NotInstalled { unit_path } => {
            println!("No service file found at {}", unit_path.display());
        }
        DaemonOutcome::Reinstalled { unit_path } => {
            println!("Wrote {}", unit_path.display());
            println!("Service reinstalled and started.");
        }
        DaemonOutcome::Stopped => println!("Stopped obsidian-borg service"),
        DaemonOutcome::Restarted => println!("Restarted obsidian-borg service"),
        DaemonOutcome::Status { raw_output } => print!("{raw_output}"),
        DaemonOutcome::NoAction => {
            println!("No daemon action specified. See: sb borg daemon --help");
        }
    }
}

fn print_receipt_rows(rows: &[borg::receipts::Receipt]) {
    if rows.is_empty() {
        println!("(no receipts rows match)");
        return;
    }
    println!(
        "{:>3}  {:<24}  {:<10}  {:<9}  {:<10}  {:<19}  trace_id",
        "#", "received_at", "method", "status", "kind", "stage"
    );
    println!("{}", "-".repeat(110));
    for (i, r) in rows.iter().enumerate() {
        let stage = r.failure_stage.as_deref().unwrap_or("-");
        println!(
            "{:>3}  {:<24}  {:<10}  {:<9}  {:<10}  {:<19}  {}",
            i + 1,
            r.received_at,
            r.method,
            r.status,
            r.kind,
            stage,
            r.trace_id
        );
    }
}

fn print_receipt_detail(r: &borg::receipts::Receipt) {
    println!("trace_id:       {}", r.trace_id);
    println!("status:         {}", r.status);
    println!("received_at:    {}", r.received_at);
    println!("method:         {}", r.method);
    println!("kind:           {}", r.kind);
    if let Some(t) = &r.terminal_at {
        println!("terminal_at:    {t}");
    }
    if let Some(n) = &r.note_path {
        println!("note_path:      {n}");
    }
    if let Some(s) = &r.failure_stage {
        println!("failure_stage:  {s}");
    }
    if let Some(reason) = &r.failure_reason {
        println!("failure_reason: {reason}");
    }
    if let Some(rep) = &r.replay_of {
        println!("replay_of:      {rep}");
    }
    let preview = vault::text::truncate_with_ellipsis(&r.raw_input, 200);
    println!("raw_input:      {preview}");
}

fn print_replay_event(event: &borg::replay::ReplayEvent) {
    use borg::replay::ReplayEvent;
    match event {
        ReplayEvent::BootstrapHeader {
            note_path,
            source,
            method,
        } => {
            println!("bootstrap: {} -> {source} (method: {method})", note_path.display());
        }
        ReplayEvent::TraceHeader { trace_id, source } => {
            println!("replay trace {trace_id}: {source}");
        }
        ReplayEvent::MatchingHeader { count } => {
            println!("replay: {count} matching trace(s)");
        }
        ReplayEvent::NoMatches => {
            println!("replay: no traces matched");
        }
        ReplayEvent::DryRunBootstrap { source } => {
            println!("  [dry-run] would re-ingest {source}");
        }
        ReplayEvent::DryRunTrace => {
            println!("  [dry-run] would re-ingest via daemon");
        }
        ReplayEvent::ResultOk { title } => {
            println!("  -> {}", title.as_deref().unwrap_or("(no title)"));
        }
        ReplayEvent::ResultDuplicate { original_date } => {
            println!("  -> duplicate (originally ingested {original_date})");
        }
        ReplayEvent::ResultFailed { reason } => {
            println!("  -> failed: {reason}");
        }
        ReplayEvent::ResultQueued => {
            println!("  -> queued");
        }
        ReplayEvent::ResultOther { description } => {
            println!("  -> {description}");
        }
        ReplayEvent::MatchingItemError { trace_id, error } => {
            println!("  trace {trace_id}: {error}");
        }
    }
}

fn print_audit_event(event: &borg::audit::AuditEvent) {
    use borg::audit::AuditEvent;
    match event {
        AuditEvent::FixStart { count } => {
            println!();
            println!("Fixing {count} finding(s)...");
        }
        AuditEvent::Fixed {
            rel_path,
            expected_type,
        } => {
            println!("  Fixed: {} -> type: {expected_type}", rel_path.display());
        }
        AuditEvent::FixError { path, error } => {
            println!("  Error fixing {}: {error}", path.display());
        }
        AuditEvent::NothingFixable => {
            println!();
            println!("No fixable findings for the requested kinds.");
        }
        AuditEvent::RowDropped { source, date } => {
            println!("  Dropped \u{1F504} ledger row: {date}  {source}");
        }
        AuditEvent::NoteRemoved { rel_path, source } => {
            println!("  rkvr rmrf: {} ({source})", rel_path.display());
        }
        AuditEvent::Quarantined {
            source,
            kept,
            quarantined,
        } => {
            println!("  Quarantined {} dup(s) for source: {source}", quarantined.len());
            println!("    kept: {}", kept.display());
            for q in quarantined {
                println!("    moved: {}", q.display());
            }
        }
        AuditEvent::DuplicateReported { count } => {
            println!(
                "  {count} notes share identical content (report only; run --fix duplicate-quarantine to quarantine)"
            );
        }
        AuditEvent::DuplicateNotEligible { count, reason } => {
            println!("  {count} notes hash alike but failed the quarantine second-proof ({reason}); not moved");
        }
        AuditEvent::RkvrUnavailable { path, error } => {
            eprintln!("  rkvr unavailable for {}: {error}", path.display());
        }
        AuditEvent::CreatorSet { rel_path, creator } => {
            println!("  set creator: {creator} on {}", rel_path.display());
        }
    }
}

fn print_audit_summary(report: &borg::audit::AuditReport, fix: bool) {
    if report.no_ledger {
        println!("No Borg Ledger found at {}", report.ledger_path.display());
        return;
    }
    println!("Auditing Borg Ledger: {}", report.ledger_path.display());
    println!("Vault: {}", report.vault_root.display());
    println!("Found {} completed ledger entries to audit", report.entries_scanned);
    println!();

    if report.findings.is_empty() {
        println!("No issues found.");
        return;
    }

    let mut mistype_count = 0;
    let mut blocked_count = 0;
    let mut raw_title_count = 0;
    let mut duplicate_count = 0;
    let mut orphan_count = 0;
    let mut github_creator_count = 0;
    for finding in &report.findings {
        match finding {
            borg::audit::AuditFinding::Mistype { .. } => mistype_count += 1,
            borg::audit::AuditFinding::Blocked { .. } => blocked_count += 1,
            borg::audit::AuditFinding::RawTitle { .. } => raw_title_count += 1,
            borg::audit::AuditFinding::Duplicate { .. } => duplicate_count += 1,
            borg::audit::AuditFinding::OrphanReplace { .. } => orphan_count += 1,
            borg::audit::AuditFinding::GithubCreatorMissing { .. } => github_creator_count += 1,
        }
    }

    println!("Audit Results:");
    if mistype_count > 0 {
        println!("  {mistype_count} misclassified types");
    }
    if blocked_count > 0 {
        println!("  {blocked_count} blocked content saved as completed");
    }
    if raw_title_count > 0 {
        println!("  {raw_title_count} raw URL titles");
    }
    if duplicate_count > 0 {
        println!("  {duplicate_count} duplicate note pairs");
    }
    if orphan_count > 0 {
        println!("  {orphan_count} orphaned replacements (replaced but no new ✅)");
    }
    if github_creator_count > 0 {
        println!("  {github_creator_count} github notes missing a creator (repo owner)");
    }

    println!();
    println!("Details:");
    for finding in &report.findings {
        println!("  {finding}");
    }

    if !fix {
        let total =
            mistype_count + blocked_count + raw_title_count + duplicate_count + orphan_count + github_creator_count;
        println!();
        println!("Run with --fix to address all {total} finding(s), or --fix <kinds...> to target specific classes.");
        println!("  Kinds: mistype | orphan-replace | blocked | raw-title | duplicate | github-creator-missing");
    }
}

fn print_migrate_report(r: &borg::migrate::MigrateReport) {
    let mode = if r.apply { "APPLY" } else { "DRY-RUN" };
    println!("Migration mode: {mode}");
    println!("Vault: {}", r.vault_root.display());
    println!("Found {} markdown files to check", r.files_scanned);
    for rel in &r.changed {
        println!("  {mode}: {rel}");
    }
    if r.seeded_ledger > 0 {
        println!("Seeded Borg Ledger with {} entries.", r.seeded_ledger);
    }
    println!();
    println!("{mode} complete: {} files would be changed", r.changed.len());
    if !r.apply && !r.changed.is_empty() {
        println!("Run with --apply to write changes.");
    }
}

fn print_reingest_failed_event(event: &borg::migrate::ReingestFailedEvent) {
    use borg::migrate::ReingestFailedEvent;
    match event {
        ReingestFailedEvent::NoMatches => {
            println!("reingest-failed: no failed-fetch notes found");
        }
        ReingestFailedEvent::Dispatching { source } => println!("  -> {source}"),
        ReingestFailedEvent::Ok { title } => {
            println!("     ok: {}", title.as_deref().unwrap_or("(no title)"));
        }
        ReingestFailedEvent::Duplicate => println!("     duplicate (unchanged)"),
        ReingestFailedEvent::Failed { reason } => println!("     failed: {reason}"),
        ReingestFailedEvent::Queued => println!("     queued"),
        ReingestFailedEvent::ParseError { path, error } => {
            println!("     {}: response parse error: {error}", path.display());
        }
        ReingestFailedEvent::HttpError { path, error } => {
            println!("     {}: HTTP error: {error}", path.display());
        }
    }
}

fn print_reingest_failed_report(r: &borg::migrate::ReingestFailedReport) {
    if r.matched.is_empty() {
        return; // NoMatches event already printed
    }
    let mode = if r.dry_run { "[dry-run] " } else { "" };
    // For apply mode the dispatching events already streamed; we only
    // print the summary header + path list at the end (matches the
    // pre-refactor output where the list preceded the per-item lines).
    if r.dry_run {
        println!("{mode}reingest-failed: {} matching note(s)", r.matched.len());
        for (path, source) in &r.matched {
            let rel = path.strip_prefix(&r.vault_root).unwrap_or(path);
            println!("  {}  <- {}", rel.display(), source);
        }
    }
}

/// Render the daemon startup banner from the typed snapshot. Lines that
/// previously went to stdout/stderr go through here so the lib stays
/// stdout-clean.
fn print_server_banner(s: &borg::ServerStartup) {
    use borg::SubsystemStatus;

    let arrow = "-->".to_string();
    match &s.telegram {
        SubsystemStatus::Active => println!("{} telegram notifier active", arrow.green()),
        SubsystemStatus::SkippedNoToken => {
            eprintln!("{} telegram notifier skipped (token not available)", arrow.yellow())
        }
        _ => {}
    }

    match &s.desktop {
        SubsystemStatus::Active => println!("{} desktop notifier active", arrow.green()),
        SubsystemStatus::SkippedHostMismatch => {
            eprintln!("{} desktop notifier skipped (host mismatch)", arrow.yellow())
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
            // Already printed; signal exit-1 to main via the typed marker.
            return Err(crate::error::SilentFailure.into());
        }
    }
    Ok(())
}

fn get_tool_validation_help() -> String {
    #[allow(clippy::type_complexity)]
    // `notify-send` is a runtime-dependency proxy: borg does not shell out to
    // it (we use the in-process notify-rust crate), but its presence indicates
    // a working libnotify stack and gives the operator a diagnostic one-liner
    // (`notify-send foo`) when desktop toasts go missing.
    let tools: &[(&str, &str, &str, &[(&str, &str, &str)])] = &[
        ("yt-dlp", "--version", "2023.0.0", &[("ffmpeg", "-version", "")]),
        ("fabric", "--version", "1.0.0", &[]),
        ("markitdown", "--version", "", &[]),
        ("notify-send", "--version", "", &[]),
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
