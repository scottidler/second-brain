//! Daemon lifecycle + OS service management (systemd / launchd) and the GNOME
//! hotkey keybinding install/uninstall. Extracted from `lib.rs` (Phase 8 bloat
//! decomposition) along the daemon-vs-cli seam; `lib.rs` keeps the HTTP server
//! (`serve_init`) and the ingest entry points, and re-exports `daemon` /
//! `DaemonOutcome` from here so the public API (`borg::daemon`) is unchanged.

use crate::config::Config;
use crate::opts;
use eyre::{Context, Result};
use std::path::{Path, PathBuf};
use vault::systemd::{Hardening, Restart, RestartPolicy, ServiceUnit, StartLimit, UnitType, render_service};

/// Outcome of a `sb borg daemon <flag>` invocation (everything except
/// `--start`, which sb routes to `serve_init`). Variants carry the typed
/// data sb needs to format the user-facing message; no pre-rendered text
/// crosses the lib boundary. `Status` carries the raw systemctl-status
/// blob because systemd's output is not contract-stable across versions;
/// parsing structured fields out of it would be brittle scope-creep
/// (per 2026-05-20 architect consensus).
#[derive(Debug)]
pub enum DaemonOutcome {
    Installed { unit_path: PathBuf },
    Uninstalled { unit_path: PathBuf },
    NotInstalled { unit_path: PathBuf },
    Reinstalled { unit_path: PathBuf },
    Stopped,
    Restarted,
    Status { raw_output: String },
    NoAction,
}

/// Internal: outcome of an uninstall attempt. `was_present = false` means
/// the unit file was already absent (no-op); `true` means a file was
/// removed.
pub(crate) struct UninstallOutcome {
    unit_path: PathBuf,
    was_present: bool,
}

/// Dispatch the non-start daemon flags (install/uninstall/reinstall/stop/restart/status).
/// `--start` is handled separately by sb via `serve_init` + `ServerHandle::wait` so the
/// startup banner can be formatted from typed data.
pub async fn daemon(config: Config, opts: opts::DaemonOpts) -> Result<DaemonOutcome> {
    use crate::opts::DaemonOpts;

    match opts {
        DaemonOpts { install: true, .. } => Ok(DaemonOutcome::Installed {
            unit_path: install_service(&config).await?,
        }),
        DaemonOpts { uninstall: true, .. } => {
            let outcome = uninstall_service().await?;
            if outcome.was_present {
                Ok(DaemonOutcome::Uninstalled {
                    unit_path: outcome.unit_path,
                })
            } else {
                Ok(DaemonOutcome::NotInstalled {
                    unit_path: outcome.unit_path,
                })
            }
        }
        DaemonOpts { reinstall: true, .. } => {
            if let Err(e) = uninstall_service().await {
                log::warn!("service: reinstall could not remove the existing service, installing over it: {e:#}");
            }
            Ok(DaemonOutcome::Reinstalled {
                unit_path: install_service(&config).await?,
            })
        }
        DaemonOpts { stop: true, .. } => {
            stop_service().await?;
            Ok(DaemonOutcome::Stopped)
        }
        DaemonOpts { restart: true, .. } => {
            restart_service().await?;
            Ok(DaemonOutcome::Restarted)
        }
        DaemonOpts { status: true, .. } => Ok(DaemonOutcome::Status {
            raw_output: show_status().await?,
        }),
        DaemonOpts { start: true, .. } => Err(eyre::eyre!(
            "borg::daemon: --start should be dispatched by sb via serve_init"
        )),
        _ => Ok(DaemonOutcome::NoAction),
    }
}

pub(crate) async fn install_service(config: &Config) -> Result<PathBuf> {
    let exe_path = std::env::current_exe().context("Failed to detect binary path")?;
    let exe = exe_path.display().to_string();

    if cfg!(target_os = "linux") {
        install_systemd(&exe, config).await
    } else if cfg!(target_os = "macos") {
        install_launchd(&exe).await
    } else {
        eyre::bail!("Unsupported platform for service install")
    }
}

pub(crate) async fn uninstall_service() -> Result<UninstallOutcome> {
    if cfg!(target_os = "linux") {
        uninstall_systemd().await
    } else if cfg!(target_os = "macos") {
        uninstall_launchd().await
    } else {
        eyre::bail!("Unsupported platform for service uninstall")
    }
}

pub(crate) async fn stop_service() -> Result<()> {
    if cfg!(target_os = "linux") {
        systemctl(&["stop", "borg"]).await?;
    } else if cfg!(target_os = "macos") {
        launchctl(&["stop", "com.borg"]).await?;
    } else {
        eyre::bail!("Unsupported platform for service stop")
    }
    Ok(())
}

pub(crate) async fn restart_service() -> Result<()> {
    if cfg!(target_os = "linux") {
        systemctl(&["restart", "borg"]).await?;
    } else if cfg!(target_os = "macos") {
        launchctl(&["stop", "com.borg"]).await.ok();
        launchctl(&["start", "com.borg"]).await?;
    } else {
        eyre::bail!("Unsupported platform for service restart")
    }
    Ok(())
}

pub(crate) async fn show_status() -> Result<String> {
    if cfg!(target_os = "linux") {
        let output = tokio::process::Command::new("systemctl")
            .args(["--user", "status", "borg"])
            .output()
            .await
            .context("Failed to run systemctl")?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else if cfg!(target_os = "macos") {
        let output = tokio::process::Command::new("launchctl")
            .args(["list", "com.borg"])
            .output()
            .await
            .context("Failed to run launchctl")?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        eyre::bail!("Unsupported platform for service status")
    }
}

/// Run `systemctl --user <args>` and return Ok if it succeeds.
pub(crate) async fn systemctl(args: &[&str]) -> Result<()> {
    let mut cmd_args = vec!["--user"];
    cmd_args.extend(args);
    let output = tokio::process::Command::new("systemctl")
        .args(&cmd_args)
        .output()
        .await
        .context("Failed to run systemctl")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eyre::bail!("systemctl --user {} failed: {stderr}", args.join(" "));
    }
    Ok(())
}

/// Run `launchctl <args>` and return Ok if it succeeds.
pub(crate) async fn launchctl(args: &[&str]) -> Result<()> {
    let output = tokio::process::Command::new("launchctl")
        .args(args)
        .output()
        .await
        .context("Failed to run launchctl")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eyre::bail!("launchctl {} failed: {stderr}", args.join(" "));
    }
    Ok(())
}

/// Describe `borg.service` and render it through `vault::systemd`. Pure - no
/// filesystem or environment access beyond the args - so `install_systemd`
/// and its tests share one seam (tests assert on the returned string instead
/// of touching the real `~/.config/systemd/user/`).
///
/// `config_path` is the borg.yml to pin with `--config` (`None` omits the
/// flag); the caller passes the file when it exists so the daemon's stripped
/// environment can't resolve a different one (cortex and harvest do the same).
fn render_systemd_unit(
    exe_path: &str,
    home: &Path,
    vault_path: &Path,
    data_path: &Path,
    config_path: Option<&Path>,
    config: &Config,
) -> String {
    log::debug!(
        "service::render_systemd_unit: exe_path={exe_path} vault_path={} config_path={config_path:?} env_bootstrap={}",
        vault_path.display(),
        config.daemon.env_bootstrap.is_some(),
    );

    let log_level = config.log_level.as_deref().unwrap_or("info");
    // `--config` is a flag on `sb borg`, so it goes before `daemon`.
    let mut exec_start = vec![exe_path.to_string(), "borg".to_string()];
    if let Some(path) = config_path {
        exec_start.push("--config".to_string());
        exec_start.push(path.display().to_string());
    }
    exec_start.extend([
        "--log-level".to_string(),
        log_level.to_string(),
        "daemon".to_string(),
        "--start".to_string(),
    ]);
    let unit = ServiceUnit {
        description: "borg - Obsidian ingestion daemon (second-brain)".to_string(),
        after: vec!["network-online.target".to_string()],
        wants: vec!["network-online.target".to_string()],
        start_limit: Some(StartLimit {
            burst: 5,
            interval_sec: 60,
        }),
        unit_type: UnitType::Simple,
        env_bootstrap: config.daemon.env_bootstrap.clone(),
        home: home.to_path_buf(),
        path_comment: None,
        extra_env: Vec::new(),
        exec_start,
        restart: Some(Restart {
            policy: RestartPolicy::Always,
            sec: 5,
        }),
        hardening: Hardening::Strict {
            rw_paths: vec![vault_path.to_path_buf(), data_path.to_path_buf()],
        },
        wanted_by: Some("default.target".to_string()),
    };
    render_service(&unit)
}

/// The `borg.service` text `--install` writes, resolved from ambient state
/// (home, vault, data dir, borg.yml). `sb doctor` calls this too, so drift is
/// measured against exactly what an install would write now.
pub fn desired_systemd_unit(exe_path: &str, config: &Config) -> Result<String> {
    log::debug!("service::desired_systemd_unit: exe_path={exe_path}");
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;

    // The vault must resolve: a unit that grants write access to a guessed
    // directory is worse than no unit. The borg-owned data dir
    // (`~/.local/share/sb/borg`: receipts DB, signal-state, staged artifacts)
    // MUST be in ReadWritePaths too - it only worked before because the user
    // manager wasn't enforcing ProtectHome; the moment it does, every
    // receipts/signal/stages write fails.
    let vault_path = config
        .vault_root()
        .context("cannot render borg.service: vault root does not resolve (set vault.root-path in borg.yml)")?;
    let data_path = vault::receipts::receipts_dir()
        .map(|d| d.parent().map(|p| p.to_path_buf()).unwrap_or(d))
        .unwrap_or_else(|_| home.join(".local/share/sb"));
    let borg_config = vault::paths::borg_config();
    let config_path = borg_config.exists().then_some(borg_config.as_path());

    Ok(render_systemd_unit(
        exe_path,
        &home,
        &vault_path,
        &data_path,
        config_path,
        config,
    ))
}

/// Render and write `borg.service` under the XDG user unit dir; returns its path.
fn write_systemd_unit(exe_path: &str, config: &Config) -> Result<PathBuf> {
    let unit_dir = vault::systemd::user_unit_dir()?;
    let unit_path = unit_dir.join("borg.service");
    let unit_content = desired_systemd_unit(exe_path, config)?;
    std::fs::create_dir_all(&unit_dir).context("Failed to create systemd user unit directory")?;
    std::fs::write(&unit_path, &unit_content).context("Failed to write systemd unit file")?;
    Ok(unit_path)
}

pub(crate) async fn install_systemd(exe_path: &str, config: &Config) -> Result<PathBuf> {
    // Render before stopping the running service: a render error must not
    // leave the daemon stopped.
    desired_systemd_unit(exe_path, config)?;

    // Stop the running service if active (ignore errors - may not be running)
    systemctl(&["stop", "borg"]).await.ok();

    let unit_path = write_systemd_unit(exe_path, config)?;

    // Reload so systemd picks up changes, then enable + start
    systemctl(&["daemon-reload"]).await?;
    systemctl(&["enable", "--now", "borg"]).await?;

    Ok(unit_path)
}

/// The launchd plist for the borg daemon. The label and log paths keep the
/// retired `obsidian-borg` name: renaming them would orphan an installed agent.
pub fn render_launchd_plist(exe_path: &str) -> String {
    log::debug!("service::render_launchd_plist: exe_path={exe_path}");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.obsidian-borg</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe_path}</string>
        <string>borg</string>
        <string>daemon</string>
        <string>--start</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/obsidian-borg.stdout.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/obsidian-borg.stderr.log</string>
</dict>
</plist>
"#
    )
}

/// The command the desktop hotkey runs: ingest the clipboard URL.
pub fn hotkey_command(exe_path: &Path) -> String {
    format!("{} borg ingest --clipboard", exe_path.display())
}

pub(crate) async fn install_launchd(exe_path: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let plist_dir = home.join("Library/LaunchAgents");
    let plist_path = plist_dir.join("com.obsidian-borg.plist");

    let plist_content = render_launchd_plist(exe_path);

    // Unload if already loaded (ignore errors - may not be loaded)
    launchctl(&["unload", &plist_path.to_string_lossy()]).await.ok();

    std::fs::create_dir_all(&plist_dir).context("Failed to create LaunchAgents directory")?;
    std::fs::write(&plist_path, &plist_content).context("Failed to write plist file")?;

    launchctl(&["load", &plist_path.to_string_lossy()]).await?;
    Ok(plist_path)
}

/// Run a service-manager command whose failure must not stop an uninstall (the unit
/// may already be stopped or unloaded), but must be visible when it happens.
fn best_effort(program: &str, args: &[&str]) {
    match std::process::Command::new(program).args(args).status() {
        Ok(status) if status.success() => {}
        Ok(status) => log::warn!("service: `{program} {}` exited with {status}", args.join(" ")),
        Err(e) => log::warn!("service: could not run `{program} {}`: {e}", args.join(" ")),
    }
}

pub(crate) async fn uninstall_systemd() -> Result<UninstallOutcome> {
    let unit_path = vault::systemd::user_unit_dir()?.join("borg.service");

    if !unit_path.exists() {
        return Ok(UninstallOutcome {
            unit_path,
            was_present: false,
        });
    }

    best_effort("systemctl", &["--user", "disable", "--now", "borg"]);

    std::fs::remove_file(&unit_path).context("Failed to remove unit file")?;

    best_effort("systemctl", &["--user", "daemon-reload"]);

    Ok(UninstallOutcome {
        unit_path,
        was_present: true,
    })
}

pub(crate) async fn uninstall_launchd() -> Result<UninstallOutcome> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let plist_path = home.join("Library/LaunchAgents/com.obsidian-borg.plist");

    if !plist_path.exists() {
        return Ok(UninstallOutcome {
            unit_path: plist_path,
            was_present: false,
        });
    }

    best_effort("launchctl", &["unload", &plist_path.to_string_lossy()]);

    std::fs::remove_file(&plist_path).context("Failed to remove plist file")?;
    Ok(UninstallOutcome {
        unit_path: plist_path,
        was_present: true,
    })
}

const GNOME_KEYBINDINGS_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const GNOME_KEYBINDING_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/obsidian-borg/";

/// Install (best-effort) the keyboard shortcut and return a non-Linux
/// fallback message when applicable. On Linux, returns None - sb prints
/// the standard installed banner.
pub(crate) async fn install_hotkey(key: &str) -> Result<(String, Option<String>)> {
    log::debug!("service::install_hotkey: key={key}");
    let exe_path = std::env::current_exe().context("Failed to detect binary path")?;
    let command = hotkey_command(&exe_path);

    if cfg!(target_os = "linux") {
        install_gnome_keybinding(&command, key)?;
        Ok((command, None))
    } else {
        let message = format!("Bind this command to {key} in your OS settings:\n  {command}");
        Ok((command, Some(message)))
    }
}

pub(crate) fn install_gnome_keybinding(command: &str, key: &str) -> Result<()> {
    // Get current custom keybinding paths
    let output = std::process::Command::new("gsettings")
        .args(["get", GNOME_KEYBINDINGS_SCHEMA, "custom-keybindings"])
        .output()
        .context("Failed to run gsettings — is GNOME available?")?;

    let current = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Parse current list and add our path if not present
    let new_list = if current == "@as []" || current.is_empty() {
        format!("['{}']", GNOME_KEYBINDING_PATH)
    } else if current.contains(GNOME_KEYBINDING_PATH) {
        current.clone()
    } else {
        // Insert before closing bracket
        let trimmed = current.trim_end_matches(']').trim_end_matches(", ");
        format!("{}, '{}']", trimmed, GNOME_KEYBINDING_PATH)
    };

    // Update the list
    std::process::Command::new("gsettings")
        .args(["set", GNOME_KEYBINDINGS_SCHEMA, "custom-keybindings", &new_list])
        .status()
        .context("Failed to update custom-keybindings list")?;

    // Set the keybinding properties
    let schema = format!(
        "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:{}",
        GNOME_KEYBINDING_PATH
    );

    for (prop, val) in [("name", "borg"), ("command", command), ("binding", key)] {
        let status = std::process::Command::new("gsettings")
            .args(["set", &schema, prop, val])
            .status()
            .context(format!("Failed to set keybinding {prop}"))?;
        if !status.success() {
            eyre::bail!("gsettings set {prop} failed");
        }
    }

    log::info!("registered GNOME keybinding: {key} -> {command}");
    Ok(())
}

pub(crate) async fn uninstall_hotkey() -> Result<()> {
    if cfg!(target_os = "linux") {
        uninstall_gnome_keybinding()?;
    }
    Ok(())
}

pub(crate) fn uninstall_gnome_keybinding() -> Result<()> {
    // Remove our path from the custom keybindings list
    let output = std::process::Command::new("gsettings")
        .args(["get", GNOME_KEYBINDINGS_SCHEMA, "custom-keybindings"])
        .output()
        .context("Failed to run gsettings")?;

    let current = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if current.contains(GNOME_KEYBINDING_PATH) {
        // Remove our entry from the list
        let new_list = current
            .replace(&format!("'{}'", GNOME_KEYBINDING_PATH), "")
            .replace(", ,", ",")
            .replace("[,", "[")
            .replace(",]", "]")
            .replace("[, ", "[")
            .replace(", ]", "]");

        // Normalize empty list
        let new_list = if new_list.trim() == "[]" || new_list.trim() == "[' ']" {
            "@as []".to_string()
        } else {
            new_list
        };

        std::process::Command::new("gsettings")
            .args(["set", GNOME_KEYBINDINGS_SCHEMA, "custom-keybindings", &new_list])
            .status()
            .context("Failed to update custom-keybindings list")?;

        // Reset the keybinding properties
        let schema = format!(
            "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:{}",
            GNOME_KEYBINDING_PATH
        );

        for prop in &["name", "command", "binding"] {
            let _ = std::process::Command::new("gsettings")
                .args(["reset", &schema, prop])
                .status();
        }

        log::info!("removed GNOME keybinding");
    } else {
        log::info!("no GNOME keybinding found to remove");
    }

    Ok(())
}

#[cfg(test)]
mod tests;
