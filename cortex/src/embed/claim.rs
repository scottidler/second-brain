//! The claim kind: a note's claims grouped into window-sized chunks and
//! swapped in per note.

use eyre::Result;
use vault::embedding::EmbeddingModel;
use vault::search::{EmbeddingKind, SearchIndex};

use super::allowance::{ChunkAllowance, TranscriptWork, cap_work_by_chunks};
use super::inference::{Inference, embed_in_sub_batches};
use super::{BatchOutcome, EmbedStats};
use crate::shutdown::StopHandle;

/// one batch of claim embeddings. Reads `notes.claims` from the
/// column (carried in `StaleTarget.summary` by the Claim arm of
/// `stale_embedding_targets` - NO file I/O, same discipline as the summary
/// path), groups the newline-joined claims into token-window-sized chunks,
/// embeds them in one flat sub-batched call, and flushes each note's chunk
/// set atomically via `swap_kind_chunks`.
///
/// The grouping is load-bearing: a note can carry up to 24 claims, whose
/// joined text can exceed bge-small's 512-token window. Passing that as one
/// string would make the model silently truncate the tail - dropping the
/// late claims. Grouping by the model's own token count guarantees every
/// claim is embedded whole.
pub(super) fn process_claim_batch(
    index: &mut SearchIndex,
    model: &dyn EmbeddingModel,
    model_version: &str,
    batch_size: usize,
    max_chunks_per_call: usize,
    allowance: ChunkAllowance,
    workers: usize,
    stop: &StopHandle,
) -> Result<BatchOutcome> {
    let mut stats = EmbedStats::default();

    // ---- 1. READ PHASE (auto-commit, no transaction; no file I/O). ----
    let targets = index.stale_embedding_targets(EmbeddingKind::Claim, model_version, batch_size as u32)?;
    if targets.is_empty() {
        return Ok(BatchOutcome::Complete(stats));
    }
    log::debug!("cortex::embed::process_claim_batch: scanned={}", targets.len());

    let mut work: Vec<TranscriptWork> = Vec::with_capacity(targets.len());
    // Defensive examined-sentinel accumulator, symmetric with the
    // summary and transcript arms; normally empty because the SQL filter
    // already excludes empty-claims notes.
    let mut examined: Vec<(String, i64)> = Vec::new();
    for t in &targets {
        stats.scanned += 1;
        // The Claim arm selects `notes.claims` into the `summary` field.
        let groups = match group_claims(model, &t.summary) {
            Ok(groups) => groups,
            Err(e) => {
                log::error!(
                    "cortex::embed: grouping claims of {} failed, note skipped: {e}",
                    t.note_path
                );
                stats.failed += 1;
                continue;
            }
        };
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
        index.mark_nothing_to_embed_batch(EmbeddingKind::Claim, model_version, &examined)?;
    }
    if work.is_empty() {
        return Ok(BatchOutcome::Complete(stats));
    }

    // ---- 2. INFERENCE PHASE (no SQLite contact). ----
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
            log::error!("cortex::embed: embed_batch failed for claims: {e}");
            stats.failed += flat.len() as u64;
            return Ok(finish(stats));
        }
    };
    if flat_vectors.len() != flat.len() {
        log::error!(
            "cortex::embed: embed_batch returned {} vectors for {} claim inputs",
            flat_vectors.len(),
            flat.len(),
        );
        stats.failed += flat.len() as u64;
        return Ok(finish(stats));
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
    Ok(finish(stats))
}

/// Group a note's newline-joined claim text into chunks that each encode to
/// at most the model's token window, so no embedding input is truncated.
/// Claims are packed in order; each group is its member claims re-joined
/// with `\n`, and every group is re-encoded as the exact string that will be
/// embedded before it is accepted.
///
/// A single claim that alone overruns the window is windowed by
/// [`EmbeddingModel::chunk`] into its own groups, so neither it nor any later
/// claim loses tokens. Returns an empty Vec when the input is blank, and the
/// tokenizer's error when it fails.
pub(super) fn group_claims(model: &dyn EmbeddingModel, claims_text: &str) -> Result<Vec<String>> {
    let limit = model.max_input_tokens();
    let mut groups: Vec<String> = Vec::new();
    let mut current = String::new();
    for claim in claims_text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if model.count_tokens(claim)? > limit {
            if !current.is_empty() {
                groups.push(std::mem::take(&mut current));
            }
            groups.extend(model.chunk("", claim)?);
            continue;
        }
        if current.is_empty() {
            current = claim.to_string();
            continue;
        }
        let candidate = format!("{current}\n{claim}");
        if model.count_tokens(&candidate)? <= limit {
            current = candidate;
        } else {
            groups.push(std::mem::replace(&mut current, claim.to_string()));
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }
    Ok(groups)
}
