//! oracle - MCP server for querying an Obsidian vault's ingested knowledge
//!
//! Provides schema-aware search, note retrieval with configurable detail levels,
//! and domain intelligence over a second-brain vault indexed into SQLite.

pub mod config;
pub mod db;
pub mod detail;
pub mod server;
pub mod tools;

pub use config::Config;
pub use db::Database;
