//! The summary kind: one embedding per note over title + capture note +
//! summary.

use std::path::Path;

use eyre::Result;
use vault::embedding::EmbeddingModel;
use vault::search::{BatchUpsert, EmbeddingKind, SearchIndex};

use super::allowance::ChunkAllowance;
use super::inference::{Inference, embed_in_sub_batches};
use super::{BatchOutcome, EmbedStats};
use crate::shutdown::StopHandle;

/// Assemble the text a note's SUMMARY embedding is computed over: `title` +
/// `capture_note` + `summary`, each non-empty segment joined by a blank line.
/// The title carries strong topical signal; the capture note ("why I captured
/// this") makes the operator's own words semantically searchable.
///
/// BYTE-IDENTICAL INVARIANT: a note WITHOUT a capture note must
/// produce the same text as before capture notes (`title\n\nsummary`, or the bare summary
/// when the title is empty) or the staleness watermark treats every existing
/// note as changed and re-embeds the whole vault. Empty segments are dropped
/// before the join, so an empty capture note contributes nothing.
///
/// Public because it IS the retrieval contract's input: the hub-body renderer's
/// `## Summary` digest only reaches the embedding through this composition, and
/// the contract test asserts against the real thing rather than a copy of it.
pub fn summary_embed_text(title: &str, capture_note: &str, summary: &str) -> String {
    let title = title.trim();
    let capture = capture_note.trim();
    let summary = summary.trim();
    let mut segments: Vec<&str> = Vec::with_capacity(3);
    if !title.is_empty() {
        segments.push(title);
    }
    if !capture.is_empty() {
        segments.push(capture);
    }
    segments.push(summary);
    segments.join("\n\n")
}

/// one batch of summary embeddings. Read auto-commit, embed
/// outside any transaction, flush in one short upsert.
pub(super) fn process_summary_batch(
    index: &mut SearchIndex,
    model: &dyn EmbeddingModel,
    model_version: &str,
    _vault_root: &Path,
    batch_size: usize,
    max_chunks_per_call: usize,
    allowance: ChunkAllowance,
    workers: usize,
    stop: &StopHandle,
) -> Result<BatchOutcome> {
    let mut stats = EmbedStats::default();

    // ---- 1. READ PHASE (auto-commit, no transaction). ----
    // A summary is one embedding per note, so it draws 1 from the tick's
    // allowance per note: select no more notes than the allowance has left.
    let limit = allowance.note_limit(batch_size);
    let targets = index.stale_embedding_targets(EmbeddingKind::Summary, model_version, limit as u32)?;
    if targets.is_empty() {
        return Ok(BatchOutcome::Complete(stats));
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
    // Defensive examined-sentinel accumulator: the SQL filter already
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
        // Assemble the embed text as
        // `title` + `capture_note` + `summary`, each non-empty segment joined
        // by a blank line. The title carries strong topical signal; the
        // capture note ("why I captured this") makes the operator's own words
        // semantically searchable.
        //
        // BYTE-IDENTICAL INVARIANT: a note WITHOUT a capture note
        // must produce the same text as before capture notes (`title\n\nsummary`, or the
        // bare summary when the title is empty) so the staleness watermark
        // does not treat every existing note as changed and re-embed the whole
        // vault. Because empty segments are dropped before the join, an empty
        // capture note contributes nothing and the result is unchanged.
        let text = summary_embed_text(&t.title, &t.capture_note, summary);
        // A summary is one vector, not chunked: an oversize one is a distill
        // defect the encoder truncates, so say so once, by note and size.
        match model.count_tokens(&text) {
            Ok(tokens) if tokens > model.max_input_tokens() => log::warn!(
                "cortex::embed: summary text of {} is {tokens} tokens, over the {}-token window; only its head is embedded",
                t.note_path,
                model.max_input_tokens(),
            ),
            Ok(_) => {}
            Err(e) => {
                log::error!(
                    "cortex::embed: tokenizing summary of {} failed, note skipped: {e}",
                    t.note_path
                );
                stats.failed += 1;
                continue;
            }
        }
        work.push(EmbedWork {
            note_path: t.note_path.clone(),
            text,
            source_modified_at: t.modified_at,
        });
    }
    if !examined.is_empty() {
        index.mark_nothing_to_embed_batch(EmbeddingKind::Summary, model_version, &examined)?;
    }
    if work.is_empty() {
        return Ok(BatchOutcome::Complete(stats));
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
    let vectors = match embed_in_sub_batches(model, &texts, max_chunks_per_call, workers, stop) {
        Ok(Inference::Vectors(v)) => v,
        Ok(Inference::Stopped) => return Ok(BatchOutcome::Stopped(stats)),
        Err(e) => {
            log::error!("cortex::embed: embed_batch failed: {e}");
            stats.failed += work.len() as u64;
            return Ok(BatchOutcome::Complete(stats));
        }
    };
    if vectors.len() != work.len() {
        log::error!(
            "cortex::embed: embed_batch returned {} vectors for {} inputs",
            vectors.len(),
            work.len(),
        );
        stats.failed += work.len() as u64;
        return Ok(BatchOutcome::Complete(stats));
    }

    // ---- 3. WRITE PHASE (one short transaction). ----
    // A per-note swap, like the chunk kinds: each note's old summary rows
    // serve until this transaction replaces all of them.
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
    index.swap_embeddings_batch(&items)?;
    stats.embedded += scanned_count as u64;
    Ok(BatchOutcome::Complete(stats))
}

pub(super) struct EmbedWork {
    pub(super) note_path: String,
    pub(super) text: String,
    pub(super) source_modified_at: i64,
}
