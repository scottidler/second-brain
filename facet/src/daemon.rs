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
/// between ticks. The portrait rollup fires on its own cadence in
/// the same loop (controlled by `portrait_interval_secs`; 0 disables).
pub async fn run_loop(config: Config, ledger: crate::Ledger, vault_root: std::path::PathBuf) -> Result<()> {
    log::info!(
        "facet::daemon::run_loop: harvest_interval_secs={} portrait_interval_secs={}",
        config.harvest_interval_secs,
        config.portrait_interval_secs
    );
    let interval = Duration::from_secs(config.harvest_interval_secs.max(60));
    let mut last_portrait_ts: u64 = 0;
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
        if config.portrait_interval_secs > 0 {
            let now = chrono::Utc::now().timestamp() as u64;
            if now.saturating_sub(last_portrait_ts) >= config.portrait_interval_secs {
                match harvest::run_portrait_rollup(&config, &ledger, &vault_root).await {
                    Ok(n) => {
                        log::info!("facet portrait rollup: {n} portraits written");
                        last_portrait_ts = now;
                    }
                    Err(e) => log::error!("facet portrait rollup failed: {e:#}"),
                }
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
