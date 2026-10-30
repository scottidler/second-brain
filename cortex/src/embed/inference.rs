//! The dedicated inference pool and the sub-batched model call every embed
//! kind funnels through.

use eyre::Result;
use vault::embedding::EmbeddingModel;

use crate::shutdown::StopHandle;

/// The dedicated inference pool, built once per process.
///
/// Every model call goes through [`in_inference_pool`], which installs this
/// pool for the duration of the call. Candle's matmul fans out on rayon, and
/// `ThreadPool::install` reparents that fan-out onto THIS pool instead of the
/// global one - so inference physically cannot consume the workers that
/// `cortex::quality`, `classify`, and the lint passes run
/// their `par_iter` on.
///
/// This is the invariant the 2026-08-16 incident was missing. Candle inference on
/// an AVX-only host (`talos`) is slow and will stay slow (no vectorized gemm microkernel
/// exists for a CPU without FMA); the defect was never the latency, it was
/// that the latency was load-bearing for governance. Slow is acceptable.
/// Slow-and-shared is not.
static INFERENCE_POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();

/// Run `f` on the dedicated inference pool.
///
/// `workers` is `embed.workers`; 0 falls back to a small fixed default rather
/// than `available_parallelism`, because "as many replicas as cores" is the
/// setting that caused the contention in the first place.
pub(super) fn in_inference_pool<T, F: FnOnce() -> T + Send>(workers: usize, f: F) -> T
where
    T: Send,
{
    let requested = if workers == 0 { DEFAULT_INFERENCE_WORKERS } else { workers };
    let pool = INFERENCE_POOL.get_or_init(|| {
        log::info!("cortex::embed: building dedicated inference pool: threads={requested}");
        rayon::ThreadPoolBuilder::new()
            .num_threads(requested)
            .thread_name(|i| format!("embed-infer-{i}"))
            .build()
            .expect("failed to build the embed inference thread pool")
    });
    // The pool is process-wide and built once, so the FIRST caller's size wins.
    // Say so out loud instead of silently ignoring a later value: a config knob
    // that quietly does nothing is worse than one that is loudly pinned.
    let actual = pool.current_num_threads();
    if actual != requested {
        log::warn!(
            "cortex::embed: inference pool already built with {actual} thread(s); ignoring request for {requested} \
             (embed.workers is read once per process - restart to change it)"
        );
    }
    pool.install(f)
}

/// Pool size when `embed.workers` is 0. Deliberately small and NOT derived
/// from core count: on a scalar-kernel host extra replicas do not convert into
/// throughput, they just widen the window where inference is holding threads.
pub const DEFAULT_INFERENCE_WORKERS: usize = 2;

/// What a sub-batched inference run produced.
pub(super) enum Inference {
    /// One vector per input, in input order.
    Vectors(Vec<Vec<f32>>),
    /// The stop flag was seen before a sub-batch; the vectors so far are
    /// dropped, because the write phase needs all of them or none.
    Stopped,
}

/// Call `embed_batch` repeatedly with at most `max_chunks_per_call`
/// inputs at a time, preserving input order. A `max_chunks_per_call`
/// of 0 is treated as "no cap" (one call with the full input).
///
/// `stop` is checked before every sub-batch: one sub-batch is one model call
/// and cannot be interrupted, so the gaps between calls are the check points.
///
/// Any single sub-batch failure aborts the whole sequence and
/// propagates the error. Vectors from earlier successful sub-batches
/// in this call are discarded - callers depend on
/// `result.len() == texts.len()` and a partial result would break
/// downstream cursor alignment (see `process_transcript_batch`).
pub(super) fn embed_in_sub_batches(
    model: &dyn EmbeddingModel,
    texts: &[&str],
    max_chunks_per_call: usize,
    workers: usize,
    stop: &StopHandle,
) -> Result<Inference> {
    log::debug!(
        "cortex::embed::embed_in_sub_batches: total={} max_per_call={} workers={}",
        texts.len(),
        max_chunks_per_call,
        workers,
    );
    if texts.is_empty() {
        return Ok(Inference::Vectors(Vec::new()));
    }
    let cap = if max_chunks_per_call == 0 { texts.len() } else { max_chunks_per_call };
    let total = texts.len();
    let started = std::time::Instant::now();
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(total);
    for (i, sub) in texts.chunks(cap).enumerate() {
        if stop.is_stopped() {
            log::info!(
                "cortex::embed: stop requested after {}/{} chunk(s); dropping this batch's vectors",
                out.len(),
                total,
            );
            return Ok(Inference::Stopped);
        }
        // EVERY model call is reparented onto the dedicated pool. Doing it
        // here, at the single choke point all three kind-batches funnel
        // through, is what makes the isolation total: a future caller cannot
        // forget to wrap it.
        let part = in_inference_pool(workers, || model.embed_batch(sub))?;
        if part.len() != sub.len() {
            eyre::bail!(
                "embed_batch returned {} vectors for {} inputs in sub-batch",
                part.len(),
                sub.len(),
            );
        }
        out.extend(part);
        // Progress, not just start/finish. The two-day tick was invisible
        // precisely because this loop was silent: there was no way to tell a
        // wedged daemon from a slow one without attaching a debugger.
        log::info!(
            "cortex::embed: inference progress {}/{} chunk(s), sub-batch {} in {:?}",
            out.len(),
            total,
            i + 1,
            started.elapsed(),
        );
    }
    Ok(Inference::Vectors(out))
}
