//! The daily `sb-eval` systemd user timer: `sb-eval.service` (oneshot:
//! `sb borg eval --unscored`) plus `sb-eval.timer`, cloned from the harvest
//! timer (`harvest::timer`) and rendered through `vault::systemd`.
//!
//! Like harvest, the timer's ONLY tunable is `OnCalendar` (`eval.schedule`);
//! `eval.limit`, `eval.judge-models` and the judge keys are read from borg.yml
//! by the service's own ExecStart at fire time. The service runs with a
//! stripped environment, so it bootstraps its own secrets (`eval.env-bootstrap`
//! into its own env-file) exactly as `sb-harvest.service` does.
//!
//! The judge keys come from `llm.api-keys`, which names the vars each judge
//! needs. The unit cannot decrypt "just those", so `eval.env-bootstrap` is
//! required whenever that map is non-empty: installing a unit that would run
//! with none of the vars its own config names is an error, not a warning.

use std::path::Path;

use eyre::{Context, Result};

use crate::config::Config;
use vault::systemd::{Hardening, ServiceUnit, TimerUnit, UnitType, render_service, render_timer};

/// The oneshot service unit filename.
pub const EVAL_SERVICE: &str = "sb-eval.service";
/// The timer unit filename.
pub const EVAL_TIMER: &str = "sb-eval.timer";

/// Describe the `(service, timer)` units and render them. Pure: no filesystem
/// or environment access beyond the args, so the tests assert on the returned
/// text. `config_path` is the borg.yml to pin with `--config` (before the
/// subcommand: it is a flag on `sb borg`, not on `eval`), or `None`.
pub fn render_units(
    home: &Path,
    binary: &Path,
    config_path: Option<&Path>,
    config: &Config,
) -> Result<(String, String)> {
    log::debug!(
        "eval::timer::render_units: binary={} config_path={:?} schedule={:?}",
        binary.display(),
        config_path,
        config.eval.schedule
    );
    if !config.llm.api_keys.is_empty() && config.eval.env_bootstrap.is_none() {
        let vars: Vec<&str> = config.llm.api_keys.iter().map(|(_, var)| var).collect();
        eyre::bail!(
            "llm.api-keys names {} but eval.env-bootstrap is not set: sb-eval.service would run \
             without those vars. Add eval.env-bootstrap (command + its own env-file) to borg.yml",
            vars.join(", ")
        );
    }

    let mut exec_start = vec![binary.display().to_string(), "borg".to_string()];
    if let Some(path) = config_path {
        exec_start.push("--config".to_string());
        exec_start.push(path.display().to_string());
    }
    exec_start.push("eval".to_string());
    exec_start.push("--unscored".to_string());

    let service = ServiceUnit {
        description: "sb borg eval --unscored - score unscored ingests into eval_scores (second-brain)".to_string(),
        after: vec!["default.target".to_string()],
        wants: Vec::new(),
        start_limit: None,
        unit_type: UnitType::Oneshot,
        env_bootstrap: config.eval.env_bootstrap.clone(),
        home: home.to_path_buf(),
        path_comment: Some(
            "Timers run with a stripped PATH; set it explicitly and use the\n\
             absolute binary below. fabric (the judge transport) resolves from\n\
             this PATH, mise shims first."
                .to_string(),
        ),
        extra_env: Vec::new(),
        exec_start,
        restart: None,
        timeout_stop_sec: None,
        kill_mode: None,
        success_exit_status: vec![130],
        hardening: Hardening::Minimal {
            why: "the eval writes ~/.local/share/sb/borg/eval-cache.db and reads staging, so no\nProtectHome/ProtectSystem lockdown here"
                .to_string(),
        },
        wanted_by: None,
    };

    let timer = TimerUnit {
        description: "Daily sb borg eval timer (second-brain)".to_string(),
        on_calendar: config.eval.schedule.clone(),
        persistent: true,
        wanted_by: "timers.target".to_string(),
    };

    Ok((render_service(&service), render_timer(&timer)))
}

/// The `(service, timer)` texts `install` writes, resolved from ambient state.
pub fn desired_units(config: &Config) -> Result<(String, String)> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let binary = std::env::current_exe().context("failed to get current executable path")?;
    let borg_config = vault::paths::borg_config();
    let config_path = borg_config.exists().then_some(borg_config.as_path());
    render_units(&home, &binary, config_path, config)
}

/// Write the eval service + timer into `~/.config/systemd/user/` and return the
/// lines to print. Enabling the timer is the caller's step (`sb bootstrap`).
pub fn install(config: &Config) -> Result<Vec<String>> {
    let service_dir = vault::systemd::user_unit_dir()?;
    std::fs::create_dir_all(&service_dir).context("failed to create systemd user dir")?;
    let (service, timer) = desired_units(config)?;

    let service_path = service_dir.join(EVAL_SERVICE);
    let timer_path = service_dir.join(EVAL_TIMER);
    std::fs::write(&service_path, &service).with_context(|| format!("write {}", service_path.display()))?;
    std::fs::write(&timer_path, &timer).with_context(|| format!("write {}", timer_path.display()))?;
    Ok(vec![
        format!("Installed: {}", service_path.display()),
        format!("Installed: {}", timer_path.display()),
    ])
}

/// The `systemctl --user` argument lists `sb bootstrap` runs after writing the
/// units, in order: pick up the new unit files, then enable and start the
/// timer. Data, so a test pins them without a systemd.
pub fn enable_commands() -> [Vec<String>; 2] {
    [
        vec!["--user".to_string(), "daemon-reload".to_string()],
        vec![
            "--user".to_string(),
            "enable".to_string(),
            "--now".to_string(),
            EVAL_TIMER.to_string(),
        ],
    ]
}

#[cfg(test)]
mod tests;
