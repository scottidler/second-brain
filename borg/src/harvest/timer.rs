//! Phase 8: the nightly harvest systemd user timer. `sb borg harvest
//! --install` writes a `sb-harvest.service` (oneshot: `sb borg harvest`) plus a
//! `sb-harvest.timer` whose ONLY tunable is `OnCalendar` (from
//! `harvest.schedule`); every behavioral knob stays in `borg.yml`, read by the
//! service's ExecStart at fire time. On-demand and scheduled runs share one
//! core (`harvest::run`); the timer is just a scheduled `sb borg harvest`.
//!
//! systemd timers run with a stripped PATH, so the ExecStart uses the ABSOLUTE
//! binary path and the unit sets an explicit `PATH=` - the run resolves even
//! with an empty inherited environment (the `clyde_binary` config default is
//! likewise absolute and tilde-expanded).
//!
//! The stripped environment also means NO decrypted secrets reach the run
//! unless the unit bootstraps them itself (design doc: 2026-07-20
//! harvest-completion, Phase 5). When `harvest.env_bootstrap` is configured,
//! `sb-harvest.service` carries the same `ExecStartPre` decrypt +
//! `EnvironmentFile` directives the borg/cortex daemon units already emit
//! (`crate::service::install_systemd`, `cortex::daemon::render_systemd_unit`),
//! written to its OWN env-file so a one-shot harvest run never clobbers the
//! long-running daemon's captured environment. `None` (the default) omits
//! both directives - a host with nothing to bootstrap still gets a valid unit.

use std::path::Path;

use eyre::{Context, Result};

use crate::config::Config;

/// The oneshot service unit filename.
pub const HARVEST_SERVICE: &str = "sb-harvest.service";
/// The timer unit filename.
pub const HARVEST_TIMER: &str = "sb-harvest.timer";

/// Render the `(service, timer)` unit contents. Pure - no filesystem or
/// environment access beyond the args - so `install` and the tests share one
/// seam (tests assert on the returned strings instead of touching the real
/// `~/.config/systemd/user/`).
pub fn render_units(home: &Path, binary: &Path, config: &Config) -> (String, String) {
    log::debug!(
        "harvest::timer::render_units: binary={} schedule={:?}",
        binary.display(),
        config.harvest.schedule
    );

    // Pin the config path explicitly when present so the timer's stripped
    // environment can't resolve a different one.
    //
    // `--config` is a flag on `sb borg`, NOT on the `harvest` subcommand, so it
    // is interpolated BEFORE `harvest` in the ExecStart below. Emitting
    // `borg harvest --config <path>` made every scheduled run die instantly with
    // `error: unexpected argument '--config' found` (exit 2), which nothing
    // noticed because the timer had never actually fired on this host.
    let config_flag = {
        let path = vault::paths::borg_config();
        if path.exists() {
            format!(" --config {}", path.display())
        } else {
            String::new()
        }
    };

    let mut service = String::from(
        "[Unit]\n\
         Description=sb borg harvest - nightly Claude-session harvest into the vault (second-brain)\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=oneshot\n",
    );

    // Same secret/env bootstrap the borg/cortex daemon units emit
    // (`borg::service::install_systemd`, `cortex::daemon::render_systemd_unit`).
    // `None` omits both directives so a host with nothing to bootstrap still
    // gets a valid, complete unit - never fabricated.
    if let Some(bootstrap) = &config.harvest.env_bootstrap {
        service.push_str(&format!(
            "ExecStartPre=/bin/sh -c '{command} > {env_file}'\n",
            command = bootstrap.command,
            env_file = bootstrap.env_file.display(),
        ));
        service.push_str(&format!("EnvironmentFile=-{}\n", bootstrap.env_file.display()));
    }

    service.push_str(&format!(
        "# Timers run with a stripped PATH; set it explicitly and use the\n\
         # absolute binary below so the run resolves with an empty inherited env.\n\
         # mise shims come first so mise-managed tools (e.g. fabric) win over\n\
         # any stale duplicate elsewhere on PATH.\n\
         Environment=\"PATH={home}/.local/share/mise/shims:{home}/.local/bin:{home}/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\"\n\
         ExecStart={binary} borg{config_flag} harvest\n\
         WorkingDirectory={home}\n\
         \n\
         # Hardening (harvest writes the vault + ~/.local/share/sb, so no\n\
         # ProtectHome/ProtectSystem lockdown here).\n\
         NoNewPrivileges=true\n\
         PrivateTmp=true\n",
        home = home.display(),
        binary = binary.display(),
    ));

    // The ONE value that IS the timer. Everything else lives in borg.yml.
    let timer = format!(
        "[Unit]\n\
         Description=Nightly sb borg harvest timer (second-brain)\n\
         \n\
         [Timer]\n\
         OnCalendar={schedule}\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n",
        schedule = config.harvest.schedule,
    );

    (service, timer)
}

/// Install the harvest service + timer into `~/.config/systemd/user/`.
/// Returns the lines `sb` should print (paths written, follow-up systemctl).
pub fn install(config: &Config) -> Result<Vec<String>> {
    log::debug!("harvest::timer::install: schedule={:?}", config.harvest.schedule);
    let service_dir = vault::paths::xdg_config_dir()
        .expect("xdg_config_dir() returned None (set HOME or XDG_CONFIG_HOME)")
        .join("systemd")
        .join("user");
    std::fs::create_dir_all(&service_dir).context("failed to create systemd user dir")?;

    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let binary = std::env::current_exe().context("failed to get current executable path")?;
    let (service, timer) = render_units(&home, &binary, config);

    let service_path = service_dir.join(HARVEST_SERVICE);
    let timer_path = service_dir.join(HARVEST_TIMER);
    std::fs::write(&service_path, &service).with_context(|| format!("write {}", service_path.display()))?;
    std::fs::write(&timer_path, &timer).with_context(|| format!("write {}", timer_path.display()))?;

    Ok(vec![
        format!("Installed: {}", service_path.display()),
        format!("Installed: {}", timer_path.display()),
        String::new(),
        "Run:".to_string(),
        "  systemctl --user daemon-reload".to_string(),
        format!("  systemctl --user enable --now {HARVEST_TIMER}"),
        format!(
            "  (mode: {:?} - flip harvest.mode to live after the soak)",
            config.harvest.mode
        ),
    ])
}

/// Uninstall the harvest service + timer units. Idempotent (a missing unit is
/// not an error).
pub fn uninstall() -> Result<Vec<String>> {
    log::debug!("harvest::timer::uninstall");
    let service_dir = vault::paths::xdg_config_dir()
        .expect("xdg_config_dir() returned None (set HOME or XDG_CONFIG_HOME)")
        .join("systemd")
        .join("user");

    let mut lines = Vec::new();
    for unit in [HARVEST_SERVICE, HARVEST_TIMER] {
        let path = service_dir.join(unit);
        match std::fs::remove_file(&path) {
            Ok(()) => lines.push(format!("Removed: {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("remove {}", path.display())),
        }
    }
    if lines.is_empty() {
        lines.push("No harvest timer units were installed.".to_string());
    } else {
        lines.push(String::new());
        lines.push("Run: systemctl --user daemon-reload".to_string());
    }
    Ok(lines)
}

#[cfg(test)]
mod tests;
