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

/// Build the `fabric -p <pattern>` child `Command`, wiring stdio to pipes and
/// -- crucially -- setting `ANTHROPIC_API_KEY` on the CHILD's environment ONLY.
///
/// `api_key_env` is the NAME of the configured credential var (e.g.
/// `ESCOTE_ANTHROPIC_API_KEY`) or a file path, resolved via
/// [`crate::config::resolve_secret`]. Fabric (the third-party Go binary) reads
/// the credential exclusively from the env var named literally
/// `ANTHROPIC_API_KEY`, so when the workspace credential is stored under a
/// persona-scoped name we translate it to fabric's required name here, on this
/// one child process. The value is NEVER set in the parent/global env and NEVER
/// written to a file at rest.
///
/// When `api_key_env` is empty, or resolution fails, `ANTHROPIC_API_KEY` is left
/// unset on the child and fabric falls back to its own `.env` (preserving prior
/// behavior). A resolution failure is `warn!`-logged (the var NAME, never the
/// value).
///
/// Factored out so it is unit-testable via `Command::get_envs()` without
/// spawning fabric.
fn build_fabric_command(binary: &str, pattern: &str, model: &str, api_key_env: &str) -> Command {
    let mut cmd = Command::new(resolve_binary(binary));
    cmd.args(["-p", &resolve_pattern(pattern)]);
    if !model.is_empty() {
        cmd.args(["-m", model]);
    }
    if !api_key_env.is_empty() {
        match crate::config::resolve_secret(api_key_env) {
            Ok(value) => {
                cmd.env("ANTHROPIC_API_KEY", value);
            }
            Err(e) => log::warn!(
                "build_fabric_command: could not resolve api-key var '{api_key_env}': {e}; \
                 leaving ANTHROPIC_API_KEY unset (fabric will fall back to its own .env)"
            ),
        }
    }
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd
}

/// Run a Fabric pattern against input text with a per-call timeout.
/// If the timeout fires, the child is killed and an error is returned.
/// Returns the pattern output or an error.
///
/// `api_key_env` names the configured credential var (or file) that carries the
/// Anthropic key; it is translated to fabric's required `ANTHROPIC_API_KEY` on
/// the child process only (see [`build_fabric_command`]). Pass `""` to leave the
/// child's `ANTHROPIC_API_KEY` untouched (fabric uses its own `.env`).
pub fn run_pattern(
    pattern: &str,
    input: &str,
    binary: &str,
    api_key_env: &str,
    model: &str,
    max_chars: usize,
    timeout_secs: u64,
) -> Result<String> {
    log::debug!(
        "fabric::run_pattern: pattern={pattern} binary={binary} api_key_env={api_key_env} model={model} max_chars={max_chars} timeout_secs={timeout_secs} input_len={}",
        input.len()
    );
    let mut cmd = build_fabric_command(binary, pattern, model, api_key_env);

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
mod tests;
