//! The transcript-chunk kind: a note's transcript windowed into chunks and
//! swapped in per note.

use std::path::Path;
use std::str::FromStr;

use eyre::Result;
use vault::embedding::EmbeddingModel;
use vault::schema::NoteType;
use vault::search::{EmbeddingKind, SearchIndex};

use super::allowance::{ChunkAllowance, TranscriptWork, cap_work_by_chunks};
use super::inference::{Inference, embed_in_sub_batches};
use super::source::{SourceRoots, read_source_transcript, revive_archived_sentinels, sentinel_watermark};
use super::{BatchOutcome, EmbedStats};
use crate::shutdown::StopHandle;

/// one batch of transcript chunks. Each transcript-eligible
/// note expands into N chunks via `vault::embedding::chunk_transcript`;
/// the chunks are embedded in one batch and flushed via
/// `swap_transcript_chunks`, which DELETE/INSERTs atomically per note
/// to avoid leaving a half-replaced chunk set visible to hybrid search.
///
/// `batch_size` bounds the number of *notes* processed, not
/// the number of chunks. A pathologically long transcript will still
/// be flushed in one short transaction (delete + N inserts); the
/// per-note CPU is dominated by chunk_count * inference latency, which
/// is bounded by the chunker's max_tokens.
pub(super) fn process_transcript_batch(
    index: &mut SearchIndex,
    model: &dyn EmbeddingModel,
    model_version: &str,
    vault_root: &Path,
    roots: SourceRoots<'_>,
    batch_size: usize,
    max_chunks_per_call: usize,
    allowance: ChunkAllowance,
    workers: usize,
    stop: &StopHandle,
) -> Result<BatchOutcome> {
    log::debug!(
        "cortex::embed::process_transcript_batch: vault_root={} archive_root={} staging_root={} batch_size={}",
        vault_root.display(),
        roots.archive.display(),
        roots.staging.display(),
        batch_size,
    );
    let mut stats = EmbedStats::default();

    // Revive notes sentinelled for "no transcript source" whose archive
    // appeared after the sentinel was written (an archive backfill), so the
    // read below selects them again.
    revive_archived_sentinels(index, model_version, roots.archive)?;

    // ---- 1. READ PHASE. ----
    // The note_type filter inside stale_embedding_targets is load-
    // bearing here: without it, every Article and Repo in the vault
    // matches `e.id IS NULL` forever and this loop spins.
    let targets = index.stale_embedding_targets(EmbeddingKind::TranscriptChunk, model_version, batch_size as u32)?;
    if targets.is_empty() {
        return Ok(BatchOutcome::Complete(stats));
    }
    log::debug!("cortex::embed::process_transcript_batch: scanned={}", targets.len());

    let mut work: Vec<TranscriptWork> = Vec::with_capacity(targets.len());
    // Notes scanned but found unembeddable (no `## Transcript` section, or a
    // section that chunks to nothing). Recording their indexed modified_at in
    // the examined sentinel is what stops the ~127-note transcript re-scan
    // every tick: without it, `e.id` stays NULL and the note is
    // re-selected forever.
    let mut examined: Vec<(String, i64)> = Vec::new();
    for t in &targets {
        stats.scanned += 1;
        // Video/Youtube/Article (2026-07-07-distillation-output-restore) and
        // Social/Reddit (2026-10-19-x-post-fetcher) transcripts live in the
        // staged distilled.yml (resolved via notes.trace), NOT the note body.
        // The verbatim-preservation kinds (Image/Audio/Note/Vocab) keep their
        // in-note `## Transcript` section.
        let from_staging = NoteType::from_str(&t.note_type)
            .map(|nt| nt.transcript_from_staging())
            .unwrap_or(false);
        let transcript = if from_staging {
            read_source_transcript(roots, &t.trace, &t.note_path)
        } else {
            let abs = vault_root.join(&t.note_path);
            read_section_text(&abs, "## Transcript")
        };
        let transcript = match transcript {
            Some(s) if !s.trim().is_empty() => s,
            _ => {
                // Missing/expired/malformed staged file OR absent in-note section:
                // record the examined sentinel keyed on the NOTE's indexed
                // modified_at (never on the staged file) so the note leaves the
                // stale set until it is re-modified or the model version bumps.
                // The per-source WARN was already emitted at the resolution site.
                log::warn!(
                    "cortex::embed: skipping {} (no transcript source; from_staging={from_staging})",
                    t.note_path
                );
                stats.skipped_empty += 1;
                let watermark = if from_staging {
                    sentinel_watermark(t.modified_at, roots.archive, &t.trace)
                } else {
                    t.modified_at
                };
                examined.push((t.note_path.clone(), watermark));
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
        index.mark_nothing_to_embed_batch(EmbeddingKind::TranscriptChunk, model_version, &examined)?;
    }
    if work.is_empty() {
        return Ok(BatchOutcome::Complete(stats));
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
    // Fit the batch to the tick's remaining chunk allowance by dropping whole
    // notes off the tail.
    // Truncating at a NOTE boundary (never mid-note) is load-bearing: the write
    // phase replaces a note's ENTIRE chunk set, so a half-embedded note would
    // land a truncated set and then read as complete. Dropped notes keep their
    // stale marker and lead the next tick.
    let selected = work.len();
    let work = cap_work_by_chunks(work, allowance);
    let finish = if work.len() < selected {
        BatchOutcome::AllowanceSpent
    } else {
        BatchOutcome::Complete
    };
    if work.is_empty() {
        return Ok(finish(stats));
    }
    let flat: Vec<&str> = work.iter().flat_map(|w| w.chunks.iter().map(|s| s.as_str())).collect();
    let flat_vectors = match embed_in_sub_batches(model, &flat, max_chunks_per_call, workers, stop) {
        Ok(Inference::Vectors(v)) => v,
        Ok(Inference::Stopped) => return Ok(BatchOutcome::Stopped(stats)),
        Err(e) => {
            log::error!("cortex::embed: embed_batch failed for transcripts: {e}");
            stats.failed += flat.len() as u64;
            return Ok(finish(stats));
        }
    };
    if flat_vectors.len() != flat.len() {
        log::error!(
            "cortex::embed: embed_batch returned {} vectors for {} chunk inputs",
            flat_vectors.len(),
            flat.len(),
        );
        stats.failed += flat.len() as u64;
        return Ok(finish(stats));
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
    Ok(finish(stats))
}

/// Chunk window defaults. The design pins these; future tuning happens
/// here, not via per-call args, because the stale-target query and the
/// chunker share the same notion of "what fits in one chunk."
pub(super) const CHUNK_MAX_TOKENS: usize = 400;
pub(super) const CHUNK_OVERLAP_TOKENS: usize = 50;

/// Read a single `## Heading` section out of a vault file. Returns
/// `None` when the file is missing/unreadable or the section is absent;
/// callers log + skip. `header` must include the leading `## ` and
/// match the anchor exactly (case-sensitive).
pub(super) fn read_section_text(abs: &Path, header: &str) -> Option<String> {
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
