//! JSONL file enumeration + parent/subagent grouping.
//!
//! Phase 2 fills this in. Phase 1 ships the FacetSession type so the
//! ledger and config can reference it.

use std::path::PathBuf;

use crate::jsonl::ParsedSlice;

/// One session's new-turn slice plus its enumeration metadata.
#[derive(Debug, Clone)]
pub struct FacetSession {
    pub session_uuid: String,
    pub cwd: PathBuf,
    pub repo_slug: Option<String>,
    pub parsed: ParsedSlice,
    pub subagent_session_uuids: Vec<String>,
}
