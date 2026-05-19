# Cortex Embed Memory Bounding

**Date:** 2026-05-19
**Status:** Proposed
**Author:** Scott (with claude)
**Tracking:** [[project-cortex-embed-memory-leak]]

## Problem

The cortex embed loop has two compounding memory issues that surfaced on 2026-05-19 after the L2 distillation backfill added 321 new notes with `## Transcript` sections to the vault:

**Catastrophic (H2): unbounded flat fan-out under big backlogs.** A single `embed_batch` call processes thousands of transcript chunks at once, triggering 8× rayon-parallel BERT inference that peaks at tens of GB.

**Persistent (H1): per-tick model load growth.** The candle model is reloaded on every `run_embed` invocation. Even when the dropped model's heap is freed, glibc/jemalloc retain pages and the daemon's steady-state baseline grows hundreds of MB per cadence cycle.

## Evidence

| Event | When | Outcome |
|-------|------|---------|
| Prior cortex instance (PID 494057) | 2026-05-19 07:02:44 | OOM-killed by kernel. journal: `Consumed 1h 47min 30s CPU over 52min 59s wall clock, 88.9G memory peak, 914.4M swap peak`. |
| New cortex (PID 644708) | 07:02:55 | systemd auto-restart. |
| Embed tick processing backlog | 07:29 - 07:47:16 | One tick: `embed complete: scanned=610 embedded=1673 skipped_empty=122 failed=0`. **Peak 73.3 GB**. |
| Backlog drained | 07:47:46+ | Every tick: `scanned=64 embedded=0`. No further pressure. |
| Steady-state observed before manual restart | ~08:34 | 1.9 GB resident, ~30x the post-restart baseline. |

**Post-restart MemoryCurrent poll (clean cortex, no backlog):**

```
08:39  70 MB
08:43  70 MB   (flat, no ticks)
08:44  86 MB   (tick, +16 MB)
08:49  542 MB  (tick, +460 MB)
08:54  545 MB  (tick, +25 MB)
08:58  520 MB  (settled, +450 MB net)
```

Baseline drifted 70 MB -> 520 MB in 20 minutes with zero embedding work. Confirms H1 is real, not just allocator retention.

## Root Cause

### H2: Flat fan-out in `process_transcript_batch`

`cortex/src/embed.rs:380-381`:

```rust
let flat: Vec<&str> = work.iter().flat_map(|w| w.chunks.iter().map(|s| s.as_str())).collect();
let flat_vectors = match model.embed_batch(&flat) { ... };
```

`work` is up to `batch_size = 64` notes. Each note's `chunks` field (from `vault::embedding::chunk_transcript`) can be ~50 chunks for a long video transcript. `flat` can therefore be ~3,000 strings, all passed to `embed_batch` in one call.

`vault/src/embedding/candle.rs:171` then fans `flat` across 8 BERT replicas via `par_iter`:

```rust
let sub_chunk_size = n.div_ceil(self.replicas.len()).max(1);  // ~400 for 3,000 / 8
let chunks: Vec<&[&str]> = texts.chunks(sub_chunk_size).collect();
let mut indexed: Vec<(usize, Result<Vec<Vec<f32>>>)> = chunks.par_iter().enumerate().map(...).collect();
```

Each of 8 worker threads runs BERT forward on ~400 sequences simultaneously. Peak activation memory per replica is roughly `sub_chunk_size × MAX_SEQ_LEN(512) × DIM(384) × layers(12) × 4 bytes` ≈ 3.8 GB. Multiplied by 8 concurrent replicas, peak activations are ~30 GB. Add tokenizer scratch, candle's allocator behavior, and intermediate hidden states the kernel hasn't released, and 73-88 GB is what the journal records.

### H1: Per-tick model load

`cortex/src/embed.rs:115`:

```rust
Box::new(load_active_model(config.embed.workers).wrap_err("failed to load embedding model")?)
```

This is inside `run_embed`, which `daemon_tick` (line 180) calls on every cadence interval (`DEFAULT_CADENCE_SECS = 600` -> every 10 min). Each call builds N BERT graph replicas via `build_inner` per replica - not a shared mmap, an independent allocation per replica. Even after the `Box<dyn EmbeddingModel>` drops at end of `run_embed`, the system allocator retains pages.

## Proposed Fix

### Primary: Bound embed_batch input at the caller

Cap the flat batch in `process_transcript_batch` so a single `embed_batch` call processes at most `max_chunks_per_call` strings. Loop over sub-batches sequentially. **Any sub-batch failure aborts the entire tick** to keep alignment with the existing write-path cursor math at `embed.rs:404-417` (the cursor depends on `flat_vectors.len() == flat.len()`).

```rust
// cortex/src/embed.rs::process_transcript_batch
let flat: Vec<&str> = work.iter()
    .flat_map(|w| w.chunks.iter().map(|s| s.as_str()))
    .collect();

let max_chunks = config.embed.max_chunks_per_call;  // default 64
let mut flat_vectors: Vec<Vec<f32>> = Vec::with_capacity(flat.len());
for sub in flat.chunks(max_chunks) {
    match model.embed_batch(sub) {
        Ok(v) => {
            if v.len() != sub.len() {
                log::error!(
                    "cortex::embed: embed_batch returned {} vectors for {} inputs (sub-batch)",
                    v.len(),
                    sub.len(),
                );
                stats.failed += flat.len() as u64;
                return Ok(stats);
            }
            flat_vectors.extend(v);
        }
        Err(e) => {
            log::error!("cortex::embed: embed_batch failed for transcripts: {e}");
            stats.failed += flat.len() as u64;
            return Ok(stats);  // abort tick; stale rows retry next tick
        }
    }
}
```

This is **bit-identical to today's failure semantics**: any inference error or vector-count mismatch marks `stats.failed = flat.len()` and returns. Stale rows stay stale and the next tick re-reads them. Vectors from successful sub-batches in this tick are discarded; no partial commits to the DB. This is deliberate - the alternative (per-note sub-batching with partial commits) is a bigger refactor and changes failure semantics for unclear gain.

The same pattern applies to `process_summary_batch` for symmetry (though summaries are 1-per-note, less explosive).

**Why this fix:**

- Narrow blast radius: one file, two loops, one config field.
- Configurable cap; the user can tune up or down based on RAM.
- Candle stays unchanged - non-cortex callers (oracle's single-query path) keep the fast `embed_inner` short-circuit at `candle.rs:155`.
- The rayon fan-out still parallelizes within each sub-batch (8 replicas × ~8 chunks/replica per call), so throughput stays close.
- Lock window does not meaningfully change. Today's behavior holds the embed file lock for the full duration of `run_embed` (which can be tens of minutes for a big tick - see the 17-min tick on 2026-05-19 07:29-07:47). Sub-batching converts one ~17-min `embed_batch` into ~47 sequential ~22-sec calls; total wall time is roughly the same and the lock was already held across the whole tick.

### Deferred: Load model once at daemon startup

**Originally proposed as Phase 2; deferred to a follow-up design after empirical measurement.**

The idea: move `load_active_model` from `run_embed` to daemon startup so the model is loaded once and reused. This would eliminate the per-tick allocator churn we measured (70 MB -> 520 MB baseline drift in 20 min of idle ticks).

The risk that prevents shipping this now: candle's `CandleBertModel` has 8 BERT replicas, each behind a `Mutex<inner>`. We do not have evidence that each replica's internal scratch tensors (intermediate hidden states, attention buffers, tokenizer arenas) are released between calls vs. kept at the high-water mark of the longest sequence ever processed. If candle's per-instance state grows monotonically with the largest sequence seen, a long-lived `Daemon`-owned model will eventually allocate to that high-water mark and stay there - converting today's "drop and reallocate every 10 min" pattern (which at least bounds peak by tick) into "grow forever until daemon restart."

**Prerequisite measurement before re-proposing:**

Run a synthetic test that:
1. Constructs one `CandleBertModel` instance.
2. Calls `embed_batch` 1000+ times with varying sequence lengths (mix of short summaries and 512-token chunks).
3. Records RSS via `procfs` between every 100 calls.

If RSS plateaus near a flat baseline, candle internals are bounded and the long-lived model is safe. If RSS grows monotonically with call count or with high-water sequence length, the long-lived model amplifies the leak rather than fixing it.

**Why deferred:**

- The Primary fix alone solves the catastrophic OOM blocker. The baseline drift (450 MB / 20 min) is uncomfortable but not crash-causing for a daemon that restarts on otto deploy or system reboot.
- Without the measurement above, shipping the long-lived model is a structural bet on candle's internals being well-behaved across the full distribution of inputs we send it.
- The follow-up design will own the measurement, the result, and the lifecycle change together.

Tracked as a follow-up in [[project-cortex-embed-memory-leak]].

### Out of Scope

- **Changes to candle internals.** The candle `embed_inner` rayon fan-out at `candle.rs:171` is correct for the use case it was designed for (small batches from oracle queries, sub-second responsiveness). Bounding at the caller is the cleaner change. Revisit only if measurement after the primary fix still shows unacceptable peaks.
- **MAX_WORKERS reduction.** Currently clamped to 8 in `candle.rs:49`. The default of `min(8, parallelism)` is reasonable for the oracle query path. Cortex daemon could set workers=2 in config to reduce per-replica memory further, but that's tuning, not a design change.

## Configuration

New field in `cortex.yml`:

```yaml
embed:
  workers: 0                  # existing - 0 means default (clamped to MAX_WORKERS=8)
  max-chunks-per-call: 64     # NEW: bound on embed_batch input size
```

Default `max-chunks-per-call = 64` keeps the rayon fan-out happy (8 replicas × 8 chunks = 64) and bounds peak activation memory to ~7 GB worst case (down from 30+ GB).

## Regression Test

Add `cortex/src/embed/tests.rs::process_transcript_batch_bounds_calls_when_input_exceeds_cap`:

- Construct a `MockEmbedder` that records every `embed_batch` call's input length.
- Build a synthetic `TranscriptWork` set with 1,000+ chunks total.
- Invoke `process_transcript_batch` with `max_chunks_per_call = 64`.
- Assert: `embed_batch` was called `ceil(N / 64)` times, each with `input.len() <= 64`, and the returned vectors are in input order.

This catches regressions where a future refactor inlines the loop or removes the cap. RSS assertions are not practical in unit tests; call-count and per-call sizes are sufficient.

## Implementation Plan

Single phase. The deferred Phase 2 (model lifetime) is its own follow-up design contingent on measurement.

1. Add `max-chunks-per-call: u32` to `EmbedConfig` in `cortex/src/config.rs` (kebab-case YAML key, default 64).
2. Refactor `cortex/src/embed.rs::process_transcript_batch` to loop `flat.chunks(max_chunks_per_call)` with the failure semantics in the Primary section.
3. Refactor `cortex/src/embed.rs::process_summary_batch` symmetrically (cap is rarely hit since summaries are 1-per-note).
4. Add regression test `cortex/src/embed/tests.rs::process_transcript_batch_bounds_calls_when_input_exceeds_cap`.
5. Validate with `otto ci`, otto deploy, observe a 30-min window of normal daemon operation.

## Open Questions

None - the evidence is unambiguous and the fix is narrow.

## Why this won't break things

- Bounded `embed_batch` calls within a tick are functionally identical to one big call - the vectors come out in the same order, the index is upserted the same way.
- The rayon fan-out inside `embed_inner` still runs with 8 replicas; just on smaller sub-batches at a time. Per-call throughput is the same (8 sub-chunks of 8 chunks each = 64 chunks parallel). Wall-clock per tick may be slightly higher only if there were enough chunks to swamp the system without bound - exactly the case this fix is meant to prevent.
- Failure semantics match today: any sub-batch error marks `stats.failed = flat.len()` and the tick aborts. Stale rows retry next tick. No partial commits to the DB; the write-path cursor math at `embed.rs:404-417` stays correct because `flat_vectors.len() == flat.len()` is preserved on the success path.
