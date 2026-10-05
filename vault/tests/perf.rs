//! Performance harness for vault::note::scan_vault.
//!
//! Marked `#[ignore]` so it does not run on `cargo test` by default. Run with
//! `cargo test --package vault --test perf -- --ignored --nocapture` to see timings.
//!
//! The harness builds a tempdir vault of `NOTE_COUNT` markdown files with frontmatter and reports
//! wall-clock time for a single `scan_vault` call. Use this to compare before/after timings when
//! evaluating the par_iter conversion (per the rayon design doc).

use std::fs;
use std::time::{Duration, Instant};
use vault::config::ScanConfig;
use vault::note::scan_vault;

const NOTE_COUNT: usize = 1000;

#[test]
#[ignore]
fn perf_scan_vault_thousand_notes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    let filler = "Filler body line. ".repeat(50);
    for i in 0..NOTE_COUNT {
        let path = root.join(format!("note-{i:04}.md"));
        let body = format!(
            "---\ntitle: Note {i}\ntype: knowledge\norigin: authored\nstatus: draft\nmethod: cli\ntags:\n  - rust\n  - perf\n---\n# Note {i}\n\n{filler}\n"
        );
        fs::write(&path, body).expect("write note");
    }

    let scan_config = ScanConfig {
        ignore: vec![".git".to_string(), ".obsidian".to_string()],
    };

    let start = Instant::now();
    let notes = scan_vault(root, &scan_config).expect("scan");
    let elapsed = start.elapsed();

    assert_eq!(notes.len(), NOTE_COUNT, "expected all notes to parse");
    println!(
        "scan_vault({NOTE_COUNT} notes) -> {} parsed in {:?}",
        notes.len(),
        elapsed
    );
}

/// Regression ceiling for `search_vector` at 21K rows x 384 dims, release
/// profile: 10x the 44.37 ms release p50 measured on a Genuine Intel 3.10 GHz,
/// 32-logical-CPU host (design doc Addendum D). This is NOT the
/// 20 ms design target, which the current scan misses; it only catches an
/// order-of-magnitude regression. Run with `otto perf`.
#[cfg(feature = "vec")]
const SEARCH_VECTOR_P50_CEILING: Duration = Duration::from_millis(450);

#[cfg(feature = "vec")]
#[test]
#[ignore = "timing test; run via `otto perf` (release profile)"]
fn perf_search_vector_21k() {
    use vault::search::EmbeddingKind;

    const ROWS: usize = 21_000;
    const DIM: usize = 384;
    const RUNS: usize = 20;
    const MODEL: &str = "perf-v1";

    let mut index = vault::search::SearchIndex::open_memory().expect("open memory index");
    index.set_active_embedding(MODEL, DIM).expect("set active embedding");

    for i in 0..ROWS {
        let path = format!("note-{i:05}.md");
        index.insert_test_note_row(&path, "knowledge", 1).expect("insert note");
        let vector = unit_vector(i as u64, DIM);
        index
            .upsert_embedding(&path, EmbeddingKind::Summary, 0, "summary", &vector, MODEL, 1)
            .expect("upsert embedding");
    }

    let query = unit_vector(ROWS as u64 + 1, DIM);
    index
        .search_vector(&query, 10, None, false, None, None)
        .expect("warmup search");

    let mut samples: Vec<Duration> = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let start = Instant::now();
        let hits = index
            .search_vector(&query, 10, None, false, None, None)
            .expect("search_vector");
        samples.push(start.elapsed());
        assert_eq!(hits.len(), 10, "expected a full page of hits");
    }
    samples.sort();
    let p50 = samples[RUNS / 2 - 1];
    println!(
        "search_vector({ROWS} rows x {DIM} dims) p50 = {p50:?}, max = {:?}",
        samples[RUNS - 1]
    );
    assert!(
        p50 < SEARCH_VECTOR_P50_CEILING,
        "search_vector p50 {p50:?} exceeds the {SEARCH_VECTOR_P50_CEILING:?} regression ceiling"
    );
}

/// Deterministic unit-norm vector (xorshift), so cosine distance is a plain dot product.
#[cfg(feature = "vec")]
fn unit_vector(seed: u64, dim: usize) -> Vec<f32> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v: Vec<f32> = (0..dim)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % 2001) as f32 / 1000.0 - 1.0
        })
        .collect();
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter_mut().for_each(|x| *x /= norm);
    v
}
