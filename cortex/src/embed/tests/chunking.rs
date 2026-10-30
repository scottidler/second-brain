//! Token-accurate chunking through the real batch paths: a tokenizer error
//! skips the note rather than truncating it, and an oversize summary is loud.

use super::*;

/// Embeds like `MockEmbedder` but its tokenizer always fails.
struct BrokenTokenizer(MockEmbedder);

impl EmbeddingModel for BrokenTokenizer {
    fn dim(&self) -> usize {
        self.0.dim()
    }
    fn model_version(&self) -> &str {
        self.0.model_version()
    }
    fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        self.0.embed_one(text)
    }
    fn max_input_tokens(&self) -> usize {
        self.0.max_input_tokens()
    }
    fn count_tokens(&self, _text: &str) -> Result<usize> {
        Err(eyre::eyre!("tokenizer exploded"))
    }
    fn chunk(&self, _prefix: &str, _text: &str) -> Result<Vec<String>> {
        Err(eyre::eyre!("tokenizer exploded"))
    }
}

fn run(index: &mut SearchIndex, m: &dyn EmbeddingModel, kind: EmbeddingKind, vault: &Path) -> EmbedStats {
    *process_batch(
        index,
        m,
        kind,
        m.model_version(),
        vault,
        roots(vault),
        16,
        DEFAULT_MAX_CHUNKS_PER_CALL,
        ChunkAllowance::per_tick(DEFAULT_MAX_CHUNKS_PER_TICK),
        1,
        &StopHandle::never(),
    )
    .expect("process")
    .stats()
}

#[test]
fn tokenizer_error_skips_the_note_without_rows_or_sentinel() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = BrokenTokenizer(MockEmbedder::new(8, "mock-broken-tokenizer"));
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");
    seed_transcript_note(&index, vault.path(), "notes/t.md", 2, 100);
    seed_claim_note(&index, "notes/c.md", 1, 100);
    seed_summary_note(&index, "notes/s.md", 100);

    for (kind, path) in [
        (EmbeddingKind::TranscriptChunk, "notes/t.md"),
        (EmbeddingKind::Claim, "notes/c.md"),
        (EmbeddingKind::Summary, "notes/s.md"),
    ] {
        let stats = run(&mut index, &m, kind, vault.path());
        assert_eq!(stats.failed, 1, "{kind:?}: the note is a failure");
        assert_eq!(
            stats.embedded, 0,
            "{kind:?}: nothing truncated is embedded in its place"
        );
        assert_eq!(index.count_embeddings(Some(kind)).expect("count"), 0, "{kind:?}");
        let stale = index
            .stale_embedding_targets(kind, m.model_version(), 64)
            .expect("stale");
        assert!(
            stale.iter().any(|t| t.note_path == path),
            "{kind:?}: no sentinel, so {path} is retried next tick"
        );
    }
}

#[test]
fn oversize_summary_warns_with_path_and_token_count() {
    vault::capture::install();
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-oversize-summary").with_max_input_tokens(16);
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");
    let long = (0..40).map(|i| format!("s{i}")).collect::<Vec<_>>().join(" ");
    index
        .insert_test_note_full("notes/oversize-summary.md", "repo", "body", &long, 100)
        .expect("long");
    index
        .insert_test_note_full("notes/fitting-summary.md", "repo", "body", "short one", 100)
        .expect("short");

    let stats = run(&mut index, &m, EmbeddingKind::Summary, vault.path());
    assert_eq!(stats.embedded, 2, "an oversize summary is still embedded, loudly");
    let warns = vault::capture::warns_containing("notes/oversize-summary.md");
    assert_eq!(warns.len(), 1, "one WARN per oversize summary: {warns:?}");
    // The title, 40 summary words and [CLS]/[SEP].
    assert!(warns[0].contains("43 tokens"), "names the token count: {}", warns[0]);
    assert!(vault::capture::warns_containing("notes/fitting-summary.md").is_empty());
}

#[test]
fn chunk_context_prefix_joins_title_and_first_sentence() {
    use crate::embed::transcript::chunk_context_prefix;
    assert_eq!(
        chunk_context_prefix("Retrieval Talk", "The speaker compares chunkers. Second sentence."),
        "Retrieval Talk. The speaker compares chunkers."
    );
    assert_eq!(chunk_context_prefix("Retrieval Talk", ""), "Retrieval Talk.");
    assert_eq!(chunk_context_prefix("Retrieval Talk", "   \n "), "Retrieval Talk.");
}

#[test]
fn chunk_text_starts_with_context() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-context-prefix");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");
    let vault = TempDir::new().expect("vault");
    let transcript = transcript_of_chunks(3);
    write_note_with_transcript(vault.path(), "notes/with.md", "audio", "", &transcript);
    write_note_with_transcript(vault.path(), "notes/without.md", "audio", "", &transcript);
    index
        .insert_test_note_full(
            "notes/with.md",
            "audio",
            "body",
            "The speaker compares chunkers. Second sentence.",
            100,
        )
        .expect("with summary");
    index
        .insert_test_note_full("notes/without.md", "audio", "body", "", 100)
        .expect("without summary");
    index.set_test_title("notes/with.md", "Retrieval Talk").expect("title");
    index.set_test_title("notes/without.md", "Quiet Talk").expect("title");

    let stats = run(&mut index, &m, EmbeddingKind::TranscriptChunk, vault.path());
    assert_eq!(stats.failed, 0);

    for (path, prefix) in [
        ("notes/with.md", "Retrieval Talk. The speaker compares chunkers."),
        ("notes/without.md", "Quiet Talk."),
    ] {
        let texts = index
            .embedding_texts(path, EmbeddingKind::TranscriptChunk)
            .expect("texts");
        assert!(texts.len() > 1, "{path}: more than one chunk, got {}", texts.len());
        let title = prefix.split('.').next().expect("title");
        for (i, text) in texts.iter().enumerate() {
            assert!(
                text.starts_with(&format!("{title}.")),
                "{path} chunk {i} must start with its title: {text:.60}"
            );
            assert!(
                text.starts_with(&format!("{prefix}\n\n")),
                "{path} chunk {i}: {text:.80}"
            );
        }
        assert!(
            texts
                .iter()
                .all(|t| m.count_tokens(t).expect("count") <= m.max_input_tokens()),
            "{path}: every prefixed chunk fits the window"
        );
    }
}
