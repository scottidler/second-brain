//! Recoverable delete with graceful fallback.
//!
//! Prefers `rkvr rmrf` (archives before deleting; recoverable via `rkvr rcvr`)
//! but does NOT require it. On a machine without `rkvr` on PATH, this falls
//! back to std fs removal (`rm -rf` semantics, NOT recoverable) and logs a
//! WARN, so borg stays portable for operators who have not installed rkvr.
//!
//! Only an *absent* binary triggers the fallback. `rkvr` present but exiting
//! non-zero is a real error (a permission problem, say) and is surfaced rather
//! than silently escalated to a non-recoverable delete.

use std::path::Path;
use std::process::Command;

use eyre::{Context, Result};

/// Delete each path, preferring `rkvr rmrf`. Empty input is a no-op.
///
/// - `rkvr` on PATH and succeeds -> archived, recoverable.
/// - `rkvr` not on PATH -> WARN + non-recoverable std removal.
/// - `rkvr` present but exits non-zero -> error (no silent fallback).
pub(crate) fn remove<P: AsRef<Path>>(paths: &[P]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }

    // Tests bypass the real binary so the recovery store under
    // ~/.local/share/rkvr/ is not polluted on dev/CI machines that have rkvr.
    if cfg!(test) {
        return fallback_remove_all(paths);
    }

    let mut cmd = Command::new("rkvr");
    cmd.arg("rmrf");
    for p in paths {
        cmd.arg(p.as_ref());
    }

    match cmd.output() {
        Ok(out) if out.status.success() => {
            log::info!("rkvr rmrf: archived {} path(s)", paths.len());
            Ok(())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eyre::bail!("rkvr rmrf failed (exit {}): {stderr}", out.status);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::warn!(
                "rkvr not found on PATH; deleting {} path(s) with non-recoverable rm -rf \
                 semantics (install rkvr from github.com/scottidler/rkvr for recoverable deletes)",
                paths.len()
            );
            fallback_remove_all(paths)
        }
        Err(e) => Err(e).context("failed to invoke rkvr"),
    }
}

fn fallback_remove_all<P: AsRef<Path>>(paths: &[P]) -> Result<()> {
    for p in paths {
        fallback_remove(p.as_ref())?;
    }
    Ok(())
}

/// Non-recoverable removal of a single path (file, directory, or symlink). A
/// missing path is a no-op - the desired end state (gone) already holds.
fn fallback_remove(path: &Path) -> Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("stat {} before removal", path.display())),
    };
    if meta.is_dir() {
        std::fs::remove_dir_all(path).with_context(|| format!("remove_dir_all {}", path.display()))
    } else {
        std::fs::remove_file(path).with_context(|| format!("remove_file {}", path.display()))
    }
}

#[cfg(test)]
mod tests;
