//! Synthetic memory-bounding test for the candle embedding backend.
//!
//! This test is the validation boundary Phase 7 of the v0.8.5 shakedown
//! cleanup design (`docs/design/2026-05-20-shakedown-v0.8.5-cleanup.md`)
//! gated the daemon-side "long-lived model" lifecycle change on. The cortex
//! daemon now holds a single `CandleBertModel` for its entire lifetime; if
//! that model's per-instance scratch state is unbounded, the daemon's RSS
//! would grow without bound over hours. This test feeds the model 1000
//! varied batches and asserts the resident set size plateaus.
//!
//! Marked `#[ignore]` because the run costs roughly a minute of CPU and
//! downloads the model weights (~100 MB) on first invocation. Run on
//! demand with:
//!
//! ```text
//! cargo test --release -p vault --test candle-bounded -- --ignored --nocapture
//! ```
//!
//! Acceptance bound: RSS at the end must not exceed baseline by more than
//! 200 MB. That leaves room for the model load itself + steady-state
//! activation/scratch buffers and catches monotonic leaks immediately.

#![cfg(all(target_os = "linux", feature = "vec-candle"))]

use vault::embedding::EmbeddingModel;
use vault::embedding::candle::CandleBertModel;
use vault::rss::{human_bytes, read_self_rss};

const TOTAL_BATCHES: usize = 1000;
const REPORT_EVERY: usize = 100;
/// RSS growth budget over baseline (post-load steady state). Generous
/// because allocators rarely give memory back to the OS, but tight
/// enough to flag a real monotonic leak.
const RSS_GROWTH_BUDGET_BYTES: u64 = 200 * 1024 * 1024;

fn batch_for(batch_idx: usize) -> Vec<String> {
    // Mix short summaries with longer chunks so the input distribution
    // exercises both the small-batch and large-batch code paths of the
    // candle backend over the run.
    let short = format!("short summary {batch_idx}: a brief note about thing {batch_idx}.");
    let medium = format!(
        "medium-length content for batch {batch_idx}. \
         This text simulates a note's distilled summary with enough \
         words to push past the trivial-input threshold.",
    );
    let long_chunk = (0..40)
        .map(|i| format!("sentence {i} for batch {batch_idx} content."))
        .collect::<Vec<_>>()
        .join(" ");
    vec![short, medium, long_chunk]
}

#[test]
#[ignore = "costly: downloads model weights and runs 1000 inference batches"]
fn candle_bert_rss_plateaus_across_1000_calls() {
    let baseline = read_self_rss().expect("VmRSS readable");
    eprintln!(
        "candle-bounded: pre-load baseline = {} ({})",
        human_bytes(baseline),
        baseline
    );

    let model = CandleBertModel::load_with_workers(1).expect("candle model loads");
    let post_load = read_self_rss().expect("VmRSS readable");
    eprintln!(
        "candle-bounded: post-load = {} (delta vs baseline = {})",
        human_bytes(post_load),
        human_bytes(post_load.saturating_sub(baseline))
    );

    // Use post-load RSS as the bound reference - the model load itself is
    // expected to allocate; we are testing per-call leak, not load cost.
    let reference = post_load;

    for batch_idx in 0..TOTAL_BATCHES {
        let owned = batch_for(batch_idx);
        let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        let _vectors = model.embed_batch(&refs).expect("embed_batch succeeds");

        if batch_idx % REPORT_EVERY == 0 {
            let now = read_self_rss().expect("VmRSS readable");
            eprintln!(
                "candle-bounded: after {batch_idx:>4} batches: rss = {} (delta vs post-load = {})",
                human_bytes(now),
                human_bytes(now.saturating_sub(reference))
            );
        }
    }

    let final_rss = read_self_rss().expect("VmRSS readable");
    let growth = final_rss.saturating_sub(reference);
    eprintln!(
        "candle-bounded: FINAL = {} (delta vs post-load = {})",
        human_bytes(final_rss),
        human_bytes(growth),
    );

    assert!(
        growth < RSS_GROWTH_BUDGET_BYTES,
        "candle leaked: post_load = {} ({}), final = {} ({}), growth = {} (budget = {})",
        human_bytes(reference),
        reference,
        human_bytes(final_rss),
        final_rss,
        human_bytes(growth),
        human_bytes(RSS_GROWTH_BUDGET_BYTES),
    );
}
