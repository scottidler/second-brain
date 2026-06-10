use eyre::{Context, Result, bail};
use std::process::Command;
use std::time::Duration;

/// Captured output of a finished subprocess: `(exit status, stdout, stderr)`.
pub type ProcessOutput = (std::process::ExitStatus, Vec<u8>, Vec<u8>);

/// Typed Fabric failure so callers can branch on a timeout WITHOUT matching the
/// error message string. `run_pattern` surfaces these through `eyre::Report`
/// (they remain downcastable), so the existing `eyre::Result` signature and all
/// its callers are unchanged. The `Display` text is preserved verbatim from the
/// old `bail!` messages for back-compat with anything still reading the string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FabricError {
    /// The pattern ran past its wall-clock budget; the child has been killed.
    #[error("fabric -p {pattern} timed out after {timeout_secs}s")]
    Timeout { pattern: String, timeout_secs: u64 },
    /// The pattern exited non-zero; carries a stderr preview.
    #[error("fabric -p {pattern} failed: {stderr}")]
    Failed { pattern: String, stderr: String },
}

impl FabricError {
    /// True when `err` is (or wraps) a Fabric timeout. Replaces the
    /// `msg.contains("timed out")` string-matching at the distiller fallback
    /// sites — a real `SQLITE_BUSY`-style "timed out" substring in some other
    /// error can no longer masquerade as a fabric timeout.
    pub fn is_timeout(err: &eyre::Report) -> bool {
        matches!(err.downcast_ref::<FabricError>(), Some(FabricError::Timeout { .. }))
    }
}

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

    let child = cmd.spawn().context("Failed to spawn fabric binary")?;

    let input_bytes = truncate_input(input, max_chars).into_bytes();
    match wait_with_timeout(child, input_bytes, Duration::from_secs(timeout_secs))? {
        None => Err(FabricError::Timeout {
            pattern: pattern.to_string(),
            timeout_secs,
        }
        .into()),
        Some((status, stdout, stderr)) => {
            if !status.success() {
                let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
                return Err(FabricError::Failed {
                    pattern: pattern.to_string(),
                    stderr,
                }
                .into());
            }
            Ok(String::from_utf8_lossy(&stdout).trim().to_string())
        }
    }
}

/// Drive a spawned child to completion with a wall-clock timeout, writing
/// `input` to its stdin and fully draining stdout+stderr - each on its own
/// thread.
///
/// This is the deadlock-safe subprocess primitive. A child that writes more
/// than the OS pipe buffer (~64KB) to stdout BLOCKS until the parent reads;
/// the previous `run_pattern` wrote all of stdin up front, then polled
/// `try_wait` without reading stdout, so any pattern emitting more than a
/// pipe-buffer of output deadlocked until the timeout killed it - misreported
/// as a fabric timeout. Writing stdin from a thread also removes the
/// unbounded blocking write on large inputs.
///
/// Returns `Ok(None)` when the timeout fired (the child has been killed and
/// reaped); `Ok(Some((status, stdout, stderr)))` otherwise.
pub fn wait_with_timeout(
    mut child: std::process::Child,
    input: Vec<u8>,
    timeout: Duration,
) -> Result<Option<ProcessOutput>> {
    use std::io::{Read, Write};

    // Feed stdin from a dedicated thread; dropping the handle closes the pipe
    // (EOF for the child). A broken pipe (child died early) is non-fatal here.
    let stdin_handle = child.stdin.take().map(|mut stdin| {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        })
    });

    // Drain stdout and stderr concurrently so neither can fill its pipe and
    // wedge the child.
    let stdout_handle = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            buf
        })
    });

    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => bail!("Failed to wait for subprocess: {e}"),
        }
    };

    let stdout = stdout_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = stderr_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    if let Some(h) = stdin_handle {
        let _ = h.join();
    }

    Ok(Some((status, stdout, stderr)))
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
    // max_chars == 0 means "no limit" here (distinct from the helper, where 0
    // truncates to empty), so short-circuit before delegating.
    let char_count = input.chars().count();
    if max_chars == 0 || char_count <= max_chars {
        input.to_string()
    } else {
        log::warn!(
            "Truncating input from {char_count} to {max_chars} chars ({} chars lost)",
            char_count - max_chars
        );
        // Character-accurate, never splits a codepoint (the old `input[..max_chars]`
        // byte slice panicked on multi-byte content straddling the cut).
        crate::text::truncate(input, max_chars).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes every env-var-mutating test in this file so they can't race
    /// each other's `XDG_CONFIG_HOME` reads/writes (cargo runs tests in parallel
    /// by default). Per the `rust.md` env-test pattern.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn fabric_error_timeout_is_downcastable_through_eyre() {
        let report: eyre::Report = FabricError::Timeout {
            pattern: "distill-article".to_string(),
            timeout_secs: 60,
        }
        .into();
        assert!(FabricError::is_timeout(&report));
        // A Failed (non-timeout) fabric error must NOT read as a timeout.
        let failed: eyre::Report = FabricError::Failed {
            pattern: "distill-article".to_string(),
            stderr: "boom".to_string(),
        }
        .into();
        assert!(!FabricError::is_timeout(&failed));
        // An unrelated error (even one whose text says "timed out") must not
        // masquerade as a typed fabric timeout.
        let unrelated = eyre::eyre!("connection timed out");
        assert!(!FabricError::is_timeout(&unrelated));
    }

    #[test]
    fn wait_with_timeout_drains_large_stdout_without_deadlock() {
        // Regression: emit far more than the ~64KB OS pipe buffer. The old
        // poll-without-reading loop deadlocked the child against an unread
        // pipe until the timeout killed it. The drain threads must collect
        // all of it and return successfully well inside the timeout.
        use std::process::{Command, Stdio};
        let big = 500_000usize;
        let child = Command::new("sh")
            .arg("-c")
            .arg(format!("head -c {big} /dev/zero | tr '\\0' 'a'"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let Ok(child) = child else {
            eprintln!("sh not available; skipping deadlock test");
            return;
        };
        let out = wait_with_timeout(child, Vec::new(), Duration::from_secs(30)).expect("wait");
        let (status, stdout, _stderr) = out.expect("must not time out on large output");
        assert!(status.success());
        assert_eq!(stdout.len(), big, "all stdout bytes must be drained");
    }

    #[test]
    fn wait_with_timeout_kills_on_timeout() {
        use std::process::{Command, Stdio};
        let child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let Ok(child) = child else {
            eprintln!("sh not available; skipping timeout test");
            return;
        };
        let out = wait_with_timeout(child, Vec::new(), Duration::from_millis(300)).expect("wait");
        assert!(out.is_none(), "a child exceeding the timeout returns None (killed)");
    }

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
        // Hold ENV_LOCK for the whole env-mutation window so no parallel test
        // observes our XDG_CONFIG_HOME override.
        let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = tempfile::tempdir().expect("tempdir");
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
