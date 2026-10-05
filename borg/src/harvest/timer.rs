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
//! (all three render through `vault::systemd::render_service`),
//! written to its OWN env-file so a one-shot harvest run never clobbers the
//! long-running daemon's captured environment. `None` (the default) omits
//! both directives - a host with nothing to bootstrap still gets a valid unit.

use std::path::Path;

use eyre::{Context, Result};

use crate::config::Config;
use vault::systemd::{Hardening, ServiceUnit, TimerUnit, UnitType, render_service, render_timer};

/// The oneshot service unit filename.
pub const HARVEST_SERVICE: &str = "sb-harvest.service";
/// The timer unit filename.
pub const HARVEST_TIMER: &str = "sb-harvest.timer";

/// Describe the `(service, timer)` units and render them through
/// `vault::systemd`. Pure - no filesystem or environment access beyond the
/// args - so `install` and the tests share one seam (tests assert on the
/// returned strings instead of touching the real `~/.config/systemd/user/`).
///
/// `config_path` is the borg.yml to pin with `--config`, or `None` to omit
/// the flag; the caller decides (it passes the file when it exists) so the
/// timer's stripped environment can't resolve a different one.
pub fn render_units(home: &Path, binary: &Path, config_path: Option<&Path>, config: &Config) -> (String, String) {
    log::debug!(
        "harvest::timer::render_units: binary={} config_path={:?} schedule={:?}",
        binary.display(),
        config_path,
        config.harvest.schedule
    );

    // `--config` is a flag on `sb borg`, NOT on the `harvest` subcommand, so it
    // goes BEFORE `harvest` in the ExecStart. Emitting
    // `borg harvest --config <path>` made every scheduled run die instantly with
    // `error: unexpected argument '--config' found` (exit 2), which nothing
    // noticed because the timer had never actually fired on this host.
    let mut exec_start = vec![binary.display().to_string(), "borg".to_string()];
    if let Some(path) = config_path {
        exec_start.push("--config".to_string());
        exec_start.push(path.display().to_string());
    }
    exec_start.push("harvest".to_string());

    // `env_bootstrap: None` omits both bootstrap directives so a host with
    // nothing to bootstrap still gets a valid, complete unit - never
    // fabricated.
    let service = ServiceUnit {
        description: "sb borg harvest - nightly Claude-session harvest into the vault (second-brain)".to_string(),
        after: vec!["default.target".to_string()],
        wants: Vec::new(),
        start_limit: None,
        unit_type: UnitType::Oneshot,
        env_bootstrap: config.harvest.env_bootstrap.clone(),
        home: home.to_path_buf(),
        path_comment: Some(
            "Timers run with a stripped PATH; set it explicitly and use the\n\
             absolute binary below so the run resolves with an empty inherited env.\n\
             mise shims come first so mise-managed tools (e.g. fabric) win over\n\
             any stale duplicate elsewhere on PATH."
                .to_string(),
        ),
        extra_env: Vec::new(),
        exec_start,
        restart: None,
        hardening: Hardening::Minimal {
            why: "harvest writes the vault + ~/.local/share/sb, so no\nProtectHome/ProtectSystem lockdown here"
                .to_string(),
        },
        // Installed through the timer, so the service has no [Install].
        wanted_by: None,
    };

    // The ONE value that IS the timer. Everything else lives in borg.yml.
    let timer = TimerUnit {
        description: "Nightly sb borg harvest timer (second-brain)".to_string(),
        on_calendar: config.harvest.schedule.clone(),
        persistent: true,
        wanted_by: "timers.target".to_string(),
    };

    (render_service(&service), render_timer(&timer))
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
    let borg_config = vault::paths::borg_config();
    let config_path = borg_config.exists().then_some(borg_config.as_path());
    let (service, timer) = render_units(&home, &binary, config_path, config);

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
