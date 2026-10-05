//! The one systemd user-unit renderer. borg's daemon, cortex's daemon, and
//! the nightly harvest timer each describe their unit as a [`ServiceUnit`]
//! (and the harvest timer as a [`TimerUnit`]); [`render_service`] and
//! [`render_timer`] turn that into unit-file text.
//!
//! Rendering is pure: no filesystem or environment access. Every path
//! (home, binary, config file, data dir) is resolved by the caller and
//! passed in, so a test renders a unit from plain values.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Secret/environment bootstrap for an installed unit: `command`'s stdout is
/// captured into `env_file` via `ExecStartPre=/bin/sh -c '<command> > <env_file>'`,
/// then the unit loads it with `EnvironmentFile=-<env_file>` (the leading `-`
/// makes a missing file non-fatal). The `env-bootstrap` block of borg.yml
/// (`daemon.env-bootstrap`, `harvest.env-bootstrap`) and cortex.yml
/// (`daemon.env-bootstrap`) all deserialize into this one type.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct EnvBootstrap {
    /// Shell command whose stdout is redirected into `env_file`.
    pub command: String,
    /// Destination path for the captured environment, e.g.
    /// `/run/user/1000/borg.env`. Tilde-expanded at load time.
    #[serde(deserialize_with = "crate::paths::deserialize_tilde_pathbuf")]
    pub env_file: PathBuf,
}

/// `[Unit]` `StartLimitBurst=` / `StartLimitIntervalSec=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartLimit {
    pub burst: u32,
    pub interval_sec: u32,
}

/// `[Service]` `Type=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitType {
    Simple,
    Oneshot,
}

impl UnitType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Oneshot => "oneshot",
        }
    }
}

/// `[Service]` `Restart=` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    Always,
    OnFailure,
}

impl RestartPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::OnFailure => "on-failure",
        }
    }
}

/// `Restart=` plus `RestartSec=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Restart {
    pub policy: RestartPolicy,
    pub sec: u32,
}

/// One `Environment="NAME=value"` line, optionally preceded by a comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvVar {
    /// Comment text; each line is rendered as `# <line>`.
    pub comment: Option<String>,
    pub name: String,
    pub value: String,
}

/// The `# Hardening` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hardening {
    /// `ProtectSystem=strict` + `ProtectHome=read-only`, with `rw_paths` the
    /// only writable locations.
    Strict { rw_paths: Vec<PathBuf> },
    /// `NoNewPrivileges` + `PrivateTmp` only; `why` says why the unit cannot
    /// take the strict lockdown and is rendered in the block's comment.
    Minimal { why: String },
}

/// A service unit, described field by field. Every path is already resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceUnit {
    pub description: String,
    /// `After=` targets, space-joined on one line.
    pub after: Vec<String>,
    /// `Wants=` targets; the line is omitted when empty.
    pub wants: Vec<String>,
    pub start_limit: Option<StartLimit>,
    pub unit_type: UnitType,
    pub env_bootstrap: Option<EnvBootstrap>,
    /// The user's home: the `PATH` prefix dirs and `WorkingDirectory=`.
    pub home: PathBuf,
    /// Comment rendered just above the `PATH` line.
    pub path_comment: Option<String>,
    /// Environment lines rendered after `PATH`, in order.
    pub extra_env: Vec<EnvVar>,
    /// `ExecStart=` argv, space-joined; the first element is the binary.
    pub exec_start: Vec<String>,
    pub restart: Option<Restart>,
    pub hardening: Hardening,
    /// `[Install]` `WantedBy=`; the section is omitted when `None` (a
    /// timer-activated service is installed through its timer).
    pub wanted_by: Option<String>,
}

/// A timer unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerUnit {
    pub description: String,
    pub on_calendar: String,
    pub persistent: bool,
    pub wanted_by: String,
}

/// The `PATH` every unit runs with: mise shims first (mise-managed tools such
/// as fabric win over any stale duplicate), then the user's bin dirs, then
/// the system dirs. Units run with a stripped environment, so this is set
/// explicitly.
pub fn unit_path(home: &Path) -> String {
    let home = home.display();
    format!(
        "{home}/.local/share/mise/shims:{home}/.local/bin:{home}/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
    )
}

fn push_comment(out: &mut String, text: &str) {
    for line in text.lines() {
        let _ = writeln!(out, "# {line}");
    }
}

fn push_env(out: &mut String, name: &str, value: &str) {
    let _ = writeln!(out, "Environment=\"{name}={value}\"");
}

/// Render a service unit's text.
pub fn render_service(unit: &ServiceUnit) -> String {
    log::debug!(
        "systemd::render_service: description={:?} type={} env_bootstrap={} extra_env={} restart={:?}",
        unit.description,
        unit.unit_type.as_str(),
        unit.env_bootstrap.is_some(),
        unit.extra_env.len(),
        unit.restart.map(|r| r.policy.as_str()),
    );

    let mut out = String::new();
    out.push_str("[Unit]\n");
    let _ = writeln!(out, "Description={}", unit.description);
    if !unit.after.is_empty() {
        let _ = writeln!(out, "After={}", unit.after.join(" "));
    }
    if !unit.wants.is_empty() {
        let _ = writeln!(out, "Wants={}", unit.wants.join(" "));
    }
    if let Some(limit) = unit.start_limit {
        let _ = writeln!(out, "StartLimitBurst={}", limit.burst);
        let _ = writeln!(out, "StartLimitIntervalSec={}", limit.interval_sec);
    }

    out.push_str("\n[Service]\n");
    let _ = writeln!(out, "Type={}", unit.unit_type.as_str());
    if let Some(bootstrap) = &unit.env_bootstrap {
        let env_file = bootstrap.env_file.display();
        let _ = writeln!(out, "ExecStartPre=/bin/sh -c '{} > {env_file}'", bootstrap.command);
        let _ = writeln!(out, "EnvironmentFile=-{env_file}");
    }
    if let Some(comment) = &unit.path_comment {
        push_comment(&mut out, comment);
    }
    push_env(&mut out, "PATH", &unit_path(&unit.home));
    for var in &unit.extra_env {
        if let Some(comment) = &var.comment {
            push_comment(&mut out, comment);
        }
        push_env(&mut out, &var.name, &var.value);
    }
    let _ = writeln!(out, "ExecStart={}", unit.exec_start.join(" "));
    if let Some(restart) = unit.restart {
        let _ = writeln!(out, "Restart={}", restart.policy.as_str());
        let _ = writeln!(out, "RestartSec={}", restart.sec);
    }
    let _ = writeln!(out, "WorkingDirectory={}", unit.home.display());

    out.push('\n');
    match &unit.hardening {
        Hardening::Strict { rw_paths } => {
            out.push_str("# Hardening\n");
            out.push_str("NoNewPrivileges=true\n");
            out.push_str("ProtectSystem=strict\n");
            out.push_str("ProtectHome=read-only\n");
            let paths: Vec<String> = rw_paths.iter().map(|p| p.display().to_string()).collect();
            let _ = writeln!(out, "ReadWritePaths={}", paths.join(" "));
            out.push_str("PrivateTmp=true\n");
        }
        Hardening::Minimal { why } => {
            push_comment(&mut out, &format!("Hardening ({why})."));
            out.push_str("NoNewPrivileges=true\n");
            out.push_str("PrivateTmp=true\n");
        }
    }

    if let Some(target) = &unit.wanted_by {
        let _ = write!(out, "\n[Install]\nWantedBy={target}\n");
    }

    out
}

/// Render a timer unit's text.
pub fn render_timer(timer: &TimerUnit) -> String {
    log::debug!(
        "systemd::render_timer: description={:?} on_calendar={:?}",
        timer.description,
        timer.on_calendar
    );

    let mut out = String::new();
    let _ = writeln!(out, "[Unit]\nDescription={}", timer.description);
    let _ = writeln!(out, "\n[Timer]\nOnCalendar={}", timer.on_calendar);
    if timer.persistent {
        out.push_str("Persistent=true\n");
    }
    let _ = writeln!(out, "\n[Install]\nWantedBy={}", timer.wanted_by);
    out
}

#[cfg(test)]
mod tests;
