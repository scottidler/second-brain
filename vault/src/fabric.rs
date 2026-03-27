use eyre::{Context, Result, bail};
use std::process::Command;

/// Run a Fabric pattern against input text.
/// Returns the pattern output or an error.
pub fn run_pattern(pattern: &str, input: &str, binary: &str, model: &str, max_chars: usize) -> Result<String> {
    let truncated = truncate_input(input, max_chars);
    let resolved = resolve_binary(binary);

    let mut cmd = Command::new(&resolved);
    cmd.args(["-p", pattern]);
    if !model.is_empty() {
        cmd.args(["-m", model]);
    }
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("Failed to spawn fabric binary")?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(truncated.as_bytes())
            .context("Failed to write to fabric stdin")?;
    }

    let output = child.wait_with_output().context("Failed to wait for fabric")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("fabric -p {pattern} failed: {stderr}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Resolve the fabric binary path - if not absolute, try `which` to find it.
pub fn resolve_binary(binary: &str) -> String {
    if binary.starts_with('/') || binary.starts_with("./") {
        return binary.to_string();
    }
    if let Ok(output) = Command::new("which")
        .arg(binary)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        && output.status.success()
    {
        let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !resolved.is_empty() {
            log::debug!("Resolved fabric binary: {binary} -> {resolved}");
            return resolved;
        }
    }
    binary.to_string()
}

/// Check if fabric is available on the system.
pub fn is_available(binary: &str) -> bool {
    let resolved = resolve_binary(binary);
    Command::new(&resolved)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Extract JSON from text that may be wrapped in markdown code blocks.
pub fn extract_json(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        return trimmed.to_string();
    }
    if let Some(start) = trimmed.find('{')
        && let Some(end) = trimmed.rfind('}')
    {
        return trimmed[start..=end].to_string();
    }
    trimmed.to_string()
}

fn truncate_input(input: &str, max_chars: usize) -> String {
    if max_chars == 0 || input.len() <= max_chars {
        input.to_string()
    } else {
        log::warn!(
            "Truncating input from {} to {} chars ({} chars lost)",
            input.len(),
            max_chars,
            input.len() - max_chars
        );
        input[..max_chars].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_bare() {
        let input = r#"{"folder": "Tech", "confidence": 0.9}"#;
        let result = extract_json(input);
        assert!(result.starts_with('{'));
        assert!(result.ends_with('}'));
    }

    #[test]
    fn test_extract_json_markdown_wrapped() {
        let input = "```json\n{\"folder\": \"Tech\", \"confidence\": 0.8}\n```";
        let result = extract_json(input);
        assert!(result.starts_with('{'));
    }

    #[test]
    fn test_truncate_input() {
        assert_eq!(truncate_input("hello world", 5), "hello");
        assert_eq!(truncate_input("hello", 10), "hello");
        assert_eq!(truncate_input("hello", 0), "hello");
    }
}
