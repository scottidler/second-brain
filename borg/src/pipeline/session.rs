//! `ContentKind::Session` handler (harvest-clyde-sessions design, Phase 5).
//!
//! Mirrors `process_text`'s shape: a thin timing/error wrapper
//! (`process_session`) around the real work (`process_session_inner`), which
//! distills the harvested thread, renders the note, and publishes it via the
//! shared atomic-publish path. Unlike every other content kind, the input
//! here was already selected/clustered/fetched upstream by
//! `harvest::publish::publish_thread` - this handler's job is distill +
//! render + publish, not fetch.

use super::*;
use crate::harvest::contract::SessionRecord;
use chrono::{DateTime, FixedOffset};
use distillers::{SessionConfig, SessionMetadata};

pub(crate) async fn process_session(
    body: &str,
    members: &[SessionRecord],
    primary_id: &str,
    body_truncated: bool,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: &str,
) -> IngestResult {
    let start = Instant::now();
    match process_session_inner(
        body,
        members,
        primary_id,
        body_truncated,
        tags,
        method,
        force,
        config,
        trace_id,
    )
    .await
    {
        Ok(mut result) => {
            let elapsed = start.elapsed();
            log::info!("[{trace_id}] Session pipeline completed in {elapsed:.2?}");
            result.elapsed_secs = Some(elapsed.as_secs_f64());
            result
        }
        Err(e) => {
            let elapsed = start.elapsed();
            log::error!("[{trace_id}] Session pipeline failed in {elapsed:.2?}: {e:?}");
            IngestResult {
                status: IngestStatus::Failed {
                    reason: format!("{:#}", e),
                },
                method: Some(method),
                elapsed_secs: Some(elapsed.as_secs_f64()),
                // The body/metadata are already in hand by the time this
                // handler runs (harvest fetched them upstream), so a
                // terminal error here is a distill/publish failure, never a
                // fetch failure.
                failure_stage: Some(vault::receipts::FailureStage::PublishFailed),
                ..Default::default()
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_session_inner(
    body: &str,
    members: &[SessionRecord],
    primary_id: &str,
    body_truncated: bool,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    let primary = members.iter().find(|m| m.session_id == primary_id).ok_or_else(|| {
        eyre::eyre!(
            "process_session: primary id {primary_id} not present among {} member(s)",
            members.len()
        )
    })?;
    log::debug!(
        "process_session_inner: trace={trace_id} primary={primary_id} members={} body_len={} body_truncated={body_truncated}",
        members.len(),
        body.len()
    );

    let session_metadata = build_session_metadata(members, primary_id, body_truncated);
    let source_url = format!("clyde://{primary_id}");

    // harvest.model empty inherits llm.model (design doc: Distillation >
    // Model, the established per-feature override precedent).
    let model = if config.harvest.model.is_empty() {
        config.llm.model.clone()
    } else {
        config.harvest.model.clone()
    };
    let session_config = SessionConfig {
        model,
        max_chars: config.fabric.max_content_chars,
        timeout_secs: config.fabric.timeout_secs,
        token_cap: config.harvest.token_cap,
    };

    let distilled = crate::stages::distill::distill_for_publish_session(
        &config.fabric,
        &config.staging,
        trace_id,
        &source_url,
        body,
        &session_metadata,
        session_config,
    )
    .await;

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.extend(distilled.tags.iter().map(|t| hygiene::sanitize_tag(t)));
    finalize_tags(&mut all_tags, config).await;

    // Scope + redaction are governance signals the design mandates on EVERY
    // harvest note ("work sessions included, scope-tagged"; "a session with
    // a nonzero [redaction] count gets a redacted-source tag"). Neither is in
    // the 110-tag canonical interest vocabulary `finalize_tags` filters
    // against, so they are appended AFTER canonicalization rather than
    // risking silent drop (see implementation notes, Deviations).
    all_tags.push(if primary.scope == "work" { "scope-work" } else { "scope-personal" }.to_string());
    if members.iter().any(|m| m.redaction_count > 0) {
        all_tags.push("redacted-source".to_string());
    }
    all_tags.sort();
    all_tags.dedup();

    // Embedding policy (design doc: only the distilled note is embedded; the
    // staged transcript is trace-recallable, never embedded) - same
    // transcript-free render policy as Article/Repo/Video publish.
    let rendered_distilled = distillers::render(
        &distilled,
        distillers::RenderOptions {
            include_transcript: false,
        },
    );
    let distilled_body = if members.len() > 1 {
        // Richer per-member footer (id, title, repo, duration) for thread
        // notes (design doc: Data Model) - `SessionPayload` only carries the
        // thread-level lead line + bare `clyde://` ids, so this reads the
        // full clustered `SessionRecord`s borg's publish layer holds
        // (`distillers::render`'s `push_session_footer` doc comment defers
        // exactly this richer footer to here).
        append_distilled_below_slides(
            rendered_distilled.body_markdown.clone(),
            &render_member_details(members),
        )
    } else {
        rendered_distilled.body_markdown.clone()
    };

    // `title` is present-null in the contract; a null OR empty title falls back
    // to `Session <id>` (the null case is new in harvest-completion Phase 1;
    // the empty-string case is preserved).
    let title = match primary.title.as_deref() {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => format!("Session {primary_id}"),
    };

    let tz = config.frontmatter.timezone_tz();
    let now = chrono::Utc::now().with_timezone(&tz);
    let mut frontmatter_additions = rendered_distilled.frontmatter_additions;
    // `repo:` rides verbatim from the export contract's `repo` field
    // (present-as-null, not omitted, when the cwd has no repo anchor -
    // design doc: Data Model / Phase 9 owns validation + hub wiring; this
    // renderer only emits the field). `repos-touched:` is Phase 9's addition
    // once clyde ships files-touched.
    frontmatter_additions.insert(
        "repo".to_string(),
        match &session_metadata.repo {
            Some(r) => serde_yaml::Value::String(r.clone()),
            None => serde_yaml::Value::Null,
        },
    );
    let expires = retention::trace_expires_for(now.date_naive(), config.staging.retention_days);
    frontmatter_additions.insert("trace-expires".to_string(), serde_yaml::Value::String(expires));

    let note = NoteContent {
        title: title.clone(),
        source_url: Some(source_url.clone()),
        asset_path: None,
        tags: all_tags.clone(),
        summary: distilled.summary.clone(),
        description: None,
        capture_note: None,
        content_type: ContentType::Session,
        embed_code: None,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: Vec::new(),
        distilled_body: Some(distilled_body),
        frontmatter_additions,
        origin: Some(vault::schema::Origin::Generated),
        status: Some(vault::schema::Status::Unread),
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let filename = format!("{}.md", hygiene::sanitize_filename(&title));

    let dest_path = config.inbox_dir()?;
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = super::atomic::resolve_publish_path(&dest_path.join(&filename), force);
    vault::note::write_atomic(&note_path, rendered.as_bytes()).context("Failed to write session note to vault")?;

    log::info!(
        "[{trace_id}] Wrote session note: {} (members={})",
        note_path.display(),
        members.len()
    );

    publish_note(
        config,
        &note_path,
        method,
        source_url,
        title,
        all_tags,
        trace_id,
        distilled.meta.validation.is_degraded(),
    )
}

/// Deterministic Stage-0 metadata for the distiller (design doc: Watermark +
/// durable identity / Distillation > Input). `session_ids` puts `primary_id`
/// first (the distillers-crate doc comment's stated order), then the
/// remaining members in their `created`-order arrival - purely cosmetic
/// (the footer just lists ids), but it keeps the anchor session visibly
/// first.
fn build_session_metadata(members: &[SessionRecord], primary_id: &str, body_truncated: bool) -> SessionMetadata {
    let mut session_ids = Vec::with_capacity(members.len());
    session_ids.push(primary_id.to_string());
    for m in members {
        if m.session_id != primary_id {
            session_ids.push(m.session_id.clone());
        }
    }
    let repo = members
        .iter()
        .find(|m| m.session_id == primary_id)
        .and_then(|m| m.repo.clone());
    let total: i64 = members.iter().map(|m| m.n_msgs).sum();
    let msg_count = u32::try_from(total.max(0)).unwrap_or(u32::MAX);
    let date_start = earliest_created(members);
    let date_end = latest_modified(members);
    log::debug!(
        "build_session_metadata: primary={primary_id} members={} repo={repo:?} msg_count={msg_count} body_truncated={body_truncated}",
        members.len()
    );
    SessionMetadata {
        repo,
        session_ids,
        msg_count,
        date_start,
        date_end,
        body_truncated,
    }
}

/// Earliest `created` across members. Every member here already passed
/// through Phase 3's `cluster_threads` (which errors loudly on an
/// unparseable timestamp), so a parse failure here would mean a caller
/// bypassed that gate - logged, not panicked, and simply excluded from the
/// min/max rather than failing the whole publish.
fn earliest_created(members: &[SessionRecord]) -> Option<String> {
    let mut best: Option<(DateTime<FixedOffset>, String)> = None;
    for m in members {
        // `created` is present-null; a null value is guarded at selection, so
        // reaching here with `None` means a caller bypassed that gate - warn
        // and skip it from the min/max rather than failing the whole publish.
        let Some(created) = m.created.as_deref() else {
            log::warn!(
                "build_session_metadata: null created timestamp on session {}",
                m.session_id
            );
            continue;
        };
        match DateTime::parse_from_rfc3339(created) {
            Ok(dt) => match &best {
                Some((b, _)) if dt >= *b => {}
                _ => best = Some((dt, created.to_string())),
            },
            Err(e) => log::warn!(
                "build_session_metadata: unparseable created timestamp {created:?} on session {}: {e}",
                m.session_id
            ),
        }
    }
    best.map(|(_, s)| s)
}

/// Latest `modified` across members - see [`earliest_created`].
fn latest_modified(members: &[SessionRecord]) -> Option<String> {
    let mut best: Option<(DateTime<FixedOffset>, String)> = None;
    for m in members {
        match DateTime::parse_from_rfc3339(&m.modified) {
            Ok(dt) => match &best {
                Some((b, _)) if dt <= *b => {}
                _ => best = Some((dt, m.modified.clone())),
            },
            Err(e) => log::warn!(
                "build_session_metadata: unparseable modified timestamp {:?} on session {}: {e}",
                m.modified,
                m.session_id
            ),
        }
    }
    best.map(|(_, s)| s)
}

/// Richer per-member footer (id, title, repo, duration) for thread notes
/// (design doc: Data Model - "a footer listing member sessions (id, title,
/// repo, duration) for thread notes"). Distinct from `distillers::render`'s
/// `## Sessions` lead-line + bare id list, which is faithful to the frozen
/// `SessionPayload` alone; this reads the full `SessionRecord`s only borg's
/// publish layer holds.
fn render_member_details(members: &[SessionRecord]) -> String {
    let mut out = String::from("## Session Details\n\n");
    for m in members {
        let repo = m.repo.as_deref().unwrap_or("-");
        let duration = m
            .duration_secs
            .map(|secs| format!("{}m", (secs as f64 / 60.0).round() as i64))
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "- clyde://{} - {} - `{repo}` - {duration}\n",
            m.session_id,
            m.title.as_deref().unwrap_or("").trim()
        ));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests;
