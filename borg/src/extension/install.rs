use std::path::PathBuf;

use eyre::Result;

use crate::config::Config;

#[derive(Debug, Default)]
pub struct InstallOpts {
    pub no_policy: bool,
    pub policy_file: Option<PathBuf>,
    pub if_installed: bool,
}

#[derive(Debug)]
pub struct InstallResult {
    pub xpi_path: Option<PathBuf>,
    pub policy_path: Option<PathBuf>,
    pub policy_changed: bool,
    pub skipped_not_installed: bool,
}

#[derive(Debug, Default)]
pub struct UninstallOpts {
    pub purge: bool,
}

#[derive(Debug)]
pub struct UninstallResult {
    pub policy_path: Option<PathBuf>,
    pub artifacts_removed: bool,
}

pub fn run(_repo_root: &std::path::Path, _config: &Config, _opts: InstallOpts) -> Result<InstallResult> {
    eyre::bail!("sb borg extension install is implemented in Phase 4 of the extension-lifecycle design")
}

pub fn uninstall(_opts: UninstallOpts) -> Result<UninstallResult> {
    eyre::bail!("sb borg extension uninstall is implemented in Phase 4 of the extension-lifecycle design")
}
