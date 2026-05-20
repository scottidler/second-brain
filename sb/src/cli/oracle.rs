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
            Commands::Serve => oracle::serve(config).await,
            Commands::Index => {
                let vault_root = config.vault_root()?;
                println!("Indexing vault: {}", vault_root.display());
                println!("Database: {}", config.db_path().display());
                let stats = oracle::index(&config)?;
                println!();
                println!("Scanned:   {}", stats.total_scanned);
                println!("Inserted:  {}", stats.inserted);
                println!("Updated:   {}", stats.updated);
                println!("Unchanged: {}", stats.unchanged);
                println!("Removed:   {}", stats.removed);
                Ok(())
            }
            Commands::Stats => {
                let stats = oracle::stats(&config)?;
                print_vault_stats(&config, &stats);
                Ok(())
            }
            Commands::Call { tool, json, list } => {
                if list {
                    print_tool_list(&oracle::tools());
                    Ok(())
                } else {
                    let tool_name = tool.as_deref().expect("clap enforces tool or --list");
                    let result = oracle::call(config, tool_name, json.as_deref()).await?;
                    print_call_result(&result)
                }
            }
        }
    }
}

fn print_vault_stats(config: &oracle::Config, stats: &vault::search::VaultStats) {
    match config.vault_root() {
        Ok(root) => println!("Vault: {}", root.display()),
        Err(e) => println!("Vault: (unresolved: {e})"),
    }
    println!("Total notes: {}", stats.total_notes);

    if !stats.schema_gaps.is_empty() {
        println!("\nSchema gaps:");
        for (field, count) in &stats.schema_gaps {
            println!("  missing {field:<10} {count}");
        }
    }

    println!("\nBy domain:");
    for (domain, count) in &stats.by_domain {
        println!("  {domain:<15} {count}");
    }

    println!("\nBy type:");
    for (note_type, count) in &stats.by_type {
        println!("  {note_type:<15} {count}");
    }

    println!("\nBy status:");
    for (status, count) in &stats.by_status {
        println!("  {status:<15} {count}");
    }
}

fn print_tool_list(tools: &[rmcp::model::Tool]) {
    for tool in tools {
        println!("{:<20} {}", tool.name, tool.description.as_deref().unwrap_or(""));
    }
}

/// Pure inspection of a tool result. Returns true if the wrapper should
/// translate this outcome into a non-zero CLI exit code.
///
/// Two failure shapes are recognized:
/// - `is_error == Some(true)`: MCP-level protocol error (invalid args, panic).
/// - Top-level JSON `"found": false`: domain-level "not found" — the tool ran
///   successfully but the requested item doesn't exist.
fn outcome_is_failure(result: &rmcp::model::CallToolResult) -> bool {
    if result.is_error == Some(true) {
        return true;
    }
    for content in &result.content {
        if let Some(text) = content.as_text()
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text.text)
            && parsed.get("found").and_then(|v| v.as_bool()) == Some(false)
        {
            return true;
        }
    }
    false
}

fn print_call_result(result: &rmcp::model::CallToolResult) -> Result<()> {
    let failure = outcome_is_failure(result);
    let is_protocol_error = result.is_error == Some(true);

    for content in &result.content {
        if let Some(text) = content.as_text() {
            if is_protocol_error {
                eprintln!("{}", text.text);
            } else if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text.text) {
                println!("{}", serde_json::to_string_pretty(&parsed)?);
            } else {
                println!("{}", text.text);
            }
        }
    }

    if failure {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
