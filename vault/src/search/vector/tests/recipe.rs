//! Embed-recipe versioning (`2026-10-29-retrieval-quality-from-rag-scour`):
//! the recipe stamp, the stale and sentinel clauses, DISTINCT
//! before LIMIT, the no-source row clear, and the drop that takes sentinels.

use super::*;

const MODEL: &str = "mock-recipe-test";

fn index_with_model() -> (SearchIndex, MockEmbedder) {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(8, MODEL);
    index
        .set_active_embedding(m.model_version(), m.dim())
        .expect("set model");
    (index, m)
}

fn chunks(m: &MockEmbedder, n: usize) -> Vec<(String, Vec<f32>)> {
    (0..n)
        .map(|i| {
            let t = format!("chunk {i}");
            let v = m.embed_one(&t).expect("embed");
            (t, v)
        })
        .collect()
}

fn paths(targets: &[StaleTarget]) -> Vec<String> {
    let mut p: Vec<String> = targets.iter().map(|t| t.note_path.clone()).collect();
    p.sort();
    p
}

fn stale(index: &SearchIndex, kind: EmbeddingKind, limit: u32) -> Vec<StaleTarget> {
    index.stale_embedding_targets(kind, MODEL, limit).expect("stale")
}

/// Notes `a` and `b` carry current summary, transcript-chunk and claim rows;
/// `c` is a transcript note with no transcript, so it holds only a current
/// transcript-chunk sentinel (plus a summary row).
fn seed_current(index: &mut SearchIndex, m: &MockEmbedder) {
    for (path, n) in [("notes/a.md", 3), ("notes/b.md", 2), ("notes/c.md", 0)] {
        insert_note(index, path, "youtube", 100);
        index.set_test_claims(path, "claim one\nclaim two").expect("claims");
        let v = m.embed_one(path).expect("embed");
        index
            .upsert_embedding(path, EmbeddingKind::Summary, 0, path, &v, MODEL, 100)
            .expect("summary");
        index
            .swap_kind_chunks(path, EmbeddingKind::Claim, &chunks(m, 1), MODEL, 100)
            .expect("claim");
        if n > 0 {
            index
                .swap_kind_chunks(path, EmbeddingKind::TranscriptChunk, &chunks(m, n), MODEL, 100)
                .expect("chunks");
        }
    }
    index
        .mark_nothing_to_embed_batch(
            EmbeddingKind::TranscriptChunk,
            MODEL,
            &[("notes/c.md".to_string(), 100)],
        )
        .expect("sentinel");
}

#[test]
fn recipe_bump_marks_only_that_kind_stale() {
    let (mut index, m) = index_with_model();
    seed_current(&mut index, &m);
    for kind in [
        EmbeddingKind::Summary,
        EmbeddingKind::TranscriptChunk,
        EmbeddingKind::Claim,
    ] {
        assert!(stale(&index, kind, 10).is_empty(), "{kind:?} starts current");
    }

    // Rows and sentinels stamped one below the constant: what a bump of the
    // transcript-chunk constant looks like to the query.
    let bumped = EmbeddingKind::TranscriptChunk;
    index.set_test_recipe(bumped, bumped.recipe() - 1).expect("restamp");

    assert_eq!(
        paths(&stale(&index, bumped, 10)),
        vec!["notes/a.md", "notes/b.md", "notes/c.md"],
        "every note with a row of the bumped kind is stale, and so is c, whose old-recipe sentinel no longer masks it"
    );
    assert!(
        stale(&index, EmbeddingKind::Summary, 10).is_empty(),
        "summary untouched"
    );
    assert!(stale(&index, EmbeddingKind::Claim, 10).is_empty(), "claim untouched");
}

#[test]
fn stale_targets_distinct() {
    let (mut index, m) = index_with_model();
    for path in ["notes/a.md", "notes/b.md"] {
        insert_note(&index, path, "youtube", 100);
        index
            .swap_kind_chunks(path, EmbeddingKind::TranscriptChunk, &chunks(&m, 9), MODEL, 100)
            .expect("chunks");
    }
    // Re-modify a later than b so the ORDER BY is deterministic: a first.
    index.set_test_modified_at("notes/a.md", 300).expect("touch a");
    index.set_test_modified_at("notes/b.md", 200).expect("touch b");

    let one = stale(&index, EmbeddingKind::TranscriptChunk, 10);
    assert_eq!(
        paths(&one),
        vec!["notes/a.md", "notes/b.md"],
        "each note once, not 9 times"
    );

    let two = stale(&index, EmbeddingKind::TranscriptChunk, 2);
    assert_eq!(
        paths(&two),
        vec!["notes/a.md", "notes/b.md"],
        "LIMIT counts notes: a's 9 stale rows must not fill both slots"
    );
}

#[test]
fn nothing_to_embed_clears_that_kinds_rows_only() {
    let (mut index, m) = index_with_model();
    seed_current(&mut index, &m);
    assert_eq!(index.transcript_chunk_count("notes/a.md").expect("count"), 3);

    index.set_test_modified_at("notes/a.md", 200).expect("touch");
    index
        .mark_nothing_to_embed_batch(
            EmbeddingKind::TranscriptChunk,
            MODEL,
            &[("notes/a.md".to_string(), 200)],
        )
        .expect("sentinel");

    assert_eq!(
        index.transcript_chunk_count("notes/a.md").expect("count"),
        0,
        "old chunk rows cleared"
    );
    assert_eq!(
        index
            .examined_watermark("notes/a.md", EmbeddingKind::TranscriptChunk, MODEL)
            .expect("watermark"),
        Some(200)
    );
    assert!(
        index
            .embedding_text("notes/a.md", EmbeddingKind::Summary)
            .expect("summary")
            .is_some(),
        "the summary row is another kind and stays"
    );
    assert_eq!(
        index.transcript_chunk_count("notes/b.md").expect("count"),
        2,
        "other notes stay"
    );
    assert!(stale(&index, EmbeddingKind::TranscriptChunk, 10).is_empty());
}

#[test]
fn drop_kind_clears_that_kinds_sentinels() {
    let (mut index, m) = index_with_model();
    seed_current(&mut index, &m);
    index
        .mark_nothing_to_embed_batch(EmbeddingKind::Claim, MODEL, &[("notes/c.md".to_string(), 100)])
        .expect("claim sentinel");

    let dropped = index.drop_embedding_kind(EmbeddingKind::TranscriptChunk).expect("drop");
    assert_eq!(dropped.rows, 5, "a's 3 + b's 2 chunk rows");
    assert_eq!(dropped.sentinels, 1, "c's transcript-chunk sentinel");

    assert_eq!(
        paths(&stale(&index, EmbeddingKind::TranscriptChunk, 10)),
        vec!["notes/a.md", "notes/b.md", "notes/c.md"],
        "a backfill after the drop re-selects every note, c included"
    );
    assert_eq!(
        index
            .examined_watermark("notes/c.md", EmbeddingKind::Claim, MODEL)
            .expect("watermark"),
        Some(100),
        "another kind's sentinel stays"
    );
}

#[test]
fn summary_swap_replaces_the_notes_rows_for_that_model_only() {
    let (mut index, m) = index_with_model();
    insert_note(&index, "notes/a.md", "article", 100);
    let v = m.embed_one("x").expect("embed");
    // A stray second summary row and another model's row.
    index
        .upsert_embedding("notes/a.md", EmbeddingKind::Summary, 1, "stray", &v, MODEL, 50)
        .expect("stray");
    index
        .upsert_embedding(
            "notes/a.md",
            EmbeddingKind::Summary,
            0,
            "other model",
            &v,
            "other-model",
            50,
        )
        .expect("other");

    index
        .swap_embeddings_batch(&[BatchUpsert {
            note_path: "notes/a.md",
            kind: EmbeddingKind::Summary,
            chunk_index: 0,
            text: "fresh",
            embedding: &v,
            model_version: MODEL,
            source_modified_at: 100,
        }])
        .expect("swap");

    let rows: Vec<(String, String)> = {
        let mut stmt = index
            .conn
            .prepare(
                "SELECT model_version, text FROM note_embeddings WHERE note_path = 'notes/a.md' ORDER BY model_version",
            )
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    };
    assert_eq!(
        rows,
        vec![
            (MODEL.to_string(), "fresh".to_string()),
            ("other-model".to_string(), "other model".to_string()),
        ],
        "the stray row of this model is gone; the other model's row stays"
    );
}

#[test]
fn sentinelled_notes_skip_an_old_recipe_sentinel() {
    let (mut index, m) = index_with_model();
    seed_current(&mut index, &m);
    let held = index
        .sentinelled_notes(EmbeddingKind::TranscriptChunk, MODEL)
        .expect("held");
    assert_eq!(held.len(), 1, "c is held at the current recipe");

    index
        .set_test_recipe(
            EmbeddingKind::TranscriptChunk,
            EmbeddingKind::TranscriptChunk.recipe() - 1,
        )
        .expect("restamp");
    assert!(
        index
            .sentinelled_notes(EmbeddingKind::TranscriptChunk, MODEL)
            .expect("held")
            .is_empty(),
        "an old-recipe sentinel holds nothing out, so it is not reported as held"
    );
}
