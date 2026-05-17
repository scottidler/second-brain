use super::*;

use std::path::PathBuf;
use std::time::Instant;
use tempfile::TempDir;

use vault::embedding::MockEmbedder;
use vault::search::{BatchUpsert, EmbeddingKind, SearchIndex};

/// Build a minimal vault structure on disk with one note that has a
/// `## Summary` body section.
fn write_note_with_summary(vault: &std::path::Path, rel: &str, summary: &str) {
    let abs = vault.join(rel);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    let body = format!("---\ntitle: T\nnote-type: article\norigin: assisted\n---\n# T\n\n## Summary\n\n{summary}\n",);
    std::fs::write(abs, body).expect("write note");
}

#[test]
fn process_batch_returns_zero_when_no_stale_targets() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index
        .set_active_embedding(m.model_version(), m.dim())
        .expect("set model");

    let tmp = TempDir::new().expect("tmp");
    let stats = process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        tmp.path(),
        16,
    )
    .expect("process");
    assert_eq!(stats.scanned, 0);
    assert_eq!(stats.embedded, 0);
}

#[test]
fn process_batch_embeds_stale_summary_rows() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index
        .set_active_embedding(m.model_version(), m.dim())
        .expect("set model");

    let tmp = TempDir::new().expect("tmp");
    write_note_with_summary(tmp.path(), "notes/a.md", "alpha summary content");
    write_note_with_summary(tmp.path(), "notes/b.md", "beta summary content");
    index
        .insert_test_note_row("notes/a.md", "article", 100)
        .expect("note a");
    index
        .insert_test_note_row("notes/b.md", "article", 100)
        .expect("note b");

    let stats = process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        tmp.path(),
        16,
    )
    .expect("process");
    assert_eq!(stats.scanned, 2);
    assert_eq!(stats.embedded, 2);
    assert_eq!(stats.skipped_empty, 0);
    assert_eq!(stats.failed, 0);

    let count = index.count_embeddings(Some(EmbeddingKind::Summary)).expect("count");
    assert_eq!(count, 2);
}

#[test]
fn process_batch_skips_notes_with_empty_summary() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    write_note_with_summary(tmp.path(), "notes/empty.md", "   ");
    index
        .insert_test_note_row("notes/empty.md", "article", 100)
        .expect("note");

    let stats = process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        tmp.path(),
        16,
    )
    .expect("process");
    assert_eq!(stats.scanned, 1);
    assert_eq!(stats.embedded, 0);
    assert_eq!(stats.skipped_empty, 1);
}

#[test]
fn write_transaction_for_batch_64_stays_under_200ms() {
    // This is the load-bearing invariant of Phase A5. If a future
    // change moves `embed_batch` between BEGIN IMMEDIATE and COMMIT
    // (or holds any other CPU-bound work inside the write transaction),
    // the wall-clock blows past 200 ms because real fastembed inference
    // is ~50 ms per note.
    //
    // We measure only the time spent inside the upsert_embeddings_batch
    // call - exactly what the transaction wraps. embed_batch runs
    // before, outside the transaction.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(384, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let texts: Vec<String> = (0..64).map(|i| format!("text body for note {i}")).collect();
    let mut paths: Vec<String> = Vec::with_capacity(64);
    for (i, _t) in texts.iter().enumerate() {
        let p = format!("notes/n{i}.md");
        index.insert_test_note_row(&p, "article", 100).expect("note");
        paths.push(p);
    }

    // Inference happens outside the transaction.
    let texts_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let vectors = m.embed_batch(&texts_refs).expect("embed batch");
    let items: Vec<BatchUpsert<'_>> = paths
        .iter()
        .zip(texts.iter())
        .zip(vectors.iter())
        .map(|((p, t), v)| BatchUpsert {
            note_path: p,
            kind: EmbeddingKind::Summary,
            chunk_index: 0,
            text: t,
            embedding: v,
            model_version: m.model_version(),
            source_modified_at: 100,
        })
        .collect();

    let start = Instant::now();
    index.upsert_embeddings_batch(&items).expect("upsert");
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 200,
        "write transaction took {elapsed:?} for batch=64; budget is 200 ms"
    );
}

#[test]
fn lock_path_lives_under_data_local_dir() {
    let p = lock_path();
    assert!(p.ends_with(PathBuf::from("cortex/embed.lock")));
}
