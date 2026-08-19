//! Retention: sweep aged-off artifacts under the configured windows.
//!
//! Two independent stores, two windows:
//!
//! - Staging trace directories under `staging.root`. Successful traces age at
//!   `staging.retention_days`; rejected traces (those with a `rejection.yml`
//!   sidecar) keep a longer window so the operator has extra time to
//!   investigate. See [`sweep`].
//! - Raw-input sidecars at `<vault>/system/intake/<trace>.txt`, written at the
//!   door by `vault::intake::write_raw_input`, aged at `intake.retention_days`.
//!   See [`sweep_sidecars`]. These had no window before that config knob
//!   existed, so the vault grew one file per trace forever (1975 files / 75 MB
//!   by 2026-08) and `git add -A` snapshots swept them into the vault repo.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use eyre::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::{Config, StagingLayout};
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::time::interval;

/// Parse a frontmatter `ingested:` value to a calendar date, accepting BOTH
/// shapes that exist in the vault today: the bare `%Y-%m-%d` written by the
/// fresh-publish path, and the full offset datetime (RFC 3339, e.g.
/// `2026-06-20T20:40:27-07:00`) written by the URL pipeline and
/// `backfill-ingested`. Returns `None` for anything that parses as neither.
pub fn parse_ingested_date(ingested: &str) -> Option<NaiveDate> {
    let trimmed = ingested.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.date_naive());
    }
    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").ok()
}

/// Compute the absolute policy expiry date for a staged trace:
/// `ingested_date + retention_days`, formatted back to `%Y-%m-%d`. This is the
/// single source of the `trace-expires` value, stamped by borg at publish
/// (Phase 3) and by `backfill-ingested` for legacy notes (Phase 4).
pub fn trace_expires_for(ingested: NaiveDate, retention_days: u32) -> String {
    (ingested + Duration::days(i64::from(retention_days)))
        .format("%Y-%m-%d")
        .to_string()
}

#[derive(Debug, Clone)]
pub struct SweepResult {
    pub scanned: usize,
    pub deleted: Vec<String>,
    pub kept: usize,
    pub bytes_freed: u64,
}

#[derive(Debug, Clone)]
pub struct StatusReport {
    pub traces: usize,
    pub rejected: usize,
    pub total_bytes: u64,
    pub root: PathBuf,
}

/// Sweep trace directories under `config.staging.root` whose mtime is older
/// than the per-status retention window. `dry_run=true` reports what would be
/// deleted without touching disk.
pub fn sweep(config: &Config, dry_run: bool) -> Result<SweepResult> {
    let root = &config.staging.root;
    if !root.is_dir() {
        return Ok(SweepResult {
            scanned: 0,
            deleted: Vec::new(),
            kept: 0,
            bytes_freed: 0,
        });
    }
    let now = Utc::now();
    let ok_window = Duration::days(config.staging.retention_days.min(3650) as i64);
    let rejected_window = Duration::days(config.staging.rejected_retention_days.min(3650) as i64);
    let mut result = SweepResult {
        scanned: 0,
        deleted: Vec::new(),
        kept: 0,
        bytes_freed: 0,
    };
    for entry in std::fs::read_dir(trace_parent(root, config.staging.layout))
        .with_context(|| format!("read_dir {}", root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        result.scanned += 1;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let window = if path.join("rejection.yml").exists() { rejected_window } else { ok_window };
        let Some(mtime) = dir_mtime(&path) else {
            result.kept += 1;
            continue;
        };
        let age = now - mtime;
        if age < window {
            result.kept += 1;
            continue;
        }
        let size = dir_size(&path).unwrap_or(0);
        result.deleted.push(name);
        result.bytes_freed += size;
        if !dry_run {
            std::fs::remove_dir_all(&path).with_context(|| format!("remove_dir_all {}", path.display()))?;
        }
    }
    Ok(result)
}

/// Snapshot totals for the staging root (counts, rejected, bytes).
pub fn status(config: &Config) -> Result<StatusReport> {
    let root = &config.staging.root;
    let mut report = StatusReport {
        traces: 0,
        rejected: 0,
        total_bytes: 0,
        root: root.clone(),
    };
    if !root.is_dir() {
        return Ok(report);
    }
    for entry in std::fs::read_dir(trace_parent(root, config.staging.layout))
        .with_context(|| format!("read_dir {}", root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        report.traces += 1;
        let path = entry.path();
        if path.join("rejection.yml").exists() {
            report.rejected += 1;
        }
        report.total_bytes += dir_size(&path).unwrap_or(0);
    }
    Ok(report)
}

/// How often the daemon's background sidecar sweep wakes up. Daily: the window
/// is measured in days, so anything finer just re-scans a directory that cannot
/// have changed its answer yet.
const SIDECAR_SWEEP_INTERVAL_SECS: u64 = 86_400;

#[derive(Debug, Clone)]
pub struct SidecarSweepResult {
    pub scanned: usize,
    pub deleted: Vec<String>,
    pub kept: usize,
    pub bytes_freed: u64,
    /// `false` when `intake.retention_days` is 0 (keep forever): nothing was
    /// scanned and nothing deleted, which is DIFFERENT from a clean sweep.
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct SidecarReport {
    pub files: usize,
    pub total_bytes: u64,
    pub dir: PathBuf,
    pub retention_days: u32,
}

/// Sweep raw-input sidecars (`<vault>/system/intake/*.txt`) whose mtime is
/// older than `intake.retention_days`. `dry_run=true` reports what would be
/// deleted without touching disk. `retention_days == 0` means keep forever and
/// short-circuits with `enabled: false`.
///
/// Deliberately separate from [`sweep`]: the staged copy of the same bytes is
/// governed by `staging.*`, and folding the two together would let one store's
/// window silently delete the other store's data.
pub fn sweep_sidecars(config: &Config, dry_run: bool) -> Result<SidecarSweepResult> {
    let days = config.intake.retention_days;
    log::debug!("retention::sweep_sidecars: retention_days={days} dry_run={dry_run}");
    let mut result = SidecarSweepResult {
        scanned: 0,
        deleted: Vec::new(),
        kept: 0,
        bytes_freed: 0,
        enabled: days > 0,
    };
    if !result.enabled {
        log::debug!("retention::sweep_sidecars: intake.retention-days=0, keeping sidecars forever");
        return Ok(result);
    }
    let vault_root = config.vault_root().context("resolve vault root for sidecar sweep")?;
    let dir = vault::intake::intake_raw_dir(&vault_root);
    if !dir.is_dir() {
        return Ok(result);
    }
    let now = Utc::now();
    let window = Duration::days(days.min(3650) as i64);
    for entry in std::fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() || path.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        result.scanned += 1;
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(mtime) = dir_mtime(&path) else {
            result.kept += 1;
            continue;
        };
        if now - mtime < window {
            result.kept += 1;
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        result.deleted.push(name);
        result.bytes_freed += size;
        if !dry_run {
            std::fs::remove_file(&path).with_context(|| format!("remove_file {}", path.display()))?;
        }
    }
    Ok(result)
}

/// Snapshot totals for the raw-input sidecar directory (count, bytes, window).
pub fn sidecar_status(config: &Config) -> Result<SidecarReport> {
    let vault_root = config.vault_root().context("resolve vault root for sidecar status")?;
    let dir = vault::intake::intake_raw_dir(&vault_root);
    let mut report = SidecarReport {
        files: 0,
        total_bytes: 0,
        dir: dir.clone(),
        retention_days: config.intake.retention_days,
    };
    if !dir.is_dir() {
        return Ok(report);
    }
    for entry in std::fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() || path.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        report.files += 1;
        report.total_bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
    }
    Ok(report)
}

/// Background task entry point for the sidecar sweep: one pass immediately at
/// startup (tokio's `interval` fires its first tick right away), then every
/// [`SIDECAR_SWEEP_INTERVAL_SECS`]. Errors are logged and the loop continues.
///
/// This task sweeps ONLY the vault-side sidecars. Staging trace directories
/// stay operator-driven via `sb borg retention sweep`, because auto-firing that
/// window for the first time against an unswept, hundreds-of-MB stages tree is
/// a surprise deletion, not a maintenance task.
pub async fn run_sidecar_sweep(config: Arc<Config>) {
    if config.intake.retention_days == 0 {
        log::info!("retention: sidecar sweep disabled (intake.retention-days=0)");
        return;
    }
    log::info!(
        "retention: sidecar sweep starting (interval={}s, window={}d)",
        SIDECAR_SWEEP_INTERVAL_SECS,
        config.intake.retention_days
    );
    let mut ticker = interval(StdDuration::from_secs(SIDECAR_SWEEP_INTERVAL_SECS));
    loop {
        ticker.tick().await;
        let cfg = config.clone();
        let result = tokio::task::spawn_blocking(move || sweep_sidecars(&cfg, false))
            .await
            .unwrap_or_else(|join_err| {
                log::error!("retention: sidecar sweep join error: {join_err}");
                Ok(SidecarSweepResult {
                    scanned: 0,
                    deleted: Vec::new(),
                    kept: 0,
                    bytes_freed: 0,
                    enabled: true,
                })
            });
        match result {
            Ok(r) if r.deleted.is_empty() => {
                log::debug!("retention: sidecar sweep clean (scanned={} kept={})", r.scanned, r.kept);
            }
            Ok(r) => {
                log::info!(
                    "retention: sidecar sweep deleted {} file(s), freed {} bytes (scanned={} kept={})",
                    r.deleted.len(),
                    r.bytes_freed,
                    r.scanned,
                    r.kept
                );
            }
            Err(e) => log::warn!("retention: sidecar sweep failed: {e:#}"),
        }
    }
}

fn trace_parent(root: &Path, layout: StagingLayout) -> PathBuf {
    match layout {
        StagingLayout::PerTrace => root.to_path_buf(),
        StagingLayout::PerStage => root.join("raw"),
    }
}

fn dir_mtime(path: &Path) -> Option<DateTime<Utc>> {
    let meta = std::fs::metadata(path).ok()?;
    let modified: SystemTime = meta.modified().ok()?;
    modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| DateTime::from_timestamp(d.as_secs() as i64, 0))
}

fn dir_size(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    if !path.is_dir() {
        return Ok(std::fs::metadata(path).map(|m| m.len()).unwrap_or(0));
    }
    for entry in std::fs::read_dir(path).with_context(|| format!("read_dir {}", path.display()))? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            total += dir_size(&entry.path())?;
        } else {
            total += meta.len();
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests;
