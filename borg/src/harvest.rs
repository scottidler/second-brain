//! `sb borg harvest`: the pull-based ingestion source that reads clyde's
//! versioned session-export contract, selects the sessions worth remembering,
//! clusters them into thread notes, and manages the watermark / durable
//! identity that keeps the loop idempotent (design doc
//! `docs/design/2026-07-17-harvest-clyde-sessions.md`).
//!
//! Phase 3 scope: export reader, selection gate, thread clustering, watermark +
//! re-appearance decisions, and the reject path (a `rejection.yml` + a
//! `rejected` receipts row keyed by a selection-time trace). It does NOT
//! distill or publish - that is Phases 4-5. Where a re-appearance decision
//! needs the input body hash, fetching the body IS in scope (it is the identity
//! anchor); distillation is not.
//!
//! Module map:
//! - [`contract`] - the clyde export JSON types + the loud `parse_export` gate
//! - [`reader`] - the ONE coupling surface (shells out to clyde)
//! - [`select`] - the selection gate (`fn -> Result<(), RejectionRecord>`)
//! - [`cluster`] - deterministic `(cwd, git-branch) + gap` thread clustering
//! - [`watermark`] - the state file, exclusive lock, and re-appearance logic
//! - [`publish`] - Phase 5: fetch bodies + door capture + pipeline dispatch
//!   for every publishable `ThreadDecision`, then the post-publish watermark
//!   update

pub mod cluster;
pub mod contract;
pub mod publish;
pub mod reader;
pub mod select;
pub mod timer;
pub mod watermark;

use std::collections::BTreeMap;

use chrono::Duration;
use eyre::{Context, Result};
use rusqlite::Connection;

use crate::config::HarvestConfig;
use crate::receipts;
use crate::stages::artifact::{ArtifactStore, FsArtifactStore};
use crate::trace;
use crate::types::{IngestMethod, RejectionRecord};
use vault::receipts::ReceiptKind;

use cluster::{Thread, cluster_threads};
use contract::{SessionExport, SessionRecord};
use reader::ExportReader;
use select::{SelectionConfig, evaluate_selection};
use watermark::{PublishedEntry, Reappearance, WatermarkState, classify_reappearance, needs_body_fetch};

/// Runtime knobs for one harvest planning pass, resolved from `HarvestConfig`
/// (+ the per-invocation `--force`). Compiles the exclusion regexes and parses
/// the thread-window span ONCE, up front, so a malformed value fails loudly
/// before any clyde call rather than mid-loop.
#[derive(Debug)]
pub struct HarvestOpts {
    pub selection: SelectionConfig,
    pub thread_window: Duration,
    pub force: bool,
}

impl HarvestOpts {
    pub fn from_config(config: &HarvestConfig, force: bool) -> Result<Self> {
        log::debug!(
            "harvest::HarvestOpts::from_config: min_msgs={} thread_window={:?} force={force}",
            config.min_msgs,
            config.thread_window
        );
        let selection = SelectionConfig::compile(config.min_msgs, &config.exclude_patterns)?;
        let std_window = humantime::parse_duration(&config.thread_window).with_context(|| {
            format!(
                "harvest.thread-window {:?} is not a valid span (e.g. 2h, 90m)",
                config.thread_window
            )
        })?;
        let thread_window = Duration::from_std(std_window)
            .with_context(|| format!("harvest.thread-window {:?} out of range", config.thread_window))?;
        Ok(Self {
            selection,
            thread_window,
            force,
        })
    }
}

/// A per-thread decision: what harvest WOULD do with this thread this run.
/// Phase 5 consumes it to fetch bodies, distill, and publish (for `NewNote` /
/// `FollowUp`).
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadDecision {
    /// The note's staging trace (the primary session's selection-time trace).
    pub trace_id: String,
    /// The primary (most-messages) session id - anchors `source:` + watermark.
    pub primary_id: String,
    /// Every member session id, in `created` order.
    pub member_ids: Vec<String>,
    /// Sum of member `n-msgs` - the watermark identity signal.
    pub total_msgs: i64,
    /// New note / follow-up / skip (with an optional in-place snapshot advance).
    pub decision: Reappearance,
    /// Full bulk-metadata records for every member (repo, scope, title,
    /// duration, redaction-count, dates), in `created` order. Phase 5 needs
    /// these for `SessionMetadata`, the note's frontmatter (`repo:`,
    /// `scope-*`/`redacted-source` tags), and the thread footer, without
    /// re-deriving them from `member_ids`. Carries no `body` (bulk metadata
    /// only) - Phase 5 fetches transcript bodies separately.
    pub members: Vec<SessionRecord>,
}

/// A declined candidate: its selection-time trace and the full rejection
/// record (which becomes the `rejection.yml` and keys the `rejected` receipts
/// row).
#[derive(Debug, Clone, PartialEq)]
pub struct RejectionOutcome {
    pub session_id: String,
    pub trace_id: String,
    pub record: RejectionRecord,
}

/// The full plan for one harvest run. Side-effect free to compute: writing the
/// reject artifacts and advancing the state are separate, explicit steps
/// ([`write_rejections`], [`apply_plan_to_state`]).
#[derive(Debug, Clone, PartialEq)]
pub struct HarvestPlan {
    pub threads: Vec<ThreadDecision>,
    pub rejections: Vec<RejectionOutcome>,
    /// The export cursor this run consumed - the watermark advances to it.
    pub new_cursor: i64,
}

impl HarvestPlan {
    /// Threads that would land a note this run (new or follow-up).
    pub fn publishable(&self) -> impl Iterator<Item = &ThreadDecision> {
        self.threads.iter().filter(|t| !t.decision.is_skip())
    }
}

/// Plan one harvest run over an already-fetched bulk export. Generates a trace
/// per candidate at selection time (before any body fetch), runs the selection
/// gate, clusters survivors, and resolves each thread's re-appearance against
/// the watermark - fetching a body via `reader` ONLY on the deep-check path
/// (published id whose `n-msgs` changed). Pure with respect to disk: it neither
/// writes receipts/rejections nor saves state.
pub async fn plan_harvest<R: ExportReader>(
    reader: &R,
    export: SessionExport,
    opts: &HarvestOpts,
    state: &WatermarkState,
) -> Result<HarvestPlan> {
    log::debug!(
        "harvest::plan_harvest: cursor={} sessions={} force={}",
        export.cursor,
        export.sessions.len(),
        opts.force
    );

    let new_cursor = export.cursor;
    let mut rejections: Vec<RejectionOutcome> = Vec::new();
    let mut selected: Vec<SessionRecord> = Vec::new();
    let mut trace_by_id: BTreeMap<String, String> = BTreeMap::new();

    for record in export.sessions {
        // Trace generated at selection time (before any body fetch) so a
        // rejected candidate has a receipts key and a rejection.yml home.
        let trace_id = trace::generate(IngestMethod::Harvest);
        match evaluate_selection(&record, &opts.selection, &trace_id) {
            Ok(()) => {
                log::trace!(
                    "harvest::plan_harvest: selected session={} trace={trace_id}",
                    record.session_id
                );
                trace_by_id.insert(record.session_id.clone(), trace_id);
                selected.push(record);
            }
            Err(rejection) => {
                log::trace!(
                    "harvest::plan_harvest: rejected session={} trace={trace_id}",
                    record.session_id
                );
                rejections.push(RejectionOutcome {
                    session_id: record.session_id,
                    trace_id,
                    record: *rejection,
                });
            }
        }
    }

    let threads = cluster_threads(&selected, opts.thread_window)?;

    let mut decisions: Vec<ThreadDecision> = Vec::with_capacity(threads.len());
    for thread in &threads {
        let decision = decide_thread(reader, thread, state, opts.force).await?;
        let primary = thread.primary();
        let trace_id = trace_by_id
            .get(&primary.session_id)
            .cloned()
            .unwrap_or_else(|| trace::generate(IngestMethod::Harvest));
        decisions.push(ThreadDecision {
            trace_id,
            primary_id: primary.session_id.clone(),
            member_ids: thread.member_ids(),
            total_msgs: thread.total_msgs(),
            decision,
            members: thread.members.clone(),
        });
    }

    log::debug!(
        "harvest::plan_harvest: threads={} rejections={} new_cursor={new_cursor}",
        decisions.len(),
        rejections.len()
    );
    Ok(HarvestPlan {
        threads: decisions,
        rejections,
        new_cursor,
    })
}

/// Resolve one thread's re-appearance against the watermark. The cheap filter
/// (`n-msgs` unchanged) short-circuits without a body fetch; only a changed
/// `n-msgs` on a published id triggers the body fetch + hash.
async fn decide_thread<R: ExportReader>(
    reader: &R,
    thread: &Thread,
    state: &WatermarkState,
    force: bool,
) -> Result<Reappearance> {
    let primary_id = &thread.primary().session_id;
    let prior = state.published.get(primary_id);
    let total_msgs = thread.total_msgs();
    log::debug!(
        "harvest::decide_thread: primary={primary_id} members={} total_msgs={total_msgs} published={} force={force}",
        thread.members.len(),
        prior.is_some()
    );

    let fresh_hash = if needs_body_fetch(prior, total_msgs, force) {
        Some(fetch_thread_hash(reader, thread).await?)
    } else {
        None
    };
    Ok(classify_reappearance(prior, total_msgs, fresh_hash.as_deref(), force))
}

/// Fetch every member's body via the reader, render the canonical thread body,
/// and hash it. This is the identity anchor for the deep re-appearance check.
async fn fetch_thread_hash<R: ExportReader>(reader: &R, thread: &Thread) -> Result<String> {
    log::debug!(
        "harvest::fetch_thread_hash: primary={} members={}",
        thread.primary().session_id,
        thread.members.len()
    );
    let mut member_bodies: Vec<(String, Vec<contract::BodyMessage>)> = Vec::with_capacity(thread.members.len());
    for member in &thread.members {
        let full = reader.export_with_body(&member.session_id).await?;
        member_bodies.push((member.session_id.clone(), full.body.unwrap_or_default()));
    }
    let text = watermark::thread_body_text(&member_bodies);
    Ok(watermark::body_hash(&text))
}

/// Commit the reject artifacts: for each rejection, promote a `received`
/// receipts row to `rejected` (`ReceiptStatus::Rejected`, `GateId::Selection`)
/// and write its `rejection.yml`. Keyed by the selection-time trace. Both
/// writes are attempted per rejection; a failure on one is logged and does not
/// abort the rest (a reject is bookkeeping, never a reason to lose the run).
pub fn write_rejections(store: &FsArtifactStore, conn: &Connection, rejections: &[RejectionOutcome]) -> Result<()> {
    log::debug!("harvest::write_rejections: count={}", rejections.len());
    for rej in rejections {
        let raw_input = rej.record.source.clone().unwrap_or_else(|| rej.session_id.clone());
        // received -> rejected: the row must exist in 'received' before
        // mark_rejected can transition it (the state machine's guard).
        if let Err(e) = receipts::record_received(
            conn,
            &rej.trace_id,
            IngestMethod::Harvest,
            ReceiptKind::Session,
            &raw_input,
        ) {
            log::error!(
                "harvest::write_rejections: record_received failed trace={}: {e:#}",
                rej.trace_id
            );
            continue;
        }
        if let Err(e) = receipts::mark_rejected(conn, &rej.trace_id, &rej.record.reason) {
            log::error!(
                "harvest::write_rejections: mark_rejected failed trace={}: {e:#}",
                rej.trace_id
            );
        }
        if let Err(e) = store.write_rejection(&rej.trace_id, &rej.record) {
            log::error!(
                "harvest::write_rejections: rejection.yml write failed trace={}: {e:#}",
                rej.trace_id
            );
        }
    }
    Ok(())
}

/// Advance the watermark for one committed run: bump the cursor and apply any
/// in-place snapshot advances from `Skip` decisions (an unchanged re-appear
/// whose `n-msgs` grew). `NewNote` / `FollowUp` snapshots are written by Phase
/// 5 AFTER publish (they need the landed note path), so this step does not
/// touch them.
pub fn apply_plan_to_state(mut state: WatermarkState, plan: &HarvestPlan) -> WatermarkState {
    log::debug!(
        "harvest::apply_plan_to_state: new_cursor={} threads={}",
        plan.new_cursor,
        plan.threads.len()
    );
    state.cursor = Some(plan.new_cursor);
    for thread in &plan.threads {
        if let Reappearance::Skip {
            snapshot_update: Some(entry),
        } = &thread.decision
        {
            log::trace!(
                "harvest::apply_plan_to_state: advancing snapshot for {} n_msgs={}",
                thread.primary_id,
                entry.n_msgs
            );
            state.published.insert(thread.primary_id.clone(), entry.clone());
        }
    }
    state
}

/// Convenience: record a freshly published thread's snapshot (Phase 5 wiring
/// seam). Kept here so the published-entry shape lives with the watermark
/// logic rather than being reconstructed in the pipeline.
pub fn record_published(
    state: &mut WatermarkState,
    primary_id: &str,
    note_path: &str,
    total_msgs: i64,
    body_hash: &str,
) {
    log::debug!("harvest::record_published: primary={primary_id} note_path={note_path} n_msgs={total_msgs}");
    state.published.insert(
        primary_id.to_string(),
        PublishedEntry {
            note_path: note_path.to_string(),
            n_msgs: total_msgs,
            body_hash: body_hash.to_string(),
        },
    );
}

/// Staged filename holding a session thread's member records, so
/// `replay --from-stage 2` can re-derive the note from the staged transcript
/// without re-fetching from clyde. This is the concrete "thread export
/// metadata" staged artifact the Data Model calls for (Phase 5 staged only a
/// generic envelope; Phase 7 adds the thread-specific records).
pub const SESSION_REPLAY_META_FILE: &str = "members.yml";

/// The thread-level metadata staged alongside `body.txt`, sufficient to
/// reconstruct [`crate::pipeline::session::process_session`]'s inputs on a
/// stage-2 replay. Full `SessionRecord`s (not just the `SessionPayload` in
/// `distilled.yml`) because the publish path derives scope + redaction tags
/// and the thread footer from them.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionReplayMeta {
    pub members: Vec<SessionRecord>,
    pub primary_id: String,
    #[serde(default)]
    pub body_truncated: bool,
}

/// The outcome of one harvest run, returned to the CLI/timer for display.
/// Libraries return typed data; only `sb` prints (borg house rule).
#[derive(Debug)]
pub struct HarvestReport {
    /// Whether this was a dry run (nothing written: no notes, no receipts
    /// mutations, no reject artifacts, no watermark advance).
    pub dry_run: bool,
    /// The computed plan (selections, rejections, and the run's cursor).
    pub plan: HarvestPlan,
    /// One publish outcome per publishable thread. Empty on a dry run.
    pub outcomes: Vec<publish::PublishOutcome>,
}

/// The one core harvest run, shared by `sb borg harvest` and the nightly timer
/// ("on-demand and scheduled share one core", design doc Architecture).
/// Acquires the exclusive state lock (loud [`watermark::HarvestLockHeld`] on
/// contention so a timer run and a hand-run never race the cursor), resolves
/// the cursor/since window, fetches the bulk export, plans, and - unless
/// `dry_run` - writes reject artifacts, publishes every publishable thread,
/// and advances the watermark atomically.
///
/// Window precedence (design doc): an explicit `since` (deliberate backfill)
/// wins; else a stored cursor (steady state); else the first-run
/// `harvest.initial-since`. `limit` caps the clyde export page - lossless
/// because clyde's paging is gap-free, so the cursor advances only over the
/// returned rows.
pub async fn run(
    config: &crate::config::Config,
    since: Option<String>,
    limit: Option<usize>,
    force: bool,
    dry_run: bool,
) -> Result<HarvestReport> {
    let state_path = vault::paths::borg_harvest_state();
    let reader = reader::ClydeExportReader::new(config.harvest.clyde_binary.clone());
    run_with(&reader, config, &state_path, since, limit, force, dry_run).await
}

/// The injectable core behind [`run`] (generic over the reader + explicit
/// state path) so it can be driven with a fake reader and a temp state path in
/// tests. `run` supplies the production `ClydeExportReader` and the
/// `vault::paths` state path.
pub async fn run_with<R: ExportReader>(
    reader: &R,
    config: &crate::config::Config,
    state_path: &std::path::Path,
    since: Option<String>,
    limit: Option<usize>,
    force: bool,
    dry_run: bool,
) -> Result<HarvestReport> {
    log::debug!(
        "harvest::run_with: state_path={} since={:?} limit={:?} force={force} dry_run={dry_run}",
        state_path.display(),
        since,
        limit
    );
    // Exclusive lock held for the whole run (RAII drop on return / process
    // exit). A second concurrent invocation fails loudly rather than racing.
    let _lock = watermark::acquire_lock(state_path)?;
    let state = WatermarkState::load(state_path)?;

    let (cursor, since_arg) = if let Some(s) = since.as_deref() {
        (None, Some(s.to_string()))
    } else if let Some(c) = state.cursor {
        (Some(c), None)
    } else {
        (None, Some(config.harvest.initial_since.clone()))
    };
    log::debug!("harvest::run: fetching bulk export cursor={cursor:?} since={since_arg:?} limit={limit:?}");
    let export = reader.export_bulk(cursor, since_arg.as_deref(), limit).await?;

    let opts = HarvestOpts::from_config(&config.harvest, force)?;
    let plan = plan_harvest(reader, export, &opts, &state).await?;

    if dry_run {
        log::info!(
            "harvest::run: dry-run - {} publishable / {} rejected (no writes)",
            plan.publishable().count(),
            plan.rejections.len()
        );
        return Ok(HarvestReport {
            dry_run: true,
            plan,
            outcomes: Vec::new(),
        });
    }

    // Live: reject artifacts first (rejection.yml + `rejected` receipts row),
    // then publish, then advance the watermark. publish_plan writes the
    // NewNote/FollowUp published snapshots; apply_plan_to_state bumps the
    // cursor and applies Skip snapshot advances.
    let store = FsArtifactStore::from_config(&config.staging);
    let conn = receipts::open_default()?;
    write_rejections(&store, &conn, &plan.rejections)?;

    let (state, outcomes) = publish::publish_plan(reader, config, &plan, state).await;
    let state = apply_plan_to_state(state, &plan);
    state.save(state_path)?;

    log::info!(
        "harvest::run: published {} thread(s), {} rejected, cursor -> {}",
        outcomes.len(),
        plan.rejections.len(),
        plan.new_cursor
    );
    Ok(HarvestReport {
        dry_run: false,
        plan,
        outcomes,
    })
}

#[cfg(test)]
mod tests;
