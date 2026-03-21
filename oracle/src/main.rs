//! oracle - MCP server for querying an Obsidian vault's ingested knowledge

use clap::Parser;
use eyre::{Context, Result};
use rmcp::ServiceExt;
use std::io;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

mod cli;

use cli::{Cli, Commands};
use oracle::{Config, Database};

fn setup_logging(verbose: bool, log_config: &oracle::config::LogConfig) -> Result<()> {
    let level = if verbose { "debug" } else { &log_config.level };
    let filter = EnvFilter::new(level);

    if let Some(ref log_file) = log_config.file {
        let expanded_path = shellexpand::tilde(log_file);
        let path = PathBuf::from(expanded_path.as_ref());

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(file)
            .with_ansi(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(io::stderr)
            .init();
    }

    Ok(())
}

async fn run_serve(config: Config) -> Result<()> {
    tracing::info!("Opening database at {}", config.db_path().display());
    let db = Database::open(&config.db_path()).context("Failed to open database")?;

    tracing::info!("Indexing vault at {}", config.vault_root().display());
    let stats = db.index_vault(&config.vault_root()).context("Failed to index vault")?;
    tracing::info!(
        "Index complete: {} scanned, {} inserted, {} updated, {} unchanged, {} removed",
        stats.total_scanned,
        stats.inserted,
        stats.updated,
        stats.unchanged,
        stats.removed
    );

    tracing::info!("Starting MCP server on stdio transport");
    let server = oracle::server::OracleMcpServer::new(config, db);
    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let service = server.serve(transport).await?;

    tracing::info!("MCP server started, waiting for requests...");
    service.waiting().await?;
    tracing::info!("MCP server shutting down");

    Ok(())
}

fn run_index(config: &Config) -> Result<()> {
    let db = Database::open(&config.db_path()).context("Failed to open database")?;

    println!("Indexing vault: {}", config.vault_root().display());
    println!("Database: {}", config.db_path().display());

    let stats = db.index_vault(&config.vault_root()).context("Failed to index vault")?;

    println!();
    println!("Scanned:   {}", stats.total_scanned);
    println!("Inserted:  {}", stats.inserted);
    println!("Updated:   {}", stats.updated);
    println!("Unchanged: {}", stats.unchanged);
    println!("Removed:   {}", stats.removed);

    Ok(())
}

fn run_stats(config: &Config) -> Result<()> {
    let db = Database::open(&config.db_path()).context("Failed to open database")?;

    db.index_vault(&config.vault_root()).context("Failed to index vault")?;

    let stats = db.stats().context("Failed to get stats")?;

    println!("Vault: {}", config.vault_root().display());
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

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let config = Config::load(cli.config.as_deref()).context("Failed to load configuration")?;

    setup_logging(cli.verbose, &config.logging)?;

    match cli.command {
        Commands::Serve => run_serve(config).await,
        Commands::Index => run_index(&config),
        Commands::Stats => run_stats(&config),
    }
}
