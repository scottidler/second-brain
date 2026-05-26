//! JSONL transcript parsing.
//!
//! Owns the typed parser over `~/.claude/projects/<encoded-cwd>/<sid>.jsonl`.
//! Phase 2 (Phase 1 ships the stub).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    Image {
        marker: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub uuid: String,
    pub parent_uuid: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ParsedSlice {
    pub session_uuid: String,
    pub turns: Vec<Turn>,
    pub end_byte_offset: u64,
    pub schema_drift_lines: u32,
}

// Phase 2 fills in parse_session_file. Phase 1 ships the types so the
// ledger schema and Config can reference them without circular concerns.
