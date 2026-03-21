//! CLI argument parsing for oracle

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "oracle",
    about = "MCP server for querying an Obsidian vault's ingested knowledge",
    version = env!("GIT_DESCRIBE"),
)]
pub struct Cli {
    /// Path to config file
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

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
