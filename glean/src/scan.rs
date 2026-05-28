//! Discover Claude Code session JSONL files under
//! `~/.claude/projects/`.
//!
//! Each subdirectory of `projects/` corresponds to one cwd that Claude
//! Code was invoked from (the dirname is a slugified absolute path).
//! Inside each subdirectory, every `*.jsonl` file is one session.
//!
//! We do NOT decode the slugified dirname to recover the cwd; the
//! authoritative cwd is the `cwd` field inside the JSONL lines
//! themselves. The scan is just a filesystem-level enumeration.

use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::error::GleanError;

const JSONL_EXT: &str = "jsonl";

/// One discovered session file with file-stat metadata. The harvester
/// uses `mtime` to skip files whose sha256 surely matches the stored
/// row (mtime unchanged is a safe early-out).
#[derive(Debug, Clone)]
pub struct DiscoveredSession {
    pub jsonl_path: PathBuf,
    pub mtime: std::time::SystemTime,
    pub size_bytes: u64,
}

/// Walk the configured projects dir and return every discovered
/// JSONL session file. Sorted by path for stable iteration order.
pub fn discover(projects_dir: &Path) -> Result<Vec<DiscoveredSession>, GleanError> {
    log::debug!("scan::discover: projects_dir={}", projects_dir.display());
    if !projects_dir.exists() {
        return Err(GleanError::Other(format!(
            "claude projects dir not found: {}",
            projects_dir.display()
        )));
    }
    let mut out = Vec::new();
    for entry in WalkDir::new(projects_dir)
        .min_depth(1)
        .max_depth(2)
        .into_iter()
        .filter_map(|r| r.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some(JSONL_EXT) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                log::warn!("scan::discover: stat failed for {}: {e}", path.display());
                continue;
            }
        };
        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        out.push(DiscoveredSession {
            jsonl_path: path.to_path_buf(),
            mtime,
            size_bytes: meta.len(),
        });
    }
    out.sort_by(|a, b| a.jsonl_path.cmp(&b.jsonl_path));
    log::debug!("scan::discover: found {} sessions", out.len());
    Ok(out)
}

#[cfg(test)]
mod tests;
