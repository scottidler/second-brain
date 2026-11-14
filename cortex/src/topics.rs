//! `sb cortex topics`: the topic map writer (`docs/plan/2026-11-14-topic-map.md`).
//!
//! Three verbs over oracle's index, each under the shared embed lock:
//!
//! - [`train`]: spherical k-means over every in-scope summary vector, inside a
//!   rayon pool built for the call, swapped in as a new generation in one
//!   transaction. Fails closed while any eligible note is still pending a
//!   summary embedding: a train during a model migration would freeze
//!   centroids built from the newest notes only.
//! - [`assign`]: give each in-scope note with no topic (or one assigned from an
//!   older embedding) the topic of its nearest frozen centroid, and drop the
//!   rows of notes that left scope. Centroids never move.
//! - [`clear`]: empty the tables, keep the generation counter.
//!
//! Every SQL statement lives in `vault::search::topic`; this module decides.

use std::collections::{BTreeSet, HashSet};

use eyre::{Result, WrapErr, bail, eyre};
use serde::Serialize;
use vault::kmeans::{self, KmeansError};
use vault::schema::NoteType;
use vault::search::{
    Assignment, EmbeddingKind, NewGeneration, SearchIndex, SummaryVector, TopicScope, TopicState, TopicStatus,
};

use crate::config::{Config, TopicsConfig};
use crate::opts::TopicsOpts;

/// Rows read and written per assign step. Each step is one short write
/// transaction; a tick's real load is tens of rows.
const ASSIGN_BATCH: usize = 1024;

/// What `sb cortex topics` prints, for every verb. `sb` renders it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TopicsReport {
    pub status: TopicStatus,
    /// The stored generation; `None` when there is none.
    pub generation: Option<i64>,
    /// `k` of the stored generation.
    pub k: Option<usize>,
    /// `topics.k` in config, which only `--train` reads.
    pub configured_k: usize,
    /// RFC 3339, UTC.
    pub trained_at: Option<String>,
    pub eligible: usize,
    /// Eligible notes with no active-model summary row that the embed loop
    /// still counts as a target.
    pub pending: usize,
    pub in_scope: usize,
    pub assigned: usize,
    /// Rows this run wrote: every in-scope note on `--train`, new and
    /// re-embedded notes on an assign.
    pub assigned_this_run: usize,
    /// Rows this run deleted: notes that left scope on an assign, the whole
    /// prior generation on `--train` and `--clear`.
    pub removed_this_run: usize,
    pub mean_cosine: Option<f64>,
    pub train_mean_cosine: Option<f64>,
    /// Present only when the configured `topics.exclude-types` differs from
    /// the list the stored generation was trained with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_types_drift: Option<ExcludeTypesDrift>,
    /// What to run next, when the status calls for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ExcludeTypesDrift {
    pub trained: Vec<String>,
    pub configured: Vec<String>,
}

/// `sb cortex topics` entry point: `--train`, `--clear`, or assign.
pub fn run(config: &Config, opts: &TopicsOpts) -> Result<TopicsReport> {
    log::debug!(
        "cortex::topics::run: train={} clear={} k={:?} seed={:?}",
        opts.train,
        opts.clear,
        opts.k,
        opts.seed
    );
    if opts.train && opts.clear {
        bail!("--train and --clear conflict: pass one");
    }
    if !opts.train && (opts.k.is_some() || opts.seed.is_some()) {
        bail!("--k and --seed are per-run overrides of --train and require it");
    }
    if opts.train {
        train(config, opts)
    } else if opts.clear {
        clear(config)
    } else {
        assign(config)
    }
}

/// Train a new generation from config (`opts.k` / `opts.seed` override it)
/// and replace the stored one. Writes nothing on any error.
pub fn train(config: &Config, opts: &TopicsOpts) -> Result<TopicsReport> {
    let lock = crate::embed::acquire_lock()?;
    let mut index = open_index(config)?;
    let report = train_index(&mut index, &config.topics, opts.k, opts.seed, now_secs())?;
    drop(lock);
    Ok(report)
}

/// Assign unassigned in-scope notes against the stored centroids. `Absent`
/// and `Stale` write nothing and say so in the report.
pub fn assign(config: &Config) -> Result<TopicsReport> {
    let lock = crate::embed::acquire_lock()?;
    let mut index = open_index(config)?;
    let report = assign_index(&mut index, &config.topics, now_secs())?;
    drop(lock);
    Ok(report)
}

/// Back out: empty the topic tables and keep the generation counter, so a
/// later train never reuses a number a client has seen.
pub fn clear(config: &Config) -> Result<TopicsReport> {
    let lock = crate::embed::acquire_lock()?;
    let mut index = open_index(config)?;
    let report = clear_index(&mut index, &config.topics)?;
    drop(lock);
    Ok(report)
}

fn open_index(config: &Config) -> Result<SearchIndex> {
    let db_path = config.oracle_db_path();
    log::debug!("cortex::topics: oracle db={}", db_path.display());
    SearchIndex::open(&db_path).wrap_err_with(|| format!("failed to open search index at {}", db_path.display()))
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn train_index(
    index: &mut SearchIndex,
    topics: &TopicsConfig,
    k: Option<usize>,
    seed: Option<u64>,
    now: i64,
) -> Result<TopicsReport> {
    let k = k.unwrap_or(topics.k);
    let seed = seed.unwrap_or(topics.seed);
    log::debug!(
        "cortex::topics::train: k={k} seed={seed} epochs={} workers={}",
        topics.epochs,
        topics.workers
    );
    if k == 0 {
        bail!("k must be at least 1");
    }
    if topics.workers == 0 {
        bail!("topics.workers must be at least 1");
    }
    let active_model = index.active_embedding_model()?;
    let dim = index.active_embedding_dim()?;
    index
        .topic_state()?
        .status(&active_model, dim)
        .wrap_err("the stored topic state is corrupt; `sb cortex topics --clear` empties it")?;

    let (scope, pending) = scope_and_pending(index, &topics.exclude_types, &active_model)?;
    if !pending.is_empty() {
        bail!(
            "{} eligible note(s) are pending a summary embedding (first: {}); run `sb cortex embed --backfill`, \
             then train. Nothing was written.",
            pending.len(),
            pending[0]
        );
    }

    let rows = index.in_scope_summaries(&topics.exclude_types)?;
    if rows.len() < k {
        bail!(
            "{} in-scope note(s) cannot make k={k} topics; lower --k. Nothing was written.",
            rows.len()
        );
    }
    let flat: Vec<f32> = rows.iter().flat_map(|r| r.embedding.iter().copied()).collect();
    let trained = in_train_pool(topics.workers, || kmeans::train(&flat, dim, k, seed, topics.epochs))?
        .map_err(|e| name_the_note(e, &rows))?;

    let mut trained_rows = vec![0i64; k];
    for &label in &trained.labels {
        trained_rows[label as usize] += 1;
    }
    let train_mean_cosine = trained.cosines.iter().map(|&c| c as f64).sum::<f64>() / rows.len() as f64;
    let assignments: Vec<Assignment> = rows
        .iter()
        .zip(trained.labels.iter().zip(&trained.cosines))
        .map(|(row, (&topic, &cosine))| Assignment {
            note_path: row.note_path.clone(),
            topic,
            cosine,
            embedding_id: row.embedding_id,
        })
        .collect();

    let generation = index.replace_generation(&NewGeneration {
        centroids: &trained.centroids,
        trained_rows: &trained_rows,
        dim,
        model_version: &active_model,
        assignments: &assignments,
        trained_at: now,
        seed,
        epochs: topics.epochs,
        e_means_version: kmeans::E_MEANS_VERSION,
        train_mean_cosine,
        exclude_types: &topics.exclude_types,
    })?;
    log::info!(
        "cortex::topics::train: generation={generation} k={k} rows={} train_mean_cosine={train_mean_cosine:.3}",
        rows.len()
    );
    report(index, topics, assignments.len(), scope.assigned)
}

fn assign_index(index: &mut SearchIndex, topics: &TopicsConfig, now: i64) -> Result<TopicsReport> {
    assign_index_until(index, topics, now, &|| false).map(|(report, _stopped)| report)
}

/// [`assign_index`] that reads `stopped` before the removal write and before
/// each batch's read, and returns whether it stopped early. A stopped run
/// leaves every batch it did write whole; the next run finishes the rest.
fn assign_index_until(
    index: &mut SearchIndex,
    topics: &TopicsConfig,
    now: i64,
    stopped: &dyn Fn() -> bool,
) -> Result<(TopicsReport, bool)> {
    let active_model = index.active_embedding_model()?;
    let dim = index.active_embedding_dim()?;
    let status = index.topic_state()?.status(&active_model, dim)?;
    log::debug!("cortex::topics::assign: status={status:?}");
    match status {
        TopicStatus::Absent => {
            log::debug!("cortex::topics::assign: no generation; nothing to assign");
            return Ok((report(index, topics, 0, 0)?, false));
        }
        TopicStatus::Stale => {
            log::debug!("cortex::topics::assign: stale generation; nothing written");
            return Ok((report(index, topics, 0, 0)?, false));
        }
        TopicStatus::Valid => {}
    }

    if stopped() {
        return Ok((report(index, topics, 0, 0)?, true));
    }
    let removed = index.remove_out_of_scope_topics(&topics.exclude_types)?;
    let (meta, centroids) = index.topic_centroids()?;
    let mut assigned = 0;
    let mut was_stopped = false;
    loop {
        if stopped() {
            was_stopped = true;
            break;
        }
        let rows = index.unassigned(&topics.exclude_types, ASSIGN_BATCH)?;
        if rows.is_empty() {
            break;
        }
        let batch = rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                kmeans::check_row(i, &row.embedding).map_err(|e| eyre!("{}: {e}", row.note_path))?;
                let (topic, cosine) = kmeans::nearest(&row.embedding, &centroids, meta.dim);
                Ok(Assignment {
                    note_path: row.note_path.clone(),
                    topic,
                    cosine,
                    embedding_id: row.embedding_id,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        assigned += index.assign_batch(&batch, now)?;
        log::debug!(
            "cortex::topics::assign: assigned {} this step, {assigned} so far",
            batch.len()
        );
    }
    log::info!(
        "cortex::topics::assign: generation={} assigned={assigned} removed={removed}",
        meta.generation
    );
    Ok((report(index, topics, assigned, removed)?, was_stopped))
}

/// What one daemon tick did, so a test (and the daemon's log line) can say.
#[derive(Debug, Clone, PartialEq)]
pub enum TickOutcome {
    /// A train, `--clear` or embed run holds the lock; the next tick retries.
    LockHeld,
    /// No generation: nothing to assign against.
    NoGeneration,
    /// The generation was trained under a model or dim the index no longer
    /// pins. Nothing was written; one WARN named `sb cortex topics --train`.
    Stale,
    /// The daemon's stop flag was set before the read or the write.
    Stopped,
    Assigned {
        generation: i64,
        assigned: usize,
        removed: usize,
    },
}

/// The daemon's assign tick: [`assign`] under the embed lock, which it skips
/// rather than waits for. No generation is a no-op, a stale one writes nothing
/// and warns. `stopped` is read before the read and before each write.
pub fn daemon_tick(config: &Config, stopped: &dyn Fn() -> bool) -> Result<TickOutcome> {
    let lock = match crate::embed::acquire_lock() {
        Ok(lock) => lock,
        Err(e) if e.downcast_ref::<crate::embed::EmbedLockHeld>().is_some() => {
            log::debug!("cortex::topics::daemon_tick: lock held; will retry next tick");
            return Ok(TickOutcome::LockHeld);
        }
        Err(e) => return Err(e),
    };
    if stopped() {
        return Ok(TickOutcome::Stopped);
    }
    let mut index = open_index(config)?;
    let outcome = tick_index(&mut index, &config.topics, now_secs(), stopped)?;
    drop(lock);
    Ok(outcome)
}

fn tick_index(
    index: &mut SearchIndex,
    topics: &TopicsConfig,
    now: i64,
    stopped: &dyn Fn() -> bool,
) -> Result<TickOutcome> {
    let active_model = index.active_embedding_model()?;
    let dim = index.active_embedding_dim()?;
    match index.topic_state()?.status(&active_model, dim)? {
        TopicStatus::Absent => {
            log::debug!("cortex::topics::daemon_tick: no generation; nothing to do");
            Ok(TickOutcome::NoGeneration)
        }
        TopicStatus::Stale => {
            log::warn!(
                "cortex::topics::daemon_tick: the topic generation does not match the index's embedding model; \
                 assigning nothing until `sb cortex topics --train` runs"
            );
            Ok(TickOutcome::Stale)
        }
        TopicStatus::Valid => {
            let (report, was_stopped) = assign_index_until(index, topics, now, stopped)?;
            if was_stopped {
                return Ok(TickOutcome::Stopped);
            }
            Ok(TickOutcome::Assigned {
                generation: report
                    .generation
                    .ok_or_else(|| eyre!("a valid generation reported none"))?,
                assigned: report.assigned_this_run,
                removed: report.removed_this_run,
            })
        }
    }
}

fn clear_index(index: &mut SearchIndex, topics: &TopicsConfig) -> Result<TopicsReport> {
    let removed = index.clear_topics()?;
    log::info!("cortex::topics::clear: removed {removed} note_topics row(s); generation counter kept");
    report(index, topics, 0, removed)
}

/// The scope counts, and the eligible notes the embed loop still owes a
/// summary vector. A note marked nothing-to-embed is not a target, so it is
/// settled, never pending.
fn scope_and_pending(
    index: &SearchIndex,
    exclude_types: &[NoteType],
    active_model: &str,
) -> Result<(TopicScope, Vec<String>)> {
    let scope = index.topic_scope(exclude_types)?;
    if scope.eligible_unembedded.is_empty() {
        return Ok((scope, Vec::new()));
    }
    let targets: HashSet<String> = index
        .stale_embedding_targets(EmbeddingKind::Summary, active_model, u32::MAX)?
        .into_iter()
        .map(|t| t.note_path)
        .collect();
    let pending = scope
        .eligible_unembedded
        .iter()
        .filter(|path| targets.contains(*path))
        .cloned()
        .collect();
    Ok((scope, pending))
}

/// Run `f` on a rayon pool of `workers` threads built for this call, so
/// training never runs on the global pool governance shares, nor on the embed
/// inference pool.
fn in_train_pool<T: Send>(workers: usize, f: impl FnOnce() -> T + Send) -> Result<T> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .thread_name(|i| format!("topics-train-{i}"))
        .build()
        .wrap_err("failed to build the topics training pool")?;
    Ok(pool.install(f))
}

fn name_the_note(err: KmeansError, rows: &[SummaryVector]) -> eyre::Report {
    match err {
        KmeansError::NonFinite { row, .. } | KmeansError::NotUnit { row, .. } => {
            let path = rows.get(row).map(|r| r.note_path.as_str()).unwrap_or("<unknown>");
            eyre!("{path}: {err}")
        }
        other => eyre::Report::new(other),
    }
}

fn report(
    index: &SearchIndex,
    topics: &TopicsConfig,
    assigned_this_run: usize,
    removed_this_run: usize,
) -> Result<TopicsReport> {
    let active_model = index.active_embedding_model()?;
    let dim = index.active_embedding_dim()?;
    let state: TopicState = index.topic_state()?;
    let status = state.status(&active_model, dim)?;
    let meta = state.meta()?;
    let (scope, pending) = scope_and_pending(index, &topics.exclude_types, &active_model)?;

    let configured: Vec<String> = topics.exclude_types.iter().map(|t| t.as_str().to_string()).collect();
    let exclude_types_drift = meta.as_ref().and_then(|m| {
        let trained: BTreeSet<&str> = m.exclude_types.iter().map(String::as_str).collect();
        let now: BTreeSet<&str> = configured.iter().map(String::as_str).collect();
        (trained != now).then(|| ExcludeTypesDrift {
            trained: m.exclude_types.clone(),
            configured: configured.clone(),
        })
    });
    let hint = match (&status, &meta) {
        (TopicStatus::Absent, _) => Some("no generation: run `sb cortex topics --train`".to_string()),
        (TopicStatus::Stale, Some(m)) => Some(format!(
            "generation {} was trained for {} dim {} but the index pins {active_model} dim {dim}: run `sb cortex topics --train`",
            m.generation, m.model_version, m.dim
        )),
        _ => None,
    };

    Ok(TopicsReport {
        status,
        generation: meta.as_ref().map(|m| m.generation),
        k: meta.as_ref().map(|m| m.k),
        configured_k: topics.k,
        trained_at: meta.as_ref().map(|m| rfc3339(m.trained_at)).transpose()?,
        eligible: scope.eligible,
        pending: pending.len(),
        in_scope: scope.in_scope,
        assigned: scope.assigned,
        assigned_this_run,
        removed_this_run,
        mean_cosine: scope.mean_cosine,
        train_mean_cosine: meta.as_ref().map(|m| m.train_mean_cosine),
        exclude_types_drift,
        hint,
    })
}

fn rfc3339(secs: i64) -> Result<String> {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .ok_or_else(|| eyre!("topic_state trained_at {secs} is not a representable time"))
}

#[cfg(test)]
mod tests;
