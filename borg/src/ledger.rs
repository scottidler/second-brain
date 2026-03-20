pub use vault::ledger::*;

use crate::config::Config;
use std::path::PathBuf;

/// Resolve the Borg Ledger path from borg config.
pub fn ledger_path(config: &Config) -> PathBuf {
    let root = expand_tilde(&config.vault.root_path);
    root.join("system").join("borg-ledger.md")
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    PathBuf::from(path)
}
