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
use crate::ledger::clusters::NewClusterAssignment;
use crate::ledger::sessions::UpsertSession;
use crate::ledger::workitems::{NewWorkItem, SessionContribution};
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
    let known = known_workitems_for_repo(ledger, session.repo_slug.as_deref())?;
    let digest = build_digest(session, &known);

    // 2. Call the cluster LLM.
    let req = request(
        "facet-cluster",
        digest,
        &config.llm.cluster_model,
        config.llm.timeout_secs,
    );
    let raw = fabric.call(req).await.context("cluster LLM call")?;
    let parsed: ClusterOutput =
        serde_yaml::from_str(&raw).with_context(|| format!("parse cluster YAML output (got {} bytes)", raw.len()))?;
    if parsed.assignments.is_empty() {
        eyre::bail!("cluster LLM returned no assignments");
    }

    // 3. Persist atomically per session: ensure session row, ensure
    //    work-items exist, link repo affinity, record contributions,
    //    insert cluster_assignments rows, advance offset.
    let now = Utc::now();
    ledger
        .upsert_session(UpsertSession {
            session_uuid: &session.session_uuid,
            cwd: &session.cwd.to_string_lossy(),
            repo_slug: session.repo_slug.as_deref(),
            seen_at: now,
        })
        .context("upsert session before persist")?;
    for a in &parsed.assignments {
        let workitem_id = match &a.kind {
            AssignmentKind::Existing { slug } => match ledger.workitem_by_slug(slug)? {
                Some(w) => w.id,
                None => {
                    log::warn!(
                        "cluster_new_turns: LLM returned existing slug {slug} but it is not in the ledger; treating as new"
                    );
                    create_workitem_with_unique_slug(ledger, slug, slug, now)?
                }
            },
            AssignmentKind::New { title } => {
                let base = derive_slug(title);
                create_workitem_with_unique_slug(ledger, &base, title, now)?
            }
        };
        if let Some(slug) = session.repo_slug.as_deref() {
            ledger.link_workitem_repo(workitem_id, slug)?;
        }
        ledger.record_contribution(SessionContribution {
            session_uuid: &session.session_uuid,
            workitem_id,
            at: now,
        })?;
        ledger.insert_cluster_assignment(NewClusterAssignment {
            session_uuid: &session.session_uuid,
            workitem_id,
            first_turn_uuid: &a.first_turn_uuid,
            last_turn_uuid: &a.last_turn_uuid,
            clustered_at: now,
            cluster_model: &config.llm.cluster_model,
        })?;
    }
    ledger
        .set_cluster_offset(
            &session.session_uuid,
            session.parsed.end_byte_offset,
            session.parsed.turns.last().map(|t| t.uuid.as_str()),
        )
        .context("advance cluster offset")?;
    Ok(parsed.assignments)
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
fn known_workitems_for_repo(ledger: &Ledger, repo_slug: Option<&str>) -> Result<Vec<(String, String, Vec<String>)>> {
    let Some(repo) = repo_slug else {
        return Ok(Vec::new());
    };
    ledger.with_conn(|c| {
        let mut stmt = c
            .prepare(
                "SELECT DISTINCT w.id, w.slug, w.title \
                 FROM work_items w \
                 JOIN work_item_repos r ON r.workitem_id = w.id \
                 WHERE r.repo_slug = ?1 AND w.status IN ('active', 'dormant') \
                 ORDER BY w.updated_at DESC \
                 LIMIT 40",
            )
            .context("prep known_workitems")?;
        let rows = stmt
            .query_map(rusqlite::params![repo], |row| {
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

/// Insert a new work-item, auto-suffixing the slug on UNIQUE violation
/// so an archived (or live) slug clash deterministically produces
/// `<base>-2`, `<base>-3`, etc. The slug is frozen on creation; the
/// title can update later via a separate path.
fn create_workitem_with_unique_slug(
    ledger: &Ledger,
    base_slug: &str,
    title: &str,
    created_at: chrono::DateTime<Utc>,
) -> Result<i64> {
    let mut candidate = derive_slug(base_slug);
    if candidate.is_empty() {
        candidate = "untitled".to_string();
    }
    for n in 1..=50 {
        let attempt = if n == 1 { candidate.clone() } else { format!("{candidate}-{n}") };
        match ledger.insert_workitem(NewWorkItem {
            slug: &attempt,
            title,
            created_at,
        }) {
            Ok(id) => return Ok(id),
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
