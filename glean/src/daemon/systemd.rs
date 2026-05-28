//! systemd user-unit lifecycle for the glean daemon.

use eyre::{Context, Result};
use std::process::Command;

const SERVICE_NAME: &str = "glean.service";

pub fn install() -> Result<Vec<String>> {
    let unit_path = unit_path()?;
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent).context("mkdir systemd user dir")?;
    }
    let body = unit_body();
    std::fs::write(&unit_path, body).context("write glean.service")?;
    let mut lines = vec![format!("wrote {}", unit_path.display())];
    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&["--user", "enable", SERVICE_NAME])?;
    run_systemctl(&["--user", "restart", SERVICE_NAME])?;
    lines.push(format!("enabled and restarted {SERVICE_NAME}"));
    Ok(lines)
}

pub fn uninstall() -> Result<Vec<String>> {
    let unit_path = unit_path()?;
    let mut lines = Vec::new();
    let _ = Command::new("systemctl")
        .args(["--user", "stop", SERVICE_NAME])
        .status();
    let _ = Command::new("systemctl")
        .args(["--user", "disable", SERVICE_NAME])
        .status();
    if unit_path.exists() {
        std::fs::remove_file(&unit_path).with_context(|| format!("remove unit file {}", unit_path.display()))?;
        lines.push(format!("removed {}", unit_path.display()));
    }
    run_systemctl(&["--user", "daemon-reload"])?;
    Ok(lines)
}

pub fn status() -> Result<Vec<String>> {
    let out = Command::new("systemctl")
        .args(["--user", "is-active", SERVICE_NAME])
        .output()
        .context("systemctl is-active")?;
    let active = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(vec![format!("{SERVICE_NAME}: {active}")])
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    log::debug!("daemon::systemd::run_systemctl: args={args:?}");
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .context("spawn systemctl")?;
    if !status.success() {
        eyre::bail!("systemctl {args:?} exited with status {status:?}");
    }
    Ok(())
}

fn unit_path() -> Result<std::path::PathBuf> {
    let home = dirs::config_dir().ok_or_else(|| eyre::eyre!("dirs::config_dir() returned None"))?;
    Ok(home.join("systemd").join("user").join(SERVICE_NAME))
}

fn unit_body() -> String {
    let bin = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "sb".to_string());
    format!(
        r#"[Unit]
Description=glean - Claude Code session distiller
After=network.target

[Service]
Type=simple
ExecStart={bin} glean daemon
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=default.target
"#
    )
}
