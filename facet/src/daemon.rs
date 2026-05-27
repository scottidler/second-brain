//! Cadence loop + systemd unit installation.
//!
//! Daemon mode (`sb facet daemon`) runs harvest, narrate, and dream on
//! configurable intervals. One-shot mode (`sb facet harvest`) runs a
//! single harvest tick. Both call the same [`harvest_once`] code path.

pub mod harvest;
pub mod systemd;

use std::path::Path;
use std::time::Duration;

use eyre::Result;

use crate::config::Config;

/// Drive one full harvest tick: scan -> cluster -> extract -> render.
/// Returns a tick report so the operator surface can show counts and
/// failures.
pub async fn harvest_once(config: &Config, ledger: &crate::Ledger, vault_root: &Path) -> Result<TickReport> {
    harvest::run_once(config, ledger, vault_root).await
}

/// Run the cadence loop forever. Each pass advances `harvest`,
/// `narrate`, and `dream` based on their own intervals (0 disables).
pub async fn run_loop(config: Config, ledger: crate::Ledger, vault_root: std::path::PathBuf) -> Result<()> {
    log::info!(
        "facet::daemon::run_loop: harvest_interval_secs={} narrate_interval_secs={} dream_interval_secs={}",
        config.harvest_interval_secs,
        config.narrate_interval_secs,
        config.dream_interval_secs
    );
    let interval = Duration::from_secs(config.harvest_interval_secs.max(60));
    let mut last_narrate_ts: u64 = 0;
    let mut last_dream_ts: u64 = 0;
    loop {
        match harvest_once(&config, &ledger, &vault_root).await {
            Ok(report) => {
                log::info!(
                    "facet daemon tick complete: sessions={} clustered={} gems={} rendered={} failures={}",
                    report.sessions_seen,
                    report.cluster_assignments_created,
                    report.gems_extracted,
                    report.workitems_rendered,
                    report.failures
                );
            }
            Err(e) => {
                log::error!("facet daemon tick failed: {e:#}");
            }
        }
        let now = chrono::Utc::now().timestamp() as u64;

        if config.narrate_interval_secs > 0 && now.saturating_sub(last_narrate_ts) >= config.narrate_interval_secs {
            match crate::narrative::run::run(
                &config,
                &ledger,
                &vault_root,
                crate::narrative::run::ArchetypeFilter::All,
            )
            .await
            {
                Ok(r) => {
                    log::info!(
                        "facet narrate: considered={} suppressed={} synthesised={} skipped_by_gate={}",
                        r.candidates_considered,
                        r.candidates_suppressed_by_rejection,
                        r.narratives_synthesised,
                        r.narratives_skipped_by_gate
                    );
                    last_narrate_ts = now;
                }
                Err(e) => log::error!("facet narrate failed: {e:#}"),
            }
        }

        if config.dream_interval_secs > 0 && now.saturating_sub(last_dream_ts) >= config.dream_interval_secs {
            match crate::dream::run::run(&config, &ledger, &vault_root) {
                Ok(r) => {
                    log::info!(
                        "facet dream: discovered={} written={}",
                        r.dreams_discovered,
                        r.notes_written
                    );
                    last_dream_ts = now;
                }
                Err(e) => log::error!("facet dream failed: {e:#}"),
            }
        }
        tokio::time::sleep(interval).await;
    }
}

#[derive(Debug, Clone, Default)]
pub struct TickReport {
    pub sessions_seen: usize,
    pub cluster_assignments_created: usize,
    pub gems_extracted: usize,
    pub workitems_rendered: usize,
    pub failures: usize,
}

pub use systemd::{install_systemd_service, uninstall_systemd_service};
