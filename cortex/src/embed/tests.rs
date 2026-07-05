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
        DEFAULT_MAX_CHUNKS_PER_CALL,
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
        DEFAULT_MAX_CHUNKS_PER_CALL,
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
fn process_batch_prefixes_title_to_summary() {
    // Phase 7a: the embedded text is `"{title}\n\n{summary}"`, not the bare
    // summary. insert_test_note_row seeds title "T" and summary "summary".
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let tmp = TempDir::new().expect("tmp");
    index.insert_test_note_row("notes/a.md", "article", 100).expect("note");

    let stats = process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        tmp.path(),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
    )
    .expect("process");
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

/// Write a note file carrying a `## Transcript` section (Phase 7). `body_head`
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

#[test]
fn process_transcript_batch_embeds_article_and_slide_youtube_notes() {
    // Phase 7 embedding-CREATION path (distinct from the FTS-parsing path
    // asserted in borg's pipeline tests). This exercises the
    // `transcript_eligible()` amendment end-to-end: an `article` note and a
    // slide-path `youtube` note must now surface as transcript targets and
    // produce `transcript-chunk` rows. Before the amendment neither note type
    // was eligible, so `stale_embedding_targets(TranscriptChunk)` returned
    // nothing and no rows were ever written.
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-batch-test");
    index
        .set_active_embedding(m.model_version(), m.dim())
        .expect("set model");

    let tmp = TempDir::new().expect("tmp");
    // Article note: fetched markdown persisted verbatim under ## Transcript.
    write_note_with_transcript(
        tmp.path(),
        "notes/article.md",
        "article",
        "## Summary\n\nAn essay on consensus.\n\n",
        "The full fetched article markdown, preserved in-note past staging retention.",
    );
    // Slide-path youtube note: slide sections FIRST, distilled ## Transcript
    // appended below (the Phase 7 splice shape).
    write_note_with_transcript(
        tmp.path(),
        "notes/yt.md",
        "youtube",
        "## Opening\n\n![[yt-slide-001.jpg]]\n\nThe speaker opens.\n\n## Summary\n\nOrchestration beats capability.\n\n",
        "Full spoken transcript of the slide-published video.",
    );
    index
        .insert_test_note_row("notes/article.md", "article", 100)
        .expect("article row");
    index
        .insert_test_note_row("notes/yt.md", "youtube", 100)
        .expect("youtube row");

    let stats = process_batch(
        &mut index,
        &m,
        EmbeddingKind::TranscriptChunk,
        m.model_version(),
        tmp.path(),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
    )
    .expect("process");

    assert_eq!(
        stats.scanned, 2,
        "both the article and the slide-path youtube note must be scanned"
    );
    assert!(
        stats.embedded >= 2,
        "each note produces at least one transcript-chunk row"
    );
    assert_eq!(stats.skipped_empty, 0);
    assert_eq!(stats.failed, 0);

    let count = index
        .count_embeddings(Some(EmbeddingKind::TranscriptChunk))
        .expect("count");
    assert!(
        count >= 2,
        "transcript-chunk rows must exist for both notes; got {count}"
    );
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

    let stats = process_batch(
        &mut index,
        &m,
        EmbeddingKind::Summary,
        m.model_version(),
        tmp.path(),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
    )
    .expect("process");
    assert_eq!(stats.scanned, 0, "SQL filter must exclude empty-summary notes");
    assert_eq!(stats.embedded, 0);
    assert_eq!(stats.skipped_empty, 0);
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

    let vectors = embed_in_sub_batches(&m, &refs, 64).expect("embed");

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
    let vectors = embed_in_sub_batches(&m, &[], 64).expect("embed");
    assert!(vectors.is_empty());
    assert!(m.calls().is_empty(), "no embed_batch call for empty input");
}

#[test]
fn embed_in_sub_batches_treats_zero_cap_as_no_cap() {
    let m = CountingMockEmbedder::new();
    let texts: Vec<String> = (0..30).map(|i| format!("doc-{i}")).collect();
    let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let vectors = embed_in_sub_batches(&m, &refs, 0).expect("embed");
    assert_eq!(vectors.len(), 30);
    let calls = m.calls();
    assert_eq!(calls, vec![30], "cap=0 means one call with the full input");
}

// ---- Phase 9: capture-note embed text + claim embeddings ----

#[test]
fn summary_embed_text_is_byte_identical_when_no_capture_note() {
    // BYTE-IDENTICAL INVARIANT: a note without a capture note must embed the
    // exact pre-Phase-9 text (title + blank line + summary), so the staleness
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
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
    )
    .expect("process");

    let text = index
        .embedding_text("notes/a.md", EmbeddingKind::Summary)
        .expect("query")
        .expect("row");
    assert_eq!(
        text, "T\n\nsummary",
        "no capture note must yield the pre-Phase-9 title+summary text byte-identical"
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
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
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

    let stats = process_batch(
        &mut index,
        &m,
        EmbeddingKind::Claim,
        m.model_version(),
        tmp.path(),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
    )
    .expect("process");

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

    let stats = process_batch(
        &mut index,
        &m,
        EmbeddingKind::Claim,
        m.model_version(),
        tmp.path(),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
    )
    .expect("process");

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
