//! Staging retention: sweep trace directories older than the configured
//! windows. Successful traces age at `staging.retention_days`; rejected
//! traces (those with a `rejection.yml` sidecar) keep a longer window so
//! the operator has extra time to investigate.

use chrono::{DateTime, Duration, Utc};
use eyre::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::{Config, StagingLayout};

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
