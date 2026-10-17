//! The `sb-eval` systemd user timers, cloned from the harvest timer
//! (`harvest::timer`) and rendered through `vault::systemd`:
//! - `sb-eval.service` (oneshot: `sb borg eval --unscored`) plus `sb-eval.timer`, daily;
//! - `sb-eval-report.service` (oneshot: `sb borg eval --weekly`) plus
//!   `sb-eval-report.timer`, weekly (`eval.report.schedule`).
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
/// The weekly report's oneshot service unit filename.
pub const REPORT_SERVICE: &str = "sb-eval-report.service";
/// The weekly report's timer unit filename.
pub const REPORT_TIMER: &str = "sb-eval-report.timer";

/// What differs between the daily scoring units and the weekly report units.
struct Spec {
    /// The `sb borg eval` flag the service runs.
    flag: &'static str,
    service_description: &'static str,
    timer_description: &'static str,
    schedule: String,
    hardening_why: &'static str,
}

/// Describe the daily `(service, timer)` units and render them. Pure: no
/// filesystem or environment access beyond the args, so the tests assert on the
/// returned text. `config_path` is the borg.yml to pin with `--config` (before
/// the subcommand: it is a flag on `sb borg`, not on `eval`), or `None`.
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
    render(
        home,
        binary,
        config_path,
        config,
        Spec {
            flag: "--unscored",
            service_description: "sb borg eval --unscored - score unscored ingests into eval_scores (second-brain)",
            timer_description: "Daily sb borg eval timer (second-brain)",
            schedule: config.eval.schedule.clone(),
            hardening_why: "the eval writes ~/.local/share/sb/borg/eval-cache.db and reads staging, so no\nProtectHome/ProtectSystem lockdown here",
        },
    )
}

/// The weekly report's `(service, timer)` units, rendered like
/// [`render_units`]. The service carries the same secret bootstrap: the one
/// clustering call needs a vendor key.
pub fn render_report_units(
    home: &Path,
    binary: &Path,
    config_path: Option<&Path>,
    config: &Config,
) -> Result<(String, String)> {
    log::debug!(
        "eval::timer::render_report_units: binary={} config_path={:?} schedule={:?}",
        binary.display(),
        config_path,
        config.eval.report.schedule
    );
    render(
        home,
        binary,
        config_path,
        config,
        Spec {
            flag: "--weekly",
            service_description: "sb borg eval --weekly - weekly failure clusters, eval-reports/<date>.md (second-brain)",
            timer_description: "Weekly sb borg eval failure-cluster report timer (second-brain)",
            schedule: config.eval.report.schedule.clone(),
            hardening_why: "the report reads eval-cache.db, receipts.db and staging and writes eval-reports/, so no\nProtectHome/ProtectSystem lockdown here",
        },
    )
}

fn render(
    home: &Path,
    binary: &Path,
    config_path: Option<&Path>,
    config: &Config,
    spec: Spec,
) -> Result<(String, String)> {
    if !config.llm.api_keys.is_empty() && config.eval.env_bootstrap.is_none() {
        let vars: Vec<&str> = config.llm.api_keys.iter().map(|(_, var)| var).collect();
        eyre::bail!(
            "llm.api-keys names {} but eval.env-bootstrap is not set: the {} service would run \
             without those vars. Add eval.env-bootstrap (command + its own env-file) to borg.yml",
            vars.join(", "),
            spec.flag
        );
    }

    let mut exec_start = vec![binary.display().to_string(), "borg".to_string()];
    if let Some(path) = config_path {
        exec_start.push("--config".to_string());
        exec_start.push(path.display().to_string());
    }
    exec_start.push("eval".to_string());
    exec_start.push(spec.flag.to_string());

    let service = ServiceUnit {
        description: spec.service_description.to_string(),
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
            why: spec.hardening_why.to_string(),
        },
        wanted_by: None,
    };

    let timer = TimerUnit {
        description: spec.timer_description.to_string(),
        on_calendar: spec.schedule,
        persistent: true,
        wanted_by: "timers.target".to_string(),
    };

    Ok((render_service(&service), render_timer(&timer)))
}

/// The daily `(service, timer)` texts `install` writes, resolved from ambient
/// state.
pub fn desired_units(config: &Config) -> Result<(String, String)> {
    let (home, binary) = ambient()?;
    let borg_config = vault::paths::borg_config();
    let config_path = borg_config.exists().then_some(borg_config.as_path());
    render_units(&home, &binary, config_path, config)
}

/// The weekly report's `(service, timer)` texts `install` writes.
pub fn desired_report_units(config: &Config) -> Result<(String, String)> {
    let (home, binary) = ambient()?;
    let borg_config = vault::paths::borg_config();
    let config_path = borg_config.exists().then_some(borg_config.as_path());
    render_report_units(&home, &binary, config_path, config)
}

fn ambient() -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("Cannot determine home directory"))?;
    let binary = std::env::current_exe().context("failed to get current executable path")?;
    Ok((home, binary))
}

/// Write the daily and weekly eval services + timers into
/// `~/.config/systemd/user/` and return the lines to print. Enabling the timers
/// is the caller's step (`sb bootstrap`). Both units render before either is
/// written, so a config that cannot render the second leaves no half install.
pub fn install(config: &Config) -> Result<Vec<String>> {
    let service_dir = vault::systemd::user_unit_dir()?;
    std::fs::create_dir_all(&service_dir).context("failed to create systemd user dir")?;
    let (service, timer) = desired_units(config)?;
    let (report_service, report_timer) = desired_report_units(config)?;

    let mut lines = Vec::new();
    for (name, text) in [
        (EVAL_SERVICE, &service),
        (EVAL_TIMER, &timer),
        (REPORT_SERVICE, &report_service),
        (REPORT_TIMER, &report_timer),
    ] {
        let path = service_dir.join(name);
        std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
        lines.push(format!("Installed: {}", path.display()));
    }
    Ok(lines)
}

/// The `systemctl --user` argument lists `sb bootstrap` runs after writing the
/// units, in order: pick up the new unit files, then enable and start both
/// timers. Data, so a test pins them without a systemd.
pub fn enable_commands() -> [Vec<String>; 2] {
    [
        vec!["--user".to_string(), "daemon-reload".to_string()],
        vec![
            "--user".to_string(),
            "enable".to_string(),
            "--now".to_string(),
            EVAL_TIMER.to_string(),
            REPORT_TIMER.to_string(),
        ],
    ]
}

#[cfg(test)]
mod tests;
