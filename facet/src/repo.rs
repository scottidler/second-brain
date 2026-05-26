//! Resolve a cwd to an `owner/repo` slug via `git remote get-url origin`.
//!
//! Patterns the implementation against `claude-report`'s
//! `repo::parse_slug` - same shape, written from scratch (cr is not a
//! runtime dep; see Alternative 2 in the design doc).
//!
//! Phase 2 fills in `resolve_slug` and the URL parser. Phase 1 ships the
//! signatures so wiring in the ledger and config compiles cleanly.

use std::path::Path;

pub fn resolve_slug(_cwd: &Path) -> Option<String> {
    // Phase 2.
    None
}
