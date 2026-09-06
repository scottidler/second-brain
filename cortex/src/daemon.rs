use eyre::{Context, Result};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::Instant;
use vault::watcher::{VaultWatcher, WatcherConfig};

use crate::config::{Config, DaemonConfig, VaultConfig};
use crate::opts::DaemonOpts;
use crate::vault::Note;

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
        // The daemon stops on SIGTERM; there is no IPC channel to signal it
        // from here. Return the instruction as outcome lines so sb PRINTS it -
        // a bare log line never reached the user running `sb cortex daemon --stop`.
        Ok(DaemonOutcome {
            lines: vec![
                "cortex has no stop IPC; stop the running daemon with one of:".to_string(),
                "  systemctl --user stop cortex   (if installed as a systemd unit)".to_string(),
                "  pkill -TERM -f 'sb cortex daemon'   (if started manually)".to_string(),
            ],
        })
    } else {
        // Default: start watching (--start or no flags). Long-running; logs
        // every transition; returns an empty outcome on clean shutdown.
        start_watching(vault_root, config).await?;
        Ok(DaemonOutcome::default())
    }
}

/// Start filesystem watcher and run actions on changes using async tokio::select! loop.
async fn start_watching(vault_root: &Path, config: &Config) -> Result<()> {
    crate::startup::validate_canonical_assets()?;
    let daemon_config = &config.daemon;
    let poll_interval = Duration::from_secs(daemon_config.poll_interval);

    let action_names: Vec<&str> = daemon_config.configured_actions();
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

    // Phase 7b: load the embedding model once at daemon startup and
    // hand it to every tick by reference. The previous per-tick
    // load-and-drop pattern leaked ~30 MB/tick of allocator scratch
    // (shakedown: 1.2 -> 2.8 GB over 50 min); the long-lived model
    // bounds it.
    // Degrade, do not crash-loop: if the embedding model fails to load, the
    // embed tick is disabled for this process but every other governance
    // action (lint/link/sweep/cold/graph/intel) still runs. The `?` here used
    // to take the whole daemon down repeatedly on a model-load failure.
    let embed_model: Option<Box<dyn vault::embedding::EmbeddingModel>> =
        match tokio::task::block_in_place(|| crate::embed::load_daemon_model(config)) {
            Ok(m) => Some(m),
            Err(e) => {
                log::error!(
                    "daemon: embedding model failed to load: {e:#}; embed tick DISABLED for this process \
                     - every non-embed action still runs"
                );
                None
            }
        };

    // Doc 3 cold-note sweep tick. Default cadence is one week; the
    // report is a checklist for review, not a polling watchdog. The
    // cold sweep is a pure consumer of the index oracle materializes;
    // cortex writes nothing to the `notes` table here, just the report
    // file at system/views/cold-notes.md.
    let mut cold_interval = tokio::time::interval(Duration::from_secs(daemon_config.cold_interval_secs));
    cold_interval.tick().await; // consume the immediate first tick

    // Graph-augmented-memory edge pass. Runs on its own cadence, ordered
    // AFTER the embed tick so semantic edges see fresh vectors. The pass takes
    // the same embed file lock, so it cannot interleave with an embed write;
    // its first run after a restart is a full rebuild (no persisted
    // last_run_at), incremental thereafter.
    let mut graph_interval = tokio::time::interval(Duration::from_secs(config.graph.graph_interval_secs));
    graph_interval.tick().await; // consume the immediate first tick

    // Typed-`fact` backfill pass (Phase 5/6). The graph tick above is
    // deterministic-only by design; this is the in-process schedule on which the
    // LLM fact layer (triple extraction + consolidation) refreshes. In-process
    // so it serializes against the embed/graph ticks on the shared embed lock
    // rather than colliding the way a separate-process timer would. Weekly by
    // default; LLM-bound and bounded by `graph.fact_max_per_run`.
    let mut fact_interval = tokio::time::interval(Duration::from_secs(config.graph.fact_interval_secs));
    fact_interval.tick().await; // consume the immediate first tick

    // Association-sweep tick (2026-07-24 cortex-association-sweep design,
    // Phase 5): a NEW periodic interval arm, deliberately NOT folded into the
    // on-change `configured_actions` loop - a merge is soft-retiring
    // (destructive-ish), so it must run on its own slow cadence, never on
    // every debounced watcher event. Gated by `is_enabled("association")`,
    // default OFF (the action is absent from `DaemonConfig::default()`'s
    // action map); the tick still fires on cadence but no-ops when disabled,
    // matching the embed-model-load-failure `continue` pattern below.
    let mut association_interval = tokio::time::interval(Duration::from_secs(config.actions.association.interval_secs));
    association_interval.tick().await; // consume the immediate first tick

    // LLM entity-discovery pass (Phase 4). Daily by default; LLM-bound and
    // bounded by `entities.max_per_run`, so it never fans unbounded calls.
    let mut entities_interval = tokio::time::interval(Duration::from_secs(config.entities.discover_interval_secs));
    entities_interval.tick().await; // consume the immediate first tick

    // Run a full sweep on startup.
    // block_in_place isolates the blocking CPU+I/O sweep from the tokio worker thread, letting
    // the watcher and timers continue to run; once Phase 1 rayon lands inside scan_vault, this
    // wrap is the boundary between the async runtime and the rayon worker pool.
    log::info!("running initial full sweep");
    applying.store(true, Ordering::Relaxed);
    let mut last_fingerprint =
        tokio::task::block_in_place(|| configured_actions(vault_root, config, daemon_config, &[]));
    applying.store(false, Ordering::Relaxed);
    // True once two consecutive periodic sweeps produced the IDENTICAL non-empty
    // fingerprint (same actions, same files) - genuine oscillation. A real user
    // edit clears it so periodic sweeps resume.
    let mut oscillating = false;

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
                // Real user edit - clear oscillation latch and re-baseline.
                oscillating = false;
                last_fingerprint = fingerprint;
                // Reset sweep interval after processing changes
                sweep_interval.reset();
            }
            _ = sweep_interval.tick() => {
                // Periodic full sweep with REAL cycle detection: compare this
                // sweep's fingerprint to the previous one. Oscillation is the
                // SAME fixes on the SAME files two sweeps running - not merely
                // "a fix was applied once" (the old placeholder-fingerprint bug
                // that froze periodic sweeps after any single fix).
                if oscillating {
                    log::warn!("oscillation latched: skipping periodic sweep (classify only) until a watcher event");
                    // classify is idempotent (marks cortex-classified: true), so it
                    // can never oscillate and must keep promoting new inbox notes.
                    tokio::task::block_in_place(|| classify_only(vault_root, config, daemon_config));
                } else {
                    log::info!("running periodic sweep");
                    applying.store(true, Ordering::Relaxed);
                    let fingerprint = tokio::task::block_in_place(|| {
                        configured_actions(vault_root, config, daemon_config, &[])
                    });
                    applying.store(false, Ordering::Relaxed);
                    if !fingerprint.is_empty() && fingerprint == last_fingerprint {
                        let actions_desc: Vec<_> = fingerprint.results.iter().map(|(a, f)| format!("{a}: {} files", f.len())).collect();
                        log::warn!("oscillation detected: identical fixes two sweeps in a row {actions_desc:?}; backing off periodic sweeps until a watcher event");
                        oscillating = true;
                    }
                    last_fingerprint = fingerprint;
                }
            }
            () = &mut daily => {
                // Scheduled daily intel
                log::info!("running scheduled daily intel");
                let opts = crate::opts::IntelOpts {
                    mode: crate::intel::IntelMode::Daily,
                    output: None,
                    as_of: None,
                };
                // Wrap in the `applying` guard just like the periodic sweep so
                // the digest write does not fire the watcher, land after the
                // flag flips false, and clear the oscillation latch (the
                // scheduled arms were previously unguarded - a self-write here
                // was one of the paths that re-triggered the watcher).
                applying.store(true, Ordering::Relaxed);
                let intel_result = tokio::task::block_in_place(|| crate::intel::run(vault_root, config, &opts));
                applying.store(false, Ordering::Relaxed);
                if let Err(e) = intel_result {
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
                    as_of: None,
                };
                // Wrap in the `applying` guard just like the periodic sweep (see
                // the daily arm above) so the review write cannot re-trigger the
                // watcher and clear the oscillation latch.
                applying.store(true, Ordering::Relaxed);
                let intel_result = tokio::task::block_in_place(|| crate::intel::run(vault_root, config, &opts));
                applying.store(false, Ordering::Relaxed);
                if let Err(e) = intel_result {
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
                let Some(model) = embed_model.as_deref() else {
                    // Model failed to load at startup; embed tick is disabled
                    // for this process. Other actions keep running.
                    continue;
                };
                match tokio::task::block_in_place(|| crate::embed::daemon_tick_with_model(vault_root, config, model)) {
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
            _ = graph_interval.tick() => {
                // Graph edge pass. block_in_place because the build runs
                // SQLite IO + brute-force cosine (when notes changed); it must
                // not starve the watcher/timers. Ordered after the embed tick
                // via cadence so semantic edges see fresh vectors.
                match tokio::task::block_in_place(|| crate::graph::daemon_tick(vault_root, config)) {
                    Ok(stats) if stats.notes_processed > 0 => log::info!(
                        "daemon graph tick: full_rebuild={} notes={} semantic={} wikilink={} shared_tag={} metadata={} repo_member={} creator_member={} source_member={}",
                        stats.full_rebuild, stats.notes_processed, stats.semantic, stats.wikilink, stats.shared_tag, stats.metadata,
                        stats.repo_member, stats.creator_member, stats.source_member,
                    ),
                    Ok(_) => {
                        // Idle tick - no changed notes. Stay quiet.
                    }
                    Err(e) => log::error!("daemon graph tick failed: {e}"),
                }
            }
            _ = fact_interval.tick() => {
                // Scheduled typed-`fact` backfill (Phase 5/6). block_in_place
                // because it runs blocking Fabric subprocess calls (triple
                // extraction) + SQLite IO; bounded by graph.fact_max_per_run.
                // Takes the embed lock in-process so it cannot collide with the
                // embed/graph ticks.
                match tokio::task::block_in_place(|| crate::graph::fact_backfill(vault_root, config)) {
                    Ok(stats) => log::info!(
                        "daemon fact tick: facts_written={} noise_removed={} contradictions={} bridges_added={}",
                        stats.facts_written, stats.noise_removed, stats.contradictions, stats.bridges_added,
                    ),
                    Err(e) => log::error!("daemon fact tick failed: {e}"),
                }
            }
            _ = association_interval.tick() => {
                // Association-sweep tick (2026-07-24 cortex-association-sweep
                // design, Phase 5). Disabled by default; the tick still fires
                // on cadence but is a no-op unless explicitly enabled, so
                // flipping the config on takes effect within one cadence
                // window with no daemon restart required.
                if !daemon_config.is_enabled("association") {
                    continue;
                }
                // block_in_place: opens its own SQLite connection and takes
                // the shared embed lock (same shape as the graph tick), so it
                // must not starve the watcher/timers while doing blocking IO.
                match tokio::task::block_in_place(|| crate::association::daemon_tick(vault_root, config)) {
                    Ok(report) => {
                        let outcomes = report.outcomes();
                        let merges = outcomes
                            .iter()
                            .filter(|o| matches!(o, crate::association::AssociationOutcome::Merge { .. }))
                            .count();
                        let cross_links = outcomes.len() - merges;
                        if outcomes.is_empty() {
                            log::debug!("daemon association tick: nothing to associate");
                        } else {
                            log::info!(
                                "daemon association tick: merges={merges} cross_links={cross_links}",
                            );
                        }
                    }
                    Err(e) => log::error!("daemon association tick failed: {e}"),
                }
            }
            _ = entities_interval.tick() => {
                // Entity-discovery tick (Phase 4). block_in_place because the
                // pass runs blocking Fabric subprocess calls; bounded by
                // entities.max_per_run.
                match tokio::task::block_in_place(|| crate::entities::daemon_tick(vault_root, config)) {
                    Ok(report) if report.proposals > 0 => log::info!(
                        "daemon entities tick: scanned={} proposals={}",
                        report.notes_scanned, report.proposals,
                    ),
                    Ok(_) => {
                        // No new proposals. Stay quiet.
                    }
                    Err(e) => log::error!("daemon entities tick failed: {e}"),
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
            _ = shutdown_signal() => {
                log::info!("received shutdown signal; shutting down daemon");
                break;
            }
        }
    }

    drop(watcher);
    Ok(())
}

/// Resolve when the daemon should shut down: Ctrl-C (SIGINT) or SIGTERM.
/// systemd stops a unit with SIGTERM; the previous `ctrl_c()`-only arm
/// ignored it, so the daemon was killed mid-write instead of breaking the
/// loop and dropping the watcher cleanly.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
            }
            Err(e) => {
                log::warn!("daemon: failed to install SIGTERM handler: {e}; relying on Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Run classify only - used during cycle detection since classify is inherently idempotent
/// (notes are marked cortex-classified: true and never reprocessed).
fn classify_only(vault_root: &Path, config: &Config, daemon_config: &DaemonConfig) {
    if !daemon_config.configured_actions().contains(&"classify") {
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
        Ok((_report, written)) => {
            if !written.is_empty() {
                log::info!("classify (cycle-exempt): wrote {} note(s)", written.len());
                log::info!("[daemon] classify: wrote {} note(s)", written.len());
            }
        }
        Err(e) => log::error!("classify action failed: {e}"),
    }
}

/// Run the configured on-change actions, returning a fingerprint of what was applied.
/// Production entry point: always scans with the real `crate::vault::scan_vault`.
/// See `configured_actions_with_scanner` for the single-scan-per-cycle logic and
/// the injectable scanner seam tests use to count scan calls.
fn configured_actions(
    vault_root: &Path,
    config: &Config,
    daemon_config: &DaemonConfig,
    changed_files: &[PathBuf],
) -> SweepFingerprint {
    configured_actions_with_scanner(
        vault_root,
        config,
        daemon_config,
        changed_files,
        crate::vault::scan_vault,
    )
}

/// Run the configured on-change actions, returning a fingerprint of what was applied.
///
/// Phase 5 (design doc `2026-07-05-cortex-daemon-oscillation-loop.md`): scan the
/// vault ONCE at the top of the cycle via the injected `scan` and share the
/// resulting `&[Note]` across every action that reads vault-wide state, instead
/// of each action independently re-scanning (previously up to 6+ redundant
/// `scan_vault` calls per cycle over 2500+ notes).
///
/// RESCAN BOUNDARY RULE (explicit, not "behavior unchanged"): a `dirty` flag
/// tracks whether any action run so far in THIS cycle actually wrote to disk.
/// Before any subsequent action consumes the shared note list, if `dirty` is
/// set the list is rescanned and the flag cleared; if not, the cached list is
/// reused. This reproduces the pre-Phase-5 behavior exactly - every action
/// always saw the freshest on-disk state, because every action always
/// re-scanned unconditionally - while skipping the rescan whenever nothing
/// changed. `classify` runs first by design (it MOVES notes to their final
/// locations via promotion), so a non-empty `promoted` list marks the cache
/// dirty before any reader (lint/link/broken-links/duplicates/auto-tag/
/// quality/sweep) runs.
///
/// Scoped to the actions that actually read vault-wide `&[Note]` state today:
/// classify, lint, link, broken-links, duplicates, auto-tag, quality, sweep.
/// `intel` and `state` are deliberately NOT wired into the shared cache here -
/// `intel` keeps its own independent scan (its idempotency/skip-regeneration
/// logic is Phase 2's concern, not this phase's, and folding it in risks a
/// regression there); `state` never calls `scan_vault` at all. If `intel`
/// writes during a cycle that also runs a cache-consuming action, the cache is
/// conservatively marked dirty (see the `"intel"` arm) so a later reader in
/// the same cycle cannot see a stale list.
fn configured_actions_with_scanner<S>(
    vault_root: &Path,
    config: &Config,
    daemon_config: &DaemonConfig,
    changed_files: &[PathBuf],
    mut scan: S,
) -> SweepFingerprint
where
    S: FnMut(&Path, &VaultConfig) -> Result<Vec<Note>>,
{
    let mut action_names: Vec<&str> = daemon_config.configured_actions();
    // Ensure classify runs first - it moves files, other actions need the final locations
    action_names.sort_by_key(|a| if *a == "classify" { 0 } else { 1 });
    log::info!("running configured actions: {:?}", action_names);
    log::debug!(
        "configured_actions_with_scanner: vault_root={} action_count={} changed_file_count={}",
        vault_root.display(),
        action_names.len(),
        changed_files.len()
    );
    let mut fingerprint = SweepFingerprint::default();

    // Single scan at the top of the cycle - the Phase 5 seam.
    let mut notes: Vec<Note> = match scan(vault_root, &config.vault) {
        Ok(n) => n,
        Err(e) => {
            log::error!("failed to scan vault at top of action cycle: {e}");
            return fingerprint;
        }
    };
    // The cache is fresh as of the scan above; nothing has written yet.
    let mut dirty = false;

    for action in &action_names {
        // Rescan boundary: a prior action in this cycle wrote to disk, so the
        // cached `notes` is stale for every reader from here on until refreshed.
        if dirty {
            match scan(vault_root, &config.vault) {
                Ok(n) => notes = n,
                Err(e) => {
                    log::error!("failed to rescan vault mid-cycle: {e}; continuing with the last-known note list");
                }
            }
            dirty = false;
        }

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
                match crate::classify::run_with_notes(&notes, vault_root, config, &opts) {
                    Ok((_report, written)) => {
                        // Fingerprint the paths classify ACTUALLY wrote - the
                        // Phase 1 lint/sweep shape - never a `"promoted"`
                        // substring sniff of violation messages. That old sniff
                        // ignored the two other write paths (`mark_needs_review`
                        // for no-signal/low-confidence inbox notes and catch-up
                        // enrichment for domainless notes/), so those writes
                        // fired the daemon watcher while being invisible to the
                        // fingerprint - reopening the oscillation defect through
                        // the classify arm.
                        if !written.is_empty() {
                            log::info!("classify: wrote {} note(s)", written.len());
                            log::info!("[daemon] classify: wrote {} note(s)", written.len());
                            fingerprint.add("classify", written);
                            // classify MOVES/rewrites notes - every reader after it
                            // in this cycle needs the final state. Rescan boundary.
                            dirty = true;
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
                match crate::lint_with_notes(&notes, vault_root, config, &opts) {
                    Ok((report, lint_apply)) => {
                        if auto {
                            // Fingerprint ONLY the paths the four appliers actually
                            // wrote (`LintApplyReport.written_paths`) - NEVER
                            // `report.violations` paths. Most violations
                            // (`tags.non-canonical`, `frontmatter.date-format`,
                            // etc.) carry `fix: None` and are never written; a
                            // detections-based fingerprint is byte-identical every
                            // cycle and permanently latches oscillation detection.
                            if !lint_apply.written_paths.is_empty() {
                                log::info!(
                                    "lint: applied fixes to {} file(s) ({} unfixable violation(s) remain)",
                                    lint_apply.written_paths.len(),
                                    lint_apply.remaining_violations
                                );
                                fingerprint.add("lint", lint_apply.written_paths);
                                // lint's four appliers rewrite notes in place - the
                                // next reader in this cycle needs those bytes.
                                dirty = true;
                            } else if lint_apply.remaining_violations > 0 {
                                log::info!(
                                    "[daemon] lint: {} violation(s), none writable this cycle",
                                    lint_apply.remaining_violations
                                );
                            }
                        } else if !report.is_empty() {
                            log::info!("[daemon] lint: {} violation(s)", report.violations.len());
                        }
                    }
                    Err(e) => log::error!("lint action failed: {e}"),
                }
            }
            "broken-links" => {
                // Read-only lint - no apply path, so it never dirties the cache.
                let report = crate::links::lint_broken_links(&notes, &notes, &config.actions.broken_links);
                if !report.is_empty() {
                    log::info!("[daemon] broken-links: {} violation(s)", report.violations.len());
                }
            }
            "link" => {
                let auto = daemon_config.is_enabled("link");
                if auto {
                    // Lint first to check if there's work, then apply only if needed -
                    // a cheap gate to skip the apply pass entirely when there is
                    // nothing to check. The gate's suggestion paths are NEVER the
                    // fingerprint: `find_mention` (detection) and
                    // `insert_first_wikilink` (mutation) can disagree, so a
                    // suggestion can be unappliable and never actually write.
                    let lint_opts = crate::opts::LinkOpts {
                        apply: false,
                        scan: crate::opts::ScanScope::All,
                    };
                    match crate::link_with_notes(&notes, vault_root, config, &lint_opts) {
                        Ok(report) if !report.is_empty() => {
                            let apply_opts = crate::opts::LinkOpts {
                                apply: true,
                                scan: crate::opts::ScanScope::All,
                            };
                            match crate::link_with_notes(&notes, vault_root, config, &apply_opts) {
                                Ok(applied_report) => {
                                    // `applied_report.applied_paths` is `apply_linking`'s
                                    // real written-path return - the ONLY thing that may
                                    // feed the fingerprint here.
                                    if !applied_report.applied_paths.is_empty() {
                                        log::info!(
                                            "link: applied wikilink fixes to {} file(s)",
                                            applied_report.applied_paths.len()
                                        );
                                        log::info!(
                                            "[daemon] link: applied wikilink fixes to {} file(s)",
                                            applied_report.applied_paths.len()
                                        );
                                        fingerprint.add("link", applied_report.applied_paths);
                                        // apply_linking rewrote notes in place.
                                        dirty = true;
                                    }
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
                    match crate::link_with_notes(&notes, vault_root, config, &opts) {
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
                if auto {
                    match crate::duplicates::apply_duplicates(vault_root, &notes, &config.actions.duplicates) {
                        Ok(paths) if !paths.is_empty() => {
                            log::info!("auto-applied duplicates: {} fix(es)", paths.len());
                            log::info!("[daemon] auto-applied duplicates: {} fix(es)", paths.len());
                            fingerprint.add("duplicates", paths);
                            dirty = true;
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
            "auto-tag" => {
                let auto = daemon_config.is_enabled("auto-tag");
                if auto {
                    match crate::autotag::apply_autotag(
                        vault_root,
                        &notes,
                        &notes,
                        &config.actions.auto_tag,
                        &config.fabric,
                    ) {
                        Ok(paths) if !paths.is_empty() => {
                            log::info!("auto-applied auto-tag: {} fix(es)", paths.len());
                            log::info!("[daemon] auto-applied auto-tag: {} fix(es)", paths.len());
                            fingerprint.add("auto-tag", paths);
                            dirty = true;
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
            "quality" => {
                let auto = daemon_config.is_enabled("quality");
                if auto {
                    match crate::quality::apply_quality(vault_root, &notes, &config.actions.quality) {
                        Ok(paths) if !paths.is_empty() => {
                            log::info!("auto-applied quality: {} fix(es)", paths.len());
                            log::info!("[daemon] auto-applied quality: {} fix(es)", paths.len());
                            fingerprint.add("quality", paths);
                            dirty = true;
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
            "intel" => {
                let opts = crate::opts::IntelOpts {
                    mode: crate::intel::IntelMode::Daily,
                    output: None,
                    as_of: None,
                };
                // intel keeps its own independent scan_vault call - deliberately
                // NOT wired into the shared cache (see the function doc comment).
                // It CAN write the digest note, so conservatively mark the shared
                // cache dirty on success: `daemon_config.configured_actions()`
                // order comes from a HashMap and is not guaranteed, so intel may
                // run before a cache-consuming reader in this very cycle.
                match crate::intel::run(vault_root, config, &opts) {
                    Ok(_) => dirty = true,
                    Err(e) => log::error!("intel action failed: {e}"),
                }
            }
            "state" => {
                // Never touches vault notes (writes only its own manifest cache
                // under `config.state.cache_dir`) - never dirties the shared cache.
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
                if auto {
                    // Run migration (rewrite non-canonical tags)
                    match crate::sweep::migrate(vault_root, &notes, &config.sweep, false) {
                        Ok(paths) if !paths.is_empty() => {
                            log::info!("sweep: migrated tags in {} note(s)", paths.len());
                            log::info!("[daemon] sweep: migrated tags in {} note(s)", paths.len());
                            fingerprint.add("sweep", paths);
                            dirty = true;
                        }
                        Ok(_) => {}
                        Err(e) => log::error!("sweep migrate failed: {e}"),
                    }
                }
                // Always scan for proposals (even if not auto-applying). Matches
                // pre-Phase-5 behavior: proposals are scanned from the SAME
                // pre-migrate note list migrate() just read (migrate() does not
                // mutate `notes` in place - only the on-disk bytes), not a
                // freshly rescanned one.
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
            "association" => {
                // Deliberately a no-op here: `daemon.actions.association.enable`
                // is read by the SEPARATE `association_interval` tick above via
                // `is_enabled("association")`, never by this on-change loop - a
                // merge/cross-link pass must run on its own slow cadence, not on
                // startup or every debounced watcher event (2026-07-24
                // cortex-association-sweep design, Phase 5). This arm exists
                // solely so registering the action in `daemon.actions` does not
                // fall through to the `unknown daemon action` warning below.
            }
            other => {
                log::warn!("unknown daemon action: {other}");
            }
        }
    }

    log::info!("daemon action cycle complete: {} changed file(s)", changed_files.len());
    fingerprint
}

/// Render the `cortex.service` unit content. Pure - no filesystem or
/// environment access beyond the args given - so `install_systemd_service`
/// and its tests share one seam: tests assert on the returned string instead
/// of touching the real `~/.config/systemd/user/`.
///
/// Emits the secret-bootstrap `ExecStartPre` + `EnvironmentFile` and the
/// rayon cap from `config.daemon` when configured (2026-07-05
/// cortex-daemon-oscillation-loop design doc, Phase 6: the live unit had
/// drifted to carry both by hand because this template omitted them). Both
/// are optional - a host with neither still gets a valid, complete unit.
fn render_systemd_unit(home: &Path, binary: &Path, vault_root: &Path, config: &Config) -> String {
    log::debug!(
        "render_systemd_unit: vault_root={} log_level={} rayon_threads={} env_bootstrap={}",
        vault_root.display(),
        config.log_level,
        config.daemon.rayon_threads,
        config.daemon.env_bootstrap.is_some(),
    );

    let vault = vault_root.display();
    let log_level = &config.log_level;

    // Cortex writes under the sb data namespace too (the oracle DB it is the
    // sole embeddings writer for lives at `~/.local/share/sb/oracle/`), so the
    // unit must name it alongside the vault or `ProtectHome=read-only` blocks
    // every embed write. Matches borg's unit (`borg/src/service.rs`).
    let data = vault::paths::xdg_data_dir()
        .expect("xdg_data_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join("sb");

    let mut config_flag = String::new();
    let config_path = vault::paths::cortex_config();
    if config_path.exists() {
        config_flag = format!(" --config {}", config_path.display());
    }

    let mut service = String::from(
        "[Unit]\n\
         Description=cortex - Obsidian vault governance daemon (second-brain)\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n",
    );

    if let Some(bootstrap) = &config.daemon.env_bootstrap {
        service.push_str(&format!(
            "ExecStartPre=/bin/sh -c '{command} > {env_file}'\n",
            command = bootstrap.command,
            env_file = bootstrap.env_file.display(),
        ));
        service.push_str(&format!("EnvironmentFile=-{}\n", bootstrap.env_file.display()));
    }

    service.push_str(&format!(
        "Environment=\"PATH={home}/.local/share/mise/shims:{home}/.local/bin:{home}/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\"\n",
        home = home.display(),
    ));

    if config.daemon.rayon_threads > 0 {
        service.push_str("# Cap rayon's global thread pool (candle's gemm degree reads the same var)\n");
        service.push_str(&format!(
            "Environment=\"RAYON_NUM_THREADS={}\"\n",
            config.daemon.rayon_threads
        ));
    }

    service.push_str(&format!(
        "ExecStart={binary} cortex{config_flag} --vault {vault} --log-level {log_level} daemon --start\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         WorkingDirectory={home}\n\
         \n\
         # Hardening\n\
         NoNewPrivileges=true\n\
         ProtectSystem=strict\n\
         ProtectHome=read-only\n\
         ReadWritePaths={vault} {data}\n\
         PrivateTmp=true\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        binary = binary.display(),
        home = home.display(),
        data = data.display(),
    ));

    service
}

/// Install a systemd user service for the daemon. Returns the lines sb
/// should print (paths written, follow-up systemctl commands).
fn install_systemd_service(vault_root: &Path, config: &Config) -> Result<Vec<String>> {
    log::debug!("install_systemd_service: vault_root={}", vault_root.display());
    let mut lines = Vec::new();
    let service_dir = vault::paths::xdg_config_dir()
        .expect("xdg_config_dir() returned None (set HOME or XDG_CONFIG_HOME)")
        .join("systemd")
        .join("user");

    std::fs::create_dir_all(&service_dir).context("failed to create systemd user dir")?;

    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let binary = std::env::current_exe().context("failed to get current executable path")?;

    let service = render_systemd_unit(&home, &binary, vault_root, config);

    let service_path = service_dir.join("cortex.service");
    std::fs::write(&service_path, &service)?;
    lines.push(format!("Installed: {}", service_path.display()));
    log::debug!("install_systemd_service: wrote unit -> {}", service_path.display());

    // NOTE: no cortex-daily / cortex-weekly intel timers are installed. The
    // long-running daemon (`daemon --start`) schedules daily/weekly intel
    // in-process via tokio timers (see the `daily`/`weekly` arms of the select!
    // loop above). Installing systemd timers too ran intel TWICE - once in the
    // daemon and once as a separate oneshot process. `uninstall` still removes
    // any timers a prior install left behind.

    lines.push(String::new());
    lines.push("Run:".to_string());
    lines.push("  systemctl --user daemon-reload".to_string());
    lines.push("  systemctl --user enable --now cortex".to_string());
    lines.push("  (daily/weekly intel is scheduled inside the daemon - no separate timers)".to_string());

    Ok(lines)
}

/// Uninstall the systemd user service and timer units. Returns the lines sb
/// should print.
fn uninstall_systemd_service() -> Result<Vec<String>> {
    let service_dir = vault::paths::xdg_config_dir()
        .expect("xdg_config_dir() returned None (set HOME or XDG_CONFIG_HOME)")
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
    let service_path = vault::paths::xdg_config_dir()
        .expect("xdg_config_dir() returned None (set HOME or XDG_CONFIG_HOME)")
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
mod tests;
