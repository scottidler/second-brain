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
