//! Error type for the glean subsystem.
//!
//! Boundary errors (JSONL parse, sqlite I/O, fabric shell-out, vault
//! render) flow through this enum; internal modules use `eyre::Result`
//! freely. The enum is what the CLI surface in `sb` displays.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GleanError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("jsonl parse error: {0}")]
    Jsonl(String),

    #[error("classify error: {0}")]
    Classify(String),

    #[error("distill error: {0}")]
    Distill(String),

    #[error("dream error: {0}")]
    Dream(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("other: {0}")]
    Other(String),
}
