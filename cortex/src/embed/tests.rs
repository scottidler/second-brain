use super::*;

use std::path::PathBuf;
use std::time::Instant;
use tempfile::TempDir;

use crate::config::EmbedConfig;

use vault::distilled::Distilled;
use vault::embedding::MockEmbedder;
use vault::search::{BatchUpsert, EmbeddingKind, SearchIndex};

mod archive;

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
    let stats = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        tmp.path(),
        roots(tmp.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process")
    .stats();
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

    let stats = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        tmp.path(),
        roots(tmp.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process")
    .stats();
    assert_eq!(stats.scanned, 2);
    assert_eq!(stats.embedded, 2);
    assert_eq!(stats.skipped_empty, 0);
    assert_eq!(stats.failed, 0);

    let count = index.count_embeddings(Some(EmbeddingKind::Summary)).expect("count");
    assert_eq!(count, 2);
}

#[test]
fn process_batch_prefixes_title_to_summary() {
    // the embedded text is `"{title}\n\n{summary}"`, not the bare
    // summary. insert_test_note_row seeds title "T" and summary "summary".
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    index.insert_test_note_row("notes/a.md", "article", 100).expect("note");

    let stats = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        tmp.path(),
        roots(tmp.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process")
    .stats();
    assert_eq!(stats.embedded, 1);

    let text = index
        .embedding_text("notes/a.md", EmbeddingKind::Summary)
        .expect("query")
        .expect("embedding row present");
    assert_eq!(
        text, "T\n\nsummary",
        "embedded text must be title + blank line + summary"
    );
}

/// Write a note file carrying a `## Transcript` section. `body_head`
/// is any markdown that precedes it (e.g. slide sections for a slide-path
/// youtube note); pass `""` for a plain article.
fn write_note_with_transcript(vault: &std::path::Path, rel: &str, note_type: &str, body_head: &str, transcript: &str) {
    let abs = vault.join(rel);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    let body = format!(
        "---\ntitle: T\nnote-type: {note_type}\norigin: assisted\n---\n# T\n\n{body_head}## Transcript\n\n{transcript}\n"
    );
    std::fs::write(abs, body).expect("write note");
}

/// An archive root that never exists, for tests about staging alone: keeps
/// them off the operator's real `~/.local/share/sb/borg/sources`.
const NO_ARCHIVE: &str = "/nonexistent/sb-test-archive";

/// Source roots with no archive, reading transcripts from `staging` only.
fn roots(staging: &std::path::Path) -> SourceRoots<'_> {
    SourceRoots {
        archive: std::path::Path::new(NO_ARCHIVE),
        staging,
    }
}

/// Stage a `distilled.yml` under `<staging_root>/<trace>/distilled.yml` carrying
/// the given transcript, mirroring what borg's `write_distilled_yml` produces.
/// The distillation-output-restore re-points cortex's transcript
/// embedding at this staged file (via `notes.trace`) for Video/Article notes.
fn stage_distilled_transcript(staging_root: &std::path::Path, trace: &str, transcript: &str) {
    let dir = staging_root.join(trace);
    std::fs::create_dir_all(&dir).expect("mkdir trace dir");
    let distilled = Distilled {
        transcript: Some(transcript.to_string()),
        ..Default::default()
    };
    let yaml = serde_yaml::to_string(&distilled).expect("serialize distilled");
    std::fs::write(dir.join("distilled.yml"), yaml).expect("write distilled.yml");
}

#[test]
fn process_transcript_batch_embeds_video_article_from_staging_ignoring_in_note_section() {
    // distillation-output-restore. Video/Youtube/Article
    // transcripts now come from the staged distilled.yml (resolved via
    // notes.trace), NOT the note body. This test proves both halves: a video
    // note with a STAGED transcript embeds even though its body has no
    // ## Transcript section, AND an article note whose body still carries an
    // in-note ## Transcript (but has no staged file) is NOT embedded from the
    // body -- it is sentinel-skipped. The old behavior (embed from the in-note
    // section for these kinds) is inverted here on purpose.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index
        .set_active_embedding(m.model_version(), m.dim())
        .expect("set model");

    let tmp = TempDir::new().expect("tmp");
    let staging = TempDir::new().expect("staging");

    // Video note: body has ONLY summary/claims (no ## Transcript), transcript
    // lives in staging keyed by trace "video-trace".
    write_note_with_summary(tmp.path(), "notes/video.md", "Orchestration beats raw capability.");
    index
        .insert_test_note_row("notes/video.md", "video", 100)
        .expect("video row");
    index.set_test_trace("notes/video.md", "video-trace").expect("trace");
    stage_distilled_transcript(
        staging.path(),
        "video-trace",
        "Full spoken transcript of the video, staged as the embedding source.",
    );

    // Article note: body DOES carry an in-note ## Transcript, but there is NO
    // staged file (trace points nowhere). The in-note section must be ignored.
    write_note_with_transcript(
        tmp.path(),
        "notes/article.md",
        "article",
        "## Summary\n\nAn essay on consensus.\n\n",
        "This in-note transcript must NOT be embedded for an article kind.",
    );
    index
        .insert_test_note_row("notes/article.md", "article", 100)
        .expect("article row");
    index
        .set_test_trace("notes/article.md", "missing-trace")
        .expect("trace");

    let stats = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::TranscriptChunk,
        m.model_version(),
        tmp.path(),
        roots(staging.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process")
    .stats();

    assert_eq!(stats.scanned, 2, "both notes are transcript-eligible and scanned");
    assert!(stats.embedded >= 1, "the video note's staged transcript must embed");
    assert_eq!(
        stats.skipped_empty, 1,
        "the article with no staged file is sentinel-skipped, not embedded from its body"
    );
    assert_eq!(stats.failed, 0);

    // Only the video note produced chunk rows; the article produced none.
    let video_chunks = index
        .transcript_chunk_count("notes/video.md")
        .expect("video chunk count");
    assert!(
        video_chunks >= 1,
        "video note has staged-sourced chunks; got {video_chunks}"
    );
    let article_chunks = index
        .transcript_chunk_count("notes/article.md")
        .expect("article chunk count");
    assert_eq!(
        article_chunks, 0,
        "article body transcript must not be embedded (staged-source only)"
    );
}

#[test]
fn process_transcript_batch_missing_staged_file_degrades_to_sentinel_skip() {
    // A Video/Article note whose staged distilled.yml is missing (never staged,
    // or the trace column is empty) degrades to the examined-sentinel skip with
    // a WARN and never spins the loop. Success criterion: "deleting the trace
    // dir then re-running embed degrades to sentinel-skip with a WARN".
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    let staging = TempDir::new().expect("staging");

    write_note_with_summary(tmp.path(), "notes/video.md", "a video with no staged transcript");
    index.insert_test_note_row("notes/video.md", "video", 100).expect("row");
    index.set_test_trace("notes/video.md", "gone-trace").expect("trace");
    // Deliberately do NOT stage a distilled.yml for "gone-trace".

    // Tick 1: scanned + skipped, sentinel persisted at the note's modified_at.
    let t1 = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::TranscriptChunk,
        m.model_version(),
        tmp.path(),
        roots(staging.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("tick1")
    .stats();
    assert_eq!(t1.scanned, 1);
    assert_eq!(t1.skipped_empty, 1);
    assert_eq!(t1.embedded, 0);
    assert_eq!(
        index
            .examined_watermark("notes/video.md", EmbeddingKind::TranscriptChunk, m.model_version())
            .expect("watermark"),
        Some(100),
        "the missing-staged-file skip must persist an examined sentinel keyed on the NOTE"
    );

    // Tick 2 on the unchanged vault: the sentinel excludes the note -- no
    // re-scan, no error loop.
    let t2 = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::TranscriptChunk,
        m.model_version(),
        tmp.path(),
        roots(staging.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("tick2")
    .stats();
    assert_eq!(t2.scanned, 0, "sentinel must stop the re-scan; no error loop");
    assert_eq!(t2.embedded, 0);
}

#[test]
fn process_transcript_batch_expired_staged_file_degrades_to_sentinel_skip() {
    // "Expired" = the staged distilled.yml existed but was swept by retention.
    // Behaviorally identical to missing: the note sentinel-skips. This mirrors
    // the doc's Testing Strategy ("present / missing / expired") and confirms a
    // deleted trace dir does not spin the loop.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    let staging = TempDir::new().expect("staging");

    write_note_with_summary(tmp.path(), "notes/video.md", "a video whose staged transcript expired");
    index.insert_test_note_row("notes/video.md", "video", 100).expect("row");
    index.set_test_trace("notes/video.md", "expired-trace").expect("trace");
    // Stage it, then delete the whole trace dir to simulate retention sweep.
    stage_distilled_transcript(staging.path(), "expired-trace", "transcript that will be swept");
    std::fs::remove_dir_all(staging.path().join("expired-trace")).expect("sweep trace dir");

    let stats = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::TranscriptChunk,
        m.model_version(),
        tmp.path(),
        roots(staging.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process")
    .stats();
    assert_eq!(stats.scanned, 1);
    assert_eq!(stats.skipped_empty, 1, "expired staged file -> sentinel skip");
    assert_eq!(stats.embedded, 0);
    assert_eq!(stats.failed, 0);
}

#[test]
fn process_transcript_batch_verbatim_kind_still_reads_in_note_section() {
    // Verbatim-preservation kinds (Image/Audio/Note/Vocab) keep the in-note
    // ## Transcript section as their embedding source -- the re-point touches
    // only Video/Youtube/Article/Social/Reddit. This proves
    // the unchanged path: an `audio` note (VoiceNote) with an in-note transcript
    // and NO staged file still embeds from the body.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    let staging = TempDir::new().expect("staging");

    write_note_with_transcript(
        tmp.path(),
        "notes/voice.md",
        "audio",
        "## Summary\n\nA voice memo.\n\n",
        "The verbatim voice-memo transcription, kept in the note body.",
    );
    index.insert_test_note_row("notes/voice.md", "audio", 100).expect("row");
    // No trace, no staged file: the verbatim path must not consult staging.

    let stats = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::TranscriptChunk,
        m.model_version(),
        tmp.path(),
        roots(staging.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process")
    .stats();

    assert_eq!(stats.scanned, 1);
    assert!(stats.embedded >= 1, "verbatim-kind in-note transcript must still embed");
    assert_eq!(stats.skipped_empty, 0);
    assert_eq!(stats.failed, 0);
    assert!(
        index.transcript_chunk_count("notes/voice.md").expect("count") >= 1,
        "audio note keeps embedding from its in-note ## Transcript section"
    );
}

#[test]
fn process_transcript_batch_marks_unembeddable_notes_examined_and_converges() {
    // docs/design/2026-07-05-cortex-daemon-oscillation-loop.md: a
    // transcript-eligible note with no `## Transcript` section is scanned,
    // skipped (no row written), and MUST be marked examined so the next tick
    // does not re-scan it. Without the sentinel, `e.id` stays NULL and the note
    // is re-selected every tick forever (the ~127-note oscillation).
    //
    // process_batch(TranscriptChunk) is the exact body of
    // daemon_tick_with_model's inner loop; oracle_db_path is a fixed global
    // path so the tick itself cannot be redirected to temp SQLite - this is the
    // hermetic seam for the convergence check.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    // Article (transcript-eligible) note carrying a ## Summary but NO
    // ## Transcript section - exactly the ~127-note shape.
    write_note_with_summary(tmp.path(), "notes/a.md", "an essay with no transcript section");
    index.insert_test_note_row("notes/a.md", "article", 100).expect("note");

    // Tick 1: scanned + skipped, nothing embedded, sentinel persisted.
    let t1 = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::TranscriptChunk,
        m.model_version(),
        tmp.path(),
        roots(tmp.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("tick1")
    .stats();
    assert_eq!(t1.scanned, 1);
    assert_eq!(t1.skipped_empty, 1);
    assert_eq!(t1.embedded, 0);
    assert_eq!(
        index
            .examined_watermark("notes/a.md", EmbeddingKind::TranscriptChunk, m.model_version())
            .expect("watermark"),
        Some(100),
        "the skip must persist an examined sentinel at the note's indexed modified_at"
    );

    // Tick 2 on the unchanged vault: the sentinel excludes the note, so it is
    // no longer even scanned. This is the convergence the sentinel exists for.
    let t2 = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::TranscriptChunk,
        m.model_version(),
        tmp.path(),
        roots(tmp.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("tick2")
    .stats();
    assert_eq!(t2.scanned, 0, "tick-2 must not re-scan the previously-skipped note");
    assert_eq!(t2.skipped_empty, 0);
    assert_eq!(t2.embedded, 0);
}

#[test]
fn process_batch_skips_notes_with_empty_summary() {
    // Notes whose `notes.summary` column is empty are filtered out at
    // the SQL level by `stale_embedding_targets`, so the embed loop
    // never sees them. This protects against an infinite-loop bug:
    // before this filter, a note with no summary text would be picked
    // up every batch, skipped (no embedding row written), then re-
    // picked up indefinitely.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    index
        .insert_test_note_full("notes/empty.md", "article", "body text", "", 100)
        .expect("note");

    let stats = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        tmp.path(),
        roots(tmp.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process")
    .stats();
    assert_eq!(stats.scanned, 0, "SQL filter must exclude empty-summary notes");
    assert_eq!(stats.embedded, 0);
    assert_eq!(stats.skipped_empty, 0);
}

#[test]
fn write_transaction_for_batch_64_stays_under_200ms() {
    // This is the load-bearing invariant of the transaction discipline. If a future
    // change moves `embed_batch` between BEGIN IMMEDIATE and COMMIT
    // (or holds any other CPU-bound work inside the write transaction),
    // the wall-clock blows past 200 ms because real embedding inference
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

/// Records every `embed_batch` call's input length so a test can
/// assert sub-batching obeys the configured cap. Delegates the actual
/// embedding to `MockEmbedder` so vector dimensions stay consistent.
struct CountingMockEmbedder {
    inner: MockEmbedder,
    call_sizes: std::sync::Mutex<Vec<usize>>,
}

impl CountingMockEmbedder {
    fn new() -> Self {
        Self {
            inner: MockEmbedder::default_384(),
            call_sizes: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<usize> {
        self.call_sizes.lock().expect("calls mutex").clone()
    }
}

impl vault::embedding::EmbeddingModel for CountingMockEmbedder {
    fn dim(&self) -> usize {
        self.inner.dim()
    }
    fn model_version(&self) -> &str {
        self.inner.model_version()
    }
    fn embed_one(&self, t: &str) -> eyre::Result<Vec<f32>> {
        self.inner.embed_one(t)
    }
    fn embed_batch(&self, texts: &[&str]) -> eyre::Result<Vec<Vec<f32>>> {
        self.call_sizes.lock().expect("calls mutex").push(texts.len());
        self.inner.embed_batch(texts)
    }
}

/// Regression guard for the 2026-05-19 OOM. If a future refactor
/// removes or bypasses the sub-batching loop in `process_transcript_batch`,
/// this test fails because `embed_batch` would be called once with
/// the full input length instead of in capped sub-batches. RSS bounds
/// are not assertable in a unit test; call counts and per-call sizes
/// are.
#[test]
fn embed_in_sub_batches_caps_inputs_and_preserves_order() {
    let m = CountingMockEmbedder::new();
    // 250 inputs with stable identifiers so we can verify input order
    // is preserved across sub-batches.
    let texts: Vec<String> = (0..250).map(|i| format!("doc-{i}")).collect();
    let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

    let vectors = expect_vectors(embed_in_sub_batches(&m, &refs, 64, 1, &StopHandle::never()).expect("embed"));

    assert_eq!(vectors.len(), 250, "must return one vector per input");

    let calls = m.calls();
    assert_eq!(calls.len(), 4, "250 inputs / cap=64 -> ceil(250/64) = 4 calls");
    assert!(
        calls.iter().all(|&n| n <= 64),
        "no sub-batch may exceed the cap; got {calls:?}"
    );
    assert_eq!(calls.iter().sum::<usize>(), 250, "every input is covered exactly once");

    // Order preservation: the seeded MockEmbedder is deterministic, so
    // recomputing each input via embed_one yields the expected vector
    // in the same slot.
    let expected_first = m.embed_one("doc-0").expect("e0");
    let expected_last = m.embed_one("doc-249").expect("e249");
    assert_eq!(vectors[0], expected_first);
    assert_eq!(vectors[249], expected_last);
}

#[test]
fn embed_in_sub_batches_handles_empty_input() {
    let m = CountingMockEmbedder::new();
    let vectors = expect_vectors(embed_in_sub_batches(&m, &[], 64, 1, &StopHandle::never()).expect("embed"));
    assert!(vectors.is_empty());
    assert!(m.calls().is_empty(), "no embed_batch call for empty input");
}

#[test]
fn embed_in_sub_batches_treats_zero_cap_as_no_cap() {
    let m = CountingMockEmbedder::new();
    let texts: Vec<String> = (0..30).map(|i| format!("doc-{i}")).collect();
    let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let vectors = expect_vectors(embed_in_sub_batches(&m, &refs, 0, 1, &StopHandle::never()).expect("embed"));
    assert_eq!(vectors.len(), 30);
    let calls = m.calls();
    assert_eq!(calls, vec![30], "cap=0 means one call with the full input");
}

// ---- capture-note embed text + claim embeddings ----

#[test]
fn summary_embed_text_is_byte_identical_when_no_capture_note() {
    // BYTE-IDENTICAL INVARIANT: a note without a capture note must embed the
    // same text as before capture notes (title + blank line + summary), so the staleness
    // watermark does not treat the whole vault as changed and re-embed it.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    // insert_test_note_row seeds title "T", summary "summary", capture_note "".
    index.insert_test_note_row("notes/a.md", "article", 100).expect("note");

    process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        tmp.path(),
        roots(tmp.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process");

    let text = index
        .embedding_text("notes/a.md", EmbeddingKind::Summary)
        .expect("query")
        .expect("row");
    assert_eq!(
        text, "T\n\nsummary",
        "no capture note must yield the pre-capture-note title+summary text byte-identical"
    );
}

#[test]
fn summary_embed_text_splices_capture_note_when_present() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    index.insert_test_note_row("notes/a.md", "article", 100).expect("note");
    index
        .set_test_capture_note("notes/a.md", "this is how we should fix borg")
        .expect("capture");

    process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        tmp.path(),
        roots(tmp.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process");

    let text = index
        .embedding_text("notes/a.md", EmbeddingKind::Summary)
        .expect("query")
        .expect("row");
    assert_eq!(
        text, "T\n\nthis is how we should fix borg\n\nsummary",
        "embed text must be title + capture-note + summary"
    );
}

#[test]
fn process_claim_batch_embeds_notes_with_claims() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    index.insert_test_note_row("notes/a.md", "article", 100).expect("note");
    index
        .set_test_claims(
            "notes/a.md",
            "the harness matters more than the model\nagents need orchestration",
        )
        .expect("claims");
    // A note with no claims must not be embedded.
    index
        .insert_test_note_row("notes/b.md", "article", 100)
        .expect("note b");

    let stats = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::Claim,
        m.model_version(),
        tmp.path(),
        roots(tmp.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process")
    .stats();

    assert_eq!(stats.scanned, 1, "only the note carrying claims is scanned");
    assert_eq!(stats.embedded, 1, "two short claims fit in one group -> one row");
    assert_eq!(stats.skipped_empty, 0);
    assert_eq!(stats.failed, 0);
    assert_eq!(index.count_embeddings(Some(EmbeddingKind::Claim)).expect("count"), 1);
}

#[test]
fn process_claim_batch_splits_oversized_claim_sets_into_multiple_rows() {
    // A claim set whose joined word count exceeds CLAIM_GROUP_MAX_WORDS (400)
    // must split into multiple embedding rows so no tail claim is dropped by
    // silent model-side truncation.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    index
        .insert_test_note_row("notes/big.md", "article", 100)
        .expect("note");
    // 500 single-word claims: budget 400 -> 400 + 100 -> two groups.
    let claims: String = (0..500).map(|i| format!("claim{i}")).collect::<Vec<_>>().join("\n");
    index.set_test_claims("notes/big.md", &claims).expect("claims");

    let stats = *process_batch(
        &mut index,
        &m,
        EmbeddingKind::Claim,
        m.model_version(),
        tmp.path(),
        roots(tmp.path()),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process")
    .stats();

    assert_eq!(stats.scanned, 1);
    assert_eq!(stats.embedded, 2, "500 one-word claims at budget 400 -> two groups");
    assert_eq!(index.count_embeddings(Some(EmbeddingKind::Claim)).expect("count"), 2);
}

#[test]
fn group_claims_splits_by_word_budget() {
    // Empty input -> no groups.
    assert!(group_claims("", 400).is_empty());
    assert!(group_claims("   \n  \n", 400).is_empty());

    // Two short claims fit in one group.
    let one = group_claims("alpha beta\ngamma delta", 400);
    assert_eq!(one, vec!["alpha beta\ngamma delta".to_string()]);

    // Budget forces a split: each claim is two words; budget 3 -> claim1 alone
    // (2 words), adding claim2 (4 > 3) starts a new group.
    let split = group_claims("aa bb\ncc dd\nee ff", 3);
    assert_eq!(
        split,
        vec!["aa bb".to_string(), "cc dd".to_string(), "ee ff".to_string()]
    );

    // A single claim that alone exceeds the budget becomes its own group (the
    // model truncates that one claim, but no LATER claim is dropped).
    let overlong = group_claims("one two three four five\nsix", 3);
    assert_eq!(overlong, vec!["one two three four five".to_string(), "six".to_string()]);
}

// ---- Config-gated embed kinds (retrieval-gate remediation) ----

#[test]
fn enabled_default_kinds_excludes_claim_by_default() {
    // The default (no-`--kind`) kind list resolved from the default config must
    // exclude Claim - the claim-free retrieval baseline - and include summary +
    // transcript-chunk. This is what `cortex embed` / `--backfill` / the daemon
    // tick generate with an absent or default `embed.kinds` config.
    let kinds = enabled_default_kinds(&EmbedKindsConfig::default());
    assert_eq!(kinds, vec![EmbeddingKind::Summary, EmbeddingKind::TranscriptChunk]);
    assert!(
        !kinds.contains(&EmbeddingKind::Claim),
        "claim must be absent from the default kind list"
    );
}

#[test]
fn enabled_default_kinds_includes_claim_when_toggled_on() {
    let kinds = enabled_default_kinds(&EmbedKindsConfig {
        summary: true,
        transcript_chunk: true,
        claim: true,
    });
    assert_eq!(
        kinds,
        vec![
            EmbeddingKind::Summary,
            EmbeddingKind::TranscriptChunk,
            EmbeddingKind::Claim,
        ]
    );
}

#[test]
fn resolve_kinds_cli_kind_claim_beats_config_toggle_off() {
    // CLI > config: `--kind claim` must embed claims regardless of the config
    // toggle (default OFF). This is the escape hatch for the future guard-first
    // claim experiment.
    let kinds = resolve_kinds(Some("claim"), &EmbedKindsConfig::default()).expect("resolve");
    assert_eq!(
        kinds,
        vec![EmbeddingKind::Claim],
        "--kind claim overrides the default-OFF config toggle"
    );
}

#[test]
fn resolve_kinds_no_override_uses_config_defaults() {
    // No `--kind` -> the config-enabled default set (claim OFF by default).
    let kinds = resolve_kinds(None, &EmbedKindsConfig::default()).expect("resolve");
    assert_eq!(kinds, vec![EmbeddingKind::Summary, EmbeddingKind::TranscriptChunk]);
}

// --- entity-hub-two-vector-synthesis: hub chunk embedding --------------------

#[test]
fn summary_embed_text_keeps_the_pre_capture_note_shape_byte_identical() {
    // The staleness watermark keys on this text: a shape change re-embeds the
    // whole vault, so a note WITHOUT a capture note must still produce exactly
    // `title\n\nsummary` (or the bare summary when the title is empty).
    assert_eq!(summary_embed_text("Title", "", "Summary."), "Title\n\nSummary.");
    assert_eq!(summary_embed_text("", "", "Summary."), "Summary.");
    assert_eq!(summary_embed_text("Title", "  ", "Summary."), "Title\n\nSummary.");
    assert_eq!(
        summary_embed_text("Title", "why I kept it", "Summary."),
        "Title\n\nwhy I kept it\n\nSummary."
    );
}

// --- per-tick chunk ceiling (governance-starvation fix) ----------
//
// The incident: `batch_size` bounds NOTES, so a tick's real work was
// `64 notes x unbounded chunks`. One tick ran two days on an AVX-only host,
// held the shared rayon pool, and starved classify. These pin the bound.

fn tw(path: &str, chunks: usize) -> TranscriptWork {
    TranscriptWork {
        note_path: path.to_string(),
        chunks: (0..chunks).map(|i| format!("chunk {i}")).collect(),
        source_modified_at: 100,
    }
}

fn total_chunks(work: &[TranscriptWork]) -> usize {
    work.iter().map(|w| w.chunks.len()).sum()
}

#[test]
fn cap_zero_disables_the_ceiling() {
    let work = vec![tw("a.md", 500), tw("b.md", 500)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(0));
    assert_eq!(out.len(), 2, "0 means no cap");
    assert_eq!(total_chunks(&out), 1000);
}

#[test]
fn work_under_the_cap_passes_through_untouched() {
    let work = vec![tw("a.md", 10), tw("b.md", 20)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(512));
    assert_eq!(out.len(), 2);
    assert_eq!(total_chunks(&out), 30);
}

#[test]
fn truncates_on_a_note_boundary_never_mid_note() {
    // The write phase replaces a note's ENTIRE chunk set, so a partially
    // embedded note would land a truncated set and then read as complete.
    // Every kept note must therefore keep ALL of its chunks.
    let work = vec![tw("a.md", 300), tw("b.md", 300), tw("c.md", 300)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(512));
    assert_eq!(out.len(), 1, "b would overshoot 512, so it is deferred whole");
    assert_eq!(out[0].note_path, "a.md");
    assert_eq!(out[0].chunks.len(), 300, "the kept note keeps every chunk");
}

#[test]
fn packs_as_many_whole_notes_as_fit() {
    let work = vec![tw("a.md", 200), tw("b.md", 200), tw("c.md", 200)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(512));
    assert_eq!(out.len(), 2, "400 fits, 600 does not");
    assert_eq!(total_chunks(&out), 400);
}

#[test]
fn a_single_oversized_note_still_makes_progress() {
    // Regression guard against a livelock: if the cap could defer a note that
    // alone exceeds it, that note would be deferred forever and never embed.
    let work = vec![tw("huge.md", 5000)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(512));
    assert_eq!(out.len(), 1, "the lone oversized note is the deliberate exception");
    assert_eq!(out[0].chunks.len(), 5000, "and it embeds whole, not truncated");
}

#[test]
fn oversized_first_note_does_not_drag_others_along() {
    let work = vec![tw("huge.md", 5000), tw("small.md", 5)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(512));
    assert_eq!(out.len(), 1, "only the oversized note; the rest defer");
    assert_eq!(out[0].note_path, "huge.md");
}

// --- dedicated inference pool ----------------------------------------------

#[test]
fn inference_runs_off_the_global_rayon_pool() {
    // The invariant the incident was missing: candle's matmul fan-out must not
    // land on the pool `classify`/`quality`/lint run their par_iter on.
    // `install` reparents the closure, so inside it the current thread must be
    // a worker of OUR pool, not of the global one.
    let name = in_inference_pool(2, || {
        std::thread::current().name().map(|s| s.to_string()).unwrap_or_default()
    });
    assert!(
        name.starts_with("embed-infer-"),
        "inference must run on the dedicated pool, got thread name {name:?}"
    );

    // And it is a DIFFERENT pool from the global one. Asserted by identity of
    // width across calls rather than an absolute number: the pool is a
    // process-wide OnceLock, so a sibling test in this binary may have built it
    // first (which `in_inference_pool` now warns about instead of silently
    // ignoring). What must hold is that inference has its OWN bounded pool.
    let inner = in_inference_pool(2, rayon::current_num_threads);
    let global = rayon::current_num_threads();
    assert!(inner > 0, "the inference pool has workers");
    assert!(
        inner <= DEFAULT_INFERENCE_WORKERS.max(2),
        "the inference pool stays small ({inner}), never core-count-wide"
    );
    assert_ne!(
        inner, global,
        "inference must not be running on the global pool ({global} threads)"
    );
}

#[test]
fn inference_pool_propagates_errors_not_panics() {
    // `embed_in_sub_batches` calls the model inside the pool; a model error has
    // to travel back out as Err rather than poisoning a pool worker.
    let out: Result<()> = in_inference_pool(2, || eyre::bail!("model exploded"));
    assert!(out.is_err());
}

// --- stop check points and the per-tick chunk allowance --------------------

/// Counts `embed_batch` calls and the chunks sent through them, and can set a
/// stop flag right after its Nth call returns (the flag a SIGTERM would set
/// while that sub-batch was in flight).
struct CountingEmbedder {
    inner: MockEmbedder,
    calls: std::sync::atomic::AtomicUsize,
    chunks: std::sync::atomic::AtomicUsize,
    stop_after_call: Option<(usize, StopHandle)>,
    fail_when_any_text_contains: Option<&'static str>,
}

impl CountingEmbedder {
    fn new() -> Self {
        Self {
            inner: MockEmbedder::new(8, "mock-counting"),
            calls: Default::default(),
            chunks: Default::default(),
            stop_after_call: None,
            fail_when_any_text_contains: None,
        }
    }

    /// Fails every call whose inputs include a text containing `needle`.
    fn failing_on(needle: &'static str) -> Self {
        Self {
            fail_when_any_text_contains: Some(needle),
            ..Self::new()
        }
    }

    fn stopping_after(call: usize, stop: &StopHandle) -> Self {
        Self {
            stop_after_call: Some((call, stop.clone())),
            ..Self::new()
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn chunks(&self) -> usize {
        self.chunks.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl EmbeddingModel for CountingEmbedder {
    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn model_version(&self) -> &str {
        self.inner.model_version()
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        self.inner.embed_one(text)
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let out = match self.fail_when_any_text_contains {
            Some(needle) if texts.iter().any(|t| t.contains(needle)) => Err(eyre::eyre!("poisoned input")),
            _ => self.inner.embed_batch(texts),
        };
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        self.chunks.fetch_add(texts.len(), std::sync::atomic::Ordering::SeqCst);
        if let Some((n, stop)) = &self.stop_after_call
            && call == *n
        {
            stop.stop();
        }
        out
    }
}

fn expect_vectors(inference: Inference) -> Vec<Vec<f32>> {
    match inference {
        Inference::Vectors(v) => v,
        Inference::Stopped => panic!("inference stopped under StopHandle::never()"),
    }
}

/// A transcript that `chunk_transcript` splits into exactly `chunks` chunks
/// (400-word window, 350-word stride).
fn transcript_of_chunks(chunks: usize) -> String {
    let words = CHUNK_MAX_TOKENS + (CHUNK_MAX_TOKENS - CHUNK_OVERLAP_TOKENS) * (chunks - 1);
    (0..words).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ")
}

/// Claims text that `group_claims` packs into exactly `groups` groups.
fn claims_of_groups(groups: usize) -> String {
    (0..groups * CLAIM_GROUP_MAX_WORDS)
        .map(|i| format!("c{i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// An `audio` note (in-note transcript source) with no summary, so it is a
/// transcript-chunk target only.
fn seed_transcript_note(index: &SearchIndex, vault: &std::path::Path, rel: &str, chunks: usize, modified_at: i64) {
    write_note_with_transcript(vault, rel, "audio", "", &transcript_of_chunks(chunks));
    index
        .insert_test_note_full(rel, "audio", "body", "", modified_at)
        .expect("transcript row");
    assert_eq!(
        vault::embedding::chunk_transcript(&transcript_of_chunks(chunks), CHUNK_MAX_TOKENS, CHUNK_OVERLAP_TOKENS).len(),
        chunks,
        "fixture must chunk to exactly {chunks}"
    );
}

/// A `repo` note (not transcript-eligible) with a summary: one summary target.
fn seed_summary_note(index: &SearchIndex, rel: &str, modified_at: i64) {
    index
        .insert_test_note_full(rel, "repo", "body", "summary", modified_at)
        .expect("summary row");
}

/// A `repo` note with no summary and `groups` claim groups: a claim target only.
fn seed_claim_note(index: &SearchIndex, rel: &str, groups: usize, modified_at: i64) {
    index
        .insert_test_note_full(rel, "repo", "body", "", modified_at)
        .expect("claim row");
    index.set_test_claims(rel, &claims_of_groups(groups)).expect("claims");
}

fn tick_config(staging_root: &std::path::Path, max_chunks_per_tick: usize, max_chunks_per_call: usize) -> Config {
    Config {
        staging_root: staging_root.to_path_buf(),
        archive_root: PathBuf::from(NO_ARCHIVE),
        embed: EmbedConfig {
            max_chunks_per_tick,
            max_chunks_per_call,
            workers: 1,
            ..EmbedConfig::default()
        },
        ..Config::default()
    }
}

#[test]
fn stop_between_sub_batches_drops_the_batch_and_writes_no_embedding_rows() {
    // The flag is set while sub-batch 1 of 3 runs. The
    // check point before sub-batch 2 sees it: exactly one model call, no
    // embedding row, and the examined sentinel written before inference stays.
    let mut index = SearchIndex::open_memory().expect("open");
    let stop = StopHandle::never();
    let m = CountingEmbedder::stopping_after(1, &stop);
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");

    seed_transcript_note(&index, vault.path(), "notes/three.md", 3, 200);
    // Transcript-eligible with no transcript section: sentinel-skipped in the
    // read phase, which runs before inference.
    index
        .insert_test_note_full("notes/skip.md", "audio", "body", "", 100)
        .expect("skip row");

    let mut config = tick_config(vault.path(), DEFAULT_MAX_CHUNKS_PER_TICK, 1);
    config.embed.kinds.summary = false;
    let mut carry = EmbedCarry::default();
    let stats = embed_tick(&mut index, &m, vault.path(), &config, &stop, &mut carry).expect("tick");

    assert_eq!(m.calls(), 1, "the check point before sub-batch 2 must stop the batch");
    assert_eq!(
        index.count_embeddings(None).expect("count"),
        0,
        "a stopped batch writes no embedding rows"
    );
    assert_eq!(stats.embedded, 0);
    assert_eq!(stats.failed, 0, "a stop is never counted as a failure");
    assert_eq!(
        index
            .examined_watermark("notes/skip.md", EmbeddingKind::TranscriptChunk, m.model_version())
            .expect("watermark"),
        Some(100),
        "the sentinel written before inference survives the stop"
    );
}

#[test]
fn stopped_batch_is_its_own_outcome() {
    // process_batch reports the stop as Stopped, never Complete with failures.
    let mut index = SearchIndex::open_memory().expect("open");
    let stop = StopHandle::never();
    let m = CountingEmbedder::stopping_after(1, &stop);
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");
    seed_summary_note(&index, "notes/a.md", 300);
    seed_summary_note(&index, "notes/b.md", 200);

    let outcome = process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        vault.path(),
        roots(vault.path()),
        16,
        1,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &stop,
    )
    .expect("process");

    assert!(
        matches!(outcome, BatchOutcome::Stopped(_)),
        "expected Stopped, got {outcome:?}"
    );
    assert_eq!(outcome.stats().failed, 0);
    assert_eq!(outcome.stats().embedded, 0);
    assert_eq!(index.count_embeddings(None).expect("count"), 0);
}

#[test]
fn preset_stop_returns_before_any_batch() {
    // With the flag already set, the tick's first check
    // point fires before process_batch. No read phase runs, so the sentinel a
    // read phase would write for the skip note is absent too.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = CountingEmbedder::new();
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");
    seed_summary_note(&index, "notes/s.md", 300);
    index
        .insert_test_note_full("notes/skip.md", "audio", "body", "", 100)
        .expect("skip row");

    let stop = StopHandle::never();
    stop.stop();
    let config = tick_config(vault.path(), DEFAULT_MAX_CHUNKS_PER_TICK, DEFAULT_MAX_CHUNKS_PER_CALL);
    let mut carry = EmbedCarry::default();
    let stats = embed_tick(&mut index, &m, vault.path(), &config, &stop, &mut carry).expect("tick");

    assert_eq!(m.calls(), 0, "no model call");
    assert_eq!(stats.scanned, 0, "no batch was selected");
    assert_eq!(
        index
            .examined_watermark("notes/skip.md", EmbeddingKind::TranscriptChunk, m.model_version())
            .expect("watermark"),
        None,
        "no read phase ran"
    );
    assert_eq!(index.count_embeddings(None).expect("count"), 0);
}

#[test]
fn one_tick_embeds_at_most_the_cap_across_all_kinds() {
    // Cap 512 and a 1300-chunk backlog across summary
    // (200 notes x 1), transcript (6 notes x 150) and claim (1 note x 200).
    // Every note fits the cap, so no tick may exceed it; the backlog drains
    // over several ticks.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = CountingEmbedder::new();
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");

    for i in 0..200 {
        seed_summary_note(&index, &format!("notes/s{i:03}.md"), 1000 + i);
    }
    for i in 0..6 {
        seed_transcript_note(&index, vault.path(), &format!("notes/t{i}.md"), 150, 500 + i);
    }
    seed_claim_note(&index, "notes/c.md", 200, 400);

    let mut config = tick_config(vault.path(), 512, DEFAULT_MAX_CHUNKS_PER_CALL);
    config.embed.kinds.claim = true;
    let stop = StopHandle::never();

    let mut per_tick: Vec<(u64, usize)> = Vec::new();
    let mut carry = EmbedCarry::default();
    for _ in 0..10 {
        let before = m.chunks();
        let stats = embed_tick(&mut index, &m, vault.path(), &config, &stop, &mut carry).expect("tick");
        if stats.embedded == 0 {
            break;
        }
        per_tick.push((stats.embedded, m.chunks() - before));
    }

    for (tick, (embedded, inferred)) in per_tick.iter().enumerate() {
        assert!(
            *embedded <= 512,
            "tick {tick} embedded {embedded} chunks, over the 512 cap: {per_tick:?}"
        );
        assert!(
            *inferred <= 512,
            "tick {tick} sent {inferred} chunks to the model: {per_tick:?}"
        );
    }
    assert!(
        per_tick.len() > 1,
        "a 1300-chunk backlog cannot drain in one tick: {per_tick:?}"
    );
    assert_eq!(
        per_tick.iter().map(|(e, _)| e).sum::<u64>(),
        1300,
        "the whole backlog drains across ticks: {per_tick:?}"
    );
    assert_eq!(index.count_embeddings(None).expect("count"), 1300);
}

#[test]
fn an_oversized_note_embeds_alone_in_its_tick() {
    // A 600-chunk note is over the 512 cap. It may only
    // take a fresh allowance, so it defers past the tick where summary drew
    // first, then embeds alone in the next one; the 100-chunk note waits.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = CountingEmbedder::new();
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");

    for i in 0..5 {
        seed_summary_note(&index, &format!("notes/s{i}.md"), 1000 + i);
    }
    seed_transcript_note(&index, vault.path(), "notes/big.md", 600, 900);
    seed_transcript_note(&index, vault.path(), "notes/small.md", 100, 800);

    let config = tick_config(vault.path(), 512, DEFAULT_MAX_CHUNKS_PER_CALL);
    let stop = StopHandle::never();

    let mut carry = EmbedCarry::default();
    let t1 = embed_tick(&mut index, &m, vault.path(), &config, &stop, &mut carry).expect("tick 1");
    assert_eq!(t1.embedded, 5, "tick 1: the summaries draw first; the big note defers");
    assert_eq!(index.transcript_chunk_count("notes/big.md").expect("count"), 0);

    let before = m.chunks();
    let t2 = embed_tick(&mut index, &m, vault.path(), &config, &stop, &mut carry).expect("tick 2");
    assert_eq!(t2.embedded, 600, "tick 2 embeds the oversized note and nothing else");
    assert_eq!(m.chunks() - before, 600);
    assert_eq!(index.transcript_chunk_count("notes/big.md").expect("count"), 600);
    assert_eq!(
        index.transcript_chunk_count("notes/small.md").expect("count"),
        0,
        "nothing joins the oversized note in its tick"
    );

    let t3 = embed_tick(&mut index, &m, vault.path(), &config, &stop, &mut carry).expect("tick 3");
    assert_eq!(t3.embedded, 100);
}

#[test]
fn partly_drawn_allowance_defers_an_oversized_note() {
    let work = vec![tw("huge.md", 600), tw("small.md", 5)];
    let allowance = ChunkAllowance::Limited {
        remaining: 500,
        fresh: false,
    };
    assert!(cap_work_by_chunks(work, allowance).is_empty());
}

#[test]
fn allowance_draws_down_and_reports_spent() {
    let mut a = ChunkAllowance::per_tick(10);
    assert!(!a.is_spent());
    a.draw(0);
    assert_eq!(
        a,
        ChunkAllowance::Limited {
            remaining: 10,
            fresh: true
        },
        "drawing nothing keeps it fresh"
    );
    a.draw(4);
    assert_eq!(
        a,
        ChunkAllowance::Limited {
            remaining: 6,
            fresh: false
        }
    );
    assert_eq!(a.note_limit(64), 6);
    a.draw(99);
    assert!(a.is_spent(), "an overdraw saturates at 0");

    let mut unlimited = ChunkAllowance::per_tick(0);
    unlimited.draw(1_000_000);
    assert_eq!(unlimited, ChunkAllowance::Unlimited);
    assert!(!unlimited.is_spent());
    assert_eq!(unlimited.note_limit(64), 64);
}

#[test]
fn sub_batches_stop_at_the_check_point() {
    let stop = StopHandle::never();
    let m = CountingEmbedder::stopping_after(2, &stop);
    let texts: Vec<String> = (0..5).map(|i| format!("t{i}")).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let out = embed_in_sub_batches(&m, &refs, 1, 1, &stop).expect("embed");
    assert!(matches!(out, Inference::Stopped));
    assert_eq!(m.calls(), 2);
}

#[test]
fn summary_draws_one_per_note_from_the_tick_allowance() {
    // Summary used to take no tick cap at all. 600 summary targets against a
    // 500 cap: the tick selects no more notes than the allowance has left, so
    // the last batch is clipped to 52 and the tick embeds exactly 500.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = CountingEmbedder::new();
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");
    for i in 0..600 {
        seed_summary_note(&index, &format!("notes/s{i:03}.md"), 1000 + i);
    }
    let config = tick_config(vault.path(), 500, DEFAULT_MAX_CHUNKS_PER_CALL);
    let stop = StopHandle::never();

    let mut carry = EmbedCarry::default();
    let t1 = embed_tick(&mut index, &m, vault.path(), &config, &stop, &mut carry).expect("tick 1");
    assert_eq!(t1.embedded, 500);
    assert_eq!(m.chunks(), 500);
    let t2 = embed_tick(&mut index, &m, vault.path(), &config, &stop, &mut carry).expect("tick 2");
    assert_eq!(t2.embedded, 100, "the rest drains next tick");
}

#[test]
fn a_summary_failing_every_tick_cannot_starve_an_oversized_note() {
    // A summary that fails inference draws 1 from the allowance every tick and
    // stays stale, so without the carry the transcript kind never saw a fresh
    // allowance and the 600-chunk note deferred forever. The carry gives it
    // the next tick's fresh allowance, inside the max(cap, largest note) bound.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = CountingEmbedder::failing_on("summary");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");
    seed_summary_note(&index, "notes/poison.md", 1000);
    seed_transcript_note(&index, vault.path(), "notes/big.md", 600, 900);
    seed_transcript_note(&index, vault.path(), "notes/small.md", 100, 800);
    let config = tick_config(vault.path(), 512, DEFAULT_MAX_CHUNKS_PER_CALL);
    let stop = StopHandle::never();
    let mut carry = EmbedCarry::default();

    let mut inferred = Vec::new();
    for tick in 1..=2 {
        let before = m.chunks();
        embed_tick(&mut index, &m, vault.path(), &config, &stop, &mut carry).expect("tick");
        inferred.push(m.chunks() - before);
        assert!(
            inferred[tick - 1] <= 600,
            "tick {tick} over max(cap, largest note): {inferred:?}"
        );
    }
    assert_eq!(
        index.transcript_chunk_count("notes/big.md").expect("count"),
        600,
        "the oversized note embeds within 2 ticks: {inferred:?}"
    );

    embed_tick(&mut index, &m, vault.path(), &config, &stop, &mut carry).expect("tick 3");
    assert_eq!(
        index.transcript_chunk_count("notes/small.md").expect("count"),
        100,
        "the summary leads again and the next note still fits behind it"
    );
}
