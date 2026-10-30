//! Cortex's embed loop. The only writer to `note_embeddings`.
//!
//! Hybrid retrieval design
//! (`docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md`).
//!
//! ## Transaction discipline (load-bearing)
//!
//! The SQLite write lock must NOT be held across `embed_batch`'s CPU
//! inference. With batch = 64 at ~50 ms/note, holding a transaction
//! across the inference call would lock the DB for ~3.2 seconds per
//! batch and starve oracle's `index_vault` writes (which exceed
//! `busy_timeout = 5 s` under load).
//!
//! The loop body must be three discrete phases:
//!
//! 1. **Read phase (auto-commit, no transaction):** query
//!    `stale_embedding_targets` to pull the next N (path, kind,
//!    source_modified_at) rows + their text. The connection returns to
//!    idle immediately.
//! 2. **Inference phase (no SQLite interaction):** call `embed_batch`.
//!    No DB lock held; oracle's `index_vault` writes proceed normally
//!    during this window.
//! 3. **Write phase (one short transaction):** `BEGIN IMMEDIATE`, call
//!    `upsert_embedding` for each result row, `COMMIT`. The
//!    transaction stays under ~50 ms because there is no CPU work
//!    inside it. Oracle may briefly wait on its next `index_vault`
//!    UPDATE; the wait is bounded by the write-transaction length, not
//!    by inference time.
//!
//! This file's `process_batch` makes the three phases visible as three
//! named function calls so a reviewer can point at the `BEGIN
//! IMMEDIATE` line and confirm `embed_batch` is *not* called between
//! it and the matching `COMMIT`. The regression test asserts
//! the write transaction wall-clock stays under 200 ms for batch = 64.

use std::path::Path;
use std::time::Duration;

use eyre::{Context, Result};
use vault::cpu::CpuFeatures;
use vault::embedding::{
    ACTIVE_MODEL_VERSION, BackendChoice, EmbeddingModel, load_model_version, prefetch_model_version,
    resolve_backend_logged,
};
use vault::search::{DroppedKind, EmbeddingKind, SearchIndex};

use crate::config::{Config, EmbedKindsConfig};
use crate::opts::EmbedOpts;
use crate::shutdown::StopHandle;

mod allowance;
mod carry;
mod claim;
mod inference;
mod loader;
mod lock;
mod source;
mod summary;
mod transcript;
pub use allowance::ChunkAllowance;
pub use carry::EmbedCarry;
use claim::process_claim_batch;
pub use inference::DEFAULT_INFERENCE_WORKERS;
pub use loader::{LoadedModel, load_embedder};
pub use lock::{EmbedLock, EmbedLockHeld, acquire_lock};
pub use source::SourceRoots;
use summary::process_summary_batch;
pub use summary::summary_embed_text;
use transcript::process_transcript_batch;

/// Default batch size for the embed loop. The transaction
/// discipline test asserts the write transaction wall-clock stays under
/// 200 ms at this batch.
pub const DEFAULT_BATCH_SIZE: usize = 64;

/// Default daemon cadence for the embed tick. Most ticks find zero
/// stale rows and return immediately; ~20 ingests/day plus user edits
/// leave only a handful of rows for a typical day.
pub const DEFAULT_CADENCE_SECS: u64 = 600;

/// Cap on the input size of any single `embed_batch` call. The flat
/// fan-out in `process_transcript_batch` can produce thousands of
/// chunks when a backlog of transcript-eligible notes drains in one
/// tick; passing that many strings to candle's 8-replica rayon
/// fan-out at once peaks activation memory at tens of GB. Sub-batching
/// at this cap bounds peak per-call memory to roughly 64 strings ×
/// 512 tokens × 384 hidden × 12 layers × 4 bytes ≈ 600 MB across the
/// replica pool. See docs/design/2026-05-19-cortex-embed-memory-bounding.md.
pub const DEFAULT_MAX_CHUNKS_PER_CALL: usize = 64;

/// Cap on total chunks embedded in a single tick, across all notes.
///
/// `DEFAULT_MAX_CHUNKS_PER_CALL` bounds peak MEMORY per model call; this
/// bounds the tick's WALL CLOCK, which is a different failure. `batch_size`
/// counts notes, so a tick's real work is `64 notes × (however many chunks
/// each transcript yields)` - unbounded. On 2026-08-16 a single tick on
/// `talos` (AVX-only; scalar gemm microkernel, no vectorized f32 path) ran for
/// two days holding the whole rayon pool, and because the batch logs nothing
/// between start and finish it was invisible: the daemon read as healthy
/// while classify never ran again.
///
/// Truncating at this cap costs nothing in total work - the notes that do not
/// fit keep their stale marker and are picked up next tick - and converts one
/// unbounded run into a predictable series.
pub const DEFAULT_MAX_CHUNKS_PER_TICK: usize = 512;

/// One pass / batch of work the embed loop produces. Surfaced so the
/// daemon and CLI can both log uniformly.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmbedStats {
    pub scanned: u64,
    pub embedded: u64,
    pub skipped_empty: u64,
    pub failed: u64,
}

impl EmbedStats {
    fn merge(&mut self, other: &EmbedStats) {
        self.scanned += other.scanned;
        self.embedded += other.embedded;
        self.skipped_empty += other.skipped_empty;
        self.failed += other.failed;
    }

    /// Chunks this batch sent to the model: the ones written plus the ones
    /// whose inference or write failed. Both cost inference time, so both draw
    /// from the tick's [`ChunkAllowance`].
    fn chunks_inferred(&self) -> usize {
        (self.embedded + self.failed) as usize
    }
}

/// How one embed batch ended. A stop is its own outcome, so no caller can
/// count it as a failure.
#[derive(Debug, Clone, Copy)]
pub enum BatchOutcome {
    /// Every note the batch selected was attempted.
    Complete(EmbedStats),
    /// The tick's chunk allowance could not fit the next note. The notes past
    /// it keep their stale marker and lead the next tick.
    AllowanceSpent(EmbedStats),
    /// The stop flag was seen before an inference call. No embedding row was
    /// written and `failed` is untouched; the examined sentinels the read phase
    /// wrote before inference stay.
    Stopped(EmbedStats),
}

impl BatchOutcome {
    pub fn stats(&self) -> &EmbedStats {
        match self {
            BatchOutcome::Complete(s) | BatchOutcome::AllowanceSpent(s) | BatchOutcome::Stopped(s) => s,
        }
    }
}

/// Prefetch the active embedding model's files for the backend `backend`
/// resolves to on this host's CPU, and return the resolved model name so sb
/// can print "Prefetched embedding model {name}". Split from `embed::run` to
/// keep `EmbedStats` Copy and avoid stuffing prefetch-only state into the
/// per-batch stats type.
pub fn prefetch(model_override: Option<&str>, backend: BackendChoice) -> Result<String> {
    // Resolve the model name from the active SearchIndex if possible (matches
    // the prior behavior); otherwise fall back to the compiled-in
    // ACTIVE_MODEL_VERSION. We accept an Option<&str> --model override so the
    // user can ask to prefetch a specific name even before the index exists.
    let resolved = match model_override {
        Some(m) => m.to_string(),
        None => ACTIVE_MODEL_VERSION.to_string(),
    };
    let backend = resolve_backend_logged(backend, &CpuFeatures::detect(), &resolved)?;
    log::info!("cortex::embed: prefetching model {resolved} for backend {backend}");
    prefetch_model_version(&resolved, backend).wrap_err("failed to prefetch embedding model")?;
    log::info!("prefetched embedding model {resolved}");
    Ok(resolved)
}

/// CLI entry point for `cortex embed`.
pub fn run(vault_root: &Path, config: &Config, opts: &EmbedOpts) -> Result<EmbedStats> {
    log::info!(
        "cortex::embed::run: vault_root={} batch_size={} kind={:?} model={:?} prefetch_model={} rss_entry={}",
        vault_root.display(),
        opts.batch_size,
        opts.kind,
        opts.model,
        opts.prefetch_model,
        vault::rss::read_self_rss()
            .map(vault::rss::human_bytes)
            .unwrap_or_else(|| "n/a".to_string()),
    );

    if opts.prefetch_model {
        prefetch(opts.model.as_deref(), config.embed.backend)?;
        return Ok(EmbedStats::default());
    }

    let db_path = config.oracle_db_path();
    log::debug!("cortex::embed::run: oracle db={}", db_path.display());
    let mut index = SearchIndex::open(&db_path)
        .wrap_err_with(|| format!("failed to open search index at {}", db_path.display()))?;

    let model_version = opts.model.clone().unwrap_or_else(|| {
        index
            .active_embedding_model()
            .unwrap_or_else(|_| ACTIVE_MODEL_VERSION.to_string())
    });

    let lock = acquire_lock()?;
    log::debug!("cortex::embed: acquired file lock");

    let model: Box<dyn EmbeddingModel> = {
        let rss_pre = vault::rss::read_self_rss();
        log::info!(
            "cortex::embed: loading embedding model {model_version} workers={} rss_pre_load={}",
            config.embed.workers,
            rss_pre
                .map(vault::rss::human_bytes)
                .unwrap_or_else(|| "n/a".to_string()),
        );
        let LoadedModel { model: m, .. } = load_embedder(
            &config.embed,
            &model_version,
            &CpuFeatures::detect(),
            load_model_version,
        )?;
        let rss_post = vault::rss::read_self_rss();
        if let (Some(pre), Some(post)) = (rss_pre, rss_post) {
            log::info!(
                "cortex::embed: model loaded; rss_post_load={} delta={}",
                vault::rss::human_bytes(post),
                vault::rss::human_bytes(post.saturating_sub(pre)),
            );
        }
        m
    };

    // Make sure embedding_config matches the model we're about to write
    // so oracle's search_vector pulls compatible rows on the next query.
    index.set_active_embedding(model.model_version(), model.dim())?;

    // CLI > config: an explicit `--kind <k>` embeds exactly that kind
    // regardless of the config toggle (the escape hatch for the future
    // guard-first claim experiment). With no `--kind`, the default pass
    // embeds only the config-enabled kinds (default: summary +
    // transcript-chunk, NOT claim - see `resolve_kinds`).
    let kinds = resolve_kinds(opts.kind.as_deref(), &config.embed.kinds)?;

    // A CLI pass drains the backlog, so each batch gets a fresh allowance: the
    // cap bounds one batch's inference here, never the whole pass.
    let stop = StopHandle::never();
    let mut total = EmbedStats::default();
    for kind in kinds {
        loop {
            let outcome = process_batch(
                &mut index,
                model.as_ref(),
                kind,
                model.model_version(),
                vault_root,
                SourceRoots::of(config),
                opts.batch_size,
                config.embed.max_chunks_per_call,
                ChunkAllowance::per_tick(config.embed.max_chunks_per_tick),
                config.embed.workers,
                &stop,
            )?;
            let batch_stats = *outcome.stats();
            total.merge(&batch_stats);
            if batch_stats.scanned == 0 {
                break;
            }
            // Termination guard against ANY zero-progress batch: if a batch
            // scanned rows but embedded none, the next batch re-selects the
            // identical stale set and the loop spins forever. This covers
            // both the all-skipped case (notes lacking the expected section)
            // AND the all-failed case (a persistently failing `embed_batch` -
            // e.g. a broken model or a poison input - which the previous
            // `scanned == skipped_empty` condition did NOT catch). Stale rows
            // remain pending until the underlying notes/model recover.
            if batch_stats.embedded == 0 {
                log::warn!(
                    "cortex::embed: kind={:?} batch scanned={} embedded=0 (skipped={} failed={}); \
                     halting to avoid an infinite retry loop. Stale rows remain pending.",
                    kind,
                    batch_stats.scanned,
                    batch_stats.skipped_empty,
                    batch_stats.failed,
                );
                break;
            }
        }
    }

    drop(lock);
    let rss_pre_drop = vault::rss::read_self_rss();
    drop(model);
    let rss_post_drop = vault::rss::read_self_rss();
    log::info!(
        "embed complete: scanned={} embedded={} skipped_empty={} failed={} rss_pre_drop={} rss_post_drop={}",
        total.scanned,
        total.embedded,
        total.skipped_empty,
        total.failed,
        rss_pre_drop
            .map(vault::rss::human_bytes)
            .unwrap_or_else(|| "n/a".to_string()),
        rss_post_drop
            .map(vault::rss::human_bytes)
            .unwrap_or_else(|| "n/a".to_string()),
    );
    Ok(total)
}

/// load the embedding model once at daemon startup. The daemon
/// holds the returned handle and passes it (by reference) to every tick
/// via `daemon_tick_with_model`, so the model's per-instance scratch
/// state is reused across ticks instead of allocated + dropped per tick.
/// The shakedown report's 1.2 -> 2.8 GB RSS climb over 50 minutes was
/// almost entirely allocator churn from the prior per-tick load-and-drop
/// pattern; this lifecycle change makes it bounded.
pub fn load_daemon_model(config: &Config) -> Result<Box<dyn EmbeddingModel>> {
    let rss_pre = vault::rss::read_self_rss();
    // Honor the pinned model in embedding_config so a restart picks up an A/B
    // flip (`sb cortex embed --model <version>`). Without this the daemon would
    // load the compiled default and then re-pin back to it, fighting the flip.
    // The flip must therefore be done with the daemon stopped, then restarted.
    let db_path = config.oracle_db_path();
    let model_version = SearchIndex::open(&db_path)
        .ok()
        .and_then(|i| i.active_embedding_model().ok())
        .unwrap_or_else(|| ACTIVE_MODEL_VERSION.to_string());
    log::info!(
        "cortex::embed::load_daemon_model: version={model_version} workers={} rss_pre={}",
        config.embed.workers,
        rss_pre
            .map(vault::rss::human_bytes)
            .unwrap_or_else(|| "n/a".to_string()),
    );
    let LoadedModel { model, .. } = load_embedder(
        &config.embed,
        &model_version,
        &CpuFeatures::detect(),
        load_model_version,
    )
    .wrap_err("failed to load embedding model for daemon")?;
    let rss_post = vault::rss::read_self_rss();
    if let (Some(pre), Some(post)) = (rss_pre, rss_post) {
        log::info!(
            "cortex::embed::load_daemon_model: loaded; rss_post={} delta={}",
            vault::rss::human_bytes(post),
            vault::rss::human_bytes(post.saturating_sub(pre)),
        );
    }
    Ok(model)
}

/// Daemon tick that reuses a long-lived model loaded once at daemon
/// startup. See `load_daemon_model`. `stop` is the daemon's stop handle;
/// the tick checks it before every batch and between inference calls.
/// `carry` is the daemon's [`EmbedCarry`], kept across ticks.
pub fn daemon_tick_with_model(
    vault_root: &Path,
    config: &Config,
    model: &dyn EmbeddingModel,
    stop: &StopHandle,
    carry: &mut EmbedCarry,
) -> Result<EmbedStats> {
    let rss_entry = vault::rss::read_self_rss();
    log::debug!(
        "cortex::embed::daemon_tick_with_model: vault_root={} rss={}",
        vault_root.display(),
        rss_entry
            .map(vault::rss::human_bytes)
            .unwrap_or_else(|| "n/a".to_string()),
    );

    let db_path = config.oracle_db_path();
    let mut index = SearchIndex::open(&db_path)
        .wrap_err_with(|| format!("failed to open search index at {}", db_path.display()))?;

    // Make sure embedding_config matches the model we're using.
    index.set_active_embedding(model.model_version(), model.dim())?;

    let lock = match acquire_lock() {
        Ok(l) => l,
        Err(e) if e.downcast_ref::<EmbedLockHeld>().is_some() => {
            log::debug!("cortex::embed::daemon_tick_with_model: lock held; will retry next tick");
            return Ok(EmbedStats::default());
        }
        Err(e) => return Err(e),
    };

    let total = embed_tick(&mut index, model, vault_root, config, stop, carry)?;

    drop(lock);
    let rss_exit = vault::rss::read_self_rss();
    if let (Some(entry), Some(exit)) = (rss_entry, rss_exit) {
        log::debug!(
            "cortex::embed::daemon_tick_with_model: scanned={} embedded={} skipped_empty={} failed={} rss_entry={} rss_exit={} delta={}",
            total.scanned,
            total.embedded,
            total.skipped_empty,
            total.failed,
            vault::rss::human_bytes(entry),
            vault::rss::human_bytes(exit),
            vault::rss::human_bytes(exit.saturating_sub(entry)),
        );
    }
    Ok(total)
}

/// The body of one daemon embed tick, on an already-open index with the
/// embed lock held. Split from `daemon_tick_with_model` because that opens
/// the index at the fixed `oracle_db_path`; this is the hermetic seam tests
/// drive on `SearchIndex::open_memory()`.
///
/// One `max-chunks-per-tick` allowance is shared by every kind in run order
/// (summary first, unless `carry` names a kind left waiting last tick). The
/// tick ends when the stop flag is set, the allowance cannot fit the next
/// note, or the backlog is empty.
fn embed_tick(
    index: &mut SearchIndex,
    model: &dyn EmbeddingModel,
    vault_root: &Path,
    config: &Config,
    stop: &StopHandle,
    carry: &mut EmbedCarry,
) -> Result<EmbedStats> {
    // The daemon tick has no per-invocation CLI surface, so the kinds it
    // generates are gated by config only. With defaults that is summary +
    // transcript-chunk (claim is default-OFF after the retrieval
    // gate failure); enabling `embed.kinds.claim` re-adds it here.
    let kinds = carry.run_order(enabled_default_kinds(&config.embed.kinds));
    log::debug!(
        "cortex::embed::embed_tick: kinds={kinds:?} max_chunks_per_tick={}",
        config.embed.max_chunks_per_tick
    );
    let mut allowance = ChunkAllowance::per_tick(config.embed.max_chunks_per_tick);
    let mut total = EmbedStats::default();
    carry.clear();
    'kinds: for (at, &kind) in kinds.iter().enumerate() {
        let mut drawn_by_kind = 0usize;
        loop {
            if stop.is_stopped() {
                log::info!("cortex::embed: stop requested; ending the embed tick before the next batch");
                break 'kinds;
            }
            if allowance.is_spent() {
                log::info!(
                    "cortex::embed: per-tick allowance of {} chunk(s) spent; the rest waits for the next tick",
                    config.embed.max_chunks_per_tick,
                );
                carry.allowance_ended(&kinds, at, drawn_by_kind);
                break 'kinds;
            }
            let outcome = process_batch(
                index,
                model,
                kind,
                model.model_version(),
                vault_root,
                SourceRoots::of(config),
                DEFAULT_BATCH_SIZE,
                config.embed.max_chunks_per_call,
                allowance,
                config.embed.workers,
                stop,
            )?;
            let batch_stats = *outcome.stats();
            total.merge(&batch_stats);
            allowance.draw(batch_stats.chunks_inferred());
            drawn_by_kind += batch_stats.chunks_inferred();
            match outcome {
                BatchOutcome::Stopped(_) => {
                    log::info!("cortex::embed: kind={kind:?} batch stopped; its vectors were dropped, nothing written");
                    break 'kinds;
                }
                BatchOutcome::AllowanceSpent(_) => {
                    carry.allowance_ended(&kinds, at, drawn_by_kind);
                    break 'kinds;
                }
                BatchOutcome::Complete(_) => {}
            }
            if batch_stats.scanned == 0 {
                break;
            }
            // Break on ANY zero-progress batch (all-skipped OR all-failed),
            // not only all-skipped: a persistently failing embed_batch
            // otherwise re-selects the same stale set every iteration.
            if batch_stats.embedded == 0 {
                log::warn!(
                    "cortex::embed::embed_tick: kind={:?} batch scanned={} embedded=0 (skipped={} failed={}); halting to avoid an infinite retry loop.",
                    kind,
                    batch_stats.scanned,
                    batch_stats.skipped_empty,
                    batch_stats.failed,
                );
                break;
            }
        }
    }
    Ok(total)
}

/// Single batch of the read / inference / write loop. This is the
/// function the transaction-discipline test exercises.
///
/// **DO NOT** wrap `model.embed_batch` in a transaction. The write
/// transaction starts on line "begin immediate" below and ends at the
/// matching "commit" - both clearly named. If a future change moves
/// `embed_batch` between those two lines, the regression test in
/// `cortex/src/embed/tests.rs` fails because the wall-clock between
/// `BEGIN IMMEDIATE` and `COMMIT` blows past 200 ms.
pub fn process_batch(
    index: &mut SearchIndex,
    model: &dyn EmbeddingModel,
    kind: EmbeddingKind,
    model_version: &str,
    vault_root: &Path,
    roots: SourceRoots<'_>,
    batch_size: usize,
    max_chunks_per_call: usize,
    allowance: ChunkAllowance,
    workers: usize,
    stop: &StopHandle,
) -> Result<BatchOutcome> {
    match kind {
        EmbeddingKind::Summary => process_summary_batch(
            index,
            model,
            model_version,
            vault_root,
            batch_size,
            max_chunks_per_call,
            allowance,
            workers,
            stop,
        ),
        EmbeddingKind::TranscriptChunk => process_transcript_batch(
            index,
            model,
            model_version,
            vault_root,
            roots,
            batch_size,
            max_chunks_per_call,
            allowance,
            workers,
            stop,
        ),
        EmbeddingKind::Claim => process_claim_batch(
            index,
            model,
            model_version,
            batch_size,
            max_chunks_per_call,
            allowance,
            workers,
            stop,
        ),
    }
}

/// Resolve the kind set for a `cortex embed` pass. CLI beats config: an
/// explicit `--kind <k>` restricts the pass to exactly that kind regardless of
/// the config toggle (the escape hatch for the future guard-first claim
/// experiment). With no `--kind`, the pass embeds the config-enabled kinds.
fn resolve_kinds(kind_override: Option<&str>, kinds: &EmbedKindsConfig) -> Result<Vec<EmbeddingKind>> {
    match kind_override {
        Some(k) => Ok(vec![parse_kind(k)?]),
        None => Ok(enabled_default_kinds(kinds)),
    }
}

/// The default (no-`--kind`) kind list, filtered by the `embed.kinds` config
/// toggles. Order is load-bearing for progress logging: summary first (one row
/// per note, fast), then transcript chunks (N rows per transcript-eligible
/// note), then claims. `claim` is default-OFF (see [`EmbedKindsConfig`]).
fn enabled_default_kinds(kinds: &EmbedKindsConfig) -> Vec<EmbeddingKind> {
    let mut out = Vec::with_capacity(3);
    if kinds.summary {
        out.push(EmbeddingKind::Summary);
    }
    if kinds.transcript_chunk {
        out.push(EmbeddingKind::TranscriptChunk);
    }
    if kinds.claim {
        out.push(EmbeddingKind::Claim);
    }
    out
}

/// Parse a `--kind` / `--drop-kind` value into an [`EmbeddingKind`].
/// Shared by `run` (which restricts the pass to one kind) and `drop_kind`
/// (the rollback verb) so both accept the same vocabulary.
fn parse_kind(s: &str) -> Result<EmbeddingKind> {
    match s {
        "summary" => Ok(EmbeddingKind::Summary),
        "transcript-chunk" => Ok(EmbeddingKind::TranscriptChunk),
        "claim" => Ok(EmbeddingKind::Claim),
        other => eyre::bail!("unknown embedding kind {other:?}; expected summary | transcript-chunk | claim"),
    }
}

/// First-class rollback verb behind `sb cortex embed --drop-kind <kind>`.
/// Deletes every embedding row and every examined sentinel of `kind` and
/// returns both counts.
///
/// Reverting cortex code does NOT stop oracle reading, e.g., claim rows -
/// `search_vector` scans all kinds - so deleting the rows is the only real
/// rollback. The sentinels go with them so a later backfill re-selects every
/// note instead of skipping the ones a sentinel still masks. Kept out of the
/// read/inference/write loop entirely: it opens the index, deletes, and
/// returns.
pub fn drop_kind(config: &Config, kind: &str) -> Result<DroppedKind> {
    let embedding_kind = parse_kind(kind)?;
    let db_path = config.oracle_db_path();
    log::debug!("cortex::embed::drop_kind: kind={kind} oracle db={}", db_path.display());
    let mut index = SearchIndex::open(&db_path)
        .wrap_err_with(|| format!("failed to open search index at {}", db_path.display()))?;
    let dropped = index.drop_embedding_kind(embedding_kind)?;
    log::info!(
        "cortex::embed::drop_kind: kind={kind} rows={} sentinels={}",
        dropped.rows,
        dropped.sentinels
    );
    Ok(dropped)
}

/// Convenience helper: how long the daemon should sleep between embed
/// ticks. Reads `embed.cadence-secs` (default [`DEFAULT_CADENCE_SECS`]).
pub fn daemon_cadence(config: &Config) -> Duration {
    Duration::from_secs(config.embed.cadence_secs)
}

#[cfg(test)]
mod tests;
