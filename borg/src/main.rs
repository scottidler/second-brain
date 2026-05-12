use borg::cli::{BlocklistAction, Cli, Command, DashboardAction, DlqAction, IntakeAction, RetentionAction};
use borg::config::Config;
use borg::logging;
use clap::Parser;
use eyre::{Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config: Config = borg::config::load_config(cli.config.as_ref()).context("Failed to load configuration")?;

    let log_level = logging::resolve_log_level(cli.log_level.as_deref(), config.log_level.as_deref());
    logging::setup_logging(&log_level).context("Failed to setup logging")?;

    log::debug!("Resolved log level: {log_level}");
    log::debug!("Config: {:?}", config);

    if cli.verbose {
        println!("{}", colored::Colorize::yellow("Verbose mode enabled"));
    }

    match cli.command {
        None => {
            Cli::parse_from(["borg", "--help"]);
            Ok(())
        }
        Some(Command::Daemon(opts)) => borg::run_daemon(config, cli.verbose, opts).await,
        Some(Command::Ingest {
            url,
            clipboard,
            file,
            tags,
            force,
        }) => {
            if let Some(file_path) = file {
                borg::run_file_ingest(config, file_path, tags, force).await
            } else {
                let resolved_url = borg::resolve_ingest_url(url, clipboard)?;
                let method = if clipboard {
                    borg::types::IngestMethod::Clipboard
                } else {
                    borg::types::IngestMethod::Cli
                };
                borg::run_ingest(config, resolved_url, tags, force, clipboard, method).await
            }
        }
        Some(Command::Note { text, clipboard, tags }) => {
            let resolved_text = borg::resolve_note_text(text, clipboard)?;
            borg::run_note(config, resolved_text, tags).await
        }
        Some(Command::Hotkey(opts)) => borg::run_hotkey(opts, &config).await,
        Some(Command::Sign) => borg::run_sign(&config).await,
        Some(Command::Migrate { dry_run: _, apply }) => borg::migrate::run_migrate(&config, apply).await,
        Some(Command::Audit {
            fix,
            invariant,
            bound_secs,
        }) => {
            if invariant {
                borg::triage::run_orphan_audit(&config, bound_secs).await
            } else {
                borg::audit::run_audit(&config, fix)
            }
        }
        Some(Command::Intake(args)) => match args.action {
            IntakeAction::List { method, since, limit } => {
                borg::triage::run_intake_list(&config, method, since, limit).await
            }
            IntakeAction::Show { trace_id } => borg::triage::run_intake_show(&config, &trace_id).await,
        },
        Some(Command::Dlq(args)) => match args.action {
            DlqAction::List {
                method,
                stage,
                status,
                limit,
            } => borg::triage::run_dlq_list(&config, method, stage, status, limit).await,
            DlqAction::Show { trace_id } => borg::triage::run_dlq_show(&config, &trace_id).await,
            DlqAction::Archive {
                trace_id,
                status,
                resolved,
            } => borg::triage::run_dlq_archive(&config, trace_id, &status, resolved).await,
            DlqAction::Replay { trace_id } => borg::triage::run_dlq_replay(&config, &trace_id).await,
        },
        Some(Command::Reingest {
            all,
            r#type,
            domain,
            source,
            before,
            after,
            dry_run,
        }) => borg::run_reingest(config, all, r#type, domain, source, before, after, dry_run).await,
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
            RetentionAction::Sweep { dry_run } => borg::retention::run_sweep(&config, dry_run),
            RetentionAction::Status => borg::retention::run_status(&config),
        },
        Some(Command::ReingestFailed { dry_run }) => borg::migrate::run_reingest_failed(&config, dry_run).await,
        Some(Command::BackfillIngested { dry_run }) => borg::backfill::run_backfill_ingested(&config, dry_run),
        Some(Command::Dashboard(args)) => match args.action {
            DashboardAction::Refresh => borg::dashboard::refresh_dashboard(&borg::dashboard::dashboard_path(&config)),
        },
        Some(Command::Blocklist(args)) => {
            use borg::blocklist::{Blocklist, default_path};
            let path = default_path();
            match args.action {
                BlocklistAction::List => {
                    let bl = Blocklist::from_file(&path)?;
                    if bl.domains.is_empty() {
                        println!("(blocklist empty)");
                    } else {
                        for (domain, entry) in bl.list() {
                            println!(
                                "{domain:30} retriable-after={} hits={} reason={}",
                                entry.retriable_after, entry.hits, entry.reason
                            );
                        }
                    }
                    Ok(())
                }
                BlocklistAction::Remove { domain } => {
                    let mut bl = Blocklist::from_file(&path)?;
                    let removed = bl.remove(&domain).is_some();
                    bl.save_to(&path)?;
                    if removed {
                        println!("removed: {domain}");
                    } else {
                        println!("not blocklisted: {domain}");
                    }
                    Ok(())
                }
                BlocklistAction::Clear => {
                    let bl = Blocklist::default();
                    bl.save_to(&path)?;
                    println!("blocklist cleared");
                    Ok(())
                }
            }
        }
    }
}
