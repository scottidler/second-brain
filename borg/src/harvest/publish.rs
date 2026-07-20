//! Phase 5: drive every publishable `ThreadDecision` from `plan_harvest`
//! through body fetch -> door capture -> pipeline dispatch -> watermark
//! update. This is the harvest-side half of the publish path (per-house
//! pattern: `trace::generate` already ran at selection time, so this module
//! picks up at `intake::record_received_with_sidecar` -> `process_content`);
//! the pipeline-side half (`ContentKind::Session` handling: distill, render,
//! atomic publish) lives in `pipeline::session`.

use eyre::{Context, Result};

use crate::config::Config;
use crate::intake;
use crate::pipeline;
use crate::stages::artifact::ArtifactStore;
use crate::types::{ContentKind, IngestMethod, IngestResult, IngestStatus};
use vault::intake::IntakeKind;
use vault::receipts::FailureStage;

use super::contract::BodyMessage;
use super::reader::ExportReader;
use super::watermark::{self, WatermarkState};
use super::{HarvestPlan, ThreadDecision};

/// Outcome of driving one thread through publish. The caller (Phase 6's CLI)
/// uses this for reporting; the mutated `WatermarkState` returned alongside
/// is the durable side effect the caller must persist.
#[derive(Debug)]
pub struct PublishOutcome {
    pub primary_id: String,
    pub trace_id: String,
    pub result: IngestResult,
}

/// Drive every publishable thread in `plan` through fetch -> pipeline dispatch
/// -> watermark update, returning the updated state alongside a per-thread
/// outcome. A single thread's failure does not abort the run - the loop
/// continues so one bad session fetch does not silently drop the rest of the
/// night's harvest (mirrors `write_rejections`'s per-item best-effort policy).
pub async fn publish_plan<R: ExportReader>(
    reader: &R,
    config: &Config,
    plan: &HarvestPlan,
    mut state: WatermarkState,
) -> (WatermarkState, Vec<PublishOutcome>) {
    let publishable: Vec<&ThreadDecision> = plan.publishable().collect();
    log::debug!("harvest::publish::publish_plan: publishable={}", publishable.len());
    let mut outcomes = Vec::with_capacity(publishable.len());
    for thread in publishable {
        outcomes.push(publish_thread(reader, config, thread, &mut state).await);
    }
    log::debug!(
        "harvest::publish::publish_plan: done outcomes={} published={}",
        outcomes.len(),
        state.published.len()
    );
    (state, outcomes)
}

/// Publish one thread, converting any error into a `Failed` outcome (and a
/// door-side receipts failure record) rather than propagating - see
/// [`publish_plan`]'s per-thread isolation policy.
async fn publish_thread<R: ExportReader>(
    reader: &R,
    config: &Config,
    thread: &ThreadDecision,
    state: &mut WatermarkState,
) -> PublishOutcome {
    log::debug!(
        "harvest::publish::publish_thread: primary={} trace={} members={}",
        thread.primary_id,
        thread.trace_id,
        thread.member_ids.len()
    );
    match publish_thread_inner(reader, config, thread, state).await {
        Ok(result) => PublishOutcome {
            primary_id: thread.primary_id.clone(),
            trace_id: thread.trace_id.clone(),
            result,
        },
        Err(e) => {
            let reason = format!("{e:#}");
            log::error!(
                "harvest::publish::publish_thread: primary={} trace={} failed: {reason}",
                thread.primary_id,
                thread.trace_id
            );
            // No door capture ran (the failure happened before or during
            // it), so upsert-then-fail rather than relying on
            // `process_content`'s terminal chokepoint, which never ran.
            intake::record_failure_at_door(
                IngestMethod::Harvest,
                &thread.trace_id,
                FailureStage::FetchFailed,
                &reason,
            );
            PublishOutcome {
                primary_id: thread.primary_id.clone(),
                trace_id: thread.trace_id.clone(),
                result: IngestResult {
                    status: IngestStatus::Failed { reason },
                    trace_id: Some(thread.trace_id.clone()),
                    method: Some(IngestMethod::Harvest),
                    failure_stage: Some(FailureStage::FetchFailed),
                    ..Default::default()
                },
            }
        }
    }
}

async fn publish_thread_inner<R: ExportReader>(
    reader: &R,
    config: &Config,
    thread: &ThreadDecision,
    state: &mut WatermarkState,
) -> Result<IngestResult> {
    // Fetch every member's parsed body. Phase 3 only fetches a body for the
    // deep re-appearance check (a `NewNote`/cheap-filter `Skip` decision has
    // none yet, and a `FollowUp`'s fetch result wasn't propagated onto
    // `ThreadDecision`), so this always re-fetches - deterministically, from
    // the same reader/ids Phase 3 already validated.
    let mut member_bodies: Vec<(String, Vec<BodyMessage>)> = Vec::with_capacity(thread.members.len());
    let mut body_truncated = false;
    for member in &thread.members {
        let fetched = reader
            .export_with_body(&member.session_id)
            .await
            .with_context(|| format!("harvest publish: fetch body for session {}", member.session_id))?;
        body_truncated |= fetched.body_truncated;
        member_bodies.push((member.session_id.clone(), fetched.body.unwrap_or_default()));
    }
    let body_text = watermark::thread_body_text(&member_bodies);
    let body_hash = watermark::body_hash(&body_text);

    // Door capture: sidecar + `received` receipts row, BEFORE dispatch
    // (`borg/AGENTS.md` invariant 1 - "every door calls
    // record_received_with_sidecar() synchronously before any pipeline
    // work"). `raw_input` is the concatenated parsed body, matching
    // `ReceiptKind::Session`'s contract ("never Text - a lying identifier").
    intake::record_received_with_sidecar(
        config,
        IngestMethod::Harvest,
        IntakeKind::Session,
        &intake::preview_text(&body_text),
        body_text.as_bytes(),
        &thread.trace_id,
    )
    .with_context(|| format!("harvest publish: door capture for trace {}", thread.trace_id))?;

    let content = ContentKind::Session {
        body: body_text,
        members: thread.members.clone(),
        primary_id: thread.primary_id.clone(),
        body_truncated,
    };
    // `force` stays false here regardless of `--force`: `--force` (design
    // doc API) means "re-distill this in-scope published id", which Phase 3
    // already expresses as a `FollowUp` decision - a brand new note, never an
    // overwrite of the prior one (notes are immutable once published). The
    // pipeline's own `force` parameter means "overwrite a same-filename
    // collision in place", a distinct and narrower concept this loop never
    // wants.
    let result = pipeline::process_content(
        content,
        Vec::new(),
        IngestMethod::Harvest,
        false,
        config,
        Some(thread.trace_id.clone()),
        None,
    )
    .await;

    // Only a landed note advances the watermark; a `Failed` result already
    // has its terminal `failed` receipts row from `process_content`'s
    // chokepoint, and must NOT be recorded as published.
    if let IngestStatus::Completed = result.status
        && let Some(note_path) = result.note_path.as_deref()
    {
        super::record_published(state, &thread.primary_id, note_path, thread.total_msgs, &body_hash);

        // Stage the thread's member records so `replay --from-stage 2` can
        // re-derive the note from the staged transcript without re-fetching
        // from clyde. Best-effort: a stage-write failure must not fail an
        // already-landed publish (the note is the durable artifact; replay is
        // a convenience).
        let store = crate::stages::artifact::FsArtifactStore::from_config(&config.staging);
        let meta = super::SessionReplayMeta {
            members: thread.members.clone(),
            primary_id: thread.primary_id.clone(),
            body_truncated,
        };
        match serde_yaml::to_string(&meta) {
            Ok(yaml) => {
                if let Err(e) =
                    store.write_attachment(&thread.trace_id, super::SESSION_REPLAY_META_FILE, yaml.as_bytes())
                {
                    log::warn!(
                        "harvest::publish: failed to stage {} for {}: {e:#}",
                        super::SESSION_REPLAY_META_FILE,
                        thread.trace_id
                    );
                }
            }
            Err(e) => log::warn!(
                "harvest::publish: serialize {} for {}: {e:#}",
                super::SESSION_REPLAY_META_FILE,
                thread.trace_id
            ),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
