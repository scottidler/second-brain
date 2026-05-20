use eyre::{Context, Result};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Cmd};

/// Init the right logger for the parsed subcommand.
/// - `oracle serve`: tracing-subscriber to file (preserves stdio for MCP JSON-RPC).
/// - Everything else: env_logger (via vault::logging) to a per-subsystem log file.
pub fn init_for(cli: &Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::Oracle(c) if matches!(c.command, crate::cli::oracle::Commands::Serve) => {
            init_tracing_to_file(cli.verbose, "oracle")
        }
        Cmd::Borg(c) => {
            let level = resolve_level(cli.log_level.as_deref(), c.log_level.as_deref(), cli.verbose);
            vault::logging::setup_logging("borg", &level)
        }
        Cmd::Cortex(c) => {
            let level = resolve_level(cli.log_level.as_deref(), c.log_level.as_deref(), cli.verbose);
            vault::logging::setup_logging("cortex", &level)
        }
        Cmd::Oracle(_) => {
            let level = resolve_level(cli.log_level.as_deref(), None, cli.verbose);
            vault::logging::setup_logging("oracle", &level)
        }
        Cmd::Status(_) | Cmd::Doctor(_) | Cmd::Bootstrap(_) => {
            let level = resolve_level(cli.log_level.as_deref(), None, cli.verbose);
            vault::logging::setup_logging("sb", &level)
        }
    }
}

fn resolve_level(root: Option<&str>, sub: Option<&str>, verbose: bool) -> String {
    if let Some(v) = root.or(sub) {
        return v.to_string();
    }
    if verbose { "debug".into() } else { "info".into() }
}

fn init_tracing_to_file(verbose: bool, app_name: &str) -> Result<()> {
    let level = if verbose { "debug" } else { "info" };
    let filter = EnvFilter::new(level);

    let log_path = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(app_name)
        .join("logs")
        .join(format!("{app_name}.log"));

    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create log directory")?;
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .context("Failed to open log file")?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .init();

    Ok(())
}
