//! `sb oracle eval --mine-queries`: Scott's own past `knowledge_search`
//! queries, read from the Claude Code transcripts clyde catalogs, as an eval
//! query set.
//!
//! clyde's `session export` is the session list: it spans every host clyde
//! has indexed and carries each session's resolved `scope`. Only `personal`
//! sessions qualify; a `work`, missing or unknown scope drops the session
//! before its transcript is opened (fail closed). Scope is clyde's decision,
//! never re-derived from a cwd or repo path: a `scottidler/` repo can be
//! `work`, and a personal session often has no repo at all.
//!
//! The export body carries message text only, not tool calls, so the queries
//! come from the transcript files the export points at: the live
//! `transcript-path`, else the staged copy, plus the session's subagent
//! transcripts.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::cache::stable_hash;

/// The one clyde scope whose sessions are mined.
pub const PERSONAL_SCOPE: &str = "personal";
/// Tool name suffix of oracle's search tool in a transcript (`mcp__oracle__knowledge_search`).
const SEARCH_TOOL: &str = "knowledge_search";
/// Prefix of every mined query id.
const MINED_ID_PREFIX: &str = "mined-";

/// The slice of clyde's `session export` envelope the miner reads. Unknown
/// fields are ignored: the envelope is clyde's additive-within-major contract.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ExportEnvelope {
    pub sessions: Vec<ExportSession>,
}

/// One exported session: its id, scope and transcript locations.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ExportSession {
    pub session_id: String,
    /// `work` | `personal` in clyde's contract; `None` when the key is absent.
    #[serde(default)]
    pub scope: Option<String>,
    pub transcript_path: PathBuf,
    pub project_dir: PathBuf,
    #[serde(default)]
    pub staged_path: Option<PathBuf>,
}

/// Why a session's scope admits or drops it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeVerdict {
    Personal,
    Work,
    /// Missing, or a value outside clyde's vocabulary.
    Unknown,
}

impl ExportSession {
    /// The session's scope verdict. Only [`ScopeVerdict::Personal`] is mined.
    pub fn scope_verdict(&self) -> ScopeVerdict {
        match self.scope.as_deref() {
            Some(PERSONAL_SCOPE) => ScopeVerdict::Personal,
            Some("work") => ScopeVerdict::Work,
            _ => ScopeVerdict::Unknown,
        }
    }

    /// The transcript files to read: the live transcript if it is a file, else
    /// the staged copy, then every `*.jsonl` under that layout's `subagents/`,
    /// sorted. Empty when neither the live nor the staged transcript exists.
    pub fn transcript_files(&self) -> Result<Vec<PathBuf>> {
        let (parent, subagents) = if self.transcript_path.is_file() {
            (
                self.transcript_path.clone(),
                self.project_dir.join(&self.session_id).join("subagents"),
            )
        } else {
            match &self.staged_path {
                Some(staged) if staged.join(format!("{}.jsonl", self.session_id)).is_file() => (
                    staged.join(format!("{}.jsonl", self.session_id)),
                    staged.join("subagents"),
                ),
                _ => return Ok(Vec::new()),
            }
        };
        let mut files = vec![parent];
        if subagents.is_dir() {
            let mut subs = Vec::new();
            for entry in std::fs::read_dir(&subagents).with_context(|| format!("reading {}", subagents.display()))? {
                let path = entry
                    .with_context(|| format!("reading {}", subagents.display()))?
                    .path();
                if path.extension().is_some_and(|e| e == "jsonl") && path.is_file() {
                    subs.push(path);
                }
            }
            subs.sort();
            files.extend(subs);
        }
        Ok(files)
    }
}

/// The `query` argument of every `knowledge_search` tool call in one JSONL
/// transcript, in order. Lines that are not JSON, or not a tool call, are
/// skipped: a transcript is a log, and a torn last line is normal.
pub fn queries_in_transcript(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.contains(SEARCH_TOOL) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(items) = value.pointer("/message/content").and_then(|c| c.as_array()) else {
            continue;
        };
        for item in items {
            let is_search = item.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                && item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| n == SEARCH_TOOL || n.ends_with(&format!("__{SEARCH_TOOL}")));
            if !is_search {
                continue;
            }
            if let Some(q) = item.pointer("/input/query").and_then(|q| q.as_str())
                && !q.trim().is_empty()
            {
                out.push(q.trim().to_string());
            }
        }
    }
    out
}

/// The dedup key of a query: lowercased, whitespace collapsed.
pub fn dedup_key(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// What one mining pass found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mined {
    /// Distinct queries, first-seen order.
    pub queries: Vec<String>,
    pub sessions_total: usize,
    pub sessions_personal: usize,
    pub sessions_work: usize,
    /// Missing or unknown scope: dropped.
    pub sessions_unknown_scope: usize,
    /// Personal sessions whose live and staged transcripts are both gone.
    pub sessions_without_transcript: usize,
    /// `knowledge_search` calls read from personal sessions, before dedup.
    pub calls: usize,
}

/// Mine `envelope`: drop every session that is not `personal` before opening
/// anything, then collect and dedup the `knowledge_search` queries of the rest.
/// A transcript that exists but cannot be read is an error.
pub fn mine(envelope: &ExportEnvelope) -> Result<Mined> {
    tracing::debug!(sessions = envelope.sessions.len(), "mine");
    let mut mined = Mined {
        sessions_total: envelope.sessions.len(),
        ..Mined::default()
    };
    let mut seen = HashSet::new();
    for session in &envelope.sessions {
        match session.scope_verdict() {
            ScopeVerdict::Personal => mined.sessions_personal += 1,
            ScopeVerdict::Work => {
                mined.sessions_work += 1;
                continue;
            }
            ScopeVerdict::Unknown => {
                tracing::debug!(session = %session.session_id, scope = ?session.scope, "dropping: unknown scope");
                mined.sessions_unknown_scope += 1;
                continue;
            }
        }
        let files = session.transcript_files()?;
        if files.is_empty() {
            mined.sessions_without_transcript += 1;
            continue;
        }
        for file in files {
            let bytes = std::fs::read(&file).with_context(|| format!("reading transcript {}", file.display()))?;
            for query in queries_in_transcript(&String::from_utf8_lossy(&bytes)) {
                mined.calls += 1;
                if seen.insert(dedup_key(&query)) {
                    mined.queries.push(query);
                }
            }
        }
    }
    tracing::debug!(queries = mined.queries.len(), calls = mined.calls, "mine done");
    Ok(mined)
}

/// One query in the written set (the `queries.yml` shape `Queries::load` reads).
#[derive(Debug, Serialize)]
struct MinedQuery<'a> {
    id: String,
    query: &'a str,
}

#[derive(Debug, Serialize)]
struct MinedSet<'a> {
    queries: Vec<MinedQuery<'a>>,
}

/// The query set as `queries.yml` YAML. Ids are `mined-<hash of the dedup
/// key>`, so the same query keeps its id (and its cached judgments) across runs.
pub fn render_yaml(mined: &Mined) -> Result<String> {
    let set = MinedSet {
        queries: mined
            .queries
            .iter()
            .map(|q| MinedQuery {
                id: format!("{MINED_ID_PREFIX}{}", stable_hash(&dedup_key(q))),
                query: q,
            })
            .collect(),
    };
    let body = serde_yaml::to_string(&set).context("serializing mined queries")?;
    Ok(format!(
        "# Mined by `sb oracle eval --mine-queries` from {} personal-scope sessions \
         ({} knowledge_search calls, {} distinct).\n{body}",
        mined.sessions_personal,
        mined.calls,
        mined.queries.len()
    ))
}

/// Run `clyde session export --include-archived` (archived sessions can still
/// have a staged transcript) and parse its envelope. A non-zero exit or an
/// unparseable envelope is an error carrying clyde's stderr.
pub fn export_from_clyde(binary: &str) -> Result<ExportEnvelope> {
    tracing::debug!(binary, "export_from_clyde");
    let output = Command::new(binary)
        .args(["session", "export", "--include-archived"])
        .output()
        .with_context(|| format!("running `{binary} session export` (is clyde installed?)"))?;
    if !output.status.success() {
        bail!(
            "`{binary} session export` exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).with_context(|| format!("parsing `{binary} session export` output"))
}

/// Mine through clyde and write the query set to `out`. An empty result is an
/// error: an eval over zero queries measures nothing.
pub fn mine_to_file(clyde: &str, out: &Path) -> Result<Mined> {
    let envelope = export_from_clyde(clyde)?;
    let mined = mine(&envelope)?;
    if mined.queries.is_empty() {
        bail!(
            "no knowledge_search queries found in {} personal-scope sessions (of {})",
            mined.sessions_personal,
            mined.sessions_total
        );
    }
    let yaml = render_yaml(&mined)?;
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(out, yaml).with_context(|| format!("writing mined queries to {}", out.display()))?;
    Ok(mined)
}

#[cfg(test)]
mod tests;
