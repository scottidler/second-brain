//! Doctor checks: the fabric patterns the code calls by name.

use std::path::PathBuf;
use std::process::Command;

use super::Finding;

/// Fabric pattern borg and cortex call unconditionally (`vault::fabric` summarize path).
const REQUIRED_FABRIC_PATTERN: &str = "summarize";

/// Verifies the fabric patterns the code actually calls are installed:
/// `summarize` (required, Error) and the configured `intel.batch-weekly`
/// fallback (Warn, since the weekly review only falls back to it when the LLM
/// call fails). `None` means no fallback is configured, so nothing to check.
/// The fallback counts as installed when fabric lists it OR it resolves to a
/// pattern file, the way the call itself resolves it (sb's shipped default).
pub(super) fn findings() -> Vec<Finding> {
    let Ok(out) = Command::new("fabric")
        .arg("-l")
        .stdin(std::process::Stdio::null())
        .output()
    else {
        // The CLI-missing case is already covered by fabric_cli_findings.
        return Vec::new();
    };
    if !out.status.success() {
        return vec![Finding::warn(
            "fabric -l exited non-zero (CLI installed, pattern list unavailable)",
            "rerun `fabric -y --update-patterns` to (re)provision Daniel Miessler's default patterns",
        )];
    }
    let weekly = cortex::config::Config::load(None)
        .map(|c| c.actions.intel.batch_weekly)
        .unwrap_or_else(|_| Some(cortex::config::DEFAULT_WEEKLY_PATTERN.to_string()));
    fabric_pattern_findings(
        &String::from_utf8_lossy(&out.stdout),
        weekly.as_deref(),
        pattern_file_exists,
    )
}

/// True when `vault::fabric::resolve_pattern` maps `name` to an existing file,
/// i.e. `fabric -p <name>` from sb runs that file rather than a fabric pattern.
fn pattern_file_exists(name: &str) -> bool {
    let resolved = PathBuf::from(vault::fabric::resolve_pattern(name));
    resolved.is_absolute() && resolved.is_file()
}

/// Pure half of `findings`: `fabric -l` stdout (one
/// pattern per line), the configured weekly fallback pattern, and the
/// pattern-file probe.
fn fabric_pattern_findings(
    list_stdout: &str,
    weekly_pattern: Option<&str>,
    file_exists: impl Fn(&str) -> bool,
) -> Vec<Finding> {
    let has = |name: &str| list_stdout.lines().any(|line| line.trim() == name);
    let mut findings = Vec::new();
    if has(REQUIRED_FABRIC_PATTERN) {
        findings.push(Finding::ok(format!(
            "fabric pattern present ({REQUIRED_FABRIC_PATTERN})"
        )));
    } else {
        findings.push(Finding::error(
            format!("missing required fabric pattern: {REQUIRED_FABRIC_PATTERN}"),
            "fabric -y --update-patterns",
        ));
    }
    if let Some(weekly) = weekly_pattern {
        if has(weekly) || file_exists(weekly) {
            findings.push(Finding::ok(format!(
                "fabric weekly fallback pattern present ({weekly})"
            )));
        } else {
            findings.push(Finding::warn(
                format!("missing fabric pattern {weekly} (intel.batch-weekly fallback for the weekly review)"),
                "sb bootstrap --force (installs sb's shipped weekly-digest), or set intel.batch-weekly to an installed pattern",
            ));
        }
    }
    findings
}

#[cfg(test)]
mod tests;
