use eyre::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

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

pub fn setup_logging(app_name: &str, log_level: &str) -> Result<()> {
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(app_name)
        .join("logs");

    fs::create_dir_all(&log_dir).context("Failed to create log directory")?;

    let log_file_path = log_dir.join(format!("{app_name}.log"));

    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
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
