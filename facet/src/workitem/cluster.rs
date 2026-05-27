//! Per-session clustering. Runs the cluster LLM against a digest of one
//! session's NEW turns, parses the resulting `assignments` list, and
//! persists the cluster decisions atomically per session.

use chrono::Utc;
use eyre::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{Assignment, AssignmentKind, derive_slug};
use crate::config::Config;
use crate::fabric::{FabricCaller, request};
use crate::jsonl::{ContentBlock, Turn};
use crate::ledger::Ledger;
use crate::scan::FacetSession;

/// LLM output shape. Wrapped in `assignments:` so the YAML body is one
/// top-level mapping.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct ClusterOutput {
    assignments: Vec<Assignment>,
}

/// Cluster one session's new turns. Persists work-items, contributions,
/// session repo affinity, and cluster_assignments rows in a single
/// per-session transaction. Advances `sessions.last_cluster_offset`
/// only on success. On LLM failure the function returns Err and the
/// caller bumps the session's `failure_count`; the offset stays where
/// it was so the next tick retries the same range.
pub async fn cluster_new_turns(
    session: &FacetSession,
    config: &Config,
    ledger: &Ledger,
    fabric: &dyn FabricCaller,
) -> Result<Vec<Assignment>> {
    log::debug!(
        "cluster_new_turns: session_uuid={} cwd={} new_turns={}",
        session.session_uuid,
        session.cwd.display(),
        session.parsed.turns.len()
    );

    // 1. Build the YAML digest the cluster LLM consumes.
    let known = known_workitems(ledger, session.repo_slug.as_deref())?;
    let digest = build_digest(session, &known);

    // 2. Call the cluster LLM.
    let req = request(
        "facet-cluster",
        digest,
        &config.llm.cluster_model,
        config.llm.timeout_secs,
    );
    let raw = fabric.call(req).await.context("cluster LLM call")?;
    let body = crate::yaml_out::strip_fences(&raw);
    let parsed: ClusterOutput = serde_yaml::from_str(body).with_context(|| {
        let preview: String = body.chars().take(240).collect();
        format!(
            "parse cluster YAML output (got {} bytes); preview: {preview:?}",
            raw.len()
        )
    })?;
    if parsed.assignments.is_empty() {
        eyre::bail!("cluster LLM returned no assignments");
    }

    // 3. Persist atomically per session inside one SQLite transaction.
    //    If any insert fails, the whole batch rolls back so the ledger
    //    never enters split-brain (Architect round 1).
    let now = Utc::now();
    let cluster_model = config.llm.cluster_model.clone();
    let session_uuid = session.session_uuid.clone();
    let cwd_str = session.cwd.to_string_lossy().to_string();
    let repo_slug_opt = session.repo_slug.clone();
    let end_offset = session.parsed.end_byte_offset;
    let last_turn_uuid = session.parsed.turns.last().map(|t| t.uuid.clone());
    let assignments = parsed.assignments.clone();

    let new_workitems: Vec<(String, String)> = ledger.with_tx(|tx| {
        let mut new_items: Vec<(String, String)> = Vec::new();
        tx_upsert_session(tx, &session_uuid, &cwd_str, repo_slug_opt.as_deref(), now)?;
        for a in &assignments {
            let (workitem_id, freshly_created) = match &a.kind {
                AssignmentKind::Existing { slug } => match tx_workitem_id_by_slug(tx, slug)? {
                    Some(id) => (id, None),
                    None => {
                        log::warn!(
                            "cluster_new_turns: LLM returned existing slug {slug} but it is not in the ledger; treating as new"
                        );
                        let (id, slug_out) = tx_create_workitem_with_unique_slug(tx, slug, slug, now)?;
                        (id, Some((slug_out, slug.clone())))
                    }
                },
                AssignmentKind::New { title } => {
                    let base = derive_slug(title);
                    let (id, slug_out) = tx_create_workitem_with_unique_slug(tx, &base, title, now)?;
                    (id, Some((slug_out, title.clone())))
                }
            };
            if let Some((slug, title)) = freshly_created {
                new_items.push((slug, title));
            }
            if let Some(slug) = repo_slug_opt.as_deref() {
                tx_link_workitem_repo(tx, workitem_id, slug)?;
            }
            tx_record_contribution(tx, &session_uuid, workitem_id, now)?;
            tx_insert_cluster_assignment(
                tx,
                &session_uuid,
                workitem_id,
                &a.first_turn_uuid,
                &a.last_turn_uuid,
                now,
                &cluster_model,
            )?;
        }
        tx_set_cluster_offset(tx, &session_uuid, end_offset, last_turn_uuid.as_deref())?;
        Ok(new_items)
    })?;
    // Notifications fire AFTER commit so a rollback never produces a
    // ghost notification for a work-item that never landed.
    for (slug, title) in &new_workitems {
        crate::notify::on_new_workitem(&config.notify, slug, title);
    }
    Ok(parsed.assignments)
}

// -- transaction-scoped persist helpers ---------------------------------
// Mirror the Ledger::* methods but accept &rusqlite::Transaction so the
// whole batch lands inside one BEGIN/COMMIT.

fn tx_upsert_session(
    tx: &rusqlite::Transaction<'_>,
    session_uuid: &str,
    cwd: &str,
    repo_slug: Option<&str>,
    seen_at: chrono::DateTime<Utc>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO sessions(session_uuid, cwd, repo_slug, first_seen_at, last_seen_at) \
         VALUES (?1, ?2, ?3, ?4, ?4) \
         ON CONFLICT(session_uuid) DO UPDATE SET \
            cwd = excluded.cwd, \
            repo_slug = excluded.repo_slug, \
            last_seen_at = excluded.last_seen_at",
        rusqlite::params![session_uuid, cwd, repo_slug, seen_at.to_rfc3339()],
    )
    .context("tx_upsert_session")?;
    Ok(())
}

fn tx_workitem_id_by_slug(tx: &rusqlite::Transaction<'_>, slug: &str) -> Result<Option<i64>> {
    use rusqlite::OptionalExtension;
    tx.query_row(
        "SELECT id FROM work_items WHERE slug = ?1",
        rusqlite::params![slug],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .context("tx_workitem_id_by_slug")
}

fn tx_insert_workitem(
    tx: &rusqlite::Transaction<'_>,
    slug: &str,
    title: &str,
    created_at: chrono::DateTime<Utc>,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO work_items(slug, title, status, created_at, updated_at) \
         VALUES (?1, ?2, 'active', ?3, ?3)",
        rusqlite::params![slug, title, created_at.to_rfc3339()],
    )
    .context("tx_insert_workitem")?;
    Ok(tx.last_insert_rowid())
}

fn tx_link_workitem_repo(tx: &rusqlite::Transaction<'_>, workitem_id: i64, repo_slug: &str) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO work_item_repos(workitem_id, repo_slug) VALUES (?1, ?2)",
        rusqlite::params![workitem_id, repo_slug],
    )
    .context("tx_link_workitem_repo")?;
    Ok(())
}

fn tx_record_contribution(
    tx: &rusqlite::Transaction<'_>,
    session_uuid: &str,
    workitem_id: i64,
    at: chrono::DateTime<Utc>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO session_workitem(session_uuid, workitem_id, first_contribution_at, last_contribution_at) \
         VALUES (?1, ?2, ?3, ?3) \
         ON CONFLICT(session_uuid, workitem_id) DO UPDATE SET \
            last_contribution_at = excluded.last_contribution_at",
        rusqlite::params![session_uuid, workitem_id, at.to_rfc3339()],
    )
    .context("tx_record_contribution")?;
    Ok(())
}

fn tx_insert_cluster_assignment(
    tx: &rusqlite::Transaction<'_>,
    session_uuid: &str,
    workitem_id: i64,
    first_turn_uuid: &str,
    last_turn_uuid: &str,
    clustered_at: chrono::DateTime<Utc>,
    cluster_model: &str,
) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO cluster_assignments \
            (session_uuid, workitem_id, first_turn_uuid, last_turn_uuid, clustered_at, cluster_model, extracted) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        rusqlite::params![
            session_uuid,
            workitem_id,
            first_turn_uuid,
            last_turn_uuid,
            clustered_at.to_rfc3339(),
            cluster_model,
        ],
    )
    .context("tx_insert_cluster_assignment")?;
    Ok(())
}

fn tx_set_cluster_offset(
    tx: &rusqlite::Transaction<'_>,
    session_uuid: &str,
    offset: u64,
    last_turn_uuid: Option<&str>,
) -> Result<()> {
    tx.execute(
        "UPDATE sessions SET last_cluster_offset = ?2, last_cluster_turn_uuid = ?3 WHERE session_uuid = ?1",
        rusqlite::params![session_uuid, offset as i64, last_turn_uuid],
    )
    .context("tx_set_cluster_offset")?;
    Ok(())
}

/// Insert a new work-item inside a transaction with auto-suffixing on
/// UNIQUE(slug) violation. Returns `(workitem_id, final_slug)` so the
/// caller can fire a post-commit notification with the slug that
/// actually landed (auto-suffixed clashes mean the caller's `base_slug`
/// is not necessarily the row's slug).
fn tx_create_workitem_with_unique_slug(
    tx: &rusqlite::Transaction<'_>,
    base_slug: &str,
    title: &str,
    created_at: chrono::DateTime<Utc>,
) -> Result<(i64, String)> {
    let mut candidate = derive_slug(base_slug);
    if candidate.is_empty() {
        candidate = "untitled".to_string();
    }
    for n in 1..=50 {
        let attempt = if n == 1 { candidate.clone() } else { format!("{candidate}-{n}") };
        match tx_insert_workitem(tx, &attempt, title, created_at) {
            Ok(id) => return Ok((id, attempt)),
            Err(e) => {
                if is_unique_violation(&e) {
                    continue;
                }
                return Err(e);
            }
        }
    }
    eyre::bail!(
        "could not allocate a free slug after 50 attempts for base {base_slug:?}; archived-slug collision storm?"
    )
}

/// Build the YAML digest the cluster pattern consumes. Truncates each
/// turn's preview to 200 chars and keeps only the most-distinguishing
/// signal per content block.
fn build_digest(session: &FacetSession, known: &[(String, String, Vec<String>)]) -> String {
    let mut s = String::new();
    s.push_str("known_workitems:\n");
    if known.is_empty() {
        s.push_str("  []\n");
    } else {
        for (slug, title, repos) in known {
            s.push_str(&format!("  - slug: {}\n", yaml_str(slug)));
            s.push_str(&format!("    title: {}\n", yaml_str(title)));
            s.push_str("    repos:\n");
            for r in repos {
                s.push_str(&format!("      - {}\n", yaml_str(r)));
            }
        }
    }
    s.push_str("turns:\n");
    for t in &session.parsed.turns {
        s.push_str(&format!("  - uuid: {}\n", yaml_str(&t.uuid)));
        s.push_str(&format!(
            "    parent_uuid: {}\n",
            match &t.parent_uuid {
                Some(p) => yaml_str(p),
                None => "null".to_string(),
            }
        ));
        s.push_str(&format!("    role: {}\n", role_str(t)));
        s.push_str(&format!("    timestamp: {}\n", t.timestamp.to_rfc3339()));
        s.push_str(&format!("    preview: {}\n", yaml_str(&turn_preview(t))));
        match &session.repo_slug {
            Some(r) => s.push_str(&format!("    repo_slug: {}\n", yaml_str(r))),
            None => s.push_str("    repo_slug: null\n"),
        }
    }
    s
}

fn role_str(t: &Turn) -> &'static str {
    match t.role {
        crate::jsonl::Role::User => "user",
        crate::jsonl::Role::Assistant => "assistant",
    }
}

fn turn_preview(t: &Turn) -> String {
    let mut buf = String::new();
    for block in &t.content {
        match block {
            ContentBlock::Text { text } | ContentBlock::Thinking { text } => {
                buf.push_str(text);
                buf.push(' ');
            }
            ContentBlock::ToolUse { name, .. } => {
                buf.push_str(&format!("[tool_use:{name}] "));
            }
            ContentBlock::ToolResult { content, is_error, .. } => {
                let tag = if *is_error { "tool_result_err" } else { "tool_result" };
                buf.push_str(&format!("[{tag}] "));
                buf.push_str(content);
                buf.push(' ');
            }
            ContentBlock::Image { .. } => buf.push_str("[image] "),
            ContentBlock::Unknown { kind } => buf.push_str(&format!("[?{kind}] ")),
        }
        if buf.len() > 200 {
            break;
        }
    }
    let trimmed = buf.trim();
    if trimmed.len() > 200 {
        let mut end = 200;
        while !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &trimmed[..end])
    } else {
        trimmed.to_string()
    }
}

fn yaml_str(s: &str) -> String {
    // Use serde_yaml to escape strings safely. Strip the trailing newline.
    serde_yaml::to_string(s)
        .unwrap_or_else(|_| format!("{s:?}"))
        .trim_end()
        .to_string()
}

/// Look up active+dormant work-items in the same repo so the cluster LLM
/// can attach new turns to existing identity. Empty `repo_slug` returns
/// no candidates (a fresh-cwd session always starts new work-items).
/// Candidate work-items the LLM is allowed to extend.
///
/// Returns up to `KNOWN_WORKITEMS_LIMIT` work-items in this priority
/// order: same-repo first (when `repo_slug` is set), then any-repo
/// most-recently-updated. Cross-repo visibility is intentional: a
/// concept that spans repos must not spawn a duplicate workitem just
/// because the second session happens to be clustered from a different
/// cwd.
fn known_workitems(ledger: &Ledger, repo_slug: Option<&str>) -> Result<Vec<(String, String, Vec<String>)>> {
    const KNOWN_WORKITEMS_LIMIT: i64 = 40;
    ledger.with_conn(|c| {
        // Two-stage select: same-repo block first, then a recency-ranked
        // fill from the rest. Done as a UNION ALL inside a CTE so
        // SQLite produces the priority order with one query plan.
        let sql = "\
            WITH same_repo AS ( \
              SELECT DISTINCT w.id, w.slug, w.title, 0 AS rank, w.updated_at \
              FROM work_items w \
              JOIN work_item_repos r ON r.workitem_id = w.id \
              WHERE r.repo_slug = COALESCE(?1, '') AND w.status IN ('active', 'dormant') \
            ), \
            other_repo AS ( \
              SELECT w.id, w.slug, w.title, 1 AS rank, w.updated_at \
              FROM work_items w \
              WHERE w.status IN ('active', 'dormant') \
                AND w.id NOT IN (SELECT id FROM same_repo) \
            ) \
            SELECT id, slug, title FROM ( \
              SELECT * FROM same_repo \
              UNION ALL \
              SELECT * FROM other_repo \
            ) ORDER BY rank ASC, updated_at DESC LIMIT ?2";
        let mut stmt = c.prepare(sql).context("prep known_workitems")?;
        let rows = stmt
            .query_map(rusqlite::params![repo_slug, KNOWN_WORKITEMS_LIMIT], |row| {
                let id: i64 = row.get(0)?;
                let slug: String = row.get(1)?;
                let title: String = row.get(2)?;
                Ok((id, slug, title))
            })
            .context("query known_workitems")?;
        let mut out = Vec::new();
        for r in rows {
            let (id, slug, title) = r.context("known_workitems row")?;
            let mut repos_stmt = c
                .prepare_cached("SELECT repo_slug FROM work_item_repos WHERE workitem_id = ?1 ORDER BY repo_slug")
                .context("prep workitem_repos")?;
            let repos_iter = repos_stmt
                .query_map(rusqlite::params![id], |row| row.get::<_, String>(0))
                .context("query workitem_repos")?;
            let mut repos = Vec::new();
            for r in repos_iter {
                repos.push(r.context("workitem_repos row")?);
            }
            out.push((slug, title, repos));
        }
        Ok(out)
    })
}

/// Walk the eyre error chain looking for the rusqlite UNIQUE violation
/// SQLite raises when an insert tries to clash on `work_items.slug`.
fn is_unique_violation(e: &eyre::Report) -> bool {
    for cause in e.chain() {
        if let Some(sqlite) = cause.downcast_ref::<rusqlite::Error>()
            && let rusqlite::Error::SqliteFailure(err, _) = sqlite
            && err.code == rusqlite::ErrorCode::ConstraintViolation
        {
            return true;
        }
        let s = cause.to_string();
        if s.contains("UNIQUE") || s.contains("constraint violation") || s.contains("constraint failed") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests;
