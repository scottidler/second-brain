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
