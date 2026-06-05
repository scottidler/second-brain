//! Borg's ledger surface is the vault crate's `ledger` module verbatim. The
//! ledger now lives in the borg data dir (see `vault::ledger::ledger_path`),
//! so there is no borg-specific path wrapper - call `ledger_path()` directly.
pub use vault::ledger::*;
