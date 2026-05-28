//! JSONL session-file parser.
//!
//! Reads `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl` from
//! disk in one shot and emits typed `Turn`s. Unlike the prior
//! resumable-cursor parser facet shipped, glean's harvest re-reads the
//! whole file every pass and re-classifies when `jsonl_sha256` shifts.
//! Simpler state model; no per-file byte-offset cursor.
//!
//! Real-world Claude Code transcripts contain more than just `user`
//! and `assistant` lines: `last-prompt`, `permission-mode`,
//! `attachment`, `file-history-snapshot`, `ai-title`, `system`. The
//! parser tolerates all of those by emitting `Turn`s only for
//! user/assistant lines and silently skipping the rest. Malformed
//! lines or unknown top-level types increment `schema_drift_lines`
//! and are logged at WARN.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use crate::error::GleanError;

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
    Unknown {
        kind: String,
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

/// One full parse of a JSONL session file. `session_uuid` is taken
/// from the first user/assistant line's `sessionId` field or, as a
/// fallback, the filename stem.
#[derive(Debug, Clone)]
pub struct ParsedSession {
    pub session_uuid: String,
    pub jsonl_path: PathBuf,
    pub jsonl_sha256: String,
    pub turns: Vec<Turn>,
    pub schema_drift_lines: u32,
    pub cwd: Option<PathBuf>,
}

impl ParsedSession {
    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        self.turns.first().map(|t| t.timestamp)
    }

    pub fn ended_at(&self) -> Option<DateTime<Utc>> {
        self.turns.last().map(|t| t.timestamp)
    }
}

/// Parse a JSONL session file end-to-end. Returns the structured
/// session, including a sha256 of the file bytes (for the
/// idempotence check in `harvest`).
pub fn parse_session_file(path: &Path) -> Result<ParsedSession, GleanError> {
    log::debug!("jsonl::parse_session_file: path={}", path.display());
    let bytes = std::fs::read(path)?;
    let jsonl_sha256 = sha256_hex(&bytes);
    let reader = BufReader::new(bytes.as_slice());
    parse_session_reader(reader, path.to_path_buf(), jsonl_sha256)
}

fn parse_session_reader<R: Read>(
    reader: BufReader<R>,
    path: PathBuf,
    jsonl_sha256: String,
) -> Result<ParsedSession, GleanError> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut schema_drift_lines: u32 = 0;
    let mut session_from_line: Option<String> = None;
    let mut cwd_from_line: Option<PathBuf> = None;

    for line in reader.lines() {
        let raw = line?;
        let trimmed = raw.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("jsonl::parse_session_reader: bad JSON line skipped: {e}");
                schema_drift_lines += 1;
                continue;
            }
        };
        let Some(line_type) = value.get("type").and_then(|v| v.as_str()) else {
            schema_drift_lines += 1;
            continue;
        };
        match line_type {
            "user" | "assistant" => {}
            "last-prompt" | "permission-mode" | "attachment" | "file-history-snapshot" | "ai-title" | "system" => {
                continue;
            }
            other => {
                log::warn!("jsonl::parse_session_reader: unknown line type {other:?}");
                schema_drift_lines += 1;
                continue;
            }
        }
        if session_from_line.is_none()
            && let Some(s) = value.get("sessionId").and_then(|v| v.as_str())
        {
            session_from_line = Some(s.to_string());
        }
        if cwd_from_line.is_none()
            && let Some(s) = value.get("cwd").and_then(|v| v.as_str())
        {
            cwd_from_line = Some(PathBuf::from(s));
        }
        match parse_turn(&value, line_type) {
            Ok(t) => turns.push(t),
            Err(reason) => {
                log::warn!("jsonl::parse_session_reader: malformed turn: {reason}");
                schema_drift_lines += 1;
            }
        }
    }

    let session_uuid = session_from_line
        .or_else(|| derive_session_uuid_from_path(&path))
        .ok_or_else(|| {
            GleanError::Jsonl(format!(
                "session uuid missing: empty file or no turn lines: {}",
                path.display()
            ))
        })?;

    Ok(ParsedSession {
        session_uuid,
        jsonl_path: path,
        jsonl_sha256,
        turns,
        schema_drift_lines,
        cwd: cwd_from_line,
    })
}

fn derive_session_uuid_from_path(path: &Path) -> Option<String> {
    path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string())
}

fn parse_turn(value: &serde_json::Value, line_type: &str) -> Result<Turn, String> {
    let uuid = value
        .get("uuid")
        .and_then(|v| v.as_str())
        .ok_or("missing uuid")?
        .to_string();
    let parent_uuid = value.get("parentUuid").and_then(|v| v.as_str()).map(|s| s.to_string());
    let ts_str = value
        .get("timestamp")
        .and_then(|v| v.as_str())
        .ok_or("missing timestamp")?;
    let timestamp = DateTime::parse_from_rfc3339(ts_str)
        .map_err(|e| format!("bad timestamp: {e}"))?
        .with_timezone(&Utc);
    let role = match line_type {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        other => return Err(format!("unexpected line_type {other}")),
    };
    let message = value.get("message").ok_or("missing message")?;
    let model = message.get("model").and_then(|v| v.as_str()).map(|s| s.to_string());
    let content = parse_content(message.get("content"))?;
    Ok(Turn {
        uuid,
        parent_uuid,
        timestamp,
        role,
        content,
        model,
    })
}

fn parse_content(value: Option<&serde_json::Value>) -> Result<Vec<ContentBlock>, String> {
    let Some(value) = value else {
        return Ok(vec![]);
    };
    if let Some(s) = value.as_str() {
        return Ok(vec![ContentBlock::Text { text: s.to_string() }]);
    }
    let Some(arr) = value.as_array() else {
        return Err("content is neither string nor array".into());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, block) in arr.iter().enumerate() {
        match parse_block(block) {
            Ok(b) => out.push(b),
            Err(e) => log::warn!("jsonl::parse_content: skipping block {i}: {e}"),
        }
    }
    Ok(out)
}

fn parse_block(block: &serde_json::Value) -> Result<ContentBlock, String> {
    let kind = block.get("type").and_then(|v| v.as_str()).ok_or("block has no type")?;
    match kind {
        "text" => {
            let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
            Ok(ContentBlock::Text { text: text.to_string() })
        }
        "thinking" => {
            let text = block.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
            Ok(ContentBlock::Thinking { text: text.to_string() })
        }
        "tool_use" => {
            let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let input = block.get("input").cloned().unwrap_or(serde_json::Value::Null);
            Ok(ContentBlock::ToolUse { id, name, input })
        }
        "tool_result" => {
            let tool_use_id = block
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let is_error = block.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
            let content = block
                .get("content")
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            Ok(ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            })
        }
        "image" => Ok(ContentBlock::Image {
            marker: "<image>".to_string(),
        }),
        other => Ok(ContentBlock::Unknown {
            kind: other.to_string(),
        }),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Path to the JSONL file's session UUID (used when the file is
/// empty or contains zero turn lines). Exposed for tests and the
/// scan stage's prefilter.
pub fn session_uuid_from_path(path: &Path) -> Option<String> {
    derive_session_uuid_from_path(path)
}

/// Compute the sha256 hex of a file's bytes. Used by the harvest's
/// idempotence check before re-parsing a session.
pub fn file_sha256(path: &Path) -> Result<String, GleanError> {
    let mut f = File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    use sha2::Digest;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests;
