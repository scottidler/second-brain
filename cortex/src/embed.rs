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
use vault::embedding::{BGE_SMALL_EN_V15_NAME, EmbeddingModel, FastEmbedModel, MockEmbedder};
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
            .unwrap_or_else(|_| BGE_SMALL_EN_V15_NAME.to_string())
    });

    if opts.prefetch_model {
        log::info!("cortex::embed: prefetching model {model_version}");
        let _ = FastEmbedModel::load().wrap_err("failed to prefetch fastembed model")?;
        println!("Prefetched embedding model {model_version}.");
        return Ok(EmbedStats::default());
    }

    let lock = acquire_lock()?;
    log::debug!("cortex::embed: acquired file lock");

    let model: Box<dyn EmbeddingModel> = if opts.use_mock {
        log::warn!("cortex::embed: using MockEmbedder (test-only)");
        Box::new(MockEmbedder::default_384())
    } else {
        log::info!("cortex::embed: loading fastembed model {model_version}");
        Box::new(FastEmbedModel::load().wrap_err("failed to load fastembed model")?)
    };

    // Make sure embedding_config matches the model we're about to write
    // so oracle's search_vector pulls compatible rows on the next query.
    index.set_active_embedding(model.model_version(), model.dim())?;

    let kinds = match opts.kind.as_deref() {
        Some("summary") => vec![EmbeddingKind::Summary],
        Some("transcript-chunk") => vec![EmbeddingKind::TranscriptChunk],
        Some(other) => eyre::bail!("unknown --kind {other:?}; expected summary | transcript-chunk"),
        // Phase A reads only summary rows; Phase B2 will extend the
        // default to include transcript-chunk.
        None => vec![EmbeddingKind::Summary],
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
    let mut stats = EmbedStats::default();

    // ---- 1. READ PHASE (auto-commit, no transaction). ----
    let targets = index.stale_embedding_targets(kind, model_version, batch_size as u32)?;
    if targets.is_empty() {
        return Ok(stats);
    }
    log::debug!(
        "cortex::embed::process_batch: scanned={} kind={:?}",
        targets.len(),
        kind,
    );

    let mut work: Vec<EmbedWork> = Vec::with_capacity(targets.len());
    for t in &targets {
        stats.scanned += 1;
        let abs = vault_root.join(&t.note_path);
        let text = match read_summary_text(&abs) {
            Some(s) if !s.trim().is_empty() => s,
            _ => {
                log::warn!(
                    "cortex::embed: skipping note {} (no summary text or empty)",
                    t.note_path
                );
                stats.skipped_empty += 1;
                continue;
            }
        };
        work.push(EmbedWork {
            note_path: t.note_path.clone(),
            text,
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
    // upsert_embeddings_batch wraps BEGIN IMMEDIATE / COMMIT around a
    // pure-SQL loop. No CPU work happens between BEGIN and COMMIT, so
    // the lock window stays brief regardless of batch size. Phase A5's
    // regression test asserts this wall-clock budget.
    let items: Vec<BatchUpsert<'_>> = work
        .iter()
        .zip(vectors.iter())
        .map(|(w, v)| BatchUpsert {
            note_path: &w.note_path,
            kind,
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

struct EmbedWork {
    note_path: String,
    text: String,
    source_modified_at: i64,
}

/// Read the `## Summary` section out of a vault file. Returns `None`
/// when the file is missing or unreadable; callers log + skip.
fn read_summary_text(abs: &Path) -> Option<String> {
    let body = std::fs::read_to_string(abs).ok()?;
    let mut lines = body.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "## Summary" {
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
