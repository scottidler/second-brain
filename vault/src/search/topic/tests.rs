use super::*;
use crate::search::BatchUpsert;
use crate::search::SearchIndex;

const MODEL: &str = "test-model";
const DIM: usize = 4;

/// An index pinned to a 4-dim test model with `n` summary-embedded notes at
/// `notes/{i}.md`, each with a distinct axis-ish vector.
fn index_with_embedded_notes(n: usize) -> SearchIndex {
    let mut index = SearchIndex::open_memory().expect("open");
    index.set_active_embedding(MODEL, DIM).expect("pin");
    for i in 0..n {
        let path = format!("notes/{i}.md");
        index.insert_test_note_row(&path, "article", 1_000).expect("note");
        embed(&mut index, &path, [1.0, i as f32, 0.0, 0.0]);
    }
    index
}

fn embed(index: &mut SearchIndex, path: &str, v: [f32; 4]) {
    index
        .swap_embeddings_batch(&[BatchUpsert {
            note_path: path,
            kind: EmbeddingKind::Summary,
            chunk_index: 0,
            text: "summary",
            embedding: &v,
            model_version: MODEL,
            source_modified_at: 1_000,
        }])
        .expect("embed");
}

fn embedding_id(index: &SearchIndex, path: &str) -> i64 {
    index
        .conn
        .query_row(
            "SELECT id FROM note_embeddings WHERE note_path = ?1",
            params![path],
            |r| r.get(0),
        )
        .expect("id")
}

fn assignments(index: &SearchIndex, topics: &[u32]) -> Vec<Assignment> {
    topics
        .iter()
        .enumerate()
        .map(|(i, topic)| {
            let path = format!("notes/{i}.md");
            Assignment {
                embedding_id: embedding_id(index, &path),
                note_path: path,
                topic: *topic,
                cosine: 0.9,
            }
        })
        .collect()
}

fn centroids(k: usize) -> Vec<f32> {
    (0..k)
        .flat_map(|t| (0..DIM).map(move |d| if d == t % DIM { 1.0 } else { 0.0 }))
        .collect()
}

fn generation<'a>(
    centroids: &'a [f32],
    members: &'a [i64],
    assigns: &'a [Assignment],
    trained_at: i64,
) -> NewGeneration<'a> {
    NewGeneration {
        centroids,
        trained_rows: members,
        dim: DIM,
        model_version: MODEL,
        assignments: assigns,
        trained_at,
        seed: 7,
        epochs: 5.0,
        e_means_version: "0.2.1",
        train_mean_cosine: 0.84,
        exclude_types: &[NoteType::Entity, NoteType::Daily],
    }
}

/// Train a two-topic generation over the first `topics.len()` notes.
fn train(index: &mut SearchIndex, topics: &[u32], trained_at: i64) -> i64 {
    let a = assignments(index, topics);
    let c = centroids(2);
    index
        .replace_generation(&generation(&c, &[2, 1], &a, trained_at))
        .expect("replace")
}

type Dump = (
    Vec<(String, String)>,
    Vec<(i64, Vec<u8>, i64)>,
    Vec<(String, i64, i64, i64)>,
);

fn dump(index: &SearchIndex) -> Dump {
    let state = index
        .conn
        .prepare("SELECT key, value FROM topic_state ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let cents = index
        .conn
        .prepare("SELECT topic, centroid, trained_rows FROM topic_centroids ORDER BY topic")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let notes = index
        .conn
        .prepare("SELECT note_path, topic, embedding_id, assigned_at FROM note_topics ORDER BY note_path")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    (state, cents, notes)
}

fn status(index: &SearchIndex) -> Result<TopicStatus> {
    index.topic_state()?.status(MODEL, DIM)
}

#[test]
fn deleting_a_note_removes_its_topic_row() {
    let mut index = index_with_embedded_notes(3);
    train(&mut index, &[0, 0, 1], 5_000);
    assert_eq!(dump(&index).2.len(), 3);

    index
        .conn
        .execute("DELETE FROM notes WHERE path = 'notes/1.md'", [])
        .expect("delete note");

    let remaining: Vec<String> = dump(&index).2.into_iter().map(|row| row.0).collect();
    assert_eq!(remaining, ["notes/0.md", "notes/2.md"]);
}

#[test]
fn replace_generation_with_an_injected_insert_failure_leaves_all_three_tables_unchanged() {
    let mut index = index_with_embedded_notes(3);
    assert_eq!(train(&mut index, &[0, 0, 1], 5_000), 1);
    let before = dump(&index);
    // Every delete and the centroid inserts have already run inside the
    // transaction when this fires on the last assignment.
    index
        .conn
        .execute_batch(
            "CREATE TRIGGER fail_insert BEFORE INSERT ON note_topics WHEN NEW.note_path = 'notes/2.md'
             BEGIN SELECT RAISE(ABORT, 'injected insert failure'); END;",
        )
        .expect("trigger");

    let a = assignments(&index, &[1, 1, 0]);
    let c = centroids(2);
    let err = index
        .replace_generation(&generation(&c, &[1, 2], &a, 9_000))
        .expect_err("the injected failure propagates");

    assert!(format!("{err:#}").contains("injected"), "{err:#}");
    assert_eq!(dump(&index), before, "state, centroids and note_topics are unchanged");
}

#[test]
fn replace_generation_increments_and_writes_every_state_key() {
    let mut index = index_with_embedded_notes(3);
    assert_eq!(train(&mut index, &[0, 0, 1], 5_000), 1);
    assert_eq!(train(&mut index, &[1, 1, 0], 6_000), 2);

    let meta = index.topic_state().unwrap().meta().unwrap().expect("trained");
    assert_eq!(
        meta,
        TopicMeta {
            generation: 2,
            trained_at: 6_000,
            k: 2,
            seed: 7,
            epochs: 5.0,
            e_means_version: "0.2.1".into(),
            model_version: MODEL.into(),
            dim: DIM,
            trained_rows: 3,
            train_mean_cosine: 0.84,
            exclude_types: vec!["entity".into(), "daily".into()],
        }
    );
    let notes = dump(&index).2;
    assert_eq!(
        notes.iter().map(|n| n.1).collect::<Vec<_>>(),
        [1, 1, 0],
        "old assignments replaced"
    );
    assert!(
        notes.iter().all(|n| n.3 == 6_000),
        "train-time assigned_at is trained_at"
    );
    assert_eq!(status(&index).unwrap(), TopicStatus::Valid);
}

#[test]
fn replace_generation_refuses_a_generation_for_another_model_or_a_malformed_one() {
    let mut index = index_with_embedded_notes(2);
    let a = assignments(&index, &[0, 1]);
    let c = centroids(2);

    let mut wrong_model = generation(&c, &[1, 1], &a, 1);
    wrong_model.model_version = "other-model";
    let err = index
        .replace_generation(&wrong_model)
        .expect_err("model differs from the pin");
    assert!(err.to_string().contains("other-model"), "{err}");

    let short = &c[..DIM * 2 - 1];
    assert!(index.replace_generation(&generation(short, &[1, 1], &a, 1)).is_err());

    let off_the_end = [Assignment {
        topic: 2,
        ..a[0].clone()
    }];
    let err = index
        .replace_generation(&generation(&c, &[1, 1], &off_the_end, 1))
        .expect_err("topic >= k");
    assert!(err.to_string().contains("topic 2"), "{err}");

    assert!(
        index.replace_generation(&generation(&[], &[], &[], 1)).is_err(),
        "k = 0"
    );
    assert_eq!(status(&index).unwrap(), TopicStatus::Absent, "nothing was written");
}

#[test]
fn status_is_absent_on_an_untrained_index() {
    let index = index_with_embedded_notes(1);
    assert_eq!(status(&index).unwrap(), TopicStatus::Absent);
    assert_eq!(index.topic_state().unwrap().meta().unwrap(), None);
}

#[test]
fn status_is_stale_after_the_active_model_changes() {
    let mut index = index_with_embedded_notes(3);
    train(&mut index, &[0, 0, 1], 5_000);
    assert_eq!(status(&index).unwrap(), TopicStatus::Valid);

    index.set_active_embedding("next-model", DIM).expect("repin model");
    assert_eq!(
        index.topic_state().unwrap().status("next-model", DIM).unwrap(),
        TopicStatus::Stale
    );
    assert_eq!(
        index.topic_state().unwrap().status(MODEL, DIM + 1).unwrap(),
        TopicStatus::Stale,
        "a dim change is stale too"
    );
}

#[test]
fn status_is_an_error_on_a_short_centroid_blob() {
    let mut index = index_with_embedded_notes(3);
    train(&mut index, &[0, 0, 1], 5_000);
    index
        .conn
        .execute("UPDATE topic_centroids SET centroid = x'0000' WHERE topic = 1", [])
        .expect("corrupt");

    let err = status(&index).expect_err("corrupt, not absent and not valid");
    assert!(err.to_string().contains("topic 1"), "{err}");
}

#[test]
fn status_is_an_error_on_unparseable_or_inconsistent_state() {
    let mut index = index_with_embedded_notes(3);
    train(&mut index, &[0, 0, 1], 5_000);

    index
        .conn
        .execute("DELETE FROM topic_centroids WHERE topic = 1", [])
        .unwrap_err();
    index
        .conn
        .execute("DELETE FROM note_topics WHERE topic = 1", [])
        .unwrap();
    index
        .conn
        .execute("DELETE FROM topic_centroids WHERE topic = 1", [])
        .unwrap();
    assert!(
        status(&index)
            .expect_err("k=2 with one centroid")
            .to_string()
            .contains("k=2"),
        "centroid count differs from k"
    );

    index
        .conn
        .execute("UPDATE topic_state SET value = 'many' WHERE key = 'k'", [])
        .unwrap();
    assert!(
        status(&index).expect_err("unparseable k").to_string().contains("`k`"),
        "unparseable value"
    );
}

#[test]
fn status_is_an_error_when_state_is_missing_beside_surviving_rows() {
    let mut index = index_with_embedded_notes(2);
    train(&mut index, &[0, 1], 5_000);

    index
        .conn
        .execute("DELETE FROM topic_state WHERE key = 'trained_at'", [])
        .unwrap();
    assert!(
        status(&index).is_err(),
        "centroids with no trained_at are corrupt, not absent"
    );

    let mut index = index_with_embedded_notes(2);
    train(&mut index, &[0, 1], 5_000);
    index
        .conn
        .execute("DELETE FROM topic_state WHERE key = 'dim'", [])
        .unwrap();
    assert!(status(&index).is_err(), "a missing trained key is corrupt");
}

#[test]
fn clear_topics_empties_the_tables_and_keeps_the_generation_counter() {
    let mut index = index_with_embedded_notes(3);
    assert_eq!(train(&mut index, &[0, 0, 1], 5_000), 1);

    assert_eq!(index.clear_topics().expect("clear"), 3);

    let (state, cents, notes) = dump(&index);
    assert_eq!(state, [("generation".to_string(), "1".to_string())]);
    assert!(cents.is_empty() && notes.is_empty());
    assert_eq!(status(&index).unwrap(), TopicStatus::Absent);
    assert_eq!(train(&mut index, &[0, 0, 1], 7_000), 2, "the counter is never reused");
}

#[test]
fn assign_batch_inserts_overwrites_and_stamps_assigned_at() {
    let mut index = index_with_embedded_notes(3);
    train(&mut index, &[0, 0], 5_000);

    let a = assignments(&index, &[0, 0, 1]);
    assert_eq!(index.assign_batch(&a[2..], 8_000).expect("assign"), 1);
    let moved = [Assignment {
        topic: 1,
        ..a[0].clone()
    }];
    index.assign_batch(&moved, 9_000).expect("overwrite");

    let notes = dump(&index).2;
    assert_eq!(notes.len(), 3);
    assert_eq!((notes[0].1, notes[0].3), (1, 9_000), "existing row overwritten");
    assert_eq!((notes[2].1, notes[2].3), (1, 8_000), "new row stamped");
    assert_eq!(
        index.topic_state().unwrap().meta().unwrap().unwrap().generation,
        1,
        "generation unchanged"
    );
}

#[test]
fn assign_batch_with_an_unknown_topic_writes_nothing() {
    let mut index = index_with_embedded_notes(3);
    train(&mut index, &[0, 0], 5_000);
    let before = dump(&index);

    let a = assignments(&index, &[0, 0, 7]);
    let batch = [
        a[2].clone(),
        Assignment {
            topic: 0,
            ..a[0].clone()
        },
    ];
    assert!(index.assign_batch(&batch, 8_000).is_err(), "topic 7 has no centroid");
    assert_eq!(dump(&index), before);
}

#[test]
fn unassigned_lists_in_scope_notes_without_a_current_topic() {
    let mut index = index_with_embedded_notes(3);
    let rows = index.unassigned(&[], 100).expect("unassigned");
    assert_eq!(
        rows.iter().map(|r| r.note_path.as_str()).collect::<Vec<_>>(),
        ["notes/0.md", "notes/1.md", "notes/2.md"]
    );
    assert_eq!(rows[1].embedding, vec![1.0, 1.0, 0.0, 0.0]);
    assert_eq!(rows[1].embedding_id, embedding_id(&index, "notes/1.md"));

    train(&mut index, &[0, 0], 5_000);
    let rows = index.unassigned(&[], 100).expect("after train");
    assert_eq!(
        rows.iter().map(|r| r.note_path.as_str()).collect::<Vec<_>>(),
        ["notes/2.md"]
    );

    // A re-embed changes note_embeddings.id, so the note is unassigned again.
    embed(&mut index, "notes/0.md", [0.0, 1.0, 0.0, 0.0]);
    let rows = index.unassigned(&[], 100).expect("after re-embed");
    assert_eq!(
        rows.iter().map(|r| r.note_path.as_str()).collect::<Vec<_>>(),
        ["notes/0.md", "notes/2.md"]
    );

    assert_eq!(index.unassigned(&[], 1).unwrap().len(), 1, "limit applies");
}

#[test]
fn unassigned_skips_excluded_types_low_quality_tombstones_and_other_models() {
    let mut index = index_with_embedded_notes(1);
    for (path, note_type) in [
        ("e.md", "entity"),
        ("low.md", "article"),
        ("tomb.md", "article"),
        ("other.md", "article"),
    ] {
        index.insert_test_note_row(path, note_type, 1_000).expect("note");
    }
    for path in ["e.md", "low.md", "tomb.md"] {
        embed(&mut index, path, [0.0, 0.0, 1.0, 0.0]);
    }
    index
        .swap_embeddings_batch(&[BatchUpsert {
            note_path: "other.md",
            kind: EmbeddingKind::Summary,
            chunk_index: 0,
            text: "s",
            embedding: &[0.0, 0.0, 0.0, 1.0],
            model_version: "not-the-active-model",
            source_modified_at: 1_000,
        }])
        .expect("other model");
    index
        .conn
        .execute("UPDATE notes SET quality = 'low' WHERE path = 'low.md'", [])
        .unwrap();
    index
        .conn
        .execute("UPDATE notes SET superseded_by = 'x' WHERE path = 'tomb.md'", [])
        .unwrap();

    let all: Vec<_> = index
        .unassigned(&[], 100)
        .unwrap()
        .into_iter()
        .map(|r| r.note_path)
        .collect();
    assert_eq!(
        all,
        ["e.md", "notes/0.md"],
        "low quality, tombstones and other-model rows never count"
    );

    let scoped: Vec<_> = index
        .unassigned(&[NoteType::Entity], 100)
        .unwrap()
        .into_iter()
        .map(|r| r.note_path)
        .collect();
    assert_eq!(scoped, ["notes/0.md"], "excluded types are parameters");
}

#[test]
fn unassigned_errors_on_a_blob_of_the_wrong_length() {
    let index = index_with_embedded_notes(1);
    index
        .conn
        .execute("UPDATE note_embeddings SET embedding = x'00'", [])
        .unwrap();
    let err = index.unassigned(&[], 10).expect_err("bad blob");
    assert!(err.to_string().contains("notes/0.md"), "{err}");
}

#[test]
fn topic_centroids_round_trip_when_valid_and_refuse_otherwise() {
    let mut index = index_with_embedded_notes(3);
    assert!(index.topic_centroids().is_err(), "absent");

    train(&mut index, &[0, 0, 1], 5_000);
    let (meta, flat) = index.topic_centroids().expect("valid");
    assert_eq!((meta.generation, meta.k), (1, 2));
    assert_eq!(flat, centroids(2));

    index.set_active_embedding("next-model", DIM).unwrap();
    assert!(index.topic_centroids().is_err(), "stale");
}

#[test]
fn a_model_id_rename_carries_the_topic_generation_with_it() {
    let mut index = index_with_embedded_notes(2);
    let legacy = "bge-small-en-v1.5-candle";
    let current = crate::embedding::BGE_SMALL_MODEL_VERSION;
    index.set_active_embedding(legacy, DIM).unwrap();
    index
        .swap_embeddings_batch(&[BatchUpsert {
            note_path: "notes/0.md",
            kind: EmbeddingKind::Summary,
            chunk_index: 0,
            text: "s",
            embedding: &[1.0, 0.0, 0.0, 0.0],
            model_version: legacy,
            source_modified_at: 1_000,
        }])
        .unwrap();
    let a = assignments(&index, &[0]);
    let c = centroids(2);
    let mut generation = generation(&c, &[1, 0], &a, 5_000);
    generation.model_version = legacy;
    index
        .replace_generation(&generation)
        .expect("train under the legacy id");

    assert!(index.migrate_backend_free_model_ids().expect("migrate"));

    assert_eq!(
        index.topic_state().unwrap().meta().unwrap().unwrap().model_version,
        current
    );
    assert_eq!(
        index.topic_state().unwrap().status(current, DIM).unwrap(),
        TopicStatus::Valid
    );
}
