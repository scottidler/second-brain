#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]

pub mod config;
pub mod fabric;
pub mod frontmatter;
pub mod hygiene;
pub mod ledger;
pub mod logging;
pub mod note;
pub mod schema;
pub mod trace;
