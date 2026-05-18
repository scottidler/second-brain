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
use vault::embedding::{ACTIVE_MODEL_VERSION, EmbeddingModel, MockEmbedder, load_active_model};
use vault::search::{BatchUpsert, EmbeddingKind, SearchIndex};

use crate::cli::EmbedOpts;
use crate::config::Config;

/// Default batch size for the embed loop. Phase A5's transaction
/// discipline test asserts the write transaction wall-clock stays under
/// 200 ms at this batch.
pub const DEFAULT_BATCH_SIZE: usize = 64;

/// Default daemon cadence for the embed tick. Most ticks find zero
/// stale rows and return immediately; ~20 ingests/day plus user edits
/// leave only a handful of rows for a typical day.
pub const DEFAULT_CADENCE_SECS: u64 = 600;

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

/// CLI entry point for `cortex embed`.
pub fn run_embed(vault_root: &Path, config: &Config, opts: &EmbedOpts) -> Result<EmbedStats> {
    log::info!(
        "cortex::embed::run_embed: vault_root={} batch_size={} kind={:?} model={:?} prefetch_model={}",
        vault_root.display(),
        opts.batch_size,
        opts.kind,
        opts.model,
        opts.prefetch_model,
    );

    let db_path = config.oracle_db_path();
    let mut index = SearchIndex::open(&db_path)
        .wrap_err_with(|| format!("failed to open search index at {}", db_path.display()))?;

    let model_version = opts.model.clone().unwrap_or_else(|| {
        index
            .active_embedding_model()
            .unwrap_or_else(|_| ACTIVE_MODEL_VERSION.to_string())
    });

    if opts.prefetch_model {
        log::info!("cortex::embed: prefetching model {model_version}");
        vault::embedding::prefetch_active_model().wrap_err("failed to prefetch embedding model")?;
        println!("Prefetched embedding model {model_version}.");
        return Ok(EmbedStats::default());
    }

    let lock = acquire_lock()?;
    log::debug!("cortex::embed: acquired file lock");

    let model: Box<dyn EmbeddingModel> = if opts.use_mock {
        log::warn!("cortex::embed: using MockEmbedder (test-only)");
        Box::new(MockEmbedder::default_384())
    } else {
        log::info!(
            "cortex::embed: loading embedding model {model_version} workers={}",
            config.embed.workers
        );
        Box::new(load_active_model(config.embed.workers).wrap_err("failed to load embedding model")?)
    };

    // Make sure embedding_config matches the model we're about to write
    // so oracle's search_vector pulls compatible rows on the next query.
    index.set_active_embedding(model.model_version(), model.dim())?;

    let kinds = match opts.kind.as_deref() {
        Some("summary") => vec![EmbeddingKind::Summary],
        Some("transcript-chunk") => vec![EmbeddingKind::TranscriptChunk],
        Some(other) => eyre::bail!("unknown --kind {other:?}; expected summary | transcript-chunk"),
        // Phase B2 default: both summary and transcript-chunk. Cortex
        // walks summary first (one row per note, fast) then transcript
        // chunks (N rows per transcript-eligible note).
        None => vec![EmbeddingKind::Summary, EmbeddingKind::TranscriptChunk],
    };

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
            )?;
            total.merge(&batch_stats);
            if batch_stats.scanned == 0 {
                break;
            }
            // Termination guard against the all-skips-no-writes pattern:
            // if a batch scanned rows but wrote nothing (every target was
            // skipped because the file lacked the expected section), the
            // next batch will return the same targets - infinite loop.
            // Bail out so the loop cannot spin. The skipped notes will
            // simply remain "stale" until either (a) cortex grows a
            // skip-sentinel mechanism, or (b) the underlying notes gain
            // the missing content.
            if batch_stats.embedded == 0 && batch_stats.scanned == batch_stats.skipped_empty {
                log::warn!(
                    "cortex::embed: kind={:?} batch scanned={} all skipped; \
                     halting to avoid an infinite loop. Stale rows remain pending \
                     until the underlying notes gain the missing section.",
                    kind,
                    batch_stats.scanned,
                );
                break;
            }
        }
    }

    drop(lock);
    println!(
        "embed complete: scanned={} embedded={} skipped_empty={} failed={}",
        total.scanned, total.embedded, total.skipped_empty, total.failed,
    );
    Ok(total)
}

/// Daemon tick: runs one bounded sweep of the same embed loop on the
/// daemon's cadence interval. The lock makes ad-hoc `cortex embed`
/// invocations safe to run concurrently with the daemon (the second
/// instance exits cleanly).
pub fn daemon_tick(vault_root: &Path, config: &Config) -> Result<EmbedStats> {
    let default_opts = EmbedOpts {
        backfill: false,
        kind: None,
        model: None,
        batch_size: DEFAULT_BATCH_SIZE,
        prefetch_model: false,
        use_mock: false,
    };
    match run_embed(vault_root, config, &default_opts) {
        Ok(stats) => Ok(stats),
        Err(e) => {
            // Lock contention with an ad-hoc `cortex embed` invocation
            // is benign; downgrade to debug so the daemon log stays
            // quiet.
            if e.to_string().contains("embed lock") {
                log::debug!("cortex::embed::daemon_tick: lock held by another invocation; will retry next tick");
                Ok(EmbedStats::default())
            } else {
                Err(e)
            }
        }
    }
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
) -> Result<EmbedStats> {
    match kind {
        EmbeddingKind::Summary => process_summary_batch(index, model, model_version, vault_root, batch_size),
        EmbeddingKind::TranscriptChunk => process_transcript_batch(index, model, model_version, vault_root, batch_size),
    }
}

/// Phase A5: one batch of summary embeddings. Read auto-commit, embed
/// outside any transaction, flush in one short upsert.
fn process_summary_batch(
    index: &mut SearchIndex,
    model: &dyn EmbeddingModel,
    model_version: &str,
    _vault_root: &Path,
    batch_size: usize,
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
    for t in &targets {
        stats.scanned += 1;
        let text = t.summary.trim();
        if text.is_empty() {
            // Defensive: stale_embedding_targets's SQL filter already
            // excludes these, but keep the skip path so a future schema
            // drift cannot reintroduce the infinite-loop bug.
            log::warn!("cortex::embed: skipping note {} (empty summary)", t.note_path);
            stats.skipped_empty += 1;
            continue;
        }
        work.push(EmbedWork {
            note_path: t.note_path.clone(),
            text: text.to_string(),
            source_modified_at: t.modified_at,
        });
    }
    if work.is_empty() {
        return Ok(stats);
    }

    // ---- 2. INFERENCE PHASE (no SQLite contact). ----
    let texts: Vec<&str> = work.iter().map(|w| w.text.as_str()).collect();
    let vectors = match model.embed_batch(&texts) {
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
                continue;
            }
        };
        let chunks = vault::embedding::chunk_transcript(&transcript, CHUNK_MAX_TOKENS, CHUNK_OVERLAP_TOKENS);
        if chunks.is_empty() {
            log::warn!("cortex::embed: chunk_transcript produced 0 chunks for {}", t.note_path);
            stats.skipped_empty += 1;
            continue;
        }
        work.push(TranscriptWork {
            note_path: t.note_path.clone(),
            chunks,
            source_modified_at: t.modified_at,
        });
    }
    if work.is_empty() {
        return Ok(stats);
    }

    // ---- 2. INFERENCE PHASE. ----
    // One flat batch of chunks across all notes - amortizes the model
    // call. The per-note grouping happens on the way out.
    let flat: Vec<&str> = work.iter().flat_map(|w| w.chunks.iter().map(|s| s.as_str())).collect();
    let flat_vectors = match model.embed_batch(&flat) {
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

/// Chunk window defaults. The design pins these; future tuning happens
/// here, not via per-call args, because the stale-target query and the
/// chunker share the same notion of "what fits in one chunk."
const CHUNK_MAX_TOKENS: usize = 400;
const CHUNK_OVERLAP_TOKENS: usize = 50;

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
fn acquire_lock() -> Result<EmbedLock> {
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
    file.try_lock_exclusive()
        .wrap_err_with(|| format!("embed lock held by another process: {}", path.display()))?;
    Ok(EmbedLock { _file: file })
}

fn lock_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("cortex")
        .join("embed.lock")
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
/// ticks. Falls back to [`DEFAULT_CADENCE_SECS`] when the config does
/// not pin a value.
pub fn daemon_cadence(_config: &Config) -> Duration {
    Duration::from_secs(DEFAULT_CADENCE_SECS)
}

#[cfg(test)]
mod tests;
