use eyre::{Context, Result};

mod args;
mod candidate;
pub mod extension;
mod render;
mod tools;
pub mod wait;
mod weekly;

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
                note,
            }) => {
                let outcome = if let Some(file_path) = file {
                    if note.is_some() {
                        eyre::bail!("--note applies to URL ingest, not --file");
                    }
                    borg::ingest_file(config, file_path, tags, force).await?
                } else {
                    let resolved_url = borg::resolve_ingest_url(url, clipboard)?;
                    let method = if clipboard {
                        borg::types::IngestMethod::Clipboard
                    } else {
                        borg::types::IngestMethod::Cli
                    };
                    borg::ingest(config, resolved_url, tags, force, method, note).await?
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
                jobs,
            }) => {
                let _report = borg::reingest(
                    config,
                    all,
                    r#type,
                    source,
                    before,
                    after,
                    dry_run,
                    jobs,
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
                        borg::ReingestEvent::SkippedNonUrl { count } => println!(
                            "Skipping {count} non-http(s) entries (session notes: use `sb borg replay <trace> --from-stage 2`)"
                        ),
                        borg::ReingestEvent::Halted { reason } => eprintln!("Stopped: {reason}"),
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
                    fork: args.fork,
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
                RetentionAction::Archive { apply } => {
                    let report = borg::archive::backfill::run(&config, apply)?;
                    let verb = if apply { "copied" } else { "to copy" };
                    println!(
                        "Scanned {} staged traces: {} {verb}, {} already archived, {} sessions skipped, {} failed",
                        report.scanned,
                        report.to_copy.len(),
                        report.already_archived,
                        report.skipped_session,
                        report.failed.len()
                    );
                    for trace in &report.to_copy {
                        println!("  {verb}: {trace}");
                    }
                    for (trace, reason) in &report.failed {
                        eprintln!("  failed: {trace}: {reason}");
                    }
                    if !report.failed.is_empty() {
                        eyre::bail!("{} staged traces could not be archived", report.failed.len());
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
            Some(Command::ChannelSearch {
                handle,
                query,
                limit,
                format,
            }) => {
                let hits = borg::youtube::search_channel(&handle, &query, limit).await?;
                let result = vault::channel::ChannelSearch { handle, query, hits };
                crate::cli::output::emit(&result, format)
            }
            Some(Command::Channels { backfill: _, apply }) => {
                let report = borg::channels::run(&config, apply).await?;
                let verb = if apply { "written" } else { "to write" };
                println!(
                    "Scanned {} youtube notes: {} {verb}, {} already complete, {} unavailable, {} failed",
                    report.scanned,
                    report.to_write.len(),
                    report.already_complete,
                    report.unavailable.len(),
                    report.failed.len()
                );
                for path in &report.to_write {
                    println!("  {verb}: {}", path.display());
                }
                for (path, reason) in &report.unavailable {
                    eprintln!("  unavailable: {}: {reason}", path.display());
                }
                for (path, reason) in &report.failed {
                    eprintln!("  failed: {}: {reason}", path.display());
                }
                if !report.failed.is_empty() {
                    eyre::bail!("{} notes could not be updated", report.failed.len());
                }
                Ok(())
            }
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
                let opts = eval_opts(&config, &a)?;
                if a.synth_intents {
                    let writer = borg::eval::fabric_intent_writer(&opts)?;
                    let report = borg::eval::synth_intents(&opts.fixtures_dir, &writer)?;
                    print!("{}", report.render());
                    return Ok(());
                }
                if a.regenerate {
                    let dispatcher = borg::eval::regenerate_dispatcher(&config.fabric, &opts);
                    let report = borg::eval::run_regenerate(&opts, &dispatcher).await?;
                    let rendered = match a.format {
                        args::EvalFormat::Text => report.render(),
                        args::EvalFormat::Json => render_records_jsonl(&report.records())?,
                    };
                    print!("{rendered}");
                    if let Some(path) = a.report {
                        std::fs::write(&path, &rendered)
                            .with_context(|| format!("writing report to {}", path.display()))?;
                        match a.format {
                            args::EvalFormat::Text => println!("\nreport written to {}", path.display()),
                            args::EvalFormat::Json => eprintln!("report written to {}", path.display()),
                        }
                    }
                    if report.all_judged_errored() {
                        let (errored, total) = report.errored_of_total();
                        eyre::bail!(
                            "every judged trace errored on --regenerate ({errored} of {total}); see the per-trace errors"
                        );
                    }
                    return Ok(());
                }
                if let Some(n) = a.candidate_split {
                    return candidate::run_split(&config, &opts, &a, n);
                }
                if let Some(file) = &a.candidate_pattern {
                    return candidate::run_score(&config, &opts, &a, file).await;
                }
                if let Some(file) = &a.apply_candidate {
                    return candidate::run_apply(&config, &opts, file).await;
                }
                if a.unscored {
                    return run_eval_unscored(&config, &opts, &a);
                }
                if a.inject {
                    return run_eval_inject(&config, &opts, &a);
                }
                if a.remediate {
                    return run_eval_remediate(&config, a.apply).await;
                }
                if a.weekly {
                    return weekly::run(&config, &opts, &a).await;
                }
                match borg::eval::run(&opts)? {
                    borg::eval::EvalOutcome::CalibrationSheet(path) => {
                        println!("wrote calibration sheet: {}", path.display());
                        println!(
                            "Fill the `human-*` scores, then copy them into {}.",
                            a.calibration.display()
                        );
                    }
                    borg::eval::EvalOutcome::Report(report) => {
                        let rendered = match a.format {
                            args::EvalFormat::Text => report.render(),
                            args::EvalFormat::Json => render_eval_jsonl(&report)?,
                        };
                        print!("{rendered}");
                        if let Some(path) = &a.report {
                            std::fs::write(path, &rendered)
                                .with_context(|| format!("writing report to {}", path.display()))?;
                            match a.format {
                                args::EvalFormat::Text => println!("\nreport written to {}", path.display()),
                                // stderr: stdout stays pure JSONL.
                                args::EvalFormat::Json => eprintln!("report written to {}", path.display()),
                            }
                        }
                        if report.all_judged_errored() {
                            eyre::bail!(
                                "every judged trace errored ({} of {}); see the per-trace errors",
                                report.errored_traces(),
                                report.traces.len()
                            );
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
                    let report = borg::harvest::run(
                        &config,
                        args.since,
                        args.limit,
                        args.force,
                        args.fork,
                        dry_run,
                        args.dormant_after,
                    )
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

/// `sb borg eval --unscored`: the backlog count, or one scoring run over it.
/// Judge keys named by `llm.api-keys` must resolve here before anything is
/// judged, so a timer run missing a var fails as one clear error.
fn run_eval_unscored(config: &borg::config::Config, opts: &borg::eval::EvalOpts, a: &args::EvalArgs) -> Result<()> {
    let staged_root = &config.staging.root;
    let receipts = borg::receipts::open_default().context("opening the receipts db")?;
    let store = borg::eval::scores::ScoreStore::open(&vault::paths::borg_eval_cache_path())?;
    let required = borg::eval::scores::required_checks(&config.eval.judge_models);

    if a.count {
        if config.eval.judge_models.is_empty() {
            eyre::bail!("eval.judge-models is empty: the required-check set is derived from it");
        }
        let backlog = borg::eval::backlog::find_backlog(&receipts, &store, staged_root, &required, None)?;
        for (trace, reason) in &backlog.unreadable {
            eprintln!("unreadable staged trace {trace}: {reason}");
        }
        eprintln!(
            "{} succeeded receipts have no staged source and are not counted",
            backlog.not_staged
        );
        println!("{}", backlog.traces.len());
        return Ok(());
    }

    borg::eval::backlog::preflight_keys(&opts.api_keys)?;
    let cache = borg::eval::cache::JudgmentCache::open(&vault::paths::borg_eval_cache_path())?;
    let factory = |o: &borg::eval::EvalOpts| -> Result<(Box<dyn borg::eval::DistillationJudge>, Box<dyn borg::eval::ClaimJudge>)> {
        let (judge, claim_judge) = borg::eval::fabric_judges(o)?;
        Ok((Box::new(judge), Box::new(claim_judge)))
    };
    let run = borg::eval::backlog::run_unscored(
        &receipts,
        &store,
        &cache,
        opts,
        staged_root,
        &config.eval.judge_models,
        a.limit.or(config.eval.limit),
        &factory,
    )?;
    for (trace, reason) in &run.unreadable {
        eprintln!("unreadable staged trace {trace}: {reason}");
    }
    for report in &run.reports {
        let rendered = match a.format {
            args::EvalFormat::Text => report.render(),
            args::EvalFormat::Json => render_eval_jsonl(report)?,
        };
        print!("{rendered}");
    }
    let summary = format!(
        "unscored: {} traces selected, {} score rows written, {} succeeded receipts with no staged source skipped",
        run.selected, run.rows_written, run.not_staged
    );
    match a.format {
        args::EvalFormat::Text => println!("\n{summary}"),
        args::EvalFormat::Json => eprintln!("{summary}"),
    }
    if run.all_errored() {
        eyre::bail!("every judged trace errored on every judge model; see the per-trace errors");
    }
    Ok(())
}

/// `sb borg eval --inject`: plant errors into a staged sample, measure every
/// configured judge on them, and write the `judge_trust` rows.
fn run_eval_inject(config: &borg::config::Config, opts: &borg::eval::EvalOpts, a: &args::EvalArgs) -> Result<()> {
    if config.eval.judge_models.is_empty() {
        eyre::bail!("eval.judge-models is empty: name the judge models --inject measures in borg.yml");
    }
    borg::eval::backlog::preflight_keys(&opts.api_keys)?;
    let sample = a.sample.unwrap_or(config.eval.trust.min_samples);
    let fixtures = borg::eval::inject::sample_fixtures(&config.staging.root, sample)?;
    let cache_path = vault::paths::borg_eval_cache_path();
    let scores = borg::eval::scores::ScoreStore::open(&cache_path)?;
    let trust = borg::eval::trust::TrustStore::open(&cache_path)?;
    let factory = |o: &borg::eval::EvalOpts| -> Result<(Box<dyn borg::eval::DistillationJudge>, Box<dyn borg::eval::ClaimJudge>)> {
        let (judge, claim_judge) = borg::eval::fabric_judges(o)?;
        Ok((Box::new(judge), Box::new(claim_judge)))
    };
    let run = borg::eval::inject::InjectRun {
        opts,
        eval: &config.eval,
        cache_path: &cache_path,
        now: chrono::Utc::now(),
    };
    let report = borg::eval::inject::run_inject(&fixtures, &run, &scores, &trust, &factory)?;
    let rendered = report.render();
    print!("{rendered}");
    if let Some(path) = &a.report {
        std::fs::write(path, &rendered).with_context(|| format!("writing report to {}", path.display()))?;
        println!("\nreport written to {}", path.display());
    }
    if report.all_judged_errored() {
        eyre::bail!("every judged trace errored on --inject; no judge was measured");
    }
    Ok(())
}

/// `sb borg eval --remediate [--apply]`: list the failing scored traces and
/// the one re-distill each actionable source gets; `--apply` claims and posts
/// them through the daemon, which must advertise `replay-of` on `/health`.
async fn run_eval_remediate(config: &borg::config::Config, apply: bool) -> Result<()> {
    use borg::eval::remediate;
    if config.eval.judge_models.is_empty() {
        eyre::bail!("eval.judge-models is empty: the required-check set --remediate reads is derived from it");
    }
    let receipts = borg::receipts::open_default().context("opening the receipts db")?;
    let cache_path = vault::paths::borg_eval_cache_path();
    let store = borg::eval::scores::ScoreStore::open(&cache_path)?;
    let trust_rows = borg::eval::trust::TrustStore::open(&cache_path)?.all()?;
    let plan = remediate::plan(
        &receipts,
        &store,
        &config.staging.root,
        &config.eval.judge_models,
        &trust_rows,
        &config.eval.trust,
    )?;
    let note_of = |trace: &str| borg::replay::capture_note_for(config, trace);
    let report = if apply {
        let daemon = vault::daemon::client::DaemonClient::new(&config.hotkey, config.server.auth_token.as_deref())?;
        let poster = remediate::DaemonPoster::connect(daemon).await?;
        remediate::run(&receipts, plan, Some(&poster), &note_of).await?
    } else {
        remediate::run::<remediate::DaemonPoster>(&receipts, plan, None, &note_of).await?
    };
    print!("{}", report.render());
    if report.post_failed() {
        eyre::bail!("a remediation post failed; see NOT DELIVERED / UNKNOWN above");
    }
    Ok(())
}

/// The `sb borg eval` options for `a`: the CLI flags plus the eval's
/// credential vars, which come from borg.yml's `llm.api-keys`, never a flag.
/// With no `--judge-model`, a judged run uses the default judge (the first of
/// `eval.judge-models`, Gemini unless configured). An explicitly emptied list
/// resolves no model, and `borg::eval::fabric_judges` refuses to judge with
/// none. A run that judges nothing (`--no-judge`) names no model, so its
/// records never claim a judge that did not run. `--synth-intents` writes
/// fixtures, it judges nothing: with no `--judge-model` it keeps fabric's own
/// default model, as before the Gemini judge default. `--staged` reads
/// borg.yml's `staging.root` unless `--staging-root` names another.
fn eval_opts(config: &borg::config::Config, a: &args::EvalArgs) -> Result<borg::eval::EvalOpts> {
    if a.judge_model.as_deref().is_some_and(|m| m.trim().is_empty()) {
        eyre::bail!(
            "--judge-model is blank: name a model, or omit the flag for the default judge \
             (a blank model runs fabric's default model, which is never trusted)"
        );
    }
    let judges = !a.no_judge && !a.synth_intents;
    let judge_model = a
        .judge_model
        .clone()
        .or_else(|| {
            judges
                .then(|| config.eval.default_judge_model().map(str::to_string))
                .flatten()
        })
        .unwrap_or_default();
    Ok(borg::eval::EvalOpts {
        judge_model,
        api_keys: config.llm.api_keys.clone(),
        patterns_dir: a
            .patterns_dir
            .as_deref()
            .map(borg::eval::resolve_patterns_dir)
            .transpose()?,
        staged_root: a.staged.then(|| {
            a.staging_root
                .as_ref()
                .map(vault::paths::expand_tilde)
                .unwrap_or_else(|| config.staging.root.clone())
        }),
        ..borg::eval::EvalOpts::from(a)
    })
}
