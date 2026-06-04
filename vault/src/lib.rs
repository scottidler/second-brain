#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]

pub mod canonical;
pub mod config;
pub mod detail;
pub mod distilled;
pub mod embedding;
pub mod fabric;
pub mod frontmatter;
pub mod hygiene;
pub mod intake;
pub mod ledger;
pub mod logging;
pub mod note;
pub mod paths;
pub mod receipts;
pub mod rss;
pub mod schema;
#[cfg(feature = "search")]
pub mod search;
pub mod table;
pub mod trace;
#[cfg(feature = "watcher")]
pub mod watcher;
