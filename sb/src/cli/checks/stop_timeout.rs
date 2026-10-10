//! Doctor checks: the cortex stop budget and the user manager's stop deadline.

use std::process::Command;

use super::Finding;

/// The stop-timeout doctor checks: the cortex stop budget against the
/// per-call timeouts (the check `sb cortex daemon --install` refuses on), and
/// the user manager's own stop deadline against the cortex budget. A config
/// that does not load is reported by the config section, not here. The
/// user-manager read only applies on a host that has cortex.service installed.
pub(super) fn findings() -> Vec<Finding> {
    let cfg = match cortex::config::Config::load_service_config(&vault::paths::cortex_config()) {
        Ok(cfg) => cfg,
        Err(_) => return Vec::new(),
    };
    let mut findings = vec![stop_budget_finding(&cfg)];
    let installed = vault::systemd::user_unit_dir().is_ok_and(|dir| dir.join("cortex.service").exists());
    if installed {
        findings.push(user_manager_stop_finding(&cfg, read_user_manager_stop_timeout));
    }
    findings
}

fn stop_budget_finding(cfg: &cortex::config::Config) -> Finding {
    log::debug!(
        "stop_budget_finding: stop_timeout_secs={}",
        cfg.daemon.stop_timeout_secs
    );
    match cfg.stop_budget_shortfall() {
        Some(shortfall) => Finding::error(
            format!("cortex stop budget: {shortfall}"),
            format!(
                "set daemon.stop-timeout-secs >= {} in {}, then sb cortex daemon --install and restart cortex",
                shortfall.required_secs(),
                vault::paths::cortex_config().display()
            ),
        ),
        None => Finding::ok(format!(
            "cortex stop budget: daemon.stop-timeout-secs ({}) covers the longest per-call timeout + {}s",
            cfg.daemon.stop_timeout_secs,
            cortex::config::STOP_TIMEOUT_MARGIN_SECS
        )),
    }
}

/// `read` returns the effective `TimeoutStopUSec` of `user@<uid>.service`.
/// Below `daemon.stop-timeout-secs` + margin, or unreadable, is an Error:
/// systemd SIGKILLs any user service still running when the manager's own stop
/// deadline passes, whatever the service's own `TimeoutStopSec`.
fn user_manager_stop_finding(
    cfg: &cortex::config::Config,
    read: impl FnOnce() -> Result<std::time::Duration, String>,
) -> Finding {
    let needed = cfg.daemon.stop_timeout_secs + cortex::config::STOP_TIMEOUT_MARGIN_SECS;
    let fix = format!(
        "set TimeoutStopSec={needed} in /etc/systemd/system/user@.service.d/zz-cortex-stop.conf \
         (dotfiles manifest entry user-manager-stop-timeout), then sudo systemctl daemon-reload"
    );
    log::debug!("user_manager_stop_finding: needed={needed}s");
    match read() {
        Err(e) => Finding::error(format!("user@ TimeoutStopUSec: cannot read ({e})"), fix),
        Ok(actual) if actual < std::time::Duration::from_secs(needed) => Finding::error(
            format!(
                "user@ TimeoutStopUSec is {}s, below daemon.stop-timeout-secs ({}) + {}s margin = {needed}s; a reboot would SIGKILL a stopping cortex",
                actual.as_secs(),
                cfg.daemon.stop_timeout_secs,
                cortex::config::STOP_TIMEOUT_MARGIN_SECS,
            ),
            fix,
        ),
        Ok(actual) => Finding::ok(format!(
            "user@ TimeoutStopUSec ({}s) covers daemon.stop-timeout-secs + {}s",
            actual.as_secs(),
            cortex::config::STOP_TIMEOUT_MARGIN_SECS
        )),
    }
}

/// `systemctl show -p TimeoutStopUSec --value user@<uid>.service` (system
/// manager), parsed.
fn read_user_manager_stop_timeout() -> Result<std::time::Duration, String> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self").map_err(|e| format!("uid: {e}"))?.uid();
    let output = Command::new("systemctl")
        .args([
            "show",
            "-p",
            "TimeoutStopUSec",
            "--value",
            &format!("user@{uid}.service"),
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    parse_timeout_stop_usec(&String::from_utf8_lossy(&output.stdout))
}

/// systemd prints a timespan as `3min 30s`, `5s` or `infinity`.
fn parse_timeout_stop_usec(text: &str) -> Result<std::time::Duration, String> {
    let text = text.trim();
    if text == "infinity" {
        return Ok(std::time::Duration::MAX);
    }
    humantime::parse_duration(text).map_err(|e| format!("unparseable TimeoutStopUSec {text:?}: {e}"))
}

#[cfg(test)]
mod tests;
