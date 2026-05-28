//! `sessions` table CRUD.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use rusqlite::OptionalExtension;
use std::path::PathBuf;

use super::Ledger;
use crate::types::SessionRecord;

impl Ledger {
    /// Upsert one session record. `session_uuid` is the primary key.
    pub fn upsert_session(&self, r: &SessionRecord) -> Result<()> {
        log::debug!(
            "ledger::upsert_session: session_uuid={} jsonl_sha256={} tags={}",
            r.session_uuid,
            &r.jsonl_sha256[..8.min(r.jsonl_sha256.len())],
            r.theme_tags.len()
        );
        let design_doc_files = serde_json::to_string(&r.design_doc_files).context("encode design_doc_files")?;
        let skill_invocations = serde_json::to_string(&r.skill_invocations).context("encode skill_invocations")?;
        let theme_tags = serde_json::to_string(&r.theme_tags).context("encode theme_tags")?;
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO sessions(\
                    session_uuid, jsonl_path, jsonl_sha256, repo_slug, repo_path, cwd, \
                    started_at, ended_at, design_doc_files, skill_invocations, \
                    interaction_normalized, summary_one_line, theme_tags, design_doc_focus, \
                    is_orphan, classified_at, classifier_model\
                 ) VALUES (\
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17\
                 ) \
                 ON CONFLICT(session_uuid) DO UPDATE SET \
                    jsonl_path = excluded.jsonl_path, \
                    jsonl_sha256 = excluded.jsonl_sha256, \
                    repo_slug = excluded.repo_slug, \
                    repo_path = excluded.repo_path, \
                    cwd = excluded.cwd, \
                    started_at = excluded.started_at, \
                    ended_at = excluded.ended_at, \
                    design_doc_files = excluded.design_doc_files, \
                    skill_invocations = excluded.skill_invocations, \
                    interaction_normalized = excluded.interaction_normalized, \
                    summary_one_line = excluded.summary_one_line, \
                    theme_tags = excluded.theme_tags, \
                    design_doc_focus = excluded.design_doc_focus, \
                    is_orphan = excluded.is_orphan, \
                    classified_at = excluded.classified_at, \
                    classifier_model = excluded.classifier_model",
                rusqlite::params![
                    r.session_uuid,
                    r.jsonl_path.to_string_lossy(),
                    r.jsonl_sha256,
                    r.repo_slug,
                    r.repo_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                    r.cwd.as_ref().map(|p| p.to_string_lossy().to_string()),
                    r.started_at.to_rfc3339(),
                    r.ended_at.to_rfc3339(),
                    design_doc_files,
                    skill_invocations,
                    r.interaction_normalized,
                    r.summary_one_line,
                    theme_tags,
                    r.design_doc_focus.as_ref().map(|p| p.to_string_lossy().to_string()),
                    if r.is_orphan { 1 } else { 0 },
                    r.classified_at.to_rfc3339(),
                    r.classifier_model,
                ],
            )
            .context("upsert sessions row")?;
            Ok(())
        })
    }

    pub fn get_session(&self, session_uuid: &str) -> Result<Option<SessionRecord>> {
        log::debug!("ledger::get_session: session_uuid={session_uuid}");
        self.with_conn(|c| {
            c.query_row(
                "SELECT session_uuid, jsonl_path, jsonl_sha256, repo_slug, repo_path, cwd, \
                        started_at, ended_at, design_doc_files, skill_invocations, \
                        interaction_normalized, summary_one_line, theme_tags, \
                        design_doc_focus, is_orphan, classified_at, classifier_model \
                 FROM sessions WHERE session_uuid = ?1",
                rusqlite::params![session_uuid],
                row_to_session,
            )
            .optional()
            .context("query sessions row")
        })
    }

    /// Look up the stored sha256 for a session, if any. Used by
    /// `harvest` to short-circuit re-classification when the file
    /// is unchanged on disk.
    pub fn get_session_sha256(&self, session_uuid: &str) -> Result<Option<String>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT jsonl_sha256 FROM sessions WHERE session_uuid = ?1",
                rusqlite::params![session_uuid],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .context("get_session_sha256")
        })
    }

    pub fn all_sessions(&self) -> Result<Vec<SessionRecord>> {
        log::debug!("ledger::all_sessions");
        self.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT session_uuid, jsonl_path, jsonl_sha256, repo_slug, repo_path, cwd, \
                            started_at, ended_at, design_doc_files, skill_invocations, \
                            interaction_normalized, summary_one_line, theme_tags, \
                            design_doc_focus, is_orphan, classified_at, classifier_model \
                     FROM sessions ORDER BY started_at",
                )
                .context("prep all_sessions")?;
            let rows = stmt.query_map([], row_to_session).context("query all_sessions")?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.context("row all_sessions")?);
            }
            Ok(out)
        })
    }

    pub fn get_sessions_by_uuids(&self, uuids: &[String]) -> Result<Vec<SessionRecord>> {
        if uuids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (0..uuids.len())
            .map(|i| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT session_uuid, jsonl_path, jsonl_sha256, repo_slug, repo_path, cwd, \
                    started_at, ended_at, design_doc_files, skill_invocations, \
                    interaction_normalized, summary_one_line, theme_tags, \
                    design_doc_focus, is_orphan, classified_at, classifier_model \
             FROM sessions WHERE session_uuid IN ({placeholders})"
        );
        self.with_conn(|c| {
            let mut stmt = c.prepare(&sql).context("prep get_sessions_by_uuids")?;
            let params: Vec<&dyn rusqlite::ToSql> = uuids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
            let rows = stmt
                .query_map(params.as_slice(), row_to_session)
                .context("query get_sessions_by_uuids")?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.context("row get_sessions_by_uuids")?);
            }
            Ok(out)
        })
    }
}

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    let started_at: String = r.get(6)?;
    let ended_at: String = r.get(7)?;
    let design_doc_files: String = r.get(8)?;
    let skill_invocations: String = r.get(9)?;
    let theme_tags: String = r.get(12)?;
    let classified_at: String = r.get(15)?;
    let is_orphan: i64 = r.get(14)?;
    let jsonl_path: String = r.get(1)?;
    let repo_path: Option<String> = r.get(4)?;
    let cwd: Option<String> = r.get(5)?;
    let design_doc_focus: Option<String> = r.get(13)?;
    let design_doc_files: Vec<PathBuf> = serde_json::from_str(&design_doc_files)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e)))?;
    let skill_invocations: Vec<String> = serde_json::from_str(&skill_invocations)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e)))?;
    let theme_tags: Vec<String> = serde_json::from_str(&theme_tags)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(12, rusqlite::types::Type::Text, Box::new(e)))?;
    Ok(SessionRecord {
        session_uuid: r.get(0)?,
        jsonl_path: PathBuf::from(jsonl_path),
        jsonl_sha256: r.get(2)?,
        repo_slug: r.get(3)?,
        repo_path: repo_path.map(PathBuf::from),
        cwd: cwd.map(PathBuf::from),
        started_at: parse_rfc3339(&started_at, 6)?,
        ended_at: parse_rfc3339(&ended_at, 7)?,
        design_doc_files,
        skill_invocations,
        interaction_normalized: r.get(10)?,
        summary_one_line: r.get(11)?,
        theme_tags,
        design_doc_focus: design_doc_focus.map(PathBuf::from),
        is_orphan: is_orphan != 0,
        classified_at: parse_rfc3339(&classified_at, 15)?,
        classifier_model: r.get(16)?,
    })
}

fn parse_rfc3339(s: &str, col: usize) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(e)))
}
