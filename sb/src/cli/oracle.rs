use clap::{Args, Subcommand};
use eyre::{Context, Result};
use std::path::PathBuf;

#[derive(Args)]
pub struct OracleCli {
    /// Path to config file
    #[arg(short, long)]
    pub config: Option<PathBuf>,

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

    /// Call a tool directly (no MCP transport)
    Call {
        /// Tool name (use --list to see available tools)
        #[arg(required_unless_present = "list")]
        tool: Option<String>,
        /// JSON arguments (default: {})
        #[arg(long)]
        json: Option<String>,
        /// List available tool names
        #[arg(long)]
        list: bool,
    },
}

impl OracleCli {
    pub async fn run(self) -> Result<()> {
        let config = oracle::Config::load(self.config.as_deref()).context("Failed to load configuration")?;
        match self.command {
            Commands::Serve => oracle::run_serve(config).await,
            Commands::Index => oracle::run_index(&config),
            Commands::Stats => oracle::run_stats(&config),
            Commands::Call { tool, json, list } => {
                if list {
                    oracle::run_list();
                    Ok(())
                } else {
                    oracle::run_call(
                        config,
                        tool.as_deref().expect("clap enforces tool or --list"),
                        json.as_deref(),
                    )
                    .await
                }
            }
        }
    }
}
