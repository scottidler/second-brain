use std::cell::RefCell;
use std::collections::HashMap;

use super::*;
use crate::schema::NoteType;
use crate::search::SearchIndex;
use crate::search::topic::{Assignment, NewGeneration};

const MODEL: &str = "test-model";
const DIM: usize = 4;

/// A pinned index with `titles.len()` notes at `notes/{i}.md`, each tagged
/// from the matching entry of `tags`.
fn index_with_notes(mut index: SearchIndex, tags: &[Vec<&str>]) -> SearchIndex {
    index.set_active_embedding(MODEL, DIM).expect("pin");
    for (i, note_tags) in tags.iter().enumerate() {
        let path = format!("notes/{i}.md");
        index.insert_test_note_row(&path, "article", 1_000).expect("note");
        index.set_test_title(&path, &format!("title {i}")).expect("title");
        index.set_test_tags(&path, note_tags).expect("tags");
    }
    index
}

fn memory_index(tags: &[Vec<&str>]) -> SearchIndex {
    index_with_notes(SearchIndex::open_memory().expect("open"), tags)
}

/// Train a `k`-topic generation. `rows` is `(note index, topic, cosine)`.
fn train(index: &mut SearchIndex, k: usize, rows: &[(usize, u32, f32)]) -> i64 {
    let assignments: Vec<Assignment> = rows
        .iter()
        .map(|(note, topic, cosine)| Assignment {
            note_path: format!("notes/{note}.md"),
            topic: *topic,
            cosine: *cosine,
            embedding_id: *note as i64 + 1,
        })
        .collect();
    let centroids: Vec<f32> = (0..k)
        .flat_map(|t| (0..DIM).map(move |d| if d == t % DIM { 1.0 } else { 0.0 }))
        .collect();
    let members = vec![1_i64; k];
    index
        .replace_generation(&NewGeneration {
            centroids: &centroids,
            trained_rows: &members,
            dim: DIM,
            model_version: MODEL,
            assignments: &assignments,
            trained_at: 5_000,
            seed: 7,
            epochs: 5.0,
            e_means_version: "0.2.1",
            train_mean_cosine: 0.8,
            exclude_types: &[NoteType::Entity],
        })
        .expect("replace generation")
}

fn tags_of(summary: &TopicSummary) -> Vec<&str> {
    summary.tags.iter().map(|t| t.tag.as_str()).collect()
}

fn strings(tags: &[&str]) -> Vec<String> {
    tags.iter().map(|t| t.to_string()).collect()
}

#[test]
fn untrained_index_reads_absent_with_no_topics() {
    let index = memory_index(&[vec!["a"], vec!["a"]]);
    let overview = index.overview(None, false).expect("overview");
    assert_eq!(overview.snapshot.status, TopicStatus::Absent);
    assert_eq!(overview.snapshot.meta, None);
    assert_eq!(overview.snapshot.assigned, 0);
    assert!(overview.topics.is_empty());
    assert!(matches!(
        index.notes(0, None, None, false, 10).expect("notes"),
        TopicNotes::Unavailable(s) if s.status == TopicStatus::Absent
    ));
    assert_eq!(index.topic_snapshot().expect("snapshot").status, TopicStatus::Absent);
}

#[test]
fn an_unpinned_index_reads_absent_not_an_error() {
    let index = SearchIndex::open_memory().expect("open");
    let overview = index.overview(None, false).expect("overview");
    assert_eq!(overview.snapshot.status, TopicStatus::Absent);
}

#[test]
fn a_generation_after_the_model_changes_reads_stale_and_empty() {
    let mut index = memory_index(&[vec!["a"], vec!["a"]]);
    train(&mut index, 1, &[(0, 0, 0.9), (1, 0, 0.8)]);
    index.set_active_embedding("another-model", DIM).expect("repin");
    let overview = index.overview(None, false).expect("overview");
    assert_eq!(overview.snapshot.status, TopicStatus::Stale);
    assert!(overview.topics.is_empty());
    assert_eq!(overview.snapshot.meta.expect("meta").generation, 1);
    assert!(matches!(
        index.notes(0, None, None, false, 10).expect("notes"),
        TopicNotes::Unavailable(s) if s.status == TopicStatus::Stale
    ));
}

#[test]
fn overview_returns_every_non_empty_topic_biggest_first_ties_by_id() {
    // k = 4; topic 3 holds nothing. Sizes: topic 0 = 1, topic 1 = 3, topic 2 = 1.
    let mut index = memory_index(&vec![vec![]; 5]);
    train(
        &mut index,
        4,
        &[(0, 0, 0.9), (1, 1, 0.9), (2, 1, 0.8), (3, 1, 0.7), (4, 2, 0.6)],
    );
    let overview = index.overview(None, false).expect("overview");
    let shape: Vec<(u32, usize)> = overview.topics.iter().map(|t| (t.topic, t.notes)).collect();
    assert_eq!(shape, vec![(1, 3), (0, 1), (2, 1)]);
    assert_eq!(overview.snapshot.assigned, 5);
}

#[test]
fn exemplars_are_the_three_nearest_titles() {
    let mut index = memory_index(&vec![vec![]; 5]);
    train(
        &mut index,
        1,
        &[(0, 0, 0.50), (1, 0, 0.95), (2, 0, 0.60), (3, 0, 0.90), (4, 0, 0.99)],
    );
    let overview = index.overview(None, false).expect("overview");
    assert_eq!(overview.topics[0].exemplars, vec!["title 4", "title 1", "title 3"]);
}

/// 6 notes in one topic: 2 carry only `a`, 2 only `b`, 2 both. `any` of
/// [a, b] counts 6, `all` counts 2.
#[test]
fn tags_any_and_all_give_different_counts_on_a_separating_fixture() {
    let mut index = memory_index(&[
        vec!["a"],
        vec!["a"],
        vec!["b"],
        vec!["b"],
        vec!["a", "b"],
        vec!["a", "b"],
    ]);
    train(&mut index, 1, &(0..6).map(|i| (i, 0, 0.9)).collect::<Vec<_>>());
    let wanted = strings(&["a", "b"]);
    let any = index.overview(Some(&wanted), false).expect("any");
    let all = index.overview(Some(&wanted), true).expect("all");
    assert_eq!(any.topics[0].notes, 6);
    assert_eq!(all.topics[0].notes, 2);
}

#[test]
fn a_filter_drops_zero_count_topics_and_leaves_the_label_alone() {
    // Topic 0: 10 notes all tagged `rust`; topic 1: 10 notes, `go`. Plus 20
    // untagged notes in topic 2 so `rust` and `go` are over-represented.
    let mut tags: Vec<Vec<&str>> = Vec::new();
    tags.extend(vec![vec!["rust"]; 10]);
    tags.extend(vec![vec!["go"]; 10]);
    tags.extend(vec![vec![]; 20]);
    let mut index = memory_index(&tags);
    let rows: Vec<(usize, u32, f32)> = (0..40)
        .map(|i| {
            (
                i,
                if i < 10 {
                    0
                } else if i < 20 {
                    1
                } else {
                    2
                },
                0.9,
            )
        })
        .collect();
    train(&mut index, 3, &rows);

    let unfiltered = index.overview(None, false).expect("overview");
    let wanted = strings(&["go"]);
    let filtered = index.overview(Some(&wanted), false).expect("filtered");

    assert_eq!(filtered.topics.len(), 1, "topics with no `go` note are dropped");
    assert_eq!(filtered.topics[0].topic, 1);
    let label_of = |overview: &TopicOverview, topic: u32| {
        overview
            .topics
            .iter()
            .find(|t| t.topic == topic)
            .map(|t| t.tags.clone())
            .expect("topic present")
    };
    assert_eq!(label_of(&filtered, 1), label_of(&unfiltered, 1));
    assert_eq!(
        tags_of(unfiltered.topics.iter().find(|t| t.topic == 0).unwrap()),
        ["rust"]
    );
}

#[test]
fn a_tag_on_30_of_40_topic_notes_and_30_of_400_assigned_labels_the_topic() {
    // Topic 0: 40 notes, 30 carry `rag`. 360 other notes in topic 1, none do.
    let mut tags: Vec<Vec<&str>> = Vec::new();
    tags.extend(vec![vec!["rag"]; 30]);
    tags.extend(vec![vec![]; 10]);
    tags.extend(vec![vec![]; 360]);
    let mut index = memory_index(&tags);
    let rows: Vec<(usize, u32, f32)> = (0..400).map(|i| (i, if i < 40 { 0 } else { 1 }, 0.9)).collect();
    train(&mut index, 2, &rows);

    let overview = index.overview(None, false).expect("overview");
    let topic0 = overview.topics.iter().find(|t| t.topic == 0).expect("topic 0");
    assert_eq!(tags_of(topic0), ["rag"]);
    let rag = &topic0.tags[0];
    assert_eq!(rag.notes, 30);
    assert!(
        (rag.lift - 10.0).abs() < 1e-9,
        "lift = (30/40)/(30/400) = 10, got {}",
        rag.lift
    );
}

#[test]
fn a_tag_on_every_note_never_labels_a_topic() {
    let mut index = memory_index(&vec![vec!["everywhere"]; 20]);
    train(
        &mut index,
        2,
        &(0..20).map(|i| (i, (i % 2) as u32, 0.9)).collect::<Vec<_>>(),
    );
    let overview = index.overview(None, false).expect("overview");
    for topic in &overview.topics {
        assert!(topic.tags.is_empty(), "lift 1.0 must not label: {:?}", topic.tags);
    }
}

#[test]
fn a_tag_on_3_of_40_topic_notes_never_labels_even_at_high_lift() {
    // 3 of 40 in topic 0 carry `rare`; nobody else does, so lift is huge (the
    // count floor is what keeps it out: 3 < max(5, 8)).
    let mut tags: Vec<Vec<&str>> = Vec::new();
    tags.extend(vec![vec!["rare"]; 3]);
    tags.extend(vec![vec![]; 37]);
    tags.extend(vec![vec![]; 360]);
    let mut index = memory_index(&tags);
    let rows: Vec<(usize, u32, f32)> = (0..400).map(|i| (i, if i < 40 { 0 } else { 1 }, 0.9)).collect();
    train(&mut index, 2, &rows);
    let overview = index.overview(None, false).expect("overview");
    let topic0 = overview.topics.iter().find(|t| t.topic == 0).expect("topic 0");
    assert!(topic0.tags.is_empty(), "{:?}", topic0.tags);
}

#[test]
fn the_share_floor_is_twenty_percent_of_the_topic_rounded_down() {
    // 50 notes: floor(20%) = 10 > 5. 9 carriers fail, 10 pass.
    let overall: HashMap<String, usize> = [("t".to_string(), 10)].into();
    let nine = label_tags(50, 1000, &[("t".to_string(), 9)], &overall);
    assert!(nine.is_empty());
    let ten = label_tags(50, 1000, &[("t".to_string(), 10)], &overall);
    assert_eq!(ten.len(), 1);
}

#[test]
fn label_takes_the_top_three_by_lift_with_ties_by_tag_name() {
    let overall: HashMap<String, usize> = ["a", "b", "c", "d", "e"].iter().map(|t| (t.to_string(), 10)).collect();
    // Same global count: lift order follows the topic count. `d` and `c` and
    // `b` tie at 8; `e` is highest at 9; `a` is lowest at 6 and falls off.
    let in_topic: Vec<(String, usize)> = [("a", 6), ("d", 8), ("c", 8), ("b", 8), ("e", 9)]
        .iter()
        .map(|(t, c)| (t.to_string(), *c))
        .collect();
    let labels = label_tags(10, 100, &in_topic, &overall);
    let names: Vec<&str> = labels.iter().map(|t| t.tag.as_str()).collect();
    assert_eq!(names, ["e", "b", "c"]);
}

#[test]
fn a_lift_of_exactly_two_labels_and_just_under_does_not() {
    let overall: HashMap<String, usize> = [("t".to_string(), 25)].into();
    // topic 10 of 100 assigned, 5 carry it: (5/10) / (25/100) = 2.0
    assert_eq!(label_tags(10, 100, &[("t".to_string(), 5)], &overall).len(), 1);
    let overall: HashMap<String, usize> = [("t".to_string(), 26)].into();
    assert!(label_tags(10, 100, &[("t".to_string(), 5)], &overall).is_empty());
}

#[test]
fn in_tags_returns_the_topics_holding_a_tag_most_first_and_limited() {
    // `x` is on 3 notes of topic 2, 2 of topic 0, 1 of topic 1.
    let mut index = memory_index(&[vec!["x"], vec!["x"], vec!["x"], vec!["x"], vec!["x"], vec!["x"], vec![]]);
    train(
        &mut index,
        3,
        &[
            (0, 2, 0.9),
            (1, 2, 0.9),
            (2, 2, 0.9),
            (3, 0, 0.9),
            (4, 0, 0.9),
            (5, 1, 0.9),
            (6, 1, 0.9),
        ],
    );
    let all = index.in_tags("x", 8).expect("in_tags");
    let shape: Vec<(u32, usize)> = all.topics.iter().map(|t| (t.topic, t.notes)).collect();
    assert_eq!(shape, vec![(2, 3), (0, 2), (1, 1)]);
    assert!(all.topics.iter().all(|t| t.exemplars.is_empty()));
    let two = index.in_tags("x", 2).expect("limited");
    assert_eq!(two.topics.len(), 2);
    let none = index.in_tags("nope", 8).expect("unknown tag");
    assert!(none.topics.is_empty());
    assert_eq!(none.snapshot.status, TopicStatus::Valid);
}

#[test]
fn notes_are_nearest_the_centroid_first_and_filter_by_tag() {
    let mut index = memory_index(&[vec!["a"], vec!["b"], vec!["a"], vec![]]);
    train(&mut index, 2, &[(0, 0, 0.5), (1, 0, 0.9), (2, 0, 0.7), (3, 1, 0.99)]);
    let TopicNotes::Notes { notes, snapshot } = index.notes(0, None, None, false, 10).expect("notes") else {
        panic!("expected notes");
    };
    assert_eq!(snapshot.status, TopicStatus::Valid);
    let paths: Vec<&str> = notes.iter().map(|n| n.path.as_str()).collect();
    assert_eq!(paths, ["notes/1.md", "notes/2.md", "notes/0.md"]);

    let wanted = strings(&["a"]);
    let TopicNotes::Notes { notes, .. } = index.notes(0, None, Some(&wanted), false, 10).expect("filtered") else {
        panic!("expected notes");
    };
    let paths: Vec<&str> = notes.iter().map(|n| n.path.as_str()).collect();
    assert_eq!(paths, ["notes/2.md", "notes/0.md"]);

    let TopicNotes::Notes { notes, .. } = index.notes(0, None, None, false, 1).expect("limited") else {
        panic!("expected notes");
    };
    assert_eq!(notes.len(), 1);
}

#[test]
fn notes_names_both_numbers_on_a_generation_mismatch_and_rejects_an_unknown_topic() {
    let mut index = memory_index(&[vec![], vec![]]);
    train(&mut index, 2, &[(0, 0, 0.9), (1, 1, 0.9)]);
    let second = train(&mut index, 2, &[(0, 0, 0.9), (1, 1, 0.9)]);
    assert_eq!(second, 2);
    assert_eq!(
        index.notes(0, Some(1), None, false, 10).expect("mismatch"),
        TopicNotes::GenerationMismatch {
            requested: 1,
            stored: 2
        }
    );
    assert!(matches!(
        index.notes(0, Some(2), None, false, 10).expect("match"),
        TopicNotes::Notes { .. }
    ));
    assert_eq!(
        index.notes(9, None, None, false, 10).expect("unknown topic"),
        TopicNotes::UnknownTopic { topic: 9, k: 2 }
    );
}

/// A second connection commits a different generation between the status read
/// and the counts. The response must still describe generation 1: this is the
/// test that bites when the reader loses its read transaction.
#[test]
fn a_generation_committed_mid_read_does_not_leak_into_the_response() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("oracle.db");
    let tags: Vec<Vec<&str>> = vec![vec!["t"]; 6];
    let mut reader = index_with_notes(SearchIndex::open(&path).expect("open"), &tags);
    // Generation 1: k = 3, all 6 notes assigned.
    train(
        &mut reader,
        3,
        &[
            (0, 0, 0.9),
            (1, 0, 0.9),
            (2, 1, 0.9),
            (3, 1, 0.9),
            (4, 2, 0.9),
            (5, 2, 0.9),
        ],
    );
    let writer = RefCell::new(SearchIndex::open(&path).expect("second connection"));

    // Generation 2: k = 1, only 2 notes assigned.
    let overview = reader
        .overview_with(None, false, None, true, &|| {
            let generation = train(&mut writer.borrow_mut(), 1, &[(0, 0, 0.9), (1, 0, 0.9)]);
            assert_eq!(generation, 2, "the interleaved write really committed");
        })
        .expect("overview");
    assert_eq!(overview.snapshot.meta.as_ref().expect("meta").generation, 1);
    assert_eq!(overview.snapshot.assigned, 6, "assigned count is generation 1's");
    let shape: Vec<(u32, usize)> = overview.topics.iter().map(|t| (t.topic, t.notes)).collect();
    assert_eq!(shape, vec![(0, 2), (1, 2), (2, 2)], "topics are generation 1's");

    // The same reader afterwards sees generation 2.
    let after = reader.overview(None, false).expect("after");
    assert_eq!(after.snapshot.meta.expect("meta").generation, 2);
    assert_eq!(after.topics.len(), 1);
}

#[test]
fn a_generation_committed_mid_notes_read_does_not_leak_into_the_response() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("oracle.db");
    let mut reader = index_with_notes(SearchIndex::open(&path).expect("open"), &vec![vec![]; 4]);
    train(&mut reader, 2, &[(0, 1, 0.9), (1, 1, 0.8), (2, 1, 0.7), (3, 0, 0.9)]);
    let writer = RefCell::new(SearchIndex::open(&path).expect("second connection"));

    let read = reader
        .notes_with(1, Some(1), None, false, 10, &|| {
            train(&mut writer.borrow_mut(), 1, &[(0, 0, 0.9)]);
        })
        .expect("notes");
    let TopicNotes::Notes { snapshot, notes } = read else {
        panic!("generation 1 was requested and read: {read:?}");
    };
    assert_eq!(snapshot.meta.expect("meta").generation, 1);
    assert_eq!(notes.len(), 3, "topic 1 of generation 1, not generation 2's topic 1");
}
