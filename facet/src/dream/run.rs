//! Dream-pass orchestrator. Reads the ledger, runs every
//! [`super::discover`] finder, renders to markdown under the
//! configured `dreams_dir`. NEVER mutates canonical (per the design
//! doc).

use std::path::Path;

use eyre::{Context, Result};

use crate::Ledger;
use crate::config::Config;
use crate::dream::discover::find_all_dreams;
use crate::dream::render::render_all;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Default)]
pub struct DreamReport {
    pub dreams_discovered: usize,
    pub notes_written: usize,
}

pub fn run(config: &Config, ledger: &Ledger, vault_root: &Path) -> Result<DreamReport> {
    log::info!(
        "dream::run: vault_root={} dreams_dir={}",
        vault_root.display(),
        config.vault.dreams_dir,
    );
    let dreams = find_all_dreams(ledger)?;
    let dreams_dir = vault_root.join(&config.vault.dreams_dir);
    let written = render_all(&dreams, &dreams_dir).context("render dream notes")?;
    let report = DreamReport {
        dreams_discovered: dreams.len(),
        notes_written: written.len(),
    };
    log::info!(
        "dream::run complete: discovered={} written={}",
        report.dreams_discovered,
        report.notes_written
    );
    Ok(report)
}
