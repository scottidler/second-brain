//! Embed-recipe versioning through the real batch paths
//! (`2026-10-29-retrieval-quality-from-rag-scour`).

use super::*;

/// Run `kind` batches until one scans nothing: a CLI-style drain.
fn drain(index: &mut SearchIndex, m: &MockEmbedder, kind: EmbeddingKind, vault: &Path, staging: &Path) -> EmbedStats {
    let mut total = EmbedStats::default();
    for _ in 0..10 {
        let stats = *process_batch(
            index,
            m,
            kind,
            m.model_version(),
            vault,
            roots(staging),
            16,
            DEFAULT_MAX_CHUNKS_PER_CALL,
            ChunkAllowance::per_tick(0),
            1,
            &StopHandle::never(),
        )
        .expect("batch")
        .stats();
        total.merge(&stats);
        if stats.scanned == 0 {
            return total;
        }
    }
    panic!("{kind:?} did not drain in 10 batches");
}

/// Two videos with staged transcripts and one with no source at all.
fn seed(index: &SearchIndex, vault: &Path, staging: &Path) {
    for (rel, trace, transcript) in [
        // Prefix "T. summary": two mock tokens.
        ("notes/v1.md", "t1", Some(transcript_of_chunks_under(2, 3))),
        ("notes/v2.md", "t2", Some(transcript_of_chunks_under(2, 2))),
        ("notes/v3.md", "t3", None),
    ] {
        write_note_with_summary(vault, rel, "a video");
        index.insert_test_note_row(rel, "video", 100).expect("row");
        index.set_test_trace(rel, trace).expect("trace");
        if let Some(t) = transcript {
            stage_distilled_transcript(staging, trace, &t);
        }
    }
}

#[test]
fn recipe_reembed_is_not_stale_next_tick() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-recipe-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");
    let staging = TempDir::new().expect("staging");
    seed(&index, vault.path(), staging.path());

    for kind in [EmbeddingKind::Summary, EmbeddingKind::TranscriptChunk] {
        drain(&mut index, &m, kind, vault.path(), staging.path());
        assert!(
            index
                .stale_embedding_targets(kind, m.model_version(), 64)
                .expect("stale")
                .is_empty(),
            "{kind:?} drained"
        );

        // A bump of this kind's recipe: every row and sentinel now sits below it.
        index.set_test_recipe(kind, kind.recipe() - 1).expect("restamp");
        let bumped = index
            .stale_embedding_targets(kind, m.model_version(), 64)
            .expect("stale");
        assert_eq!(
            bumped.len(),
            3,
            "{kind:?}: the bump re-selects every note, v3's sentinel included"
        );

        let stats = drain(&mut index, &m, kind, vault.path(), staging.path());
        assert!(stats.embedded > 0, "{kind:?}: the drain re-embedded");
        assert!(
            index
                .stale_embedding_targets(kind, m.model_version(), 64)
                .expect("stale")
                .is_empty(),
            "{kind:?}: nothing is stale on the next tick"
        );
        let (rows, sentinels) = index.test_recipes(kind).expect("recipes");
        assert_eq!(
            rows,
            vec![kind.recipe()],
            "{kind:?}: every row carries the current recipe"
        );
        assert!(
            sentinels.iter().all(|&r| r == kind.recipe()),
            "{kind:?}: every sentinel carries the current recipe: {sentinels:?}"
        );
    }
    assert_eq!(index.transcript_chunk_count("notes/v1.md").expect("count"), 3);
    assert_eq!(index.transcript_chunk_count("notes/v2.md").expect("count"), 2);
}

#[test]
fn empty_source_clears_old_rows() {
    // The 17 notes the index-arm spike found: chunk rows embedded from a transcript that later
    // vanished, then the note was re-modified. The re-embed finds no source.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-recipe-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");
    let staging = TempDir::new().expect("staging");
    seed(&index, vault.path(), staging.path());
    drain(&mut index, &m, EmbeddingKind::Summary, vault.path(), staging.path());
    drain(
        &mut index,
        &m,
        EmbeddingKind::TranscriptChunk,
        vault.path(),
        staging.path(),
    );
    assert_eq!(index.transcript_chunk_count("notes/v1.md").expect("count"), 3);

    std::fs::remove_dir_all(staging.path().join("t1")).expect("source gone");
    index.set_test_modified_at("notes/v1.md", 200).expect("re-modify");
    let stats = drain(
        &mut index,
        &m,
        EmbeddingKind::TranscriptChunk,
        vault.path(),
        staging.path(),
    );
    assert_eq!(stats.skipped_empty, 1);

    assert_eq!(
        index.transcript_chunk_count("notes/v1.md").expect("count"),
        0,
        "the rows of a transcript the system can no longer produce are gone"
    );
    assert_eq!(
        index
            .examined_watermark("notes/v1.md", EmbeddingKind::TranscriptChunk, m.model_version())
            .expect("watermark"),
        Some(200),
        "and a current sentinel holds the note out"
    );
    assert!(
        index
            .stale_embedding_targets(EmbeddingKind::TranscriptChunk, m.model_version(), 64)
            .expect("stale")
            .is_empty()
    );
    assert!(
        index
            .embedding_text("notes/v1.md", EmbeddingKind::Summary)
            .expect("summary")
            .is_some(),
        "the summary kind is untouched"
    );
    assert_eq!(
        index.transcript_chunk_count("notes/v2.md").expect("count"),
        2,
        "other notes untouched"
    );
}
