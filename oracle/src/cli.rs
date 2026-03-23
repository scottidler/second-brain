//! CLI argument parsing for oracle

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::LazyLock;

static AFTER_HELP: LazyLock<String> = LazyLock::new(|| {
    let log_path = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("oracle")
        .join("logs")
        .join("oracle.log");
    format!("Logs are written to: {}", log_path.display())
});

#[derive(Parser)]
#[command(
    name = "oracle",
    about = "MCP server for querying an Obsidian vault's ingested knowledge",
    version = env!("GIT_DESCRIBE"),
    after_help = AFTER_HELP.as_str(),
)]
pub struct Cli {
    /// Path to config file
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Log level: trace, debug, info, warn, error
    /// Resolution: --log-level > LOG_LEVEL env > config > info
    #[arg(short, long, global = true)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the MCP server (stdio transport)
    Serve,

    /// Index the vault into SQLite (or reindex changed files)
    Index,

    /// Show vault statistics
    Stats,
}
