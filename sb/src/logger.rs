use eyre::{Context, Result};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Cmd};

/// Init the right logger for the parsed subcommand.
///
/// All logs land under `~/.local/share/sb/<name>.log`:
/// - Borg verbs   -> sb/borg.log
/// - Cortex verbs -> sb/cortex.log
/// - Oracle verbs -> sb/oracle.log (tracing-subscriber for `oracle serve` so MCP stdio stays clean; env_logger for the rest)
/// - status / doctor / bootstrap -> sb/{status,doctor,bootstrap}.log
pub fn init_for(cli: &Cli) -> Result<()> {
    let (name, level) = name_and_level(cli);
    let path = log_path(&name);

    // The `oracle serve` MCP server uses tracing, not log; route it to the
    // tracing-subscriber file writer. Everything else goes through env_logger.
    if matches!(&cli.cmd, Cmd::Oracle(c) if matches!(c.command, crate::cli::oracle::Commands::Serve)) {
        return init_tracing_to_file(&path, &level);
    }
    vault::logging::setup_logging(&path, &level)
}

fn name_and_level(cli: &Cli) -> (String, String) {
    match &cli.cmd {
        Cmd::Borg(c) => (
            "borg".into(),
            resolve_level(cli.log_level.as_deref(), c.log_level.as_deref(), cli.verbose),
        ),
        Cmd::Cortex(c) => (
            "cortex".into(),
            resolve_level(cli.log_level.as_deref(), c.log_level.as_deref(), cli.verbose),
        ),
        Cmd::Oracle(_) => (
            "oracle".into(),
            resolve_level(cli.log_level.as_deref(), None, cli.verbose),
        ),
        Cmd::Status(_) => (
            "status".into(),
            resolve_level(cli.log_level.as_deref(), None, cli.verbose),
        ),
        Cmd::Doctor(_) => (
            "doctor".into(),
            resolve_level(cli.log_level.as_deref(), None, cli.verbose),
        ),
        Cmd::Bootstrap(_) => (
            "bootstrap".into(),
            resolve_level(cli.log_level.as_deref(), None, cli.verbose),
        ),
    }
}

fn log_path(name: &str) -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sb")
        .join(format!("{name}.log"))
}

fn resolve_level(root: Option<&str>, sub: Option<&str>, verbose: bool) -> String {
    if let Some(v) = root.or(sub) {
        return v.to_string();
    }
    if verbose { "debug".into() } else { "info".into() }
}

fn init_tracing_to_file(log_path: &std::path::Path, level: &str) -> Result<()> {
    let filter = EnvFilter::new(level);

    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create log directory")?;
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .context("Failed to open log file")?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .init();

    Ok(())
}
