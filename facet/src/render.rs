//! Fencepost-merging vault note renderer.
//!
//! Managed sections are wrapped in HTML-comment fenceposts:
//!
//! ```text
//! <!-- facet:auto:begin section:foo -->
//! ...generated content...
//! <!-- facet:auto:end section:foo -->
//! ```
//!
//! Content OUTSIDE fenceposts is operator-owned and preserved across
//! re-renders. Frontmatter is treated as one fencepost-wrapped block.
//!
//! Submodules:
//! - [`block`]: fencepost-merge primitive shared by prism + spectrum
//! - [`prism`]: per-workitem prism notes (one note, many gem sections)
//! - [`quarantine`]: per-session failure notes

pub mod block;
pub mod prism;
pub mod quarantine;

use std::path::{Path, PathBuf};

use eyre::{Context, Result};

/// Write `body` to `path` atomically via tempfile + rename. Used by
/// every renderer in this module.
pub(crate) fn write_atomic(path: &Path, body: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("target path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    let tmp = make_temp_path(path);
    std::fs::write(&tmp, body).with_context(|| format!("write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn make_temp_path(target: &Path) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let base = target.file_name().and_then(|s| s.to_str()).unwrap_or("facet.tmp");
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    parent.join(format!(".{base}.tmp-{pid}-{nanos}"))
}
