//! Daemon: cadence loop that ticks harvest+cluster+distill on a fast
//! interval and dream on a slow interval.
//!
//! Installs / uninstalls via systemd user units written to
//! `~/.config/systemd/user/glean.service`. Mirrors the borg/cortex
//! daemon convention; sb's CLI delegates `sb glean daemon
//! --install|--uninstall|--status` to this module.

pub mod systemd;

use eyre::{Context, Result};
use std::time::Duration;

use crate::config::Config;
use crate::ledger::Ledger;
use crate::opts::DaemonOpts;
use crate::{cluster, distill, dream};

#[derive(Debug, Default)]
pub struct DaemonOutcome {
    pub lines: Vec<String>,
}

pub async fn run(config: &Config, opts: &DaemonOpts) -> Result<DaemonOutcome> {
    log::debug!(
        "daemon::run: install={} uninstall={} status={}",
        opts.install,
        opts.uninstall,
        opts.status
    );
    if opts.install {
        return Ok(DaemonOutcome {
            lines: systemd::install()?,
        });
    }
    if opts.uninstall {
        return Ok(DaemonOutcome {
            lines: systemd::uninstall()?,
        });
    }
    if opts.status {
        return Ok(DaemonOutcome {
            lines: systemd::status()?,
        });
    }
    run_loop(config).await?;
    Ok(DaemonOutcome::default())
}

async fn run_loop(config: &Config) -> Result<()> {
    log::info!(
        "daemon::run_loop: harvest_interval_secs={} dream_interval_secs={}",
        config.daemon.harvest_interval_secs,
        config.daemon.dream_interval_secs
    );
    let ledger = Ledger::open(vault::paths::glean_db_path()).context("open glean ledger")?;
    let mut last_harvest = tokio::time::Instant::now() - Duration::from_secs(config.daemon.harvest_interval_secs);
    let mut last_dream = tokio::time::Instant::now() - Duration::from_secs(config.daemon.dream_interval_secs);
    loop {
        let now = tokio::time::Instant::now();
        if now.duration_since(last_harvest) >= Duration::from_secs(config.daemon.harvest_interval_secs) {
            if let Err(e) = run_harvest_cycle(&ledger, config) {
                log::warn!("daemon::run_loop: harvest cycle failed: {e:?}");
            }
            last_harvest = tokio::time::Instant::now();
        }
        if now.duration_since(last_dream) >= Duration::from_secs(config.daemon.dream_interval_secs) {
            if let Err(e) = run_dream_cycle(&ledger, config) {
                log::warn!("daemon::run_loop: dream cycle failed: {e:?}");
            }
            last_dream = tokio::time::Instant::now();
        }
        tokio::time::sleep(Duration::from_secs(config.daemon.debounce_secs.max(1))).await;
    }
}

fn run_harvest_cycle(ledger: &Ledger, config: &Config) -> Result<()> {
    log::info!("daemon::run_harvest_cycle");
    let opts = crate::opts::HarvestOpts::default();
    crate::harvest(ledger, config, &opts).context("daemon harvest")?;
    cluster::run(ledger, &config.cluster).context("daemon cluster")?;
    distill::distill_all(ledger, config).context("daemon distill")?;
    Ok(())
}

fn run_dream_cycle(ledger: &Ledger, config: &Config) -> Result<()> {
    log::info!("daemon::run_dream_cycle");
    dream::run_all(ledger, config).context("daemon dream")?;
    Ok(())
}
