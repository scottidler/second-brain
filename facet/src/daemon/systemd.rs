//! systemd unit installation. Mirrors the borg / cortex install path:
//! writes a per-user unit at `~/.config/systemd/user/sb-facet.service`
//! with `ExecStart=` resolved to the running sb binary's
//! `std::env::current_exe()`.

use std::path::PathBuf;

use eyre::{Context, Result};

const UNIT_NAME: &str = "sb-facet.service";

#[derive(Debug, Clone)]
pub struct InstallOutcome {
    pub unit_path: PathBuf,
}

/// Write `~/.config/systemd/user/sb-facet.service`. Idempotent
/// overwrite; the caller is responsible for `systemctl --user
/// daemon-reload && systemctl --user enable --now sb-facet.service`.
pub fn install_systemd_service() -> Result<InstallOutcome> {
    let service_dir = dirs::config_dir()
        .expect("dirs::config_dir() returned None (set HOME or XDG_CONFIG_HOME)")
        .join("systemd")
        .join("user");
    std::fs::create_dir_all(&service_dir).context("mkdir systemd/user")?;
    let binary = std::env::current_exe().context("get current_exe")?;
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("dirs::home_dir() returned None"))?;
    let unit = format!(
        "[Unit]\n\
         Description=facet - judgment-moment harvester for Claude Code JSONL transcripts\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         Environment=\"PATH={home}/.local/bin:{home}/.cargo/bin:{home}/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\"\n\
         ExecStart={binary} facet daemon\n\
         Restart=on-failure\n\
         RestartSec=30\n\
         WorkingDirectory={home}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        home = home.display(),
        binary = binary.display(),
    );
    let path = service_dir.join(UNIT_NAME);
    std::fs::write(&path, unit).with_context(|| format!("write {}", path.display()))?;
    Ok(InstallOutcome { unit_path: path })
}

/// Remove the unit file if present. The caller is responsible for
/// `systemctl --user stop sb-facet.service && systemctl --user
/// disable sb-facet.service` before/after.
pub fn uninstall_systemd_service() -> Result<Option<PathBuf>> {
    let path = dirs::config_dir()
        .expect("dirs::config_dir() returned None")
        .join("systemd")
        .join("user")
        .join(UNIT_NAME);
    if !path.exists() {
        return Ok(None);
    }
    std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    Ok(Some(path))
}
