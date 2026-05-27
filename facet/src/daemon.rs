//! Cadence loop + systemd unit installation.
//!
//! Daemon mode (`sb facet daemon`) runs harvest on a configurable
//! interval. One-shot mode (`sb facet harvest`) runs a single tick.
//! Both call the same [`harvest_once`] code path.

pub mod harvest;
pub mod systemd;

use std::path::Path;
use std::time::Duration;

use eyre::Result;

use crate::config::Config;

/// Drive one full tick: scan -> cluster -> extract -> render. Returns a
/// tick report so the operator surface can show counts and failures.
pub async fn harvest_once(config: &Config, ledger: &crate::Ledger, vault_root: &Path) -> Result<TickReport> {
    harvest::run_once(config, ledger, vault_root).await
}

/// Run the cadence loop forever, sleeping `harvest_interval_secs`
/// between ticks. The portrait rollup (Phase 7) fires on its own
/// cadence in the same loop.
pub async fn run_loop(config: Config, ledger: crate::Ledger, vault_root: std::path::PathBuf) -> Result<()> {
    log::info!(
        "facet::daemon::run_loop: harvest_interval_secs={} portrait_interval_secs={}",
        config.harvest_interval_secs,
        config.portrait_interval_secs
    );
    let interval = Duration::from_secs(config.harvest_interval_secs.max(60));
    loop {
        match harvest_once(&config, &ledger, &vault_root).await {
            Ok(report) => {
                log::info!(
                    "facet daemon tick complete: sessions={} clustered={} extracted={} rendered={} failures={}",
                    report.sessions_seen,
                    report.cluster_assignments_created,
                    report.moments_extracted,
                    report.workitems_rendered,
                    report.failures
                );
            }
            Err(e) => {
                log::error!("facet daemon tick failed: {e:#}");
            }
        }
        tokio::time::sleep(interval).await;
    }
}

#[derive(Debug, Clone, Default)]
pub struct TickReport {
    pub sessions_seen: usize,
    pub cluster_assignments_created: usize,
    pub moments_extracted: usize,
    pub workitems_rendered: usize,
    pub failures: usize,
}

pub use systemd::{install_systemd_service, uninstall_systemd_service};
