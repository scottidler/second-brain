use eyre::{Context, Result};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Timeout for markitdown execution (30 seconds).
const MARKITDOWN_TIMEOUT_SECS: u64 = 30;

/// Extract markdown text from a file using markitdown.
///
/// Returns the extracted markdown content, or an error if the tool
/// is not found or extraction fails. Applies a 30-second timeout
/// to prevent hangs on problematic files.
pub fn extract_markdown(file_path: &Path) -> Result<String> {
    // Bail early if file doesn't exist - avoids spawning a process that may hang
    if !file_path.exists() {
        eyre::bail!("File does not exist: {}", file_path.display());
    }

    let mut child = Command::new("markitdown")
        .arg(file_path.as_os_str())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("markitdown not found - install with: pipx install markitdown")?;

    // Wait with timeout to prevent hangs
    let timeout = Duration::from_secs(MARKITDOWN_TIMEOUT_SECS);
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    eyre::bail!(
                        "markitdown timed out after {}s for {}",
                        MARKITDOWN_TIMEOUT_SECS,
                        file_path.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                eyre::bail!("Failed to wait for markitdown: {e}");
            }
        }
    }

    let output = child
        .wait_with_output()
        .context("Failed to collect markitdown output")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eyre::bail!("markitdown failed for {}: {stderr}", file_path.display());
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        eyre::bail!("markitdown produced no output for {}", file_path.display());
    }

    Ok(text)
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
mod tests {
    use super::*;

    #[test]
    fn test_extract_markdown_nonexistent_file() {
        let path = Path::new("/tmp/obsidian-borg-test-nonexistent-file.pdf");
        let result = extract_markdown(path);
        assert!(result.is_err());
        let err = format!("{}", result.expect_err("should fail"));
        assert!(err.contains("does not exist"), "got: {err}");
    }

    #[test]
    fn test_is_available() {
        // Just ensure it doesn't panic - result depends on environment
        let _ = is_available();
    }
}
