use eyre::{Result, bail};
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use vault::process::{self, Outcome};

/// Extract markdown text from a file using markitdown.
///
/// Returns the extracted markdown content, or an error if the tool
/// is not found, times out, or extraction fails. `timeout_secs` is the
/// per-call bound (threaded from `pipeline.markitdown_timeout_secs`,
/// default 60).
pub fn extract_markdown(file_path: &Path, timeout_secs: u64) -> Result<String> {
    // Bail early if file doesn't exist - avoids spawning a process that may hang
    if !file_path.exists() {
        bail!("File does not exist: {}", file_path.display());
    }
    run_extraction(
        markitdown_command(file_path),
        Duration::from_secs(timeout_secs),
        &file_path.display().to_string(),
    )
}

fn markitdown_command(file_path: &Path) -> Command {
    let mut cmd = Command::new("markitdown");
    cmd.arg(file_path.as_os_str());
    cmd
}

/// Run a markitdown-shaped command and return its trimmed stdout. A timeout,
/// a non-zero exit, and empty output are each `Err` naming `subject`.
pub(crate) fn run_extraction(cmd: Command, timeout: Duration, subject: &str) -> Result<String> {
    log::debug!("extraction: running markitdown for {subject} (timeout={timeout:?})");
    match process::run(cmd, None, timeout, &format!("markitdown {subject}"))? {
        Outcome::TimedOut { after } => bail!("markitdown timed out after {after:?} for {subject}"),
        Outcome::Exited { status, stderr, .. } if !status.success() => {
            bail!("markitdown failed for {subject}: {}", String::from_utf8_lossy(&stderr))
        }
        Outcome::Exited { stdout, .. } => {
            let text = String::from_utf8_lossy(&stdout).trim().to_string();
            if text.is_empty() {
                bail!("markitdown produced no output for {subject}");
            }
            Ok(text)
        }
    }
}

/// Check if markitdown is available on PATH.
pub fn is_available() -> bool {
    Command::new("markitdown")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests;
