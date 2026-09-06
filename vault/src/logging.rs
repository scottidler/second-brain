use eyre::{Context, Result};
use file_rotate::compression::Compression;
use file_rotate::suffix::AppendCount;
use file_rotate::{ContentLimit, FileRotate};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

/// Per-file byte cap for the rotated daemon log. Chosen so a debug-level
/// day of daemon activity (the pre-fix live unit ran `--log-level debug`
/// unrotated, growing to 16 GB over 46 days - ~350 MB/day) still rotates
/// well within a day at info level; see the Phase 6 implementation notes
/// (`docs/design/2026-07-05-cortex-daemon-oscillation-loop-implementation-notes.md`)
/// for the measured info-level rate this was picked against.
pub const LOG_ROTATE_MAX_BYTES: usize = 50 * 1024 * 1024; // 50 MiB per file
/// Number of ROTATED backups retained, in addition to the active file
/// (so total on-disk cap = `(LOG_ROTATE_MAX_FILES + 1) * LOG_ROTATE_MAX_BYTES`
/// = ~300 MiB, versus the pre-fix unrotated 16 GB).
pub const LOG_ROTATE_MAX_FILES: usize = 5;

/// Resolve log level from: CLI flag > LOG_LEVEL env > config file > "info"
pub fn resolve_log_level(cli_level: Option<&str>, config_level: Option<&str>) -> String {
    if let Some(level) = cli_level {
        return level.to_string();
    }
    if let Ok(level) = std::env::var("LOG_LEVEL") {
        return level;
    }
    if let Some(level) = config_level {
        return level.to_string();
    }
    "info".to_string()
}

/// Build the rotating file writer used as the file half of `DualWriter`,
/// and by `sb`'s `oracle serve` tracing writer (behind
/// `tracing_appender::non_blocking`). Exposed so no caller re-declares the
/// 50 MiB x 5 policy: one rotator, one set of constants.
/// No logger init here (pure aside from opening/creating `log_file_path` and
/// its parent dir) so tests can drive rotation directly without touching the
/// process-global `env_logger`/`log` singleton.
/// Delete `<stem>-<pid>.log*` files in `dir` whose pid is no longer running.
///
/// Companion to the per-process log path below: without a sweep, every
/// `sb oracle serve` a client ever spawned would leave a file behind. Called
/// at serve startup, so the set stays bounded by the number of LIVE servers.
/// Best-effort by design - a log we cannot read or remove is skipped, never
/// fatal, because failing to tidy a log must not stop a server from starting.
pub fn prune_dead_pid_logs(dir: &Path, stem: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(&format!("{stem}-")) else {
            continue;
        };
        // `<pid>.log` and its rotations `<pid>.log.1` ... `<pid>.log.5`.
        let Some(pid_str) = rest.split(".log").next() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else { continue };
        if !pid_is_alive(pid) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// True when `/proc/<pid>` exists. Linux-only, which is what sb targets; on
/// any other platform this returns true so nothing is ever deleted wrongly.
fn pid_is_alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{pid}")).exists()
    } else {
        true
    }
}

pub fn rotating_log_writer(log_file_path: &Path) -> FileRotate<AppendCount> {
    // No log::debug! here: this runs before env_logger::Builder::init(), so
    // the `log` facade has no installed logger yet and the record would be
    // silently dropped. The rotation settings are logged from `setup_logging`
    // instead, right after `.init()`.
    FileRotate::new(
        log_file_path,
        AppendCount::new(LOG_ROTATE_MAX_FILES),
        ContentLimit::Bytes(LOG_ROTATE_MAX_BYTES),
        Compression::None,
        None,
    )
}

/// Init env_logger writing to both the given file (rotated, append + create)
/// and stderr. Caller owns path construction so the on-disk layout is not
/// baked in here.
pub fn setup_logging(log_file_path: &Path, log_level: &str) -> Result<()> {
    if let Some(parent) = log_file_path.parent() {
        fs::create_dir_all(parent).context("Failed to create log directory")?;
    }

    let log_file = rotating_log_writer(log_file_path);

    env_logger::Builder::new()
        .parse_filters(log_level)
        .format(|buf, record| {
            writeln!(
                buf,
                "{} [{}] {}: {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                record.level(),
                record.target(),
                record.args()
            )
        })
        .target(env_logger::Target::Pipe(Box::new(DualWriter {
            file: std::sync::Mutex::new(log_file),
            stderr: std::io::stderr(),
        })))
        .init();

    log::info!(
        "Logging initialized (level={log_level}), writing to: {} + stderr \
         (rotated at {LOG_ROTATE_MAX_BYTES} bytes/file, {LOG_ROTATE_MAX_FILES} backups retained)",
        log_file_path.display()
    );
    Ok(())
}

/// Init env_logger writing to stderr only, with no "Logging initialized" line.
///
/// For short-lived CLI verbs (status/list/show/install/doctor/bootstrap) where
/// writing into the daemon's `<subsystem>.log` is pure noise — the daemon
/// process owns that file. Inspection verbs go to stderr, period.
pub fn setup_logging_stderr(log_level: &str) -> Result<()> {
    env_logger::Builder::new()
        .parse_filters(log_level)
        .format(|buf, record| {
            writeln!(
                buf,
                "{} [{}] {}: {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                record.level(),
                record.target(),
                record.args()
            )
        })
        .target(env_logger::Target::Stderr)
        .init();
    Ok(())
}

/// Probe installed by whoever owns the process's non-blocking log writer,
/// read by whoever writes the shutdown line. The writer stack is already
/// process-global (one `tracing` subscriber per process), and the crate that
/// installs it (`sb`) is downstream of the crate that reports at shutdown
/// (`oracle`), so the counter cannot be threaded as an argument - it lives
/// here, in the crate both depend on.
static DROPPED_LOG_LINES: OnceLock<Box<dyn Fn() -> u64 + Send + Sync>> = OnceLock::new();

/// Register the drop counter of a non-blocking log writer. Called once, from
/// the process's logger initializer. A second call is ignored: two writers in
/// one process would already have panicked installing two global subscribers.
pub fn set_dropped_log_lines_probe(probe: Box<dyn Fn() -> u64 + Send + Sync>) {
    let _ = DROPPED_LOG_LINES.set(probe);
}

/// Log lines the non-blocking writer dropped because its channel was full.
/// Zero when no probe is installed (every logger path but `oracle serve`
/// writes synchronously and cannot drop).
pub fn dropped_log_lines() -> u64 {
    DROPPED_LOG_LINES.get().map_or(0, |probe| probe())
}

struct DualWriter {
    file: std::sync::Mutex<FileRotate<AppendCount>>,
    stderr: std::io::Stderr,
}

impl Write for DualWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.stderr.lock().write_all(buf);
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.stderr.lock().flush();
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
