//! Shared health checks consumed by `sb status` (informational rendering)
//! and `sb doctor` (severity-tagged findings).

use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Ok,
    Info,
    Warn,
    Error,
}

impl Severity {
    pub fn icon(self) -> &'static str {
        match self {
            Severity::Ok => "\u{2705}",
            Severity::Info => "\u{1f4ac}",
            Severity::Warn => "\u{26a0}\u{fe0f} ",
            Severity::Error => "\u{274c}",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
    pub suggested_fix: Option<String>,
}

impl Finding {
    pub fn ok(msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Ok,
            message: msg.into(),
            suggested_fix: None,
        }
    }
    pub fn info(msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            message: msg.into(),
            suggested_fix: None,
        }
    }
    pub fn warn(msg: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warn,
            message: msg.into(),
            suggested_fix: Some(fix.into()),
        }
    }
    pub fn error(msg: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: msg.into(),
            suggested_fix: Some(fix.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Section {
    pub name: &'static str,
    pub findings: Vec<Finding>,
}

pub fn all_sections() -> Vec<Section> {
    vec![
        Section {
            name: "systemd",
            findings: systemd_findings(),
        },
        Section {
            name: "config",
            findings: config_findings(),
        },
        Section {
            name: "patterns",
            findings: pattern_findings(),
        },
        Section {
            name: "embedding",
            findings: embedding_findings(),
        },
    ]
}

fn systemd_findings() -> Vec<Finding> {
    let mut findings = Vec::new();
    for unit in &["borg.service", "cortex.service"] {
        match systemctl_show(unit) {
            Ok(state) => {
                if state.active_state == "active" {
                    findings.push(Finding::ok(format!(
                        "{unit}: active (PID {pid}, RSS {rss})",
                        unit = unit,
                        pid = state.main_pid,
                        rss = human_bytes(state.memory_current),
                    )));
                } else if state.active_state == "inactive" {
                    findings.push(Finding::warn(
                        format!("{unit}: inactive"),
                        format!("systemctl --user start {unit}"),
                    ));
                } else {
                    findings.push(Finding::error(
                        format!("{unit}: {}", state.active_state),
                        format!("systemctl --user status {unit}"),
                    ));
                }
            }
            Err(e) => findings.push(Finding::error(
                format!("{unit}: query failed ({e})"),
                format!("systemctl --user status {unit}"),
            )),
        }
    }
    findings
}

fn config_findings() -> Vec<Finding> {
    let candidates = [
        ("borg", config_path("borg", "borg.yml")),
        ("cortex", config_path("obsidian-cortex", "obsidian-cortex.yml")),
        ("oracle", config_path("oracle", "oracle.yml")),
    ];
    candidates
        .iter()
        .map(|(name, path)| {
            if path.exists() {
                Finding::ok(format!("{name}: {}", path.display()))
            } else {
                Finding::warn(
                    format!("{name}: missing ({})", path.display()),
                    "sb bootstrap".to_string(),
                )
            }
        })
        .collect()
}

fn pattern_findings() -> Vec<Finding> {
    // Compare borg/patterns/*.md in the repo (working tree) against ~/.config/borg/patterns/*.md.
    // We can only detect drift on machines where the repo is checked out at the expected path.
    let repo_patterns = std::path::Path::new("borg/patterns");
    let installed = dirs::config_dir().map(|d| d.join("borg/patterns"));
    let mut findings = Vec::new();
    let Some(installed) = installed else {
        findings.push(Finding::error(
            "dirs::config_dir() returned None",
            "check XDG_CONFIG_HOME",
        ));
        return findings;
    };
    if !repo_patterns.exists() {
        findings.push(Finding::info(
            "borg/patterns not present in CWD (run from repo root for drift detection)",
        ));
        return findings;
    }
    if !installed.exists() {
        findings.push(Finding::warn(
            format!("{} missing", installed.display()),
            "otto deploy".to_string(),
        ));
        return findings;
    }
    let repo_files: Vec<_> = std::fs::read_dir(repo_patterns)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    let mut drift = 0usize;
    for entry in &repo_files {
        let name = entry.file_name();
        let installed_path = installed.join(&name);
        if !installed_path.exists() {
            drift += 1;
            continue;
        }
        if std::fs::read(entry.path()).ok() != std::fs::read(&installed_path).ok() {
            drift += 1;
        }
    }
    if drift == 0 {
        findings.push(Finding::ok(format!("{} patterns in sync", repo_files.len())));
    } else {
        findings.push(Finding::warn(
            format!("{drift} of {} patterns drifted vs repo", repo_files.len()),
            "otto deploy".to_string(),
        ));
    }
    findings
}

fn embedding_findings() -> Vec<Finding> {
    // The fastembed cache lives at ~/.cache/fastembed/... — presence of any
    // model directory under that root indicates the prefetch happened.
    let cache_root = dirs::cache_dir().map(|c| c.join("fastembed"));
    let mut findings = Vec::new();
    let Some(cache_root) = cache_root else {
        findings.push(Finding::error(
            "dirs::cache_dir() returned None",
            "check XDG_CACHE_HOME",
        ));
        return findings;
    };
    if cache_root.exists() {
        findings.push(Finding::ok(format!(
            "fastembed cache present: {}",
            cache_root.display()
        )));
    } else {
        findings.push(Finding::warn(
            "fastembed cache missing".to_string(),
            "sb bootstrap (or sb cortex embed --prefetch-model)".to_string(),
        ));
    }
    findings
}

struct SystemdState {
    active_state: String,
    main_pid: String,
    memory_current: u64,
}

fn systemctl_show(unit: &str) -> Result<SystemdState, String> {
    let output = Command::new("systemctl")
        .args(["--user", "show", "-p", "ActiveState,MemoryCurrent,MainPID", unit])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut state = SystemdState {
        active_state: "unknown".into(),
        main_pid: "0".into(),
        memory_current: 0,
    };
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ActiveState=") {
            state.active_state = v.to_string();
        } else if let Some(v) = line.strip_prefix("MainPID=") {
            state.main_pid = v.to_string();
        } else if let Some(v) = line.strip_prefix("MemoryCurrent=") {
            state.memory_current = v.parse().unwrap_or(0);
        }
    }
    Ok(state)
}

fn config_path(dir: &str, filename: &str) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join(dir)
        .join(filename)
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", UNITS[i])
}
