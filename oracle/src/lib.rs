//! oracle - MCP server for querying an Obsidian vault's ingested knowledge
//!
//! Provides schema-aware search, note retrieval with configurable detail levels,
//! and domain intelligence over a second-brain vault indexed into SQLite.
//!
//! The search index and detail extraction are provided by the shared vault crate.

pub mod config;
pub mod server;
pub mod tools;

pub use config::Config;

use eyre::{Context, Result};
use rmcp::ServiceExt;
use vault::search::SearchIndex;
use vault::watcher::{VaultWatcher, WatcherConfig};

pub async fn run_serve(config: Config) -> Result<()> {
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
    let server = server::OracleMcpServer::new(config.clone(), db);

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

    // Periodic inbound-link recompute. Not wired into the watcher path: the watcher fires sub-second
    // on every save; a full-table wikilink scan inside the SearchIndex mutex would block every
    // concurrent note_read / knowledge_search. A 10-minute cadence keeps inbound counts at most
    // minutes stale relative to consumers.
    {
        let db_handle = server.db_handle();
        let interval_secs = config.inbound_recompute_interval_secs;
        tracing::info!("Inbound-link recompute task starting (interval: {interval_secs}s)");
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            tick.tick().await;
            loop {
                tick.tick().await;
                let changed = match db_handle.lock() {
                    Ok(mut db) => db.recompute_inbound_link_counts(),
                    Err(e) => {
                        tracing::warn!("inbound recompute: db mutex poisoned: {e}");
                        continue;
                    }
                };
                match changed {
                    Ok(n) => tracing::info!("inbound recompute: {n} rows changed"),
                    Err(e) => tracing::warn!("inbound recompute failed: {e}"),
                }
            }
        });
    }

    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let service = server.serve(transport).await?;

    tracing::info!("MCP server started, waiting for requests...");
    service.waiting().await?;
    tracing::info!("MCP server shutting down");

    Ok(())
}

pub fn run_index(config: &Config) -> Result<()> {
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

pub async fn run_call(config: Config, tool: &str, args_json: Option<&str>) -> Result<()> {
    let db = SearchIndex::open(&config.db_path()).context("Failed to open database")?;
    db.index_vault(&config.vault_root()).context("Failed to index vault")?;

    let server = server::OracleMcpServer::new(config, db);

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

pub fn run_list() {
    for tool in server::OracleMcpServer::list_tools() {
        println!("{:<20} {}", tool.name, tool.description.as_deref().unwrap_or(""));
    }
}

pub fn run_stats(config: &Config) -> Result<()> {
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
