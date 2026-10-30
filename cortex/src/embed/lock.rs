//! The embed file lock: serializes the daemon's embed tick, an ad-hoc
//! `cortex embed`, and the graph pass.

use std::path::PathBuf;

use eyre::{Context, Result};
use fs2::FileExt;

/// Try to acquire the embed file lock. The lock prevents the cortex
/// daemon's embed tick from racing an ad-hoc `cortex embed` invocation.
///
/// `pub` so the graph pass (`cortex::graph`) can take the *same* lock and
/// thereby serialize against any concurrent `cortex embed` write — the graph
/// pass reads `note_embeddings` and must not interleave with an embed batch
/// rewriting them.
pub fn acquire_lock() -> Result<EmbedLock> {
    let path = lock_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).wrap_err("failed to create lock dir")?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .wrap_err_with(|| format!("failed to open embed lock file at {}", path.display()))?;
    // Surface a TYPED contention error (downcastable through eyre) so callers
    // detect "lock held" via `EmbedLockHeld` instead of substring-matching the
    // error message — a brittle test that any reworded wrap_err would break.
    file.try_lock_exclusive()
        .map_err(|_| EmbedLockHeld { path: path.clone() })?;
    Ok(EmbedLock { _file: file })
}

/// The embed lock is held by another process/tick. A marker error (not a
/// message substring) so the daemon's "skip this tick" branch is type-checked.
/// Hand-rolled rather than pulling `thiserror` into cortex (which is otherwise
/// eyre-only); it stays downcastable from `eyre::Report`.
#[derive(Debug)]
pub struct EmbedLockHeld {
    pub path: PathBuf,
}

impl std::fmt::Display for EmbedLockHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "embed lock held by another process: {}", self.path.display())
    }
}

impl std::error::Error for EmbedLockHeld {}

pub(super) fn lock_path() -> PathBuf {
    // Under the sb/ data namespace via vault::paths (was the raw
    // ~/.local/share/cortex/embed.lock, outside sb/).
    vault::paths::cortex_lock_path()
}

/// RAII guard that releases the file lock on drop.
pub struct EmbedLock {
    _file: std::fs::File,
}

impl Drop for EmbedLock {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}
