use eyre::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::Path;

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

/// Init env_logger writing to both the given file (append + create) and stderr.
/// Caller owns path construction so the on-disk layout is not baked in here.
pub fn setup_logging(log_file_path: &Path, log_level: &str) -> Result<()> {
    if let Some(parent) = log_file_path.parent() {
        fs::create_dir_all(parent).context("Failed to create log directory")?;
    }

    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file_path)
        .context("Failed to open log file")?;

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
        "Logging initialized (level={log_level}), writing to: {} + stderr",
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

struct DualWriter {
    file: std::sync::Mutex<fs::File>,
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
