//! Archive-first transcript resolution and the archive-driven sentinel revival.

use super::*;

#[test]
fn process_transcript_batch_resolves_from_archive_after_staging_is_swept() {
    // 2026-10-14-note-sources-intent-tools-channels success criterion: delete
    // the staging trace dir, keep the archive, and the embed still resolves
    // the transcript. Mirrors the expired-staging test, which sentinel-skips
    // because it has no archive.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    let staging = TempDir::new().expect("staging");
    let archive = TempDir::new().expect("archive");

    write_note_with_summary(tmp.path(), "notes/video.md", "a video whose staging was swept");
    index.insert_test_note_row("notes/video.md", "video", 100).expect("row");
    index.set_test_trace("notes/video.md", "swept-trace").expect("trace");
    stage_distilled_transcript(staging.path(), "swept-trace", "the durable verbatim transcript");
    stage_distilled_transcript(archive.path(), "swept-trace", "the durable verbatim transcript");
    std::fs::remove_dir_all(staging.path().join("swept-trace")).expect("sweep trace dir");

    let stats = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::TranscriptChunk,
        m.model_version(),
        tmp.path(),
        SourceRoots {
            archive: archive.path(),
            staging: staging.path(),
        },
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process")
    .stats();
    assert_eq!(stats.scanned, 1);
    assert_eq!(
        stats.skipped_empty, 0,
        "the archive must stand in for the swept staging copy"
    );
    assert!(stats.embedded >= 1);
    assert!(index.transcript_chunk_count("notes/video.md").expect("count") >= 1);
}

#[test]
fn process_transcript_batch_prefers_the_archive_over_staging() {
    let tmp = TempDir::new().expect("tmp");
    let staging = TempDir::new().expect("staging");
    let archive = TempDir::new().expect("archive");
    write_note_with_summary(tmp.path(), "notes/video.md", "v");
    stage_distilled_transcript(staging.path(), "both", "staged copy text");
    stage_distilled_transcript(archive.path(), "both", "archived copy text");

    let roots = SourceRoots {
        archive: archive.path(),
        staging: staging.path(),
    };
    assert_eq!(
        read_source_transcript(roots, "both", "notes/video.md").as_deref(),
        Some("archived copy text")
    );
    // An archive without the trace falls back to staging.
    std::fs::remove_dir_all(archive.path().join("both")).expect("rm archive");
    assert_eq!(
        read_source_transcript(roots, "both", "notes/video.md").as_deref(),
        Some("staged copy text")
    );
    // A traversal trace never resolves through the archive.
    assert_eq!(read_source_transcript(roots, "../both", "notes/video.md"), None);
}

/// One transcript-chunk batch over `roots`.
fn transcript_tick(
    index: &mut SearchIndex,
    m: &MockEmbedder,
    vault: &std::path::Path,
    roots: SourceRoots<'_>,
) -> EmbedStats {
    *process_batch(
        index,
        m,
        EmbeddingKind::TranscriptChunk,
        m.model_version(),
        vault,
        roots,
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("tick")
    .stats()
}

#[test]
fn missing_source_sentinel_is_retried_once_an_archive_appears() {
    // A note sentinelled for "no transcript source" is revived when its archive
    // dir appears afterwards (the archive backfill), and embeds.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    let staging = TempDir::new().expect("staging");
    let archive = TempDir::new().expect("archive");
    write_note_with_summary(tmp.path(), "notes/video.md", "v");
    index.insert_test_note_row("notes/video.md", "video", 100).expect("row");
    index.set_test_trace("notes/video.md", "late-trace").expect("trace");
    let roots = SourceRoots {
        archive: archive.path(),
        staging: staging.path(),
    };

    let t1 = transcript_tick(&mut index, &m, tmp.path(), roots);
    assert_eq!((t1.scanned, t1.skipped_empty), (1, 1), "no source anywhere: sentinel");
    assert_eq!(
        transcript_tick(&mut index, &m, tmp.path(), roots).scanned,
        0,
        "the sentinel holds without an archive"
    );

    stage_distilled_transcript(archive.path(), "late-trace", "transcript restored by a backfill");
    let t3 = transcript_tick(&mut index, &m, tmp.path(), roots);
    assert_eq!(t3.scanned, 1, "an archive newer than the sentinel revives the note");
    assert!(t3.embedded >= 1);
    assert!(index.transcript_chunk_count("notes/video.md").expect("count") >= 1);
}

#[test]
fn archive_without_transcript_does_not_revive_the_sentinel_every_tick() {
    // An archive that exists but carries no transcript (an article whose
    // transcript the source gate cleared) must not reopen the oscillation loop:
    // the re-sentinel's watermark is raised to the archive's mtime, so the
    // archive no longer reads as newer than the sentinel.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    let staging = TempDir::new().expect("staging");
    let archive = TempDir::new().expect("archive");
    write_note_with_summary(tmp.path(), "notes/article.md", "a");
    index
        .insert_test_note_row("notes/article.md", "article", 100)
        .expect("row");
    index.set_test_trace("notes/article.md", "empty-trace").expect("trace");
    let dir = archive.path().join("empty-trace");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let yaml = serde_yaml::to_string(&Distilled::default()).expect("yaml");
    std::fs::write(dir.join("distilled.yml"), yaml).expect("write");
    let roots = SourceRoots {
        archive: archive.path(),
        staging: staging.path(),
    };

    let t1 = transcript_tick(&mut index, &m, tmp.path(), roots);
    assert_eq!((t1.scanned, t1.skipped_empty), (1, 1));
    let watermark = index
        .examined_watermark("notes/article.md", EmbeddingKind::TranscriptChunk, m.model_version())
        .expect("watermark")
        .expect("sentinel");
    assert!(
        watermark > 100,
        "the watermark is raised to the archive mtime, got {watermark}"
    );
    assert_eq!(
        transcript_tick(&mut index, &m, tmp.path(), roots).scanned,
        0,
        "no revive loop on a transcript-less archive"
    );
}
