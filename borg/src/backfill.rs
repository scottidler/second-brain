//! One-shot helpers that adapt the existing vault to schema changes.
//!
//! Currently houses `backfill_ingested`, which walks the vault, finds every
//! `origin: assisted` note that lacks an `ingested:` frontmatter field, and
//! sets `ingested: <date:>` so the dashboard's `WHERE ingested = ...`
//! queries can find every legacy note from day 1.

use crate::config::Config;
use crate::pipeline::atomic::{apply_ingested_date, write_atomic};
use eyre::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Default)]
pub struct BackfillReport {
    pub scanned: usize,
    pub skipped_authored: usize,
    pub skipped_already_present: usize,
    pub skipped_recently_modified: usize,
    pub skipped_no_date: usize,
    pub backfilled: usize,
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    PathBuf::from(path)
}

fn collect_md_files(root: &Path, skip_folders: &[String]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, root, skip_folders, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(current: &Path, root: &Path, skip_folders: &[String], out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(current).with_context(|| format!("read_dir {}", current.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string();
            if skip_folders.iter().any(|s| rel.starts_with(s)) {
                continue;
            }
            walk(&path, root, skip_folders, out)?;
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    Ok(())
}

fn extract_frontmatter_field(content: &str, field: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after = &trimmed[3..];
    let end = after.find("\n---")?;
    let fm = &after[..end];
    for line in fm.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&format!("{field}:")) {
            let val = rest.trim().trim_matches('"');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Skip notes whose mtime is within `min_age` of now - guards against
/// rewriting a file currently being written by another borg process.
fn is_recently_modified(path: &Path, min_age: Duration) -> Result<bool> {
    let meta = std::fs::metadata(path).with_context(|| format!("metadata {}", path.display()))?;
    let mtime = meta.modified().context("modified time")?;
    let age = SystemTime::now().duration_since(mtime).unwrap_or(Duration::ZERO);
    Ok(age < min_age)
}

pub async fn run_backfill_ingested(config: &Config, dry_run: bool) -> Result<()> {
    let vault_root = expand_tilde(&config.vault.root_path);
    log::debug!(
        "backfill::run_backfill_ingested: vault={} dry_run={dry_run}",
        vault_root.display()
    );
    let md_files = collect_md_files(&vault_root, &config.migration.skip_folders).context("Failed to walk vault")?;
    log::info!("backfill-ingested: scanning {} note files", md_files.len());

    let mut report = BackfillReport::default();
    let min_age = Duration::from_secs(60);

    for path in &md_files {
        report.scanned += 1;
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("backfill-ingested: cannot read {}: {e}", path.display());
                continue;
            }
        };

        let origin = extract_frontmatter_field(&content, "origin");
        if origin.as_deref() != Some("assisted") {
            report.skipped_authored += 1;
            continue;
        }
        if extract_frontmatter_field(&content, "ingested").is_some() {
            report.skipped_already_present += 1;
            continue;
        }
        let Some(date) = extract_frontmatter_field(&content, "date") else {
            log::debug!("backfill-ingested: skipping {} (no date: field)", path.display());
            report.skipped_no_date += 1;
            continue;
        };

        match is_recently_modified(path, min_age) {
            Ok(true) => {
                log::debug!(
                    "backfill-ingested: skipping {} (mtime within {:?})",
                    path.display(),
                    min_age
                );
                report.skipped_recently_modified += 1;
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                log::warn!("backfill-ingested: mtime check failed for {}: {e}", path.display());
            }
        }

        if dry_run {
            log::info!("backfill-ingested: WOULD set ingested={} on {}", date, path.display());
            report.backfilled += 1;
            continue;
        }

        let updated = apply_ingested_date(&content, &date);
        if updated == content {
            // The helper returned no change: either no frontmatter, or the
            // insertion path failed. Either way, skip rather than write.
            log::warn!(
                "backfill-ingested: apply_ingested_date produced no change for {}",
                path.display()
            );
            continue;
        }
        write_atomic(path, updated.as_bytes()).with_context(|| format!("write {}", path.display()))?;
        log::info!("backfill-ingested: set ingested={date} on {}", path.display());
        report.backfilled += 1;
    }

    println!(
        "backfill-ingested complete:\n  scanned: {}\n  backfilled: {}{}\n  skipped (already had ingested:): {}\n  skipped (origin != assisted): {}\n  skipped (recent mtime): {}\n  skipped (no date: field): {}",
        report.scanned,
        report.backfilled,
        if dry_run { " (dry-run)" } else { "" },
        report.skipped_already_present,
        report.skipped_authored,
        report.skipped_recently_modified,
        report.skipped_no_date,
    );

    Ok(())
}

#[cfg(test)]
mod tests;
