//! Cortex's embed loop. The only writer to `note_embeddings`.
//!
//! Phase A5 of the hybrid retrieval design
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
//! it and the matching `COMMIT`. Phase A5's regression test asserts
//! the write transaction wall-clock stays under 200 ms for batch = 64.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eyre::{Context, Result};
use fs2::FileExt;
use vault::embedding::{
    ACTIVE_MODEL_VERSION, EmbeddingModel, MockEmbedder, load_model_version, prefetch_model_version,
};
use vault::search::{BatchUpsert, EmbeddingKind, SearchIndex};

use crate::config::{Config, EmbedKindsConfig};
use crate::opts::EmbedOpts;

/// Default batch size for the embed loop. Phase A5's transaction
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
}

/// Prefetch the active embedding model and return the resolved model name
/// so sb can print "Prefetched embedding model {name}". Split from
/// `embed::run` to keep `EmbedStats` Copy and avoid stuffing prefetch-only
/// state into the per-batch stats type.
pub fn prefetch(model_override: Option<&str>) -> Result<String> {
    // Resolve the model name from the active SearchIndex if possible (matches
    // the prior behavior); otherwise fall back to the compiled-in
    // ACTIVE_MODEL_VERSION. We accept an Option<&str> --model override so the
    // user can ask to prefetch a specific name even before the index exists.
    let resolved = match model_override {
        Some(m) => m.to_string(),
        None => ACTIVE_MODEL_VERSION.to_string(),
    };
    log::info!("cortex::embed: prefetching model {resolved}");
    prefetch_model_version(&resolved).wrap_err("failed to prefetch embedding model")?;
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
        prefetch(opts.model.as_deref())?;
        return Ok(EmbedStats::default());
    }

    let db_path = config.oracle_db_path();
    let mut index = SearchIndex::open(&db_path)
        .wrap_err_with(|| format!("failed to open search index at {}", db_path.display()))?;

    let model_version = opts.model.clone().unwrap_or_else(|| {
        index
            .active_embedding_model()
            .unwrap_or_else(|_| ACTIVE_MODEL_VERSION.to_string())
    });

    let lock = acquire_lock()?;
    log::debug!("cortex::embed: acquired file lock");

    let model: Box<dyn EmbeddingModel> = if opts.use_mock {
        log::warn!("cortex::embed: using MockEmbedder (test-only)");
        Box::new(MockEmbedder::default_384())
    } else {
        let rss_pre = vault::rss::read_self_rss();
        log::info!(
            "cortex::embed: loading embedding model {model_version} workers={} rss_pre_load={}",
            config.embed.workers,
            rss_pre
                .map(vault::rss::human_bytes)
                .unwrap_or_else(|| "n/a".to_string()),
        );
        let m = load_model_version(&model_version, config.embed.workers).wrap_err("failed to load embedding model")?;
        let rss_post = vault::rss::read_self_rss();
        if let (Some(pre), Some(post)) = (rss_pre, rss_post) {
            log::info!(
                "cortex::embed: model loaded; rss_post_load={} delta={}",
                vault::rss::human_bytes(post),
                vault::rss::human_bytes(post.saturating_sub(pre)),
            );
        }
        Box::new(m)
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

    let mut total = EmbedStats::default();
    for kind in kinds {
        loop {
            let batch_stats = process_batch(
                &mut index,
                model.as_ref(),
                kind,
                model.model_version(),
                vault_root,
                opts.batch_size,
                config.embed.max_chunks_per_call,
            )?;
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

/// Phase 7b: load the embedding model once at daemon startup. The daemon
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
    let model: Box<dyn EmbeddingModel> = Box::new(
        load_model_version(&model_version, config.embed.workers)
            .wrap_err("failed to load embedding model for daemon")?,
    );
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
/// startup. See `load_daemon_model`.
pub fn daemon_tick_with_model(vault_root: &Path, config: &Config, model: &dyn EmbeddingModel) -> Result<EmbedStats> {
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

    // The daemon tick has no per-invocation CLI surface, so the kinds it
    // generates are gated by config only. With defaults that is summary +
    // transcript-chunk (claim is default-OFF after the 2026-07-05 retrieval
    // gate failure); enabling `embed.kinds.claim` re-adds it here.
    let kinds = enabled_default_kinds(&config.embed.kinds);
    let mut total = EmbedStats::default();
    for kind in kinds {
        loop {
            let batch_stats = process_batch(
                &mut index,
                model,
                kind,
                model.model_version(),
                vault_root,
                DEFAULT_BATCH_SIZE,
                config.embed.max_chunks_per_call,
            )?;
            total.merge(&batch_stats);
            if batch_stats.scanned == 0 {
                break;
            }
            // Break on ANY zero-progress batch (all-skipped OR all-failed),
            // not only all-skipped: a persistently failing embed_batch
            // otherwise re-selects the same stale set every iteration.
            if batch_stats.embedded == 0 {
                log::warn!(
                    "cortex::embed::daemon_tick_with_model: kind={:?} batch scanned={} embedded=0 (skipped={} failed={}); halting to avoid an infinite retry loop.",
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
    batch_size: usize,
    max_chunks_per_call: usize,
) -> Result<EmbedStats> {
    match kind {
        EmbeddingKind::Summary => {
            process_summary_batch(index, model, model_version, vault_root, batch_size, max_chunks_per_call)
        }
        EmbeddingKind::TranscriptChunk => {
            process_transcript_batch(index, model, model_version, vault_root, batch_size, max_chunks_per_call)
        }
        EmbeddingKind::Claim => process_claim_batch(index, model, model_version, batch_size, max_chunks_per_call),
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

/// First-class rollback verb behind `sb cortex embed --drop-kind <kind>`
/// (Phase 9). Deletes every embedding row of `kind` and returns the count.
///
/// Reverting cortex code does NOT stop oracle reading, e.g., claim rows -
/// `search_vector` scans all kinds - so deleting the rows is the only real
/// rollback. Kept out of the read/inference/write loop entirely: it opens
/// the index, deletes, and returns.
pub fn drop_kind(config: &Config, kind: &str) -> Result<usize> {
    let embedding_kind = parse_kind(kind)?;
    let db_path = config.oracle_db_path();
    let index = SearchIndex::open(&db_path)
        .wrap_err_with(|| format!("failed to open search index at {}", db_path.display()))?;
    let deleted = index.delete_embeddings_of_kind(embedding_kind)?;
    log::info!("cortex::embed::drop_kind: kind={kind} deleted={deleted}");
    Ok(deleted)
}

/// Phase A5: one batch of summary embeddings. Read auto-commit, embed
/// outside any transaction, flush in one short upsert.
fn process_summary_batch(
    index: &mut SearchIndex,
    model: &dyn EmbeddingModel,
    model_version: &str,
    _vault_root: &Path,
    batch_size: usize,
    max_chunks_per_call: usize,
) -> Result<EmbedStats> {
    let mut stats = EmbedStats::default();

    // ---- 1. READ PHASE (auto-commit, no transaction). ----
    let targets = index.stale_embedding_targets(EmbeddingKind::Summary, model_version, batch_size as u32)?;
    if targets.is_empty() {
        return Ok(stats);
    }
    log::debug!("cortex::embed::process_summary_batch: scanned={}", targets.len());

    // Use the snapshot of `notes.summary` returned by
    // stale_embedding_targets directly. The indexer populates that
    // column via vault::search::parse_body_summary (with
    // detail::extract_summary as a fallback), so it is the canonical
    // source of summary text and is always non-empty (the SQL filter
    // excludes empty rows). No file I/O on the hot path; no
    // "skipped without writing" loop on notes that lack a ## Summary
    // heading in the markdown body.
    let mut work: Vec<EmbedWork> = Vec::with_capacity(targets.len());
    // Defensive examined-sentinel accumulator (Phase 3): the SQL filter already
    // excludes empty-summary notes, so this is normally empty, but recording a
    // skip here keeps the "examined, nothing to embed" mechanism coherent
    // across every kind should a future schema drift reintroduce the skip path.
    let mut examined: Vec<(String, i64)> = Vec::new();
    for t in &targets {
        stats.scanned += 1;
        let summary = t.summary.trim();
        if summary.is_empty() {
            // Defensive: stale_embedding_targets's SQL filter already
            // excludes these, but keep the skip path so a future schema
            // drift cannot reintroduce the infinite-loop bug.
            log::warn!("cortex::embed: skipping note {} (empty summary)", t.note_path);
            stats.skipped_empty += 1;
            examined.push((t.note_path.clone(), t.modified_at));
            continue;
        }
        // Phase 7a + Phase 9: assemble the embed text as
        // `title` + `capture_note` + `summary`, each non-empty segment joined
        // by a blank line. The title carries strong topical signal; the
        // capture note ("why I captured this") makes the operator's own words
        // semantically searchable.
        //
        // BYTE-IDENTICAL INVARIANT (Phase 9): a note WITHOUT a capture note
        // must produce the exact pre-Phase-9 text (`title\n\nsummary`, or the
        // bare summary when the title is empty) so the staleness watermark
        // does not treat every existing note as changed and re-embed the whole
        // vault. Because empty segments are dropped before the join, an empty
        // capture note contributes nothing and the result is unchanged.
        let title = t.title.trim();
        let capture = t.capture_note.trim();
        let mut segments: Vec<&str> = Vec::with_capacity(3);
        if !title.is_empty() {
            segments.push(title);
        }
        if !capture.is_empty() {
            segments.push(capture);
        }
        segments.push(summary);
        let text = segments.join("\n\n");
        work.push(EmbedWork {
            note_path: t.note_path.clone(),
            text,
            source_modified_at: t.modified_at,
        });
    }
    if !examined.is_empty() {
        index.mark_embedding_examined_batch(EmbeddingKind::Summary, model_version, &examined)?;
    }
    if work.is_empty() {
        return Ok(stats);
    }

    // ---- 2. INFERENCE PHASE (no SQLite contact). ----
    // Sub-batch the inputs so any one `embed_batch` call sees at most
    // `max_chunks_per_call` strings. With batch_size=64 summaries this
    // is usually a no-op (one sub-batch); the cap matters mostly for
    // the transcript path. Any failure aborts the whole tick - vectors
    // from earlier sub-batches are discarded so the write phase's
    // `vectors.len() == work.len()` invariant holds. Stale rows retry
    // next tick.
    let texts: Vec<&str> = work.iter().map(|w| w.text.as_str()).collect();
    let vectors = match embed_in_sub_batches(model, &texts, max_chunks_per_call) {
        Ok(v) => v,
        Err(e) => {
            log::error!("cortex::embed: embed_batch failed: {e}");
            stats.failed += work.len() as u64;
            return Ok(stats);
        }
    };
    if vectors.len() != work.len() {
        log::error!(
            "cortex::embed: embed_batch returned {} vectors for {} inputs",
            vectors.len(),
            work.len(),
        );
        stats.failed += work.len() as u64;
        return Ok(stats);
    }

    // ---- 3. WRITE PHASE (one short transaction). ----
    let items: Vec<BatchUpsert<'_>> = work
        .iter()
        .zip(vectors.iter())
        .map(|(w, v)| BatchUpsert {
            note_path: &w.note_path,
            kind: EmbeddingKind::Summary,
            chunk_index: 0,
            text: &w.text,
            embedding: v,
            model_version,
            source_modified_at: w.source_modified_at,
        })
        .collect();
    let scanned_count = work.len();
    index.upsert_embeddings_batch(&items)?;
    stats.embedded += scanned_count as u64;
    Ok(stats)
}

/// Phase B2: one batch of transcript chunks. Each transcript-eligible
/// note expands into N chunks via `vault::embedding::chunk_transcript`;
/// the chunks are embedded in one batch and flushed via
/// `swap_transcript_chunks`, which DELETE/INSERTs atomically per note
/// to avoid leaving a half-replaced chunk set visible to hybrid search.
///
/// Phase B2's batch_size bounds the number of *notes* processed, not
/// the number of chunks. A pathologically long transcript will still
/// be flushed in one short transaction (delete + N inserts); the
/// per-note CPU is dominated by chunk_count * inference latency, which
/// is bounded by the chunker's max_tokens.
fn process_transcript_batch(
    index: &mut SearchIndex,
    model: &dyn EmbeddingModel,
    model_version: &str,
    vault_root: &Path,
    batch_size: usize,
    max_chunks_per_call: usize,
) -> Result<EmbedStats> {
    let mut stats = EmbedStats::default();

    // ---- 1. READ PHASE. ----
    // The note_type filter inside stale_embedding_targets is load-
    // bearing here: without it, every Article and Repo in the vault
    // matches `e.id IS NULL` forever and this loop spins.
    let targets = index.stale_embedding_targets(EmbeddingKind::TranscriptChunk, model_version, batch_size as u32)?;
    if targets.is_empty() {
        return Ok(stats);
    }
    log::debug!("cortex::embed::process_transcript_batch: scanned={}", targets.len());

    let mut work: Vec<TranscriptWork> = Vec::with_capacity(targets.len());
    // Notes scanned but found unembeddable (no `## Transcript` section, or a
    // section that chunks to nothing). Recording their indexed modified_at in
    // the examined sentinel is what stops the ~127-note transcript re-scan
    // every tick (Phase 3): without it, `e.id` stays NULL and the note is
    // re-selected forever.
    let mut examined: Vec<(String, i64)> = Vec::new();
    for t in &targets {
        stats.scanned += 1;
        let abs = vault_root.join(&t.note_path);
        let transcript = match read_section_text(&abs, "## Transcript") {
            Some(s) if !s.trim().is_empty() => s,
            _ => {
                log::warn!(
                    "cortex::embed: skipping {} (no ## Transcript section or empty)",
                    t.note_path
                );
                stats.skipped_empty += 1;
                examined.push((t.note_path.clone(), t.modified_at));
                continue;
            }
        };
        let chunks = vault::embedding::chunk_transcript(&transcript, CHUNK_MAX_TOKENS, CHUNK_OVERLAP_TOKENS);
        if chunks.is_empty() {
            log::warn!("cortex::embed: chunk_transcript produced 0 chunks for {}", t.note_path);
            stats.skipped_empty += 1;
            examined.push((t.note_path.clone(), t.modified_at));
            continue;
        }
        work.push(TranscriptWork {
            note_path: t.note_path.clone(),
            chunks,
            source_modified_at: t.modified_at,
        });
    }
    // Persist the examined sentinel before any early return so the skipped
    // notes leave the stale set until their indexed modified_at advances.
    if !examined.is_empty() {
        index.mark_embedding_examined_batch(EmbeddingKind::TranscriptChunk, model_version, &examined)?;
    }
    if work.is_empty() {
        return Ok(stats);
    }

    // ---- 2. INFERENCE PHASE. ----
    // One flat batch of chunks across all notes - amortizes the model
    // call. The per-note grouping happens on the way out. Sub-batched
    // at `max_chunks_per_call` to bound peak activation memory inside
    // candle's 8-replica rayon fan-out; an unbounded flat batch of
    // thousands of chunks would allocate tens of GB and OOM-kill the
    // daemon (observed 2026-05-19). Any failure aborts the whole tick;
    // the write phase below depends on
    // `flat_vectors.len() == flat.len()` for cursor alignment.
    let flat: Vec<&str> = work.iter().flat_map(|w| w.chunks.iter().map(|s| s.as_str())).collect();
    let flat_vectors = match embed_in_sub_batches(model, &flat, max_chunks_per_call) {
        Ok(v) => v,
        Err(e) => {
            log::error!("cortex::embed: embed_batch failed for transcripts: {e}");
            stats.failed += flat.len() as u64;
            return Ok(stats);
        }
    };
    if flat_vectors.len() != flat.len() {
        log::error!(
            "cortex::embed: embed_batch returned {} vectors for {} chunk inputs",
            flat_vectors.len(),
            flat.len(),
        );
        stats.failed += flat.len() as u64;
        return Ok(stats);
    }

    // ---- 3. WRITE PHASE. ----
    // One short transaction per note (DELETE + N INSERTs). The
    // transactions are sequential so the lock window per note stays
    // bounded; the alternative (one transaction across all notes)
    // would extend the write lock for the whole batch.
    let mut cursor = 0;
    for w in &work {
        let n = w.chunks.len();
        let mut pairs: Vec<(String, Vec<f32>)> = Vec::with_capacity(n);
        for i in 0..n {
            pairs.push((w.chunks[i].clone(), flat_vectors[cursor + i].clone()));
        }
        cursor += n;
        if let Err(e) = index.swap_transcript_chunks(&w.note_path, &pairs, model_version, w.source_modified_at) {
            log::error!("cortex::embed: transcript chunk swap failed for {}: {e}", w.note_path);
            stats.failed += n as u64;
            continue;
        }
        stats.embedded += n as u64;
    }
    Ok(stats)
}

/// Phase 9: one batch of claim embeddings. Reads `notes.claims` from the
/// column (carried in `StaleTarget.summary` by the Claim arm of
/// `stale_embedding_targets` - NO file I/O, same discipline as the summary
/// path), groups the newline-joined claims into token-window-sized chunks,
/// embeds them in one flat sub-batched call, and flushes each note's chunk
/// set atomically via `swap_kind_chunks`.
///
/// The grouping is load-bearing: a note can carry up to 24 claims, whose
/// joined text can exceed bge-small's 512-token window. Passing that as one
/// string would make the model silently truncate the tail - dropping the
/// late claims, the exact defect this design removes. Splitting into
/// sub-window groups guarantees every claim is embedded.
fn process_claim_batch(
    index: &mut SearchIndex,
    model: &dyn EmbeddingModel,
    model_version: &str,
    batch_size: usize,
    max_chunks_per_call: usize,
) -> Result<EmbedStats> {
    let mut stats = EmbedStats::default();

    // ---- 1. READ PHASE (auto-commit, no transaction; no file I/O). ----
    let targets = index.stale_embedding_targets(EmbeddingKind::Claim, model_version, batch_size as u32)?;
    if targets.is_empty() {
        return Ok(stats);
    }
    log::debug!("cortex::embed::process_claim_batch: scanned={}", targets.len());

    let mut work: Vec<TranscriptWork> = Vec::with_capacity(targets.len());
    // Defensive examined-sentinel accumulator (Phase 3), symmetric with the
    // summary and transcript arms; normally empty because the SQL filter
    // already excludes empty-claims notes.
    let mut examined: Vec<(String, i64)> = Vec::new();
    for t in &targets {
        stats.scanned += 1;
        // The Claim arm selects `notes.claims` into the `summary` field.
        let groups = group_claims(&t.summary, CLAIM_GROUP_MAX_WORDS);
        if groups.is_empty() {
            // Defensive: the SQL filter already excludes empty-claims notes,
            // but keep the skip path so a schema drift cannot reintroduce the
            // infinite-loop bug (a scanned-but-never-embedded note).
            log::warn!("cortex::embed: skipping {} (no claims text)", t.note_path);
            stats.skipped_empty += 1;
            examined.push((t.note_path.clone(), t.modified_at));
            continue;
        }
        work.push(TranscriptWork {
            note_path: t.note_path.clone(),
            chunks: groups,
            source_modified_at: t.modified_at,
        });
    }
    if !examined.is_empty() {
        index.mark_embedding_examined_batch(EmbeddingKind::Claim, model_version, &examined)?;
    }
    if work.is_empty() {
        return Ok(stats);
    }

    // ---- 2. INFERENCE PHASE (no SQLite contact). ----
    let flat: Vec<&str> = work.iter().flat_map(|w| w.chunks.iter().map(|s| s.as_str())).collect();
    let flat_vectors = match embed_in_sub_batches(model, &flat, max_chunks_per_call) {
        Ok(v) => v,
        Err(e) => {
            log::error!("cortex::embed: embed_batch failed for claims: {e}");
            stats.failed += flat.len() as u64;
            return Ok(stats);
        }
    };
    if flat_vectors.len() != flat.len() {
        log::error!(
            "cortex::embed: embed_batch returned {} vectors for {} claim inputs",
            flat_vectors.len(),
            flat.len(),
        );
        stats.failed += flat.len() as u64;
        return Ok(stats);
    }

    // ---- 3. WRITE PHASE (one short transaction per note). ----
    let mut cursor = 0;
    for w in &work {
        let n = w.chunks.len();
        let mut pairs: Vec<(String, Vec<f32>)> = Vec::with_capacity(n);
        for i in 0..n {
            pairs.push((w.chunks[i].clone(), flat_vectors[cursor + i].clone()));
        }
        cursor += n;
        if let Err(e) = index.swap_kind_chunks(
            &w.note_path,
            EmbeddingKind::Claim,
            &pairs,
            model_version,
            w.source_modified_at,
        ) {
            log::error!("cortex::embed: claim chunk swap failed for {}: {e}", w.note_path);
            stats.failed += n as u64;
            continue;
        }
        stats.embedded += n as u64;
    }
    Ok(stats)
}

/// Group a note's newline-joined claim text into chunks whose word count
/// stays under `max_words`, so no single embedding input overruns the
/// model's token window (bge-small: 512 tokens ≈ 400 words at the ~0.75
/// word:token ratio the chunker assumes). Each returned chunk is its member
/// claims re-joined with `\n`.
///
/// A single claim that alone exceeds the budget becomes its own chunk: the
/// model truncates that one pathological claim, but no *later* claim is
/// silently dropped - which is the whole point (dropping tail claims is the
/// Phase 9 defect; truncating one overlong sentence is acceptable and rare).
/// Returns an empty Vec when the input is blank.
fn group_claims(claims_text: &str, max_words: usize) -> Vec<String> {
    let claims: Vec<&str> = claims_text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if claims.is_empty() {
        return Vec::new();
    }
    let mut groups: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut current_words = 0usize;
    for claim in claims {
        let words = claim.split_whitespace().count();
        if !current.is_empty() && current_words + words > max_words {
            groups.push(current.join("\n"));
            current.clear();
            current_words = 0;
        }
        current.push(claim);
        current_words += words;
    }
    if !current.is_empty() {
        groups.push(current.join("\n"));
    }
    groups
}

/// Word budget per claim embedding group. Matches [`CHUNK_MAX_TOKENS`] (the
/// transcript chunker's per-chunk word budget) so both paths share the same
/// "what fits in one embedding input" notion against bge-small's 512-token
/// window.
const CLAIM_GROUP_MAX_WORDS: usize = 400;

/// Chunk window defaults. The design pins these; future tuning happens
/// here, not via per-call args, because the stale-target query and the
/// chunker share the same notion of "what fits in one chunk."
const CHUNK_MAX_TOKENS: usize = 400;
const CHUNK_OVERLAP_TOKENS: usize = 50;

/// Call `embed_batch` repeatedly with at most `max_chunks_per_call`
/// inputs at a time, preserving input order. A `max_chunks_per_call`
/// of 0 is treated as "no cap" (one call with the full input).
///
/// Any single sub-batch failure aborts the whole sequence and
/// propagates the error. Vectors from earlier successful sub-batches
/// in this call are discarded - callers depend on
/// `result.len() == texts.len()` and a partial result would break
/// downstream cursor alignment (see `process_transcript_batch`).
fn embed_in_sub_batches(
    model: &dyn EmbeddingModel,
    texts: &[&str],
    max_chunks_per_call: usize,
) -> Result<Vec<Vec<f32>>> {
    log::debug!(
        "cortex::embed::embed_in_sub_batches: total={} max_per_call={}",
        texts.len(),
        max_chunks_per_call,
    );
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let cap = if max_chunks_per_call == 0 { texts.len() } else { max_chunks_per_call };
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
    for sub in texts.chunks(cap) {
        let part = model.embed_batch(sub)?;
        if part.len() != sub.len() {
            eyre::bail!(
                "embed_batch returned {} vectors for {} inputs in sub-batch",
                part.len(),
                sub.len(),
            );
        }
        out.extend(part);
    }
    Ok(out)
}

struct EmbedWork {
    note_path: String,
    text: String,
    source_modified_at: i64,
}

struct TranscriptWork {
    note_path: String,
    chunks: Vec<String>,
    source_modified_at: i64,
}

/// Read a single `## Heading` section out of a vault file. Returns
/// `None` when the file is missing/unreadable or the section is absent;
/// callers log + skip. `header` must include the leading `## ` and
/// match the anchor exactly (case-sensitive).
fn read_section_text(abs: &Path, header: &str) -> Option<String> {
    let body = std::fs::read_to_string(abs).ok()?;
    let mut lines = body.lines();
    while let Some(line) = lines.next() {
        if line.trim() == header {
            let mut collected = String::new();
            for next in lines.by_ref() {
                if next.starts_with("## ") {
                    break;
                }
                collected.push_str(next);
                collected.push('\n');
            }
            return Some(collected.trim().to_string());
        }
    }
    None
}

/// Try to acquire the embed file lock. The lock prevents the cortex
/// daemon's embed tick from racing an ad-hoc `cortex embed` invocation.
///
/// `pub` so the graph pass (`cortex::graph`) can take the *same* lock and
/// thereby serialize against any concurrent `cortex embed` write — the graph
/// pass reads `note_embeddings` and must not interleave with an embed batch
/// rewriting them.
pub fn acquire_lock() -> Result<EmbedLock> {
    let path = lock_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).wrap_err("failed to create lock dir")?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .wrap_err_with(|| format!("failed to open embed lock file at {}", path.display()))?;
    // Surface a TYPED contention error (downcastable through eyre) so callers
    // detect "lock held" via `EmbedLockHeld` instead of substring-matching the
    // error message — a brittle test that any reworded wrap_err would break.
    file.try_lock_exclusive()
        .map_err(|_| EmbedLockHeld { path: path.clone() })?;
    Ok(EmbedLock { _file: file })
}

/// The embed lock is held by another process/tick. A marker error (not a
/// message substring) so the daemon's "skip this tick" branch is type-checked.
/// Hand-rolled rather than pulling `thiserror` into cortex (which is otherwise
/// eyre-only); it stays downcastable from `eyre::Report`.
#[derive(Debug)]
pub struct EmbedLockHeld {
    pub path: PathBuf,
}

impl std::fmt::Display for EmbedLockHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "embed lock held by another process: {}", self.path.display())
    }
}

impl std::error::Error for EmbedLockHeld {}

fn lock_path() -> PathBuf {
    // Under the sb/ data namespace via vault::paths (was the raw
    // ~/.local/share/cortex/embed.lock, outside sb/).
    vault::paths::cortex_lock_path()
}

/// RAII guard that releases the file lock on drop.
pub struct EmbedLock {
    _file: std::fs::File,
}

impl Drop for EmbedLock {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

/// Convenience helper: how long the daemon should sleep between embed
/// ticks. Reads `embed.cadence-secs` (default [`DEFAULT_CADENCE_SECS`]).
pub fn daemon_cadence(config: &Config) -> Duration {
    Duration::from_secs(config.embed.cadence_secs)
}

#[cfg(test)]
mod tests;
