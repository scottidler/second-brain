use eyre::{Context, Result};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::Instant;
use vault::watcher::{VaultWatcher, WatcherConfig};

use crate::config::{Config, DaemonConfig};
use crate::opts::DaemonOpts;

/// Fingerprint of a single sweep's apply results.
/// Used to detect oscillation between consecutive sweeps.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct SweepFingerprint {
    /// Sorted list of (action, sorted file paths) for actions that applied changes.
    results: Vec<(String, Vec<String>)>,
}

impl SweepFingerprint {
    fn is_empty(&self) -> bool {
        self.results.is_empty() || self.results.iter().all(|(_, files)| files.is_empty())
    }

    fn add(&mut self, action: &str, mut files: Vec<String>) {
        files.sort();
        files.dedup();
        if !files.is_empty() {
            self.results.push((action.to_string(), files));
        }
    }
}

/// Outcome of a `sb cortex daemon` invocation. `Installed`/`Uninstalled`/`Status`
/// carry pre-rendered lines for sb to print. `LoopExited` indicates the
/// long-running watcher loop exited cleanly (ctrl-C or SIGTERM); sb stays quiet.
#[derive(Debug, Default)]
pub struct DaemonOutcome {
    pub lines: Vec<String>,
}

/// Run the daemon based on subcommand options.
pub async fn run(vault_root: &Path, config: &Config, opts: &DaemonOpts) -> Result<DaemonOutcome> {
    if opts.install {
        Ok(DaemonOutcome {
            lines: install_systemd_service(vault_root, config)?,
        })
    } else if opts.uninstall {
        Ok(DaemonOutcome {
            lines: uninstall_systemd_service()?,
        })
    } else if opts.status {
        Ok(DaemonOutcome { lines: show_status()? })
    } else if opts.stop {
        log::info!("daemon --stop: send SIGTERM to the running daemon process to stop it");
        Ok(DaemonOutcome::default())
    } else {
        // Default: start watching (--start or no flags). Long-running; logs
        // every transition; returns an empty outcome on clean shutdown.
        start_watching(vault_root, config).await?;
        Ok(DaemonOutcome::default())
    }
}

/// Start filesystem watcher and run actions on changes using async tokio::select! loop.
async fn start_watching(vault_root: &Path, config: &Config) -> Result<()> {
    let daemon_config = &config.daemon;
    let poll_interval = Duration::from_secs(daemon_config.poll_interval);

    let action_names: Vec<&str> = daemon_config.enabled_actions();
    let any_enabled = daemon_config.actions.values().any(|a| a.enable);

    log::info!("starting daemon, watching: {}", vault_root.display());
    log::info!(
        "debounce: {}s, actions: {}{}",
        daemon_config.debounce_secs,
        action_names.join(", "),
        if any_enabled { " (auto-apply enabled)" } else { "" },
    );

    // Flag to suppress watcher events during auto-apply (prevents feedback loops)
    let applying = Arc::new(AtomicBool::new(false));

    // Shared VaultWatcher from the vault crate
    let watcher_config = WatcherConfig {
        debounce_secs: daemon_config.debounce_secs,
        ignore_dirs: config.vault.ignore.clone(),
    };
    let (watcher, mut watch_rx) = VaultWatcher::start(vault_root, watcher_config, Some(Arc::clone(&applying)))?;

    log::info!("daemon started: {}", vault_root.display());

    // Timers
    let mut sweep_interval = tokio::time::interval(poll_interval);
    sweep_interval.tick().await; // consume the immediate first tick

    // Scheduled intel timers
    let intel_enabled = daemon_config.is_enabled("intel");
    let daily_dur = match (&daemon_config.daily_at, intel_enabled) {
        (Some(time_str), true) => {
            let dur = duration_until_next(time_str);
            log::info!(
                "daily intel scheduled at {time_str} (in {:.0}m)",
                dur.as_secs_f64() / 60.0
            );
            dur
        }
        _ => Duration::MAX, // inert
    };
    let daily = tokio::time::sleep(daily_dur);
    tokio::pin!(daily);

    let weekly_dur = match (&daemon_config.weekly_at, intel_enabled) {
        (Some(schedule_str), true) => {
            let dur = duration_until_next(schedule_str);
            log::info!(
                "weekly intel scheduled for {schedule_str} (in {:.1}h)",
                dur.as_secs_f64() / 3600.0
            );
            dur
        }
        _ => Duration::MAX, // inert
    };
    let weekly = tokio::time::sleep(weekly_dur);
    tokio::pin!(weekly);

    // Phase A5 / B2: periodic embed tick. Most ticks find zero stale
    // rows and return in <1 ms; the load only spikes when borg has
    // just ingested new content. The cadence is decoupled from the
    // sweep cadence because embed is a CPU-bound batch operation and
    // the sweep cadence is governed by debounce_secs.
    let mut embed_interval = tokio::time::interval(crate::embed::daemon_cadence(config));
    embed_interval.tick().await; // consume the immediate first tick

    // Doc 3 cold-note sweep tick. Default cadence is one week; the
    // report is a checklist for review, not a polling watchdog. The
    // cold sweep is a pure consumer of the index oracle materializes;
    // cortex writes nothing to the `notes` table here, just the report
    // file at system/views/cold-notes.md.
    let mut cold_interval = tokio::time::interval(Duration::from_secs(daemon_config.cold_interval_secs));
    cold_interval.tick().await; // consume the immediate first tick

    // Run a full sweep on startup.
    // block_in_place isolates the blocking CPU+I/O sweep from the tokio worker thread, letting
    // the watcher and timers continue to run; once Phase 1 rayon lands inside scan_vault, this
    // wrap is the boundary between the async runtime and the rayon worker pool.
    log::info!("running initial full sweep");
    applying.store(true, Ordering::Relaxed);
    let mut last_fingerprint =
        tokio::task::block_in_place(|| configured_actions(vault_root, config, daemon_config, &[]));
    applying.store(false, Ordering::Relaxed);

    loop {
        tokio::select! {
            Some(change) = watch_rx.recv() => {
                // VaultWatcher already debounced and filtered - process immediately
                let pending: Vec<PathBuf> = change.changed_paths.iter()
                    .map(|p| p.strip_prefix(vault_root).unwrap_or(p).to_path_buf())
                    .collect();
                log::info!("processing changes: {} file(s)", pending.len());
                for path in &pending {
                    log::info!("  changed: {}", path.display());
                }
                applying.store(true, Ordering::Relaxed);
                let fingerprint = tokio::task::block_in_place(|| {
                    configured_actions(vault_root, config, daemon_config, &pending)
                });
                applying.store(false, Ordering::Relaxed);
                // Real user edit - reset cycle detection so periodic sweeps re-enable
                last_fingerprint = if fingerprint.is_empty() { SweepFingerprint::default() } else { fingerprint };
                // Reset sweep interval after processing changes
                sweep_interval.reset();
            }
            _ = sweep_interval.tick() => {
                // Periodic full sweep with cycle detection
                if !last_fingerprint.is_empty() {
                    let actions_desc: Vec<_> = last_fingerprint.results.iter().map(|(a, f)| format!("{a}: {} files", f.len())).collect();
                    log::warn!("cycle detected: previous sweep had fixes, skipping to avoid oscillation: {:?}", actions_desc);
                    // Don't run most actions - last sweep applied fixes, so running again risks repeating them.
                    // Exception: classify is inherently idempotent (marks notes cortex-classified: true),
                    // so it can never cause a cycle and must always run to promote new inbox notes.
                    tokio::task::block_in_place(|| classify_only(vault_root, config, daemon_config));
                    // A real user edit will reset last_fingerprint and re-enable sweeps.
                } else {
                    log::info!("running periodic sweep");
                    applying.store(true, Ordering::Relaxed);
                    let fingerprint = tokio::task::block_in_place(|| {
                        configured_actions(vault_root, config, daemon_config, &[])
                    });
                    applying.store(false, Ordering::Relaxed);
                    last_fingerprint = fingerprint;
                }
            }
            () = &mut daily => {
                // Scheduled daily intel
                log::info!("running scheduled daily intel");
                let opts = crate::opts::IntelOpts {
                    mode: crate::intel::IntelMode::Daily,
                    output: None,
                };
                if let Err(e) = tokio::task::block_in_place(|| crate::intel::run(vault_root, config, &opts)) {
                    log::error!("scheduled daily intel failed: {e}");
                }
                // Reschedule for next day
                if let Some(time_str) = &daemon_config.daily_at {
                    let next = duration_until_next(time_str);
                    log::info!("daily intel rescheduled: next in {}s", next.as_secs());
                    daily.as_mut().reset(Instant::now() + next);
                }
            }
            () = &mut weekly => {
                // Scheduled weekly intel
                log::info!("running scheduled weekly intel");
                let opts = crate::opts::IntelOpts {
                    mode: crate::intel::IntelMode::Weekly,
                    output: None,
                };
                if let Err(e) = tokio::task::block_in_place(|| crate::intel::run(vault_root, config, &opts)) {
                    log::error!("scheduled weekly intel failed: {e}");
                }
                // Reschedule for next week
                if let Some(schedule_str) = &daemon_config.weekly_at {
                    let next = duration_until_next(schedule_str);
                    log::info!("weekly intel rescheduled: next in {}s", next.as_secs());
                    weekly.as_mut().reset(Instant::now() + next);
                }
            }
            _ = embed_interval.tick() => {
                // Phase A5 / B2 embed tick. block_in_place because the
                // embed loop runs SQLite IO + fastembed CPU inference
                // (when there are stale rows); we don't want to starve
                // the watcher or the scheduled-intel timers if the
                // embedder is currently chewing on a batch.
                match tokio::task::block_in_place(|| crate::embed::daemon_tick(vault_root, config)) {
                    Ok(stats) if stats.scanned > 0 => {
                        log::info!(
                            "daemon embed tick: scanned={} embedded={} skipped_empty={} failed={}",
                            stats.scanned, stats.embedded, stats.skipped_empty, stats.failed,
                        );
                    }
                    Ok(_) => {
                        // Idle tick - nothing to embed. Stay quiet to keep the log readable.
                    }
                    Err(e) => log::error!("daemon embed tick failed: {e}"),
                }
            }
            _ = cold_interval.tick() => {
                // Doc 3 cold-note sweep tick. block_in_place because
                // daemon_cold_tick opens a SQLite connection and does a
                // single SELECT + atomic write; bounded and fast, but
                // blocking. Matches the embed-tick shape so the
                // daemon's select! arms stay symmetric.
                log::info!("running periodic cold-note sweep");
                match tokio::task::block_in_place(|| crate::sweep::daemon_cold_tick(vault_root, config)) {
                    Ok(stats) => log::info!(
                        "daemon cold sweep: scanned={} surfaced={} pinned_excluded={}",
                        stats.scanned, stats.surfaced, stats.pinned_excluded,
                    ),
                    Err(e) => log::error!("daemon cold sweep failed: {e}"),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                log::info!("received shutdown signal; shutting down daemon");
                break;
            }
        }
    }

    drop(watcher);
    Ok(())
}

/// Run classify only - used during cycle detection since classify is inherently idempotent
/// (notes are marked cortex-classified: true and never reprocessed).
fn classify_only(vault_root: &Path, config: &Config, daemon_config: &DaemonConfig) {
    if !daemon_config.enabled_actions().contains(&"classify") {
        return;
    }
    let opts = crate::opts::ClassifyOpts {
        apply: true,
        path: None,
        force: false,
        review_only: false,
        reclassify_domain: None,
    };
    match crate::classify::run(vault_root, config, &opts) {
        Ok(report) => {
            let promoted = report
                .violations
                .iter()
                .filter(|v| v.message.contains("promoted"))
                .count();
            if promoted > 0 {
                log::info!("classify (cycle-exempt): promoted {promoted} note(s)");
                log::info!("[daemon] classify: promoted {promoted} note(s) from inbox/");
            }
        }
        Err(e) => log::error!("classify action failed: {e}"),
    }
}

/// Run the configured on-change actions, returning a fingerprint of what was applied.
fn configured_actions(
    vault_root: &Path,
    config: &Config,
    daemon_config: &DaemonConfig,
    changed_files: &[PathBuf],
) -> SweepFingerprint {
    let mut action_names: Vec<&str> = daemon_config.enabled_actions();
    // Ensure classify runs first - it moves files, other actions need the final locations
    action_names.sort_by_key(|a| if *a == "classify" { 0 } else { 1 });
    log::info!("running configured actions: {:?}", action_names);
    let mut fingerprint = SweepFingerprint::default();

    for action in &action_names {
        match *action {
            "classify" => {
                // Classify runs first - moves inbox notes to notes/ before other actions
                let opts = crate::opts::ClassifyOpts {
                    apply: true,
                    path: None,
                    force: false,
                    review_only: false,
                    reclassify_domain: None,
                };
                match crate::classify::run(vault_root, config, &opts) {
                    Ok(report) => {
                        let promoted = report
                            .violations
                            .iter()
                            .filter(|v| v.message.contains("promoted"))
                            .count();
                        if promoted > 0 {
                            fingerprint.add("classify", vec!["__applied__".to_string()]);
                            log::info!("classify: promoted {promoted} note(s)");
                            log::info!("[daemon] classify: promoted {promoted} note(s) from inbox/");
                        }
                    }
                    Err(e) => log::error!("classify action failed: {e}"),
                }
            }
            "lint" => {
                let auto = daemon_config.is_enabled("lint");
                let opts = crate::opts::LintOpts {
                    apply: auto,
                    format: crate::opts::LintFormat::Human,
                    rule: Vec::new(),
                    path: None,
                };
                match crate::lint(vault_root, config, &opts) {
                    Ok(report) => {
                        if auto {
                            // Only mark as applied when violations were found (some may have been fixed).
                            // Previously this was unconditional, which permanently triggered cycle detection.
                            if !report.is_empty() {
                                fingerprint.add("lint", vec!["__applied__".to_string()]);
                                let remaining = report.violations.len();
                                log::info!("lint: applied fixes ({remaining} unfixable violation(s) remain)");
                            }
                        } else if !report.is_empty() {
                            log::info!("[daemon] lint: {} violation(s)", report.violations.len());
                        }
                    }
                    Err(e) => log::error!("lint action failed: {e}"),
                }
            }
            "broken-links" => {
                let notes = match crate::vault::scan_vault(vault_root, &config.vault) {
                    Ok(n) => n,
                    Err(e) => {
                        log::error!("failed to scan vault for broken links: {e}");
                        continue;
                    }
                };
                let report = crate::links::lint_broken_links(&notes, &notes, &config.actions.broken_links);
                if !report.is_empty() {
                    log::info!("[daemon] broken-links: {} violation(s)", report.violations.len());
                }
            }
            "link" => {
                let auto = daemon_config.is_enabled("link");
                if auto {
                    // Lint first to check if there's work, then apply only if needed.
                    // Previously fingerprinted unconditionally, permanently triggering cycle detection.
                    let lint_opts = crate::opts::LinkOpts {
                        apply: false,
                        scan: crate::opts::ScanScope::All,
                    };
                    match crate::link(vault_root, config, &lint_opts) {
                        Ok(report) if !report.is_empty() => {
                            let apply_opts = crate::opts::LinkOpts {
                                apply: true,
                                scan: crate::opts::ScanScope::All,
                            };
                            match crate::link(vault_root, config, &apply_opts) {
                                Ok(_) => {
                                    fingerprint.add("link", vec!["__applied__".to_string()]);
                                    log::info!("link: applied wikilink fixes");
                                    log::info!("[daemon] link: applied wikilink fixes");
                                }
                                Err(e) => log::error!("link apply failed: {e}"),
                            }
                        }
                        Ok(_) => {}
                        Err(e) => log::error!("link lint failed: {e}"),
                    }
                } else {
                    let opts = crate::opts::LinkOpts {
                        apply: false,
                        scan: crate::opts::ScanScope::All,
                    };
                    match crate::link(vault_root, config, &opts) {
                        Ok(report) if !report.is_empty() => {
                            log::info!("[daemon] link: {} suggestion(s)", report.violations.len());
                        }
                        Ok(_) => {}
                        Err(e) => log::error!("link action failed: {e}"),
                    }
                }
            }
            "duplicates" => {
                let auto = daemon_config.is_enabled("duplicates");
                match crate::vault::scan_vault(vault_root, &config.vault) {
                    Ok(notes) => {
                        if auto {
                            match crate::duplicates::apply_duplicates(vault_root, &notes, &config.actions.duplicates) {
                                Ok(count) if count > 0 => {
                                    fingerprint.add("duplicates", vec!["__applied__".to_string()]);
                                    log::info!("auto-applied duplicates: {count} fix(es)");
                                    log::info!("[daemon] auto-applied duplicates: {count} fix(es)");
                                }
                                Ok(_) => {}
                                Err(e) => log::error!("duplicates apply failed: {e}"),
                            }
                        } else {
                            let report = crate::duplicates::lint_duplicates(&notes, &config.actions.duplicates);
                            if !report.is_empty() {
                                log::info!("[daemon] duplicates: {} violation(s)", report.violations.len());
                            }
                        }
                    }
                    Err(e) => log::error!("failed to scan vault for duplicates: {e}"),
                }
            }
            "auto-tag" => {
                let auto = daemon_config.is_enabled("auto-tag");
                match crate::vault::scan_vault(vault_root, &config.vault) {
                    Ok(notes) => {
                        if auto {
                            match crate::autotag::apply_autotag(vault_root, &notes, &notes, &config.actions.auto_tag) {
                                Ok(count) if count > 0 => {
                                    fingerprint.add("auto-tag", vec!["__applied__".to_string()]);
                                    log::info!("auto-applied auto-tag: {count} fix(es)");
                                    log::info!("[daemon] auto-applied auto-tag: {count} fix(es)");
                                }
                                Ok(_) => {}
                                Err(e) => log::error!("auto-tag apply failed: {e}"),
                            }
                        } else {
                            let report = crate::autotag::lint_autotag(&notes, &notes, &config.actions.auto_tag);
                            if !report.is_empty() {
                                log::info!("[daemon] auto-tag: {} suggestion(s)", report.violations.len());
                            }
                        }
                    }
                    Err(e) => log::error!("failed to scan vault for auto-tag: {e}"),
                }
            }
            "quality" => {
                let auto = daemon_config.is_enabled("quality");
                match crate::vault::scan_vault(vault_root, &config.vault) {
                    Ok(notes) => {
                        if auto {
                            match crate::quality::apply_quality(vault_root, &notes, &config.actions.quality) {
                                Ok(count) if count > 0 => {
                                    fingerprint.add("quality", vec!["__applied__".to_string()]);
                                    log::info!("auto-applied quality: {count} fix(es)");
                                    log::info!("[daemon] auto-applied quality: {count} fix(es)");
                                }
                                Ok(_) => {}
                                Err(e) => log::error!("quality apply failed: {e}"),
                            }
                        } else {
                            let report = crate::quality::lint_quality(&notes, &config.actions.quality);
                            if !report.is_empty() {
                                log::info!("[daemon] quality: {} violation(s)", report.violations.len());
                            }
                        }
                    }
                    Err(e) => log::error!("failed to scan vault for quality: {e}"),
                }
            }
            "intel" => {
                let opts = crate::opts::IntelOpts {
                    mode: crate::intel::IntelMode::Daily,
                    output: None,
                };
                if let Err(e) = crate::intel::run(vault_root, config, &opts) {
                    log::error!("intel action failed: {e}");
                }
            }
            "state" => {
                let opts = crate::opts::StateOpts {
                    refresh: true,
                    diff: false,
                };
                if let Err(e) = crate::state::run(vault_root, config, &opts) {
                    log::error!("state action failed: {e}");
                }
            }
            "sweep" => {
                let auto = daemon_config.is_enabled("sweep");
                match crate::vault::scan_vault(vault_root, &config.vault) {
                    Ok(notes) => {
                        if auto {
                            // Run migration (rewrite non-canonical tags)
                            match crate::sweep::migrate(vault_root, &notes, &config.sweep, false) {
                                Ok(count) if count > 0 => {
                                    fingerprint.add("sweep", vec!["__applied__".to_string()]);
                                    log::info!("sweep: migrated tags in {count} note(s)");
                                    log::info!("[daemon] sweep: migrated tags in {count} note(s)");
                                }
                                Ok(_) => {}
                                Err(e) => log::error!("sweep migrate failed: {e}"),
                            }
                        }
                        // Always scan for proposals (even if not auto-applying)
                        match crate::sweep::scan_proposals(&notes, &config.sweep) {
                            Ok(proposals) if !proposals.is_empty() => {
                                log::info!("sweep: {} tag(s) needing review", proposals.len());
                                if let Err(e) = crate::sweep::write_proposals(&config.sweep, proposals) {
                                    log::error!("sweep: failed to write proposals: {e}");
                                }
                            }
                            Ok(_) => {}
                            Err(e) => log::error!("sweep proposals scan failed: {e}"),
                        }
                    }
                    Err(e) => log::error!("failed to scan vault for sweep: {e}"),
                }
            }
            other => {
                log::warn!("unknown daemon action: {other}");
            }
        }
    }

    log::info!("daemon action cycle complete: {} changed file(s)", changed_files.len());
    fingerprint
}

/// Install a systemd user service for the daemon. Returns the lines sb
/// should print (paths written, follow-up systemctl commands).
fn install_systemd_service(vault_root: &Path, config: &Config) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    let service_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("systemd")
        .join("user");

    std::fs::create_dir_all(&service_dir).context("failed to create systemd user dir")?;

    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let binary = std::env::current_exe().context("failed to get current executable path")?;
    let vault = vault_root.display();

    let mut config_flag = String::new();
    let config_path = vault::paths::cortex_config();
    if config_path.exists() {
        config_flag = format!(" --config {}", config_path.display());
    }

    let log_level = &config.log_level;

    let service = format!(
        "[Unit]\n\
         Description=cortex - Obsidian vault governance daemon (second-brain)\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         Environment=\"PATH={home}/.local/bin:{home}/.cargo/bin:{home}/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\"\n\
         ExecStart={binary} cortex{config_flag} --vault {vault} --log-level {log_level} daemon --start\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         WorkingDirectory={home}\n\
         \n\
         # Hardening\n\
         NoNewPrivileges=true\n\
         ProtectSystem=strict\n\
         ProtectHome=read-only\n\
         ReadWritePaths={vault}\n\
         PrivateTmp=true\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        home = home.display(),
        binary = binary.display(),
    );

    let service_path = service_dir.join("cortex.service");
    std::fs::write(&service_path, &service)?;
    lines.push(format!("Installed: {}", service_path.display()));

    // Daily intel timer - runs at 23:00 every day
    let daily_service = format!(
        "[Unit]\n\
         Description=cortex daily intel\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         Environment=\"PATH={home}/.local/bin:{home}/.cargo/bin:{home}/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\"\n\
         ExecStart={binary} cortex{config_flag} --vault {vault} intel --daily\n",
        home = home.display(),
        binary = binary.display(),
    );

    let daily_timer = "[Unit]\n\
         Description=cortex daily intel timer\n\
         \n\
         [Timer]\n\
         OnCalendar=*-*-* 23:00:00\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n";

    let daily_svc_path = service_dir.join("cortex-daily.service");
    let daily_timer_path = service_dir.join("cortex-daily.timer");
    std::fs::write(&daily_svc_path, daily_service)?;
    std::fs::write(&daily_timer_path, daily_timer)?;
    lines.push(format!("Installed: {}", daily_svc_path.display()));
    lines.push(format!("Installed: {}", daily_timer_path.display()));

    // Weekly intel timer - runs Sunday at 22:00
    let weekly_service = format!(
        "[Unit]\n\
         Description=cortex weekly intel\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         Environment=\"PATH={home}/.local/bin:{home}/.cargo/bin:{home}/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\"\n\
         ExecStart={binary} cortex{config_flag} --vault {vault} intel --weekly\n",
        home = home.display(),
        binary = binary.display(),
    );

    let weekly_timer = "[Unit]\n\
         Description=cortex weekly intel timer\n\
         \n\
         [Timer]\n\
         OnCalendar=Sun *-*-* 22:00:00\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n";

    let weekly_svc_path = service_dir.join("cortex-weekly.service");
    let weekly_timer_path = service_dir.join("cortex-weekly.timer");
    std::fs::write(&weekly_svc_path, weekly_service)?;
    std::fs::write(&weekly_timer_path, weekly_timer)?;
    lines.push(format!("Installed: {}", weekly_svc_path.display()));
    lines.push(format!("Installed: {}", weekly_timer_path.display()));

    lines.push(String::new());
    lines.push("Run:".to_string());
    lines.push("  systemctl --user daemon-reload".to_string());
    lines.push("  systemctl --user enable --now cortex".to_string());
    lines.push("  systemctl --user enable --now cortex-daily.timer".to_string());
    lines.push("  systemctl --user enable --now cortex-weekly.timer".to_string());

    Ok(lines)
}

/// Uninstall the systemd user service and timer units. Returns the lines sb
/// should print.
fn uninstall_systemd_service() -> Result<Vec<String>> {
    let service_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("systemd")
        .join("user");

    let units = [
        "cortex.service",
        "cortex-daily.service",
        "cortex-daily.timer",
        "cortex-weekly.service",
        "cortex-weekly.timer",
    ];

    let mut lines = Vec::new();
    let mut removed = false;
    for unit in &units {
        let path = service_dir.join(unit);
        if path.exists() {
            std::fs::remove_file(&path)?;
            lines.push(format!("Removed: {}", path.display()));
            removed = true;
        }
    }

    if removed {
        lines.push("Run: systemctl --user daemon-reload".to_string());
    } else {
        lines.push("No service files found".to_string());
    }

    Ok(lines)
}

/// Show daemon status by shelling out to `systemctl --user status cortex --no-pager`,
/// mirroring borg's `--status` pattern. Returns the lines sb should print.
fn show_status() -> Result<Vec<String>> {
    let service_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("systemd")
        .join("user")
        .join("cortex.service");

    if !service_path.exists() {
        return Ok(vec![
            "Daemon not installed. Run: sb cortex daemon --install".to_string(),
        ]);
    }

    let output = std::process::Command::new("systemctl")
        .args(["--user", "status", "cortex", "--no-pager"])
        .output()
        .context("Failed to run systemctl")?;
    // systemctl status returns non-zero when the unit is inactive/failed; the
    // stdout text is still what the user wants to read.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<String> = stdout.lines().map(|s| s.to_string()).collect();
    if lines.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(stderr.lines().map(|s| s.to_string()).collect())
    } else {
        Ok(lines)
    }
}

/// Convert a human-friendly schedule string to a cron expression.
///
/// Supported formats:
///   "M-F 07:00"       -> "0 7 * * 1-5"
///   "Mon-Fri 07:00"   -> "0 7 * * 1-5"
///   "Sat-Sun 10:00"   -> "0 10 * * 0,6"
///   "Sun 22:00"       -> "0 22 * * 0"
///   "Mon 09:30"       -> "30 9 * * 1"
///   "07:00"           -> "0 7 * * *"
pub fn schedule_to_cron(schedule: &str) -> String {
    let parts: Vec<&str> = schedule.split_whitespace().collect();
    let (day_part, time_part) = if parts.len() >= 2 {
        (Some(parts[0]), parts[1])
    } else {
        (None, parts.first().copied().unwrap_or("00:00"))
    };

    let time_parts: Vec<&str> = time_part.split(':').collect();
    let hour = time_parts.first().copied().unwrap_or("0");
    let minute = time_parts.get(1).copied().unwrap_or("0");

    let dow = match day_part {
        None => "*".to_string(),
        Some(d) => day_spec_to_cron(d),
    };

    format!("{minute} {hour} * * {dow}")
}

/// Convert a day specifier to cron day-of-week field.
///
/// Supports: single days (Mon, Tue, Sun), ranges (M-F, Mon-Fri, Sat-Sun),
/// and common abbreviations.
fn day_spec_to_cron(spec: &str) -> String {
    // Check for range (e.g., "M-F", "Mon-Fri", "Sat-Sun")
    if let Some((start, end)) = spec.split_once('-') {
        let start_num = day_to_cron_num(start);
        let end_num = day_to_cron_num(end);
        if start_num <= end_num {
            format!("{start_num}-{end_num}")
        } else {
            // Wrap around (e.g., Fri-Mon -> 5,6,0,1)
            let mut days: Vec<u8> = Vec::new();
            let mut d = start_num;
            loop {
                days.push(d);
                if d == end_num {
                    break;
                }
                d = (d + 1) % 7;
            }
            days.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(",")
        }
    } else {
        day_to_cron_num(spec).to_string()
    }
}

/// Map day name/abbreviation to cron number (0=Sun, 1=Mon, ..., 6=Sat).
fn day_to_cron_num(day: &str) -> u8 {
    match day.to_lowercase().as_str() {
        "m" | "mon" | "monday" => 1,
        "t" | "tu" | "tue" | "tuesday" => 2,
        "w" | "wed" | "wednesday" => 3,
        "th" | "thu" | "thursday" => 4,
        "f" | "fri" | "friday" => 5,
        "sa" | "sat" | "saturday" => 6,
        "su" | "sun" | "sunday" => 0,
        _ => 0,
    }
}

/// Compute Duration until the next occurrence of a human-friendly schedule.
///
/// Uses croner to parse the translated cron expression and find the next match.
pub fn duration_until_next(schedule: &str) -> Duration {
    let cron_expr = schedule_to_cron(schedule);
    let cron = match croner::Cron::from_str(&cron_expr) {
        Ok(c) => c,
        Err(e) => {
            log::error!("invalid schedule expression: schedule={schedule}, cron={cron_expr}, error={e}");
            return Duration::MAX;
        }
    };

    let now = chrono::Local::now();
    match cron.find_next_occurrence(&now, false) {
        Ok(next) => {
            let dur: chrono::Duration = next - now;
            dur.to_std().unwrap_or(Duration::from_secs(3600))
        }
        Err(e) => {
            log::error!("could not find next occurrence: schedule={schedule}, error={e}");
            Duration::MAX
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DaemonConfig;
    use chrono::Datelike;

    #[test]
    fn test_is_enabled_default_is_false() {
        let config = DaemonConfig::default();
        assert!(!config.is_enabled("lint"));
        assert!(!config.is_enabled("link"));
        assert!(!config.is_enabled("nonexistent"));
    }

    #[test]
    fn test_is_enabled_explicit_true() {
        let mut config = DaemonConfig::default();
        config
            .actions
            .insert("lint".to_string(), crate::config::DaemonAction { enable: true });
        assert!(config.is_enabled("lint"));
        assert!(!config.is_enabled("link"));
    }

    #[test]
    fn test_is_enabled_explicit_false() {
        let config = DaemonConfig::default();
        // lint is in default actions but enable defaults to false
        assert!(!config.is_enabled("lint"));
    }

    #[test]
    fn test_enabled_actions() {
        let config = DaemonConfig::default();
        let actions = config.enabled_actions();
        assert!(actions.contains(&"lint"));
        assert!(actions.contains(&"broken-links"));
    }

    #[test]
    fn test_daemon_config_deserialize_actions() {
        let yaml =
            "actions:\n  lint:\n    enable: true\n  broken-links: {}\n  link:\n    enable: false\ndebounce-secs: 10\n";
        let config: DaemonConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(config.debounce_secs, 10);
        assert!(config.is_enabled("lint"));
        assert!(!config.is_enabled("broken-links"));
        assert!(!config.is_enabled("link"));
        assert!(!config.is_enabled("nonexistent"));
        assert_eq!(config.actions.len(), 3);
    }

    #[test]
    fn test_sweep_fingerprint_empty_default() {
        let fp = SweepFingerprint::default();
        assert!(fp.is_empty());
    }

    #[test]
    fn test_sweep_fingerprint_non_empty() {
        let mut fp = SweepFingerprint::default();
        fp.add("lint", vec!["a.md".to_string(), "b.md".to_string()]);
        assert!(!fp.is_empty());
    }

    #[test]
    fn test_sweep_fingerprint_equality() {
        let mut fp1 = SweepFingerprint::default();
        fp1.add("lint", vec!["b.md".to_string(), "a.md".to_string()]);

        let mut fp2 = SweepFingerprint::default();
        fp2.add("lint", vec!["a.md".to_string(), "b.md".to_string()]);

        // Both should sort to the same order
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_sweep_fingerprint_different_files() {
        let mut fp1 = SweepFingerprint::default();
        fp1.add("lint", vec!["a.md".to_string()]);

        let mut fp2 = SweepFingerprint::default();
        fp2.add("lint", vec!["b.md".to_string()]);

        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_sweep_fingerprint_empty_files_ignored() {
        let mut fp = SweepFingerprint::default();
        fp.add("lint", vec![]);
        assert!(fp.is_empty());
    }

    #[test]
    fn test_duration_until_daily_future_today() {
        // If we ask for a time that hasn't passed yet today, it should be today (on weekdays)
        // or next Monday (on weekends)
        let now = chrono::Local::now();
        let future_hour = (now.format("%H").to_string().parse::<u32>().unwrap_or(0) + 1) % 24;
        let time_str = format!("{future_hour:02}:00");
        let dur = duration_until_next(&time_str);
        // Should be within 3 days (worst case: Saturday -> Monday)
        assert!(dur < Duration::from_secs(3 * 24 * 3600));
        assert!(dur > Duration::ZERO);
    }

    #[test]
    fn test_duration_until_daily_already_passed() {
        // If we ask for a time that already passed, it should be next weekday
        let now = chrono::Local::now();
        let past_hour = if now.format("%H").to_string().parse::<u32>().unwrap_or(0) > 0 {
            now.format("%H").to_string().parse::<u32>().unwrap_or(0) - 1
        } else {
            23
        };
        let time_str = format!("{past_hour:02}:00");
        let dur = duration_until_next(&time_str);
        // Should be within 3 days (worst case: Friday past -> Monday)
        assert!(dur > Duration::ZERO);
        assert!(dur <= Duration::from_secs(3 * 24 * 3600));
    }

    #[test]
    fn test_duration_until_weekday_schedule() {
        // "M-F 12:00" should always land on a weekday (Mon-Fri)
        let dur = duration_until_next("M-F 12:00");
        let now = chrono::Local::now();
        let target = now + chrono::Duration::from_std(dur).expect("valid duration");
        let weekday = target.weekday();
        assert!(
            matches!(
                weekday,
                chrono::Weekday::Mon
                    | chrono::Weekday::Tue
                    | chrono::Weekday::Wed
                    | chrono::Weekday::Thu
                    | chrono::Weekday::Fri
            ),
            "M-F schedule should only fire on weekdays, got {weekday:?}"
        );
    }

    #[test]
    fn test_duration_until_weekly_returns_valid_duration() {
        let dur = duration_until_next("Sun 22:00");
        // Should be within 7 days
        assert!(dur <= Duration::from_secs(7 * 24 * 3600));
        assert!(dur > Duration::ZERO);
    }

    #[test]
    fn test_duration_until_weekly_all_days() {
        for day in &["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] {
            let schedule = format!("{day} 12:00");
            let dur = duration_until_next(&schedule);
            assert!(dur <= Duration::from_secs(7 * 24 * 3600), "failed for {day}");
            assert!(dur > Duration::ZERO, "failed for {day}");
        }
    }

    #[test]
    fn test_daemon_config_deserialize_schedule_fields() {
        let yaml = "daily-at: \"23:00\"\nweekly-at: \"Sun 22:00\"\n";
        let config: DaemonConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(config.daily_at.as_deref(), Some("23:00"));
        assert_eq!(config.weekly_at.as_deref(), Some("Sun 22:00"));
    }

    #[test]
    fn test_schedule_to_cron_weekdays() {
        assert_eq!(schedule_to_cron("M-F 07:00"), "00 07 * * 1-5");
        assert_eq!(schedule_to_cron("Mon-Fri 07:00"), "00 07 * * 1-5");
    }

    #[test]
    fn test_schedule_to_cron_single_day() {
        assert_eq!(schedule_to_cron("Sun 22:00"), "00 22 * * 0");
        assert_eq!(schedule_to_cron("Mon 09:30"), "30 09 * * 1");
    }

    #[test]
    fn test_schedule_to_cron_weekend() {
        assert_eq!(schedule_to_cron("Sat-Sun 10:00"), "00 10 * * 6,0");
    }

    #[test]
    fn test_schedule_to_cron_bare_time() {
        assert_eq!(schedule_to_cron("07:00"), "00 07 * * *");
    }

    #[test]
    fn test_daemon_config_default_no_schedule() {
        let config = DaemonConfig::default();
        assert!(config.daily_at.is_none());
        assert!(config.weekly_at.is_none());
    }

    // Phase 0 smoke test: scan_vault wrapped in tokio::task::block_in_place runs to completion
    // from a multi-thread tokio runtime without panicking. This is the guardrail for the design
    // doc's Phase 0 wrapping pattern.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scan_vault_inside_block_in_place_does_not_panic() {
        use crate::config::VaultConfig;
        use std::fs;

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(
            root.join("a.md"),
            "---\ndomain: tools\ntype: knowledge\norigin: authored\nstatus: draft\nmethod: cli\n---\n# A\n",
        )
        .expect("write a");
        fs::write(
            root.join("b.md"),
            "---\ndomain: tools\ntype: knowledge\norigin: authored\nstatus: draft\nmethod: cli\n---\n# B\n",
        )
        .expect("write b");

        let vault_config = VaultConfig::default();
        let notes = tokio::task::block_in_place(|| crate::vault::scan_vault(root, &vault_config))
            .expect("scan_vault should succeed");
        assert_eq!(notes.len(), 2, "expected 2 notes from tempdir scan");
    }
}
