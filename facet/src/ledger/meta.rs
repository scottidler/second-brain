//! `ledger_meta` key/value accessors. Stores schema version, last-tick
//! timestamps, current-tick budget usage, etc.

use eyre::{Context, Result};
use rusqlite::OptionalExtension;

use super::Ledger;

impl Ledger {
    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT value FROM ledger_meta WHERE key = ?1",
                rusqlite::params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .context("meta_get")
        })
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        log::debug!("ledger::meta_set: key={key} value_len={}", value.len());
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO ledger_meta(key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![key, value],
            )
            .context("meta_set")?;
            Ok(())
        })
    }
}
