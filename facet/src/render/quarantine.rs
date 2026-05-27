//! Quarantine notes for sessions whose cluster or extract failed.
//!
//! One quarantine note per session_uuid with `failure_count > 0`. The
//! note carries the latest failure stage + reason from the ledger plus
//! a `sb facet retry` hint. Re-rendered every tick while the session
//! remains in quarantine; removed (via `rkvr rmrf` from the caller's
//! cleanup pass) once the failure is cleared.

use std::path::{Path, PathBuf};

use eyre::Result;

use crate::ledger::sessions::SessionRow;

/// Render the quarantine markdown body for a session-with-failure.
pub fn render_body(session: &SessionRow) -> String {
    let short = short_uuid(&session.session_uuid);
    let title = format!("facet quarantine: {short}");
    let today = chrono::Utc::now().format("%Y-%m-%d");
    let stage = session.last_failure_stage.as_deref().unwrap_or("unknown");
    let reason = session.last_failure_reason.as_deref().unwrap_or("(no reason recorded)");

    let mut s = String::new();
    s.push_str("---\n");
    s.push_str(&format!("title: {title}\n"));
    s.push_str(&format!("date: {today}\n"));
    s.push_str("type: facet-quarantine\n");
    s.push_str("origin: assisted\n");
    s.push_str("method: facet\n");
    s.push_str("status: error\n");
    s.push_str("domain: ai\n");
    s.push_str(&format!("facet-session-uuid: {}\n", session.session_uuid));
    s.push_str(&format!("facet-failure-stage: {stage}\n"));
    s.push_str(&format!("facet-failure-count: {}\n", session.failure_count));
    s.push_str(&format!(
        "facet-failure-last-seen: {}\n",
        session.last_seen_at.to_rfc3339()
    ));
    if let Some(repo) = &session.repo_slug {
        s.push_str(&format!("facet-repo: {repo}\n"));
    }
    s.push_str("tags:\n  - facet\n  - quarantine\n  - error\n");
    s.push_str("---\n\n");

    s.push_str(&format!("# {title}\n\n"));
    s.push_str("## Context\n\n");
    s.push_str(&format!("- Session: `{}`\n", session.session_uuid));
    s.push_str(&format!("- CWD: `{}`\n", session.cwd));
    if let Some(repo) = &session.repo_slug {
        s.push_str(&format!("- Repo: `{repo}`\n"));
    }
    s.push_str(&format!("- Stage: `{stage}`\n"));
    s.push_str(&format!("- Failures so far: {}\n", session.failure_count));
    s.push_str(&format!(
        "- Last seen: {}\n",
        session.last_seen_at.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    s.push('\n');

    s.push_str("## Error\n\n");
    s.push_str("```text\n");
    s.push_str(reason);
    if !reason.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("```\n\n");

    s.push_str("## Retry\n\n");
    s.push_str("```sh\n");
    s.push_str(&format!("sb facet retry {}\n", session.session_uuid));
    s.push_str("```\n\n");
    s.push_str(
        "Rewinds this session's cluster offset so the next tick reprocesses it from byte 0. \
        Useful when the upstream LLM has improved or the parser has been hardened.\n",
    );
    s
}

/// Render and write a quarantine note for a session. Atomic.
pub fn render(target_path: &Path, session: &SessionRow) -> Result<()> {
    log::debug!(
        "facet::quarantine::render: target={} session_uuid={} stage={:?}",
        target_path.display(),
        session.session_uuid,
        session.last_failure_stage
    );
    let body = render_body(session);
    super::write_atomic(target_path, &body)
}

/// Compose the target file path for a session under the configured
/// quarantine directory. Uses a 12-character session prefix for
/// readability while still being collision-resistant within a vault.
pub fn target_path(vault_root: &Path, quarantine_dir: &str, session_uuid: &str) -> PathBuf {
    vault_root
        .join(quarantine_dir)
        .join(format!("{}.md", short_uuid(session_uuid)))
}

fn short_uuid(uuid: &str) -> String {
    uuid.chars().take(12).collect()
}
