//! JSONL file enumeration, parent/subagent grouping, and per-session
//! new-turn parsing.
//!
//! Patterned on `claude-report`'s `scan::find_session_files` —
//! same Parent/Subagent grouping shape, written from scratch
//! (claude-report is not a runtime dep; see Alternative 2 in the
//! design doc).
//!
//! `enumerate` is the entry point. It:
//!
//! 1. Walks `claude-projects-root` looking for `*.jsonl` files at the
//!    project-dir level (parents) and inside `<parent>/subagents/`
//!    sub-trees (subagents).
//! 2. For each parent JSONL: parses new turns from
//!    `sessions.last_cluster_offset` via [`crate::jsonl::parse_session_file`].
//! 3. Filters by include-cwds / exclude-cwds prefix matches on the
//!    decoded cwd (the encoded-cwd directory name has `-` for `/`).
//! 4. Resolves the cwd to a repo slug via [`crate::repo::Resolver`].
//! 5. Drops sessions with no new turns.

use eyre::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::jsonl::{self, ParsedSlice};
use crate::ledger::Ledger;
use crate::repo::Resolver;

/// One session's new-turn slice plus its enumeration metadata.
#[derive(Debug, Clone)]
pub struct FacetSession {
    pub session_uuid: String,
    pub cwd: PathBuf,
    pub repo_slug: Option<String>,
    pub parsed: ParsedSlice,
    pub subagent_session_uuids: Vec<String>,
}

/// One JSONL discovered under the projects root, classified by role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionFileKind {
    Parent,
    Subagent,
}

#[derive(Debug, Clone)]
pub struct SessionFile {
    pub path: PathBuf,
    pub group_id: String,
    pub kind: SessionFileKind,
    /// Decoded cwd from the parent project directory name (e.g.
    /// `-home-saidler-repos-scottidler-loopr` becomes
    /// `/home/saidler/repos/scottidler/loopr`).
    pub cwd: PathBuf,
}

/// Walk the projects root and emit all parent + subagent JSONL files.
pub fn find_session_files(projects_dir: &Path) -> Result<Vec<SessionFile>> {
    log::debug!("scan::find_session_files: projects_dir={}", projects_dir.display());
    if !projects_dir.exists() {
        log::warn!(
            "scan::find_session_files: projects dir missing on disk: {}",
            projects_dir.display()
        );
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for project_entry in read_dir(projects_dir)? {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let cwd = decode_cwd(&project_path);
        for entry in read_dir(&project_path)? {
            let entry_path = entry.path();
            if entry_path.is_file() && entry_path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let Some(stem) = entry_path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if is_empty_file(&entry_path) {
                    continue;
                }
                out.push(SessionFile {
                    path: entry_path.clone(),
                    group_id: stem.to_string(),
                    kind: SessionFileKind::Parent,
                    cwd: cwd.clone(),
                });
                continue;
            }
            if entry_path.is_dir() {
                let stem = match entry_path.file_name().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let subagents_dir = entry_path.join("subagents");
                if !subagents_dir.is_dir() {
                    continue;
                }
                for sub in read_dir(&subagents_dir)? {
                    let sub_path = sub.path();
                    if !sub_path.is_file() || sub_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    if is_empty_file(&sub_path) {
                        continue;
                    }
                    out.push(SessionFile {
                        path: sub_path,
                        group_id: stem.clone(),
                        kind: SessionFileKind::Subagent,
                        cwd: cwd.clone(),
                    });
                }
            }
        }
    }
    log::info!("scan::find_session_files: discovered {} files", out.len());
    Ok(out)
}

/// Run a full enumerate-and-parse pass. Returns one FacetSession per
/// parent session that has new turns after the include/exclude filter.
/// Per session, the byte offset is read from the ledger (or 0 for
/// unseen sessions); subagent files are linked to their parent's
/// FacetSession.
pub fn enumerate(config: &Config, ledger: &Ledger) -> Result<Vec<FacetSession>> {
    log::debug!(
        "scan::enumerate: claude_projects_root={}",
        config.claude_projects_root.display()
    );
    let files = find_session_files(&config.claude_projects_root)?;
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let resolver = Resolver::new();

    // Group children by parent_group_id.
    let mut parents: Vec<SessionFile> = Vec::new();
    let mut children_by_parent: HashMap<String, Vec<SessionFile>> = HashMap::new();
    for f in files {
        match f.kind {
            SessionFileKind::Parent => parents.push(f),
            SessionFileKind::Subagent => {
                children_by_parent.entry(f.group_id.clone()).or_default().push(f);
            }
        }
    }

    let mut out = Vec::new();
    for parent in parents {
        let last_offset = ledger
            .get_session(&parent.group_id)
            .context("ledger lookup")?
            .map(|r| r.last_cluster_offset)
            .unwrap_or(0);
        let parsed = match jsonl::parse_session_file(&parent.path, last_offset) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("scan::enumerate: parse failed for {}: {e}", parent.path.display());
                continue;
            }
        };
        if parsed.turns.is_empty() {
            continue;
        }
        // Prefer the cwd carried inside the JSONL (set by Claude Code on every
        // user/assistant line); fall back to the lossy directory-name decode
        // only when no turn carried it. The decoded form is unreliable when
        // path segments contain literal `-`s (`tatari-tv` decodes to
        // `tatari/tv`), which matters for include/exclude filtering.
        let cwd = parsed.cwd.clone().unwrap_or_else(|| parent.cwd.clone());
        if !config.is_cwd_eligible(&cwd) {
            log::debug!("scan::enumerate: skipping cwd by include/exclude: {}", cwd.display());
            continue;
        }
        let repo_slug = resolver.resolve(&cwd);
        let subagent_uuids = children_by_parent
            .get(&parent.group_id)
            .map(|v| {
                v.iter()
                    .filter_map(|f| f.path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        out.push(FacetSession {
            session_uuid: parsed.session_uuid.clone(),
            cwd,
            repo_slug,
            parsed,
            subagent_session_uuids: subagent_uuids,
        });
    }
    log::info!("scan::enumerate: produced {} sessions with new turns", out.len());
    Ok(out)
}

/// Decode the encoded-cwd directory name back to an absolute path.
///
/// Claude Code's encoding replaces every `/` with `-` and prefixes with
/// `-` so `/home/saidler/repos/x` becomes `-home-saidler-repos-x`.
/// Decoding is the reverse: strip the leading `-` and replace `-` with
/// `/`. This is lossy when path segments contain literal `-`s but is
/// good enough for cwd-prefix include/exclude matching.
pub fn decode_cwd(project_dir: &Path) -> PathBuf {
    let name = match project_dir.file_name().and_then(|s| s.to_str()) {
        Some(n) => n,
        None => return project_dir.to_path_buf(),
    };
    if let Some(stripped) = name.strip_prefix('-') {
        PathBuf::from(format!("/{}", stripped.replace('-', "/")))
    } else {
        project_dir.to_path_buf()
    }
}

fn read_dir(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut out = Vec::new();
    let iter = fs::read_dir(path).with_context(|| format!("read_dir {}", path.display()))?;
    for entry in iter {
        match entry {
            Ok(e) => out.push(e),
            Err(e) => log::warn!("read_dir: error iterating {}: {e}", path.display()),
        }
    }
    Ok(out)
}

fn is_empty_file(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(m) => m.len() == 0,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests;
