#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]

use clap::{CommandFactory, FromArgMatches};
use eyre::{Context, Result};

use cortex::cli::{self, Cli, Command};
use cortex::config::Config;
use cortex::logging;

#[tokio::main]
async fn main() -> Result<()> {
    // Augment clap with runtime after_help (tool checks + log path) before parsing
    let matches = Cli::command().after_help(cli::after_help_text()).get_matches();
    let cli = Cli::from_arg_matches(&matches).context("failed to parse arguments")?;

    // Load config first (needed for log level resolution)
    let config = Config::load(cli.config.as_ref()).context("failed to load configuration")?;

    // Resolve and setup logging
    let level = logging::resolve_log_level(cli.log_level.as_deref(), &config.log_level);
    logging::setup_logging(&level)?;

    log::info!("cortex starting (version={})", env!("GIT_DESCRIBE"));

    let vault_root = config.vault_root(cli.vault.as_ref());
    log::info!("resolved vault root: {}", vault_root.display());

    match &cli.command {
        Command::Classify(opts) => {
            cortex::run_classify(&vault_root, &config, opts)?;
        }
        Command::Lint(opts) => {
            cortex::run_lint(&vault_root, &config, opts)?;
        }
        Command::Link(opts) => {
            cortex::run_link(&vault_root, &config, opts)?;
        }
        Command::Intel(opts) => {
            cortex::run_intel(&vault_root, &config, opts)?;
        }
        Command::State(opts) => {
            cortex::run_state(&vault_root, &config, opts)?;
        }
        Command::Daemon(opts) => {
            cortex::daemon::run_daemon(&vault_root, &config, opts).await?;
        }
        Command::Migrate(opts) => {
            cortex::run_migrate(&vault_root, &config, opts)?;
        }
    }

    Ok(())
}
