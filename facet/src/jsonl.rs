//! JSONL transcript parsing.
//!
//! Owns the typed parser over `~/.claude/projects/<encoded-cwd>/<sid>.jsonl`.
//!
//! Real-world Claude Code transcripts contain more than just `user` and
//! `assistant` lines: `last-prompt`, `permission-mode`, `attachment`,
//! `file-history-snapshot`, `ai-title`, `system`. The parser tolerates
//! all of those by emitting Turns *only* for user/assistant lines and
//! silently skipping the rest.
//!
//! The byte-offset cursor is exact: parsing resumes after the last
//! newline-terminated line of the previous tick. A partial trailing
//! line (file mid-write by Claude Code) is left untouched and re-read
//! on the next tick.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One typed content block of a Turn. The real-world JSONL line shapes
/// include `text`, `thinking`, `tool_use`, `tool_result`, and `image`.
/// Anything else is rendered as [`ContentBlock::Unknown`] and the line
/// is counted toward `schema_drift_lines` for later diagnosis.
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

/// One parsed Turn. The `uuid`/`parent_uuid` pair is what the cluster
/// stage uses to record bounded ranges; `content` carries the typed
/// blocks; `model` is set on assistant turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub uuid: String,
    pub parent_uuid: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub model: Option<String>,
}

/// Output of one parse pass over a session JSONL file from a start
/// byte offset. Only new (post-offset) turns appear in `turns`.
#[derive(Debug, Clone)]
pub struct ParsedSlice {
    pub session_uuid: String,
    pub turns: Vec<Turn>,
    pub end_byte_offset: u64,
    pub schema_drift_lines: u32,
    /// The `cwd` field as it appears inside the JSONL on user/assistant
    /// lines (captured from the first such line in the slice). The
    /// encoded-directory-name decoder is lossy when path segments
    /// contain literal `-`s, so this is the canonical cwd.
    pub cwd: Option<std::path::PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum JsonlError {
    #[error("I/O error opening {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("session uuid missing - empty file or no user/assistant turns: {path}")]
    NoSessionUuid { path: String },
}

/// Parse a JSONL session file from `start_byte_offset` to EOF.
///
/// - Lines before `start_byte_offset` are ignored (the cursor advanced
///   on the previous tick).
/// - Only `user` and `assistant` `type` lines are emitted as Turns.
/// - `last-prompt`, `permission-mode`, `attachment`,
///   `file-history-snapshot`, `ai-title`, `system` are skipped silently.
/// - Lines with an unrecognised top-level `type` are counted in
///   `schema_drift_lines` and logged at WARN.
/// - A trailing partial line (no terminating `\n`) is left untouched;
///   `end_byte_offset` points just past the last complete line.
///
/// `session_uuid` is taken from the first `user` or `assistant` line
/// encountered in the new slice or the session UUID derived from the
/// filename if no turn lines are present. If neither source is
/// available, returns [`JsonlError::NoSessionUuid`].
pub fn parse_session_file(path: &Path, start_byte_offset: u64) -> Result<ParsedSlice, JsonlError> {
    log::debug!(
        "parse_session_file: path={} start_byte_offset={start_byte_offset}",
        path.display()
    );
    let mut file = File::open(path).map_err(|source| JsonlError::Io {
        path: path.display().to_string(),
        source,
    })?;
    file.seek(SeekFrom::Start(start_byte_offset))
        .map_err(|source| JsonlError::Io {
            path: path.display().to_string(),
            source,
        })?;
    let mut reader = BufReader::new(file);

    let mut turns: Vec<Turn> = Vec::new();
    let mut schema_drift_lines: u32 = 0;
    let mut session_from_line: Option<String> = None;
    let mut cwd_from_line: Option<std::path::PathBuf> = None;
    let mut consumed_bytes: u64 = start_byte_offset;

    let mut buf = String::new();
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf).map_err(|source| JsonlError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if n == 0 {
            break;
        }
        // Per parse_session_file contract: only count complete (newline-
        // terminated) lines toward the advance. A partial trailing line is
        // re-read next tick.
        let complete = buf.ends_with('\n');
        if !complete {
            log::debug!("parse_session_file: partial trailing line at offset {consumed_bytes}; deferring");
            break;
        }
        consumed_bytes += n as u64;

        let trimmed = buf.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("parse_session_file: skipping malformed JSON line at offset {consumed_bytes}: {e}");
                schema_drift_lines += 1;
                continue;
            }
        };

        let Some(line_type) = value.get("type").and_then(|v| v.as_str()) else {
            log::warn!("parse_session_file: skipping line with no top-level `type` at offset {consumed_bytes}");
            schema_drift_lines += 1;
            continue;
        };

        match line_type {
            "user" | "assistant" => {}
            "last-prompt" | "permission-mode" | "attachment" | "file-history-snapshot" | "ai-title" | "system" => {
                continue;
            }
            other => {
                log::warn!("parse_session_file: unknown line type {other:?}; skipping");
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
            cwd_from_line = Some(std::path::PathBuf::from(s));
        }

        match parse_turn(&value, line_type) {
            Ok(t) => turns.push(t),
            Err(reason) => {
                log::warn!("parse_session_file: skipping malformed turn at offset {consumed_bytes}: {reason}");
                schema_drift_lines += 1;
            }
        }
    }

    let session_uuid = session_from_line
        .or_else(|| derive_session_uuid_from_path(path))
        .ok_or_else(|| JsonlError::NoSessionUuid {
            path: path.display().to_string(),
        })?;

    Ok(ParsedSlice {
        session_uuid,
        turns,
        end_byte_offset: consumed_bytes,
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
            Err(e) => log::warn!("parse_content: skipping content block {i}: {e}"),
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
            let content_value = block.get("content");
            let content = match content_value {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(blocks)) => {
                    // Flatten the array of sub-blocks. Common shapes:
                    // [{type: text, text: "..."}], [{type: tool_reference, ...}].
                    let mut parts = Vec::new();
                    for b in blocks {
                        if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                            parts.push(t.to_string());
                        } else if let Some(s) = b.as_str() {
                            parts.push(s.to_string());
                        } else {
                            parts.push(b.to_string());
                        }
                    }
                    parts.join("\n")
                }
                Some(other) => other.to_string(),
                None => String::new(),
            };
            let is_error = block.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
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

#[cfg(test)]
mod tests;
