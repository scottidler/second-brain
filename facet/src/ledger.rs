//! SQLite ledger for facet.
//!
//! Lives at `~/.local/share/sb/facet/state.db`. The ledger is the
//! durable, daemon-internal source of truth for sessions seen,
//! work-items, cluster assignments, gems + interaction turns, and
//! narratives. Vault output is rendered from this ledger; the ledger
//! is never reconstructed from the vault.
//!
//! Decomposed into submodules so the file count stays well under the
//! 1500 line bloat limit even as the schema grows:
//!
//! - [`schema`]     - DDL, applied idempotently on every open
//! - [`sessions`]   - sessions table accessors
//! - [`workitems`]  - work_items, work_item_repos, session_workitem
//! - [`clusters`]   - cluster_assignments
//! - [`gems`]       - gems + interaction_turns
//! - [`narratives`] - narratives + narrative_axes
//! - [`meta`]       - ledger_meta

pub mod clusters;
pub mod gems;
pub mod meta;
pub mod narratives;
pub mod schema;
pub mod sessions;
pub mod workitems;

use eyre::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Handle to the facet SQLite ledger. Wraps the rusqlite Connection in a
/// Mutex because rusqlite::Connection is `!Sync` but the daemon needs
/// shared access across tokio tasks. CPU-bound transactions stay short.
pub struct Ledger {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl Ledger {
    /// Open (or create) the ledger at the given path. Applies the
    /// schema idempotently.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        log::debug!("Ledger::open: path={}", path.display());
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let conn = Connection::open(&path).context("open sqlite")?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("pragma journal_mode=WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .context("pragma foreign_keys=ON")?;
        let ledger = Self {
            conn: Mutex::new(conn),
            path,
        };
        schema::apply(&ledger).context("apply schema")?;
        Ok(ledger)
    }

    /// Open an in-memory ledger. For tests only.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory sqlite")?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .context("pragma foreign_keys=ON")?;
        let ledger = Self {
            conn: Mutex::new(conn),
            path: PathBuf::from(":memory:"),
        };
        schema::apply(&ledger).context("apply schema (in-memory)")?;
        Ok(ledger)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run a closure with the underlying connection locked. Keep the
    /// closure body short - this serialises all ledger access.
    pub fn with_conn<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut Connection) -> Result<R>,
    {
        let mut guard = self.conn.lock().map_err(|e| eyre::eyre!("ledger poisoned: {e}"))?;
        f(&mut guard)
    }

    /// Run a closure inside one SQLite transaction. The closure receives a
    /// `&rusqlite::Transaction`; if it returns `Ok`, the transaction
    /// commits, otherwise it rolls back.
    pub fn with_tx<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<R>,
    {
        let mut guard = self.conn.lock().map_err(|e| eyre::eyre!("ledger poisoned: {e}"))?;
        let tx = guard.transaction().context("begin tx")?;
        let out = f(&tx)?;
        tx.commit().context("commit tx")?;
        Ok(out)
    }
}
