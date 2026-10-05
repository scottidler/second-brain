use eyre::{Context, Result};

mod args;
pub mod extension;
mod render;
mod tools;
pub mod wait;

pub use args::*;
use render::*;

#[cfg(test)]
mod tests;

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
                        command,
                        host,
                        port,
                        post_install,
                    } => {
                        if let Some(msg) = post_install {
                            println!("{msg}");
                        } else {
                            println!("Hotkey installed: {key} -> {command}");
                        }
                        println!("Daemon target: http://{host}:{port}/ingest (hotkey.host/port in borg.yml)");
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
            Some(Command::Queue { format }) => {
                let snapshot = borg::queue::fetch(&config, None, config.hotkey.request_timeout).await?;
                crate::cli::output::emit(&snapshot, format)
            }
            Some(Command::Wait { timeout, format }) => {
                let outcome = wait::run(&config, timeout).await?;
                match &outcome.snapshot {
                    Some(snapshot) => crate::cli::output::emit(snapshot, format)?,
                    None => eprintln!(
                        "timeout {} reached before the daemon at {}:{} answered",
                        humantime::format_duration(timeout),
                        config.hotkey.host,
                        config.hotkey.port
                    ),
                }
                match outcome.code {
                    0 => Ok(()),
                    code => Err(crate::error::ExitWith(code).into()),
                }
            }
            Some(Command::Reingest {
                all,
                r#type,
                source,
                before,
                after,
                dry_run,
            }) => {
                let _report = borg::reingest(
                    config,
                    all,
                    r#type,
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
                RetentionAction::Sweep { dry_run, sidecars_only } => {
                    let action = if dry_run { "Would delete" } else { "Deleted" };
                    if !sidecars_only {
                        let result = borg::retention::sweep(&config, dry_run)?;
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
                    }
                    let sidecars = borg::retention::sweep_sidecars(&config, dry_run)?;
                    if sidecars.enabled {
                        println!(
                            "Scanned {} sidecars, kept {}, {} {} (freed {} bytes)",
                            sidecars.scanned,
                            sidecars.kept,
                            action.to_ascii_lowercase(),
                            sidecars.deleted.len(),
                            sidecars.bytes_freed
                        );
                        for name in &sidecars.deleted {
                            println!("  {action}: {name}");
                        }
                    } else {
                        println!("Sidecar sweep disabled (intake.retention-days=0)");
                    }
                    Ok(())
                }
                RetentionAction::Status => {
                    let report = borg::retention::status(&config)?;
                    println!("Staging root:   {}", report.root.display());
                    println!("Traces:         {}", report.traces);
                    println!("Rejected:       {}", report.rejected);
                    println!("Disk usage:     {} bytes", report.total_bytes);
                    let sidecars = borg::retention::sidecar_status(&config)?;
                    let window = if sidecars.retention_days == 0 {
                        "forever (sweep disabled)".to_string()
                    } else {
                        format!("{} days", sidecars.retention_days)
                    };
                    println!("Sidecar dir:    {}", sidecars.dir.display());
                    println!("Sidecar files:  {}", sidecars.files);
                    println!("Sidecar bytes:  {}", sidecars.total_bytes);
                    println!("Sidecar window: {window}");
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
            Some(Command::Eval(a)) => {
                let opts = borg::eval::EvalOpts::from(&a);
                match borg::eval::run(&opts)? {
                    borg::eval::EvalOutcome::CalibrationSheet(path) => {
                        println!("wrote calibration sheet: {}", path.display());
                        println!(
                            "Fill the `human-*` scores, then copy them into {}.",
                            a.calibration.display()
                        );
                    }
                    borg::eval::EvalOutcome::Report(report) => {
                        let rendered = report.render();
                        print!("{rendered}");
                        if let Some(path) = a.report {
                            std::fs::write(&path, &rendered)
                                .with_context(|| format!("writing report to {}", path.display()))?;
                            println!("\nreport written to {}", path.display());
                        }
                    }
                }
                Ok(())
            }
            Some(Command::Harvest(args)) => {
                if args.install {
                    for line in borg::harvest::timer::install(&config)? {
                        println!("{line}");
                    }
                    Ok(())
                } else if args.uninstall {
                    for line in borg::harvest::timer::uninstall()? {
                        println!("{line}");
                    }
                    Ok(())
                } else {
                    // `--dry-run` forces dry-run, `--live` forces live; with
                    // neither, the harvest.mode config default decides (DryRun
                    // out of the box, per Rollout Plan).
                    let dry_run = config.harvest.mode.resolve_dry_run(args.dry_run, args.live);
                    let report =
                        borg::harvest::run(&config, args.since, args.limit, args.force, dry_run, args.dormant_after)
                            .await?;
                    print_harvest_report(&report);
                    Ok(())
                }
            }
            Some(Command::DedupeSessions { apply, purge }) => {
                let report = borg::dedupe::run(&config, &borg::dedupe::DedupeOpts { apply, purge })?;
                print_dedupe_report(&report);
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
