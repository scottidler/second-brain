//! Shared path resolution for the unified `sb` binary.
//!
//! Every loader, bootstrap site, doctor site, and systemd unit template
//! reads from this module. Hardcoded paths anywhere else are a
//! code-review reject.
//!
//! On-disk layout:
//!
//! ```text
//! ~/.config/sb/
//!   borg.yml
//!   cortex.yml
//!   oracle.yml
//!   canonical-tags.yml
//!   tag-mapping.yml
//!   tag-proposals.yml
//!   patterns/
//!     distill-article.md
//!     ...
//! ```
//!
//! Logs sit symmetrically under `~/.local/share/sb/` (see `vault::logging`).

use std::path::{Path, PathBuf};

use eyre::{Result, eyre};

/// Subdirectory under `dirs::config_dir()` that owns every sb config file.
pub const SB_DIR: &str = "sb";

/// `~/.config/sb/` (or platform equivalent via `dirs`).
///
/// Panics only if `dirs::config_dir()` returns `None`, which on Linux
/// means both `$HOME` and `$XDG_CONFIG_HOME` are unset - a broken
/// environment where nothing else in sb would work either.
pub fn config_root() -> PathBuf {
    dirs::config_dir()
        .expect("dirs::config_dir() returned None (set HOME or XDG_CONFIG_HOME)")
        .join(SB_DIR)
}

pub fn borg_config() -> PathBuf {
    config_root().join("borg.yml")
}

pub fn cortex_config() -> PathBuf {
    config_root().join("cortex.yml")
}

pub fn oracle_config() -> PathBuf {
    config_root().join("oracle.yml")
}

pub fn canonical_tags() -> PathBuf {
    config_root().join("canonical-tags.yml")
}

pub fn tag_mapping() -> PathBuf {
    config_root().join("tag-mapping.yml")
}

pub fn tag_proposals() -> PathBuf {
    config_root().join("tag-proposals.yml")
}

pub fn patterns_dir() -> PathBuf {
    config_root().join("patterns")
}

/// Resolve the vault root with explicit precedence: CLI > config > marker-gated CWD.
///
/// Returns an error rather than silently picking up an arbitrary working directory.
/// The CWD fallback only fires when the working directory contains a `.obsidian/`
/// directory - Obsidian writes this the moment a vault is opened in the app, so
/// this is the universal "this is a vault" signal.
pub fn resolve_vault_root(cli_override: Option<&Path>, config_value: Option<&str>) -> Result<PathBuf> {
    log::debug!(
        "resolve_vault_root: cli_override={:?} config_value={:?}",
        cli_override,
        config_value
    );
    if let Some(p) = cli_override {
        return Ok(p.to_path_buf());
    }
    if let Some(s) = config_value {
        let expanded = shellexpand::tilde(s);
        return Ok(PathBuf::from(expanded.as_ref()));
    }
    let cwd = std::env::current_dir()?;
    if cwd.join(".obsidian").is_dir() {
        return Ok(cwd);
    }
    Err(eyre!(
        "vault root not set: pass --vault <path>, set `vault.root-path` in your config, \
         or run from a directory that contains a `.obsidian/` directory.\n\
         (current directory: {})",
        cwd.display()
    ))
}

#[cfg(test)]
mod tests;
