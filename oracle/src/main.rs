//! oracle - MCP server for querying an Obsidian vault's ingested knowledge

use clap::Parser;
use eyre::{Context, Result};
use rmcp::ServiceExt;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;
use vault::search::SearchIndex;
use vault::watcher::{VaultWatcher, WatcherConfig};

mod cli;

use cli::{Cli, Commands};
use oracle::Config;

fn setup_logging(verbose: bool, log_config: &oracle::config::LogConfig) -> Result<()> {
    let level = if verbose { "debug" } else { &log_config.level };
    let filter = EnvFilter::new(level);

    let log_path = match log_config.file {
        Some(ref path) => {
            let expanded = shellexpand::tilde(path);
            PathBuf::from(expanded.as_ref())
        }
        None => {
            // Default XDG log path
            dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("oracle")
                .join("logs")
                .join("oracle.log")
        }
    };

    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
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

async fn run_serve(config: Config) -> Result<()> {
    tracing::info!("Opening database at {}", config.db_path().display());
    let db = SearchIndex::open(&config.db_path()).context("Failed to open database")?;

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
    let server = oracle::server::OracleMcpServer::new(config.clone(), db);

    // Spawn file watcher for live reindex
    if config.watcher.enable {
        let watcher_config = WatcherConfig {
            debounce_secs: config.watcher.debounce_secs,
            ignore_dirs: config.watcher.ignore.clone(),
        };
        let vault_root = config.vault_root();
        match VaultWatcher::start(&vault_root, watcher_config, None) {
            Ok((watcher, mut rx)) => {
                let db_handle = server.db_handle();
                let vault_root = config.vault_root();
                tracing::info!("File watcher started (debounce: {}s)", config.watcher.debounce_secs);
                tokio::spawn(async move {
                    // Hold watcher alive for the lifetime of this task
                    let _keep = watcher;
                    while let Some(change) = rx.recv().await {
                        tracing::info!("vault changed ({} files), reindexing", change.changed_paths.len());
                        if let Ok(db) = db_handle.lock() {
                            match db.index_vault(&vault_root) {
                                Ok(stats) => tracing::info!(
                                    "reindex: {} updated, {} inserted, {} unchanged",
                                    stats.updated,
                                    stats.inserted,
                                    stats.unchanged
                                ),
                                Err(e) => tracing::warn!("reindex failed: {e}"),
                            }
                        }
                    }
                });
            }
            Err(e) => {
                tracing::warn!("Failed to start file watcher, continuing without live reindex: {e}");
            }
        }
    }

    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let service = server.serve(transport).await?;

    tracing::info!("MCP server started, waiting for requests...");
    service.waiting().await?;
    tracing::info!("MCP server shutting down");

    Ok(())
}

fn run_index(config: &Config) -> Result<()> {
    let db = SearchIndex::open(&config.db_path()).context("Failed to open database")?;

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

async fn run_call(config: Config, tool: &str, args_json: Option<&str>) -> Result<()> {
    let db = SearchIndex::open(&config.db_path()).context("Failed to open database")?;
    db.index_vault(&config.vault_root()).context("Failed to index vault")?;

    let server = oracle::server::OracleMcpServer::new(config, db);

    let args: serde_json::Value = match args_json {
        Some(json) => serde_json::from_str(json).context("invalid JSON arguments")?,
        None => serde_json::json!({}),
    };

    let result = server
        .dispatch(tool, args)
        .await
        .map_err(|e| eyre::eyre!("{}", e.message))?;

    if result.is_error == Some(true) {
        for content in &result.content {
            if let Some(text) = content.as_text() {
                eprintln!("{}", text.text);
            }
        }
        std::process::exit(1);
    }

    for content in &result.content {
        if let Some(text) = content.as_text() {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text.text) {
                println!("{}", serde_json::to_string_pretty(&parsed)?);
            } else {
                println!("{}", text.text);
            }
        }
    }

    Ok(())
}

fn run_list() {
    for tool in oracle::server::OracleMcpServer::list_tools() {
        println!("{:<20} {}", tool.name, tool.description.as_deref().unwrap_or(""));
    }
}

fn run_stats(config: &Config) -> Result<()> {
    let db = SearchIndex::open(&config.db_path()).context("Failed to open database")?;

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
        Commands::Call { tool, json, list } => {
            if list {
                run_list();
                Ok(())
            } else {
                run_call(
                    config,
                    tool.as_deref().expect("clap enforces tool or --list"),
                    json.as_deref(),
                )
                .await
            }
        }
    }
}
