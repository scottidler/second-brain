pub use vault::ledger::*;

use crate::config::Config;
use eyre::Result;
use std::path::PathBuf;

/// Resolve the Borg Ledger path from borg config.
pub fn ledger_path(config: &Config) -> Result<PathBuf> {
    let root = config.vault_root()?;
    Ok(root.join("system").join("views").join("borg-ledger.md"))
}
