#![deny(dead_code)]
#![deny(unused_variables)]

pub mod canonical;
#[cfg(any(test, feature = "test-util"))]
pub mod capture;
pub mod config;
pub mod daemon;
pub mod detail;
pub mod distilled;
pub mod embedding;
pub mod fabric;
pub mod frontmatter;
#[cfg(feature = "http")]
pub mod http;
pub mod hygiene;
pub mod identity;
pub mod intake;
pub mod ledger;
pub mod logging;
pub mod note;
pub mod paths;
pub mod process;
pub mod queue;
pub mod receipts;
pub mod rss;
pub mod schema;
#[cfg(feature = "search")]
pub mod search;
pub mod systemd;
pub mod table;
pub mod text;
pub mod tombstone;
pub mod trace;
#[cfg(feature = "watcher")]
pub mod watcher;
pub mod wikilink;
