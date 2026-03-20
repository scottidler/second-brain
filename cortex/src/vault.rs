pub use vault::frontmatter::{Frontmatter, parse_frontmatter};
pub use vault::note::{Note, parse_note};

use eyre::Result;
use std::path::Path;

use crate::config::VaultConfig;

/// Scan an entire vault and return all parsed notes.
/// Uses vault's scan_vault but adapts cortex's VaultConfig to vault's ScanConfig.
pub fn scan_vault(vault_root: &Path, vault_config: &VaultConfig) -> Result<Vec<Note>> {
    let scan_config = vault::config::ScanConfig {
        ignore: vault_config.ignore.clone(),
    };
    vault::note::scan_vault(vault_root, &scan_config)
}
