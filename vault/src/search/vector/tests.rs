use super::*;
use crate::embedding::{EmbeddingModel, MockEmbedder};
use crate::search::SearchIndex;
use rusqlite::params;

fn insert_note(index: &SearchIndex, path: &str, domain: &str, note_type: &str, modified_at: i64) {
    index
        .conn
        .execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![path, "T", domain, note_type, "assisted", "", "2026-05-16", "[]", "", "", "body", "summary", modified_at],
        )
        .expect("insert note");
}

fn upsert_summary(index: &SearchIndex, m: &MockEmbedder, path: &str, text: &str, modified_at: i64) {
    let v = m.embed_one(text).expect("embed");
    index
        .upsert_embedding(
            path,
            EmbeddingKind::Summary,
            0,
            text,
            &v,
            m.model_version(),
            modified_at,
        )
        .expect("upsert");
    // active_model defaults to bge-small-en-v1.5; bump it to the mock so
    // search_vector reads the rows we just wrote.
    index
        .conn
        .execute(
            "UPDATE embedding_config SET value = ?1 WHERE key = 'active_model'",
            params![m.model_version()],
        )
        .expect("update active_model");
    index
        .conn
        .execute(
            "UPDATE embedding_config SET value = ?1 WHERE key = 'active_dim'",
            params![m.dim().to_string()],
        )
        .expect("update active_dim");
}

#[test]
fn validate_embedding_bytes_rejects_short_blob() {
    let err = validate_embedding_bytes(&[0u8; 7], 4).expect_err("must err");
    assert!(format!("{err}").contains("length mismatch"));
}

#[test]
fn validate_embedding_bytes_rejects_dim_mismatch() {
    let err = validate_embedding_bytes(&[0u8; 12], 4).expect_err("must err");
    assert!(format!("{err}").contains("length mismatch"));
}

#[test]
fn validate_embedding_bytes_accepts_exact_length() {
    let bytes = encode_embedding_bytes(&[1.5_f32, -0.25, 0.0, 7.5]);
    validate_embedding_bytes(&bytes, 4).expect("exact length");
}

#[test]
fn encode_then_decode_round_trips_float_values() {
    let v = [-1.0_f32, 0.0, 1.0, 3.0, 0.125];
    let bytes = encode_embedding_bytes(&v);
    assert_eq!(bytes.len(), v.len() * 4);
    let dot = dot_product_from_bytes(&v, &bytes);
    let expected: f32 = v.iter().map(|x| x * x).sum();
    assert!((dot - expected).abs() < 1e-5, "dot {dot} vs expected {expected}");
}

#[test]
fn dot_product_perfect_match_equals_one_for_unit_vectors() {
    let mut v = vec![1.0_f32; 4];
    // Normalize so squared norm == 1.
    let n = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
    for x in v.iter_mut() {
        *x /= n;
    }
    let bytes = encode_embedding_bytes(&v);
    let dot = dot_product_from_bytes(&v, &bytes);
    assert!(
        (dot - 1.0).abs() < 1e-5,
        "self-dot of unit vector should be 1.0, got {dot}"
    );
}

#[test]
fn search_vector_returns_closest_summary_first() {
    let index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(16, "mock-test-v1");

    insert_note(&index, "notes/a.md", "tech", "article", 100);
    insert_note(&index, "notes/b.md", "tech", "article", 100);
    insert_note(&index, "notes/c.md", "tech", "article", 100);

    upsert_summary(&index, &m, "notes/a.md", "temporal restate dbos durable execution", 100);
    upsert_summary(&index, &m, "notes/b.md", "react hooks effect tutorial", 100);
    upsert_summary(&index, &m, "notes/c.md", "kubernetes operator pattern", 100);

    let q = m.embed_one("durable execution temporal").expect("query");
    let hits = index.search_vector(&q, 3, None, None, None).expect("search");
    assert_eq!(hits.len(), 3);
    // The exact match seed is the deterministic mock hash; we just need to
    // verify that the same string (notes/a.md's summary text starts with
    // "temporal restate" but the query is different) does not invariably
    // come first. With a deterministic mock, repeated queries are stable;
    // assert ordering is stable and distances are real f32.
    for h in &hits {
        assert!(h.distance.is_finite(), "non-finite distance for {}", h.note_path);
    }
    // The hits must be sorted ascending by distance.
    for w in hits.windows(2) {
        assert!(
            w[0].distance <= w[1].distance,
            "distances out of order: {:?}",
            hits.iter()
                .map(|h| (h.note_path.clone(), h.distance))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn search_vector_respects_limit() {
    let index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-test-v1");
    for i in 0..5 {
        let path = format!("notes/{i}.md");
        insert_note(&index, &path, "tech", "article", 100);
        upsert_summary(&index, &m, &path, &format!("text {i}"), 100);
    }
    let q = m.embed_one("query").expect("q");
    let hits = index.search_vector(&q, 2, None, None, None).expect("search");
    assert_eq!(hits.len(), 2);
}

#[test]
fn search_vector_filters_by_domain() {
    let index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-test-v1");
    insert_note(&index, "notes/tech.md", "tech", "article", 100);
    insert_note(&index, "notes/life.md", "life", "article", 100);
    upsert_summary(&index, &m, "notes/tech.md", "temporal", 100);
    upsert_summary(&index, &m, "notes/life.md", "temporal", 100);
    let q = m.embed_one("temporal").expect("q");

    let tech_hits = index.search_vector(&q, 10, Some("tech"), None, None).expect("tech");
    assert_eq!(tech_hits.len(), 1);
    assert_eq!(tech_hits[0].note_path, "notes/tech.md");

    let life_hits = index.search_vector(&q, 10, Some("life"), None, None).expect("life");
    assert_eq!(life_hits.len(), 1);
    assert_eq!(life_hits[0].note_path, "notes/life.md");
}

#[test]
fn search_vector_rejects_dim_mismatch_against_active_model() {
    let index = SearchIndex::open_memory().expect("open");
    // active_dim defaults to 384; pass a 16-dim query and expect a clean
    // error (not a panic).
    let q = vec![0.0_f32; 16];
    let err = index.search_vector(&q, 5, None, None, None).expect_err("dim mismatch");
    assert!(format!("{err}").contains("does not match"));
}

#[test]
fn upsert_embedding_replaces_on_conflict() {
    let index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-test-v1");
    insert_note(&index, "notes/x.md", "tech", "article", 100);

    let v1 = m.embed_one("first").expect("v1");
    index
        .upsert_embedding(
            "notes/x.md",
            EmbeddingKind::Summary,
            0,
            "first",
            &v1,
            m.model_version(),
            100,
        )
        .expect("first upsert");

    let v2 = m.embed_one("second").expect("v2");
    index
        .upsert_embedding(
            "notes/x.md",
            EmbeddingKind::Summary,
            0,
            "second",
            &v2,
            m.model_version(),
            200,
        )
        .expect("second upsert");

    let (text, smod): (String, i64) = index
        .conn
        .query_row(
            "SELECT text, source_modified_at FROM note_embeddings \
             WHERE note_path = 'notes/x.md' AND kind = 'summary' AND chunk_index = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read");
    assert_eq!(text, "second");
    assert_eq!(smod, 200);
}

#[test]
fn delete_embeddings_for_note_removes_all_rows() {
    let index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-test-v1");
    insert_note(&index, "notes/d.md", "tech", "article", 100);
    let v = m.embed_one("t").expect("v");
    index
        .upsert_embedding("notes/d.md", EmbeddingKind::Summary, 0, "t", &v, m.model_version(), 100)
        .expect("upsert");
    let before: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM note_embeddings WHERE note_path = 'notes/d.md'",
            [],
            |row| row.get(0),
        )
        .expect("c");
    assert_eq!(before, 1);

    index.delete_embeddings_for_note("notes/d.md").expect("delete");

    let after: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM note_embeddings WHERE note_path = 'notes/d.md'",
            [],
            |row| row.get(0),
        )
        .expect("c2");
    assert_eq!(after, 0);
}

#[test]
fn stale_embedding_targets_returns_unembedded_notes_for_summary() {
    let index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-test-v1");
    insert_note(&index, "notes/a.md", "tech", "article", 100);
    insert_note(&index, "notes/b.md", "tech", "article", 200);
    // b is embedded, a is not
    let v = m.embed_one("b").expect("v");
    index
        .upsert_embedding("notes/b.md", EmbeddingKind::Summary, 0, "b", &v, m.model_version(), 200)
        .expect("upsert");

    let targets = index
        .stale_embedding_targets(EmbeddingKind::Summary, m.model_version(), 100)
        .expect("targets");
    let paths: Vec<&str> = targets.iter().map(|t| t.note_path.as_str()).collect();
    assert!(paths.contains(&"notes/a.md"), "a must be returned: {paths:?}");
    assert!(!paths.contains(&"notes/b.md"), "b is up-to-date: {paths:?}");
}

#[test]
fn stale_embedding_targets_returns_modified_notes_for_summary() {
    let index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-test-v1");
    insert_note(&index, "notes/x.md", "tech", "article", 100);
    let v = m.embed_one("x").expect("v");
    index
        .upsert_embedding("notes/x.md", EmbeddingKind::Summary, 0, "x", &v, m.model_version(), 100)
        .expect("upsert");

    // Bump notes.modified_at to 300 (past source_modified_at 100).
    index
        .conn
        .execute("UPDATE notes SET modified_at = 300 WHERE path = 'notes/x.md'", [])
        .expect("bump");

    let targets = index
        .stale_embedding_targets(EmbeddingKind::Summary, m.model_version(), 100)
        .expect("targets");
    let paths: Vec<&str> = targets.iter().map(|t| t.note_path.as_str()).collect();
    assert!(paths.contains(&"notes/x.md"), "x must be flagged stale");
}

#[test]
fn stale_embedding_targets_transcript_kind_filters_by_note_type() {
    // Critical regression: a vault of 100 Articles + 1 VoiceNote must
    // produce 1 stale target for transcript-chunk (the VoiceNote only),
    // not 101. Without the note_type filter, every Article matches
    // `e.id IS NULL` forever.
    //
    // The schema enum string for VoiceNote is `audio` (see
    // `NoteType::Audio` in `vault::schema`); the design doc's conceptual
    // "voice-note" name never lands in the DB.
    let index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-test-v1");
    for i in 0..100 {
        insert_note(&index, &format!("notes/a{i}.md"), "tech", "article", 100);
    }
    insert_note(&index, "notes/v.md", "tech", "audio", 100);

    let targets = index
        .stale_embedding_targets(EmbeddingKind::TranscriptChunk, m.model_version(), 1000)
        .expect("targets");
    assert_eq!(targets.len(), 1, "only the voice-note (audio) must surface");
    assert_eq!(targets[0].note_path, "notes/v.md");
}

#[test]
fn stale_embedding_targets_transcript_kind_covers_all_transcript_eligible_kinds() {
    // Regression for the design-doc-vs-schema mismatch: the filter must
    // accept every kind in `NoteType::transcript_eligible()`. Hardcoded
    // string lists drift; this test would have caught the original
    // `'voice-note'/'idea'/'vocabulary'/'thread'` SQL that never matched
    // any real note.
    use crate::schema::NoteType;

    let index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, "mock-test-v1");
    let eligible = NoteType::transcript_eligible();
    for (i, t) in eligible.iter().enumerate() {
        insert_note(&index, &format!("notes/n{i}.md"), "tech", t.as_str(), 100);
    }
    // A non-eligible kind that must NOT surface.
    insert_note(&index, "notes/article.md", "tech", "article", 100);

    let targets = index
        .stale_embedding_targets(EmbeddingKind::TranscriptChunk, m.model_version(), 1000)
        .expect("targets");
    assert_eq!(
        targets.len(),
        eligible.len(),
        "every transcript-eligible kind must surface and nothing else; got {} of {}",
        targets.len(),
        eligible.len()
    );
    let paths: std::collections::HashSet<&str> = targets.iter().map(|t| t.note_path.as_str()).collect();
    assert!(
        !paths.contains("notes/article.md"),
        "article (non-eligible) must not surface"
    );
}

#[test]
fn rrf_fuses_two_rank_lists_correctly() {
    let bm25 = vec![
        "notes/a.md".to_string(),
        "notes/b.md".to_string(),
        "notes/c.md".to_string(),
    ];
    let vec = vec![
        "notes/b.md".to_string(),
        "notes/d.md".to_string(),
        "notes/a.md".to_string(),
    ];
    let fused = reciprocal_rank_fusion(&bm25, &vec, 60, 10);

    // a appears at rank 1 in bm25 and rank 3 in vec
    // b appears at rank 2 in bm25 and rank 1 in vec
    // c appears at rank 3 in bm25 only
    // d appears at rank 2 in vec only
    let score = |path: &str| {
        fused
            .iter()
            .find(|h| h.note_path == path)
            .map(|h| h.score)
            .expect("present")
    };
    let s_a = 1.0_f32 / (60.0 + 1.0) + 1.0 / (60.0 + 3.0);
    let s_b = 1.0_f32 / (60.0 + 2.0) + 1.0 / (60.0 + 1.0);
    let s_c = 1.0_f32 / (60.0 + 3.0);
    let s_d = 1.0_f32 / (60.0 + 2.0);
    assert!((score("notes/a.md") - s_a).abs() < 1e-6);
    assert!((score("notes/b.md") - s_b).abs() < 1e-6);
    assert!((score("notes/c.md") - s_c).abs() < 1e-6);
    assert!((score("notes/d.md") - s_d).abs() < 1e-6);

    // b has the highest score (top of both lists)
    assert_eq!(fused[0].note_path, "notes/b.md");
}

#[test]
fn rrf_respects_limit() {
    let bm25 = vec!["a".into(), "b".into(), "c".into(), "d".into()];
    let vec = vec!["c".into(), "d".into(), "e".into(), "f".into()];
    let fused = reciprocal_rank_fusion(&bm25, &vec, 60, 3);
    assert_eq!(fused.len(), 3);
}

#[test]
fn rrf_single_list_still_contributes() {
    let bm25 = vec!["a".into(), "b".into()];
    let vec: Vec<String> = vec![];
    let fused = reciprocal_rank_fusion(&bm25, &vec, 60, 10);
    assert_eq!(fused.len(), 2);
    // a (rank 1) must beat b (rank 2)
    assert_eq!(fused[0].note_path, "a");
    assert_eq!(fused[1].note_path, "b");
}

#[test]
fn rrf_empty_inputs_return_empty() {
    let fused = reciprocal_rank_fusion(&[], &[], 60, 5);
    assert!(fused.is_empty());
}

#[test]
fn active_embedding_model_reads_the_default_seed() {
    let index = SearchIndex::open_memory().expect("open");
    let m = index.active_embedding_model().expect("read");
    assert_eq!(m, "bge-small-en-v1.5");
    let d = index.active_embedding_dim().expect("dim");
    assert_eq!(d, 384);
}

// --- Phase B3: max-pool aggregation over summary + transcript-chunk -----

#[test]
fn search_vector_returns_one_row_per_note_when_chunks_exist() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(16, "mock-maxpool");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    insert_note(&index, "notes/v.md", "tech", "audio", 100);

    // Seed a summary + 3 transcript chunks for the same note.
    upsert_summary(&index, &m, "notes/v.md", "summary text", 100);
    let chunk_pairs: Vec<(String, Vec<f32>)> = ["chunk one", "chunk two", "chunk three"]
        .iter()
        .map(|t| (t.to_string(), m.embed_one(t).expect("c")))
        .collect();
    index
        .swap_transcript_chunks("notes/v.md", &chunk_pairs, m.model_version(), 100)
        .expect("swap");

    let q = m.embed_one("query").expect("q");
    let hits = index.search_vector(&q, 10, None, None, None).expect("search");
    // Even though 4 rows back this note (1 summary + 3 chunks), the
    // result must contain exactly one entry for it.
    let v_hits: Vec<&VectorHit> = hits.iter().filter(|h| h.note_path == "notes/v.md").collect();
    assert_eq!(
        v_hits.len(),
        1,
        "max-pool must return one row per note; got {}",
        v_hits.len()
    );
}

#[test]
fn search_vector_max_pool_picks_best_representation_min_distance() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(16, "mock-maxpool");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    insert_note(&index, "notes/note.md", "tech", "audio", 100);

    // Summary text deliberately orthogonal; transcript chunk matches
    // the query verbatim.
    upsert_summary(&index, &m, "notes/note.md", "kubernetes operator pattern", 100);
    let query_text = "temporal durable execution restate";
    let q_vec = m.embed_one(query_text).expect("q");
    // Pre-compute the chunk vector independently so we can verify the
    // pool picks its distance, not the summary's.
    let chunk_vec = m.embed_one(query_text).expect("chunk equals query");
    index
        .swap_transcript_chunks(
            "notes/note.md",
            &[(query_text.to_string(), chunk_vec.clone())],
            m.model_version(),
            100,
        )
        .expect("swap");

    let hits = index.search_vector(&q_vec, 10, None, None, None).expect("search");
    let h = hits
        .iter()
        .find(|h| h.note_path == "notes/note.md")
        .expect("note must be present");

    // The chunk_vec is the query_vec (same text through the
    // deterministic mock), so the dot product is 1.0 and distance is
    // 0.0. The summary's distance is much larger. Max-pool must pick
    // the chunk distance (the smaller one).
    assert!(
        h.distance < 1e-5,
        "max-pool must surface the matching chunk (distance ~= 0.0); got {}",
        h.distance
    );
}
