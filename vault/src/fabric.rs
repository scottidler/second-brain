use eyre::{Context, Result, bail};
use std::process::Command;
use std::time::Duration;

/// Map a bare pattern name (e.g. `distill-article`) to its absolute file path
/// inside `vault::paths::patterns_dir()` (the unified `~/.config/sb/patterns/`).
/// Tries the literal name first, then with `.md` appended. Path-like inputs
/// (`/`, `.`, `~`) pass through unchanged. Falls back to the bare name when
/// nothing matches, letting fabric's own pattern-loader take a try.
pub fn resolve_pattern(name: &str) -> String {
    if name.starts_with('/') || name.starts_with('.') || name.starts_with('~') {
        return name.to_string();
    }
    let base = crate::paths::patterns_dir();
    for candidate in [base.join(name), base.join(format!("{name}.md"))] {
        if candidate.exists() {
            log::debug!("resolve_pattern: {name} -> {}", candidate.display());
            return candidate.to_string_lossy().to_string();
        }
    }
    name.to_string()
}

/// Run a Fabric pattern against input text with a per-call timeout.
/// If the timeout fires, the child is killed and an error is returned.
/// Returns the pattern output or an error.
pub fn run_pattern(
    pattern: &str,
    input: &str,
    binary: &str,
    model: &str,
    max_chars: usize,
    timeout_secs: u64,
) -> Result<String> {
    let mut cmd = Command::new(resolve_binary(binary));
    cmd.args(["-p", &resolve_pattern(pattern)]);
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
            .write_all(truncate_input(input, max_chars).as_bytes())
            .context("Failed to write to fabric stdin")?;
    }

    let timeout = Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("fabric -p {pattern} timed out after {timeout_secs}s");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => bail!("Failed to wait for fabric: {e}"),
        }
    }

    let output = child.wait_with_output().context("Failed to collect fabric output")?;

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

    #[test]
    fn test_resolve_pattern_passes_paths_through() {
        // Path-like inputs return unchanged regardless of filesystem state.
        assert_eq!(resolve_pattern("/abs/path.md"), "/abs/path.md");
        assert_eq!(resolve_pattern("./rel.md"), "./rel.md");
        assert_eq!(resolve_pattern("~/in-home.md"), "~/in-home.md");
    }

    #[test]
    fn test_resolve_pattern_unknown_name_passes_through() {
        // No file at vault::paths::patterns_dir() for this name, so the bare
        // name is returned and fabric's own loader can try.
        let result = resolve_pattern("this-pattern-does-not-exist-xyz-12345");
        assert_eq!(result, "this-pattern-does-not-exist-xyz-12345");
    }

    #[test]
    fn test_resolve_pattern_canonical_path_for_present_file() {
        // Write a fake pattern file at the canonical patterns_dir() location
        // (under a tempdir-pointed XDG_CONFIG_HOME) and assert the resolver
        // joins it correctly.
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: env var mutation; serialized via #[serial_test::serial] or run
        // with RUST_TEST_THREADS=1 if the surrounding test file grows parallel-unsafe tests.
        // This single test only sets, reads, then unsets, so a brief mutation window is fine.
        let original = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: env mutation is intentional for testing path resolution.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.path()) };

        let patterns_dir = crate::paths::patterns_dir();
        std::fs::create_dir_all(&patterns_dir).expect("create patterns dir");
        let pattern_path = patterns_dir.join("test-pattern.md");
        std::fs::write(&pattern_path, "test content").expect("write pattern");

        let resolved = resolve_pattern("test-pattern");
        assert_eq!(resolved, pattern_path.to_string_lossy());

        // SAFETY: restore env to avoid leaking state to other tests.
        unsafe {
            match original {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }
}
