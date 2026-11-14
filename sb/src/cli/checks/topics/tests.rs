use super::*;
use crate::cli::checks::Severity;
use vault::schema::NoteType;
use vault::search::{Assignment, BatchUpsert, EmbeddingKind, NewGeneration};

const MODEL: &str = "test-model";
const DIM: usize = 4;
const TRAINED_AT: i64 = 1_000_000;

/// An on-disk index with two embedded notes, one centroid, a generation
/// trained at `TRAINED_AT`, and one note assigned an hour after the train.
fn trained_db() -> (tempfile::TempDir, std::path::PathBuf, SearchIndex) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("oracle.db");
    let mut index = SearchIndex::open(&path).expect("open");
    index.set_active_embedding(MODEL, DIM).expect("pin");
    for note in ["notes/a.md", "notes/b.md"] {
        index.insert_test_note_row(note, "article", 100).expect("note");
        index
            .swap_embeddings_batch(&[BatchUpsert {
                note_path: note,
                kind: EmbeddingKind::Summary,
                chunk_index: 0,
                text: "summary",
                embedding: &[1.0, 0.0, 0.0, 0.0],
                model_version: MODEL,
                source_modified_at: 100,
            }])
            .expect("embed");
    }
    index
        .replace_generation(&NewGeneration {
            centroids: &[1.0, 0.0, 0.0, 0.0],
            trained_rows: &[1],
            dim: DIM,
            model_version: MODEL,
            assignments: &[Assignment {
                note_path: "notes/a.md".to_string(),
                topic: 0,
                cosine: 0.9,
                embedding_id: 1,
            }],
            trained_at: TRAINED_AT,
            seed: 7,
            epochs: 5.0,
            e_means_version: "0.2.1",
            train_mean_cosine: 0.9,
            exclude_types: &[NoteType::Entity],
        })
        .expect("generation");
    index
        .assign_batch(
            &[Assignment {
                note_path: "notes/b.md".to_string(),
                topic: 0,
                cosine: 0.6,
                embedding_id: 2,
            }],
            TRAINED_AT + 3_600,
        )
        .expect("assign");
    (dir, path, index)
}

fn findings_at(path: &std::path::Path, now: i64) -> Vec<Finding> {
    findings_for(SearchIndex::peek_topics(path, &[NoteType::Entity]), now)
}

#[test]
fn a_valid_generation_is_ok_and_reports_coverage_and_drift() {
    let (_dir, path, _index) = trained_db();
    let findings = findings_at(&path, TRAINED_AT + 2 * 86_400);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Ok);
    let msg = &findings[0].message;
    for want in [
        "status: valid",
        "generation 1",
        "trained 2d ago",
        "assigned 2 / in-scope 2",
        "mean cosine 0.600 over 1 note(s)",
        "train-mean-cosine 0.900",
    ] {
        assert!(msg.contains(want), "missing {want:?} in {msg}");
    }
}

#[test]
fn a_stale_generation_warns_and_names_the_train_command() {
    let (_dir, path, mut index) = trained_db();
    index.set_active_embedding("next-model", DIM).expect("repin");
    let findings = findings_at(&path, TRAINED_AT + 60);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Warn);
    assert!(findings[0].message.contains("status: stale"), "{}", findings[0].message);
    assert_eq!(findings[0].suggested_fix.as_deref(), Some("sb cortex topics --train"));
}

#[test]
fn an_absent_generation_is_informational_and_names_train() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("oracle.db");
    SearchIndex::open(&path)
        .expect("open")
        .set_active_embedding(MODEL, DIM)
        .expect("pin");
    let findings = findings_at(&path, TRAINED_AT);
    assert_eq!(findings[0].severity, Severity::Info);
    assert!(findings[0].message.contains("status: absent"));
    assert!(findings[0].message.contains("sb cortex topics --train"));
}

#[test]
fn no_search_db_is_informational() {
    let dir = tempfile::tempdir().expect("tempdir");
    let findings = findings_at(&dir.path().join("none.db"), TRAINED_AT);
    assert_eq!(findings[0].severity, Severity::Info);
}

#[test]
fn unreadable_state_is_an_error() {
    let findings = findings_for(Err(eyre::eyre!("corrupt topic state")), TRAINED_AT);
    assert_eq!(findings[0].severity, Severity::Error);
    assert!(findings[0].message.contains("corrupt topic state"));
}

#[test]
fn age_text_picks_the_largest_unit() {
    assert_eq!(age_text(59), "0m");
    assert_eq!(age_text(7_200), "2h");
    assert_eq!(age_text(3 * 86_400 + 5), "3d");
    assert_eq!(age_text(-5), "0m");
}
