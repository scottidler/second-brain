//! oracle - MCP server for querying an Obsidian vault's ingested knowledge
//!
//! Provides schema-aware search, note retrieval with configurable detail levels,
//! and domain intelligence over a second-brain vault indexed into SQLite.
//!
//! The search index and detail extraction are provided by the shared vault crate.

// Lib invariant: oracle pub fns return typed data; sb owns stdout/stderr.
// Production code emits nothing via println!/eprintln! - tracing::* routes
// through the logger initializer instead. Test modules that print captured
// stdout are exempted via the not(test) guard below.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod config;
pub mod server;
pub mod tools;

pub use config::Config;

use eyre::{Context, Result};
use rmcp::ServiceExt;
use vault::search::SearchIndex;
use vault::watcher::{VaultWatcher, WatcherConfig};

pub async fn serve(config: Config) -> Result<()> {
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

/// Reindex the vault and return the IndexStats. Caller formats the report.
pub fn index(config: &Config) -> Result<vault::search::IndexStats> {
    let db = SearchIndex::open(&config.db_path()).context("Failed to open database")?;
    db.index_vault(&config.vault_root()).context("Failed to index vault")
}

/// Dispatch a single MCP tool call (no transport). Caller formats `result.content`.
pub async fn call(config: Config, tool: &str, args_json: Option<&str>) -> Result<rmcp::model::CallToolResult> {
    let db = SearchIndex::open(&config.db_path()).context("Failed to open database")?;
    db.index_vault(&config.vault_root()).context("Failed to index vault")?;

    let server = server::OracleMcpServer::new(config, db);

    let args: serde_json::Value = match args_json {
        Some(json) => serde_json::from_str(json).context("invalid JSON arguments")?,
        None => serde_json::json!({}),
    };

    server
        .dispatch(tool, args)
        .await
        .map_err(|e| eyre::eyre!("{}", e.message))
}

/// Available MCP tools (no I/O). Caller formats the table.
pub fn tools() -> Vec<rmcp::model::Tool> {
    server::OracleMcpServer::list_tools()
}

/// Open the SQLite index and return vault statistics. Caller formats them.
pub fn stats(config: &Config) -> Result<vault::search::VaultStats> {
    let db = SearchIndex::open(&config.db_path()).context("Failed to open database")?;
    db.index_vault(&config.vault_root()).context("Failed to index vault")?;
    db.stats().context("Failed to get stats")
}
