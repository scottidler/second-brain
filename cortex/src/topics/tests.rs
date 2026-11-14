use super::*;
use crate::testutil::EnvGuard;
use vault::search::{BatchUpsert, TopicTables};

const MODEL: &str = "test-model";
const DIM: usize = 8;
const PER_AXIS: usize = 10;

/// Deterministic unit vector near axis `axis`: 1.0 on the axis plus xorshift
/// noise of at most 0.1 on every component, then normalized. Three axes give
/// three well-separated blobs (the `vault/tests/perf.rs` helper pattern).
fn blob_vector(axis: usize, seed: u64) -> Vec<f32> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v: Vec<f32> = (0..DIM)
        .map(|d| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let noise = (state % 2001) as f32 / 10_000.0 - 0.1;
            if d == axis { 1.0 + noise } else { noise }
        })
        .collect();
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter_mut().for_each(|x| *x /= norm);
    v
}

fn embed(index: &mut SearchIndex, path: &str, vector: &[f32]) {
    index
        .swap_embeddings_batch(&[BatchUpsert {
            note_path: path,
            kind: EmbeddingKind::Summary,
            chunk_index: 0,
            text: "summary",
            embedding: vector,
            model_version: MODEL,
            source_modified_at: 100,
        }])
        .expect("embed");
}

fn add_embedded(index: &mut SearchIndex, path: &str, note_type: &str, vector: &[f32]) {
    index.insert_test_note_row(path, note_type, 100).expect("note");
    embed(index, path, vector);
}

/// `PER_AXIS` article notes around each of axes 0, 1, 2 (`notes/a{axis}-{i}.md`)
/// plus one embedded `entity` note, which is out of scope by default.
fn fixture() -> SearchIndex {
    let mut index = SearchIndex::open_memory().expect("open");
    index.set_active_embedding(MODEL, DIM).expect("pin");
    for axis in 0..3 {
        for i in 0..PER_AXIS {
            let seed = (axis * 100 + i) as u64 + 1;
            add_embedded(
                &mut index,
                &format!("notes/a{axis}-{i}.md"),
                "article",
                &blob_vector(axis, seed),
            );
        }
    }
    add_embedded(&mut index, "entities/hub.md", "entity", &blob_vector(0, 999));
    index
}

fn topics_config(k: usize) -> TopicsConfig {
    TopicsConfig {
        k,
        ..TopicsConfig::default()
    }
}

fn tables(index: &SearchIndex) -> TopicTables {
    index.test_topic_tables().expect("dump")
}

fn topic_of(index: &SearchIndex, path: &str) -> Option<i64> {
    tables(index).2.into_iter().find(|row| row.0 == path).map(|row| row.1)
}

#[test]
fn train_writes_one_row_per_in_scope_note_k_centroids_and_none_for_an_entity() {
    let mut index = fixture();
    let report = train_index(&mut index, &topics_config(3), None, None, 5_000).expect("train");

    let (_state, centroids, notes) = tables(&index);
    assert_eq!(centroids.len(), 3, "k centroid rows");
    assert_eq!(notes.len(), 3 * PER_AXIS, "one row per in-scope note");
    assert!(
        notes.iter().all(|row| row.0 != "entities/hub.md"),
        "an entity note gets no topic"
    );
    assert!(
        notes.iter().all(|row| row.3 == 5_000),
        "train-time assigned_at is trained_at"
    );

    // The planted blobs come back as three topics, one per axis.
    let mut per_axis = Vec::new();
    for axis in 0..3 {
        let topics: BTreeSet<i64> = (0..PER_AXIS)
            .map(|i| topic_of(&index, &format!("notes/a{axis}-{i}.md")).expect("assigned"))
            .collect();
        assert_eq!(topics.len(), 1, "axis {axis} split across {topics:?}");
        per_axis.extend(topics);
    }
    per_axis.sort();
    per_axis.dedup();
    assert_eq!(per_axis, [0, 1, 2], "each axis owns its own topic");

    assert_eq!(report.status, TopicStatus::Valid);
    assert_eq!(report.generation, Some(1));
    assert_eq!((report.k, report.configured_k), (Some(3), 3));
    assert_eq!(report.eligible, 3 * PER_AXIS);
    assert_eq!(report.in_scope, 3 * PER_AXIS);
    assert_eq!(report.assigned, 3 * PER_AXIS);
    assert_eq!(report.assigned_this_run, 3 * PER_AXIS);
    assert_eq!((report.pending, report.removed_this_run), (0, 0));
    assert_eq!(report.trained_at.as_deref(), Some("1970-01-01T01:23:20Z"));
    let train_mean = report.train_mean_cosine.expect("train mean");
    assert!(train_mean > 0.9, "tight blobs sit near their centroid: {train_mean}");
    assert_eq!(report.hint, None);
    assert_eq!(report.exclude_types_drift, None);
}

#[test]
fn assign_gives_a_note_embedded_after_train_a_row_and_keeps_the_generation() {
    let mut index = fixture();
    let topics = topics_config(3);
    train_index(&mut index, &topics, None, None, 5_000).expect("train");
    let axis1_topic = topic_of(&index, "notes/a1-0.md").expect("assigned");

    add_embedded(&mut index, "notes/late.md", "article", &blob_vector(1, 4242));
    let first = assign_index(&mut index, &topics, 6_000).expect("assign");
    assert_eq!(first.generation, Some(1), "generation unchanged by an assign");
    assert_eq!(first.assigned_this_run, 1);
    assert_eq!(topic_of(&index, "notes/late.md"), Some(axis1_topic), "nearest centroid");
    let late = tables(&index).2.into_iter().find(|r| r.0 == "notes/late.md").unwrap();
    assert_eq!(late.3, 6_000, "assigned_at is the assign time");

    let second = assign_index(&mut index, &topics, 7_000).expect("assign again");
    assert_eq!(second.assigned_this_run, 0);
    assert_eq!(second.generation, Some(1));
}

#[test]
fn assign_reassigns_a_re_embedded_note_without_moving_centroids() {
    let mut index = fixture();
    let topics = topics_config(3);
    train_index(&mut index, &topics, None, None, 5_000).expect("train");
    let centroids_before = tables(&index).1;
    let axis2_topic = topic_of(&index, "notes/a2-0.md").expect("assigned");

    embed(&mut index, "notes/a0-0.md", &blob_vector(2, 77));
    let report = assign_index(&mut index, &topics, 6_000).expect("assign");
    assert_eq!(report.assigned_this_run, 1);
    assert_eq!(topic_of(&index, "notes/a0-0.md"), Some(axis2_topic));
    assert_eq!(tables(&index).1, centroids_before, "centroids are frozen");
}

#[test]
fn assign_removes_rows_for_notes_that_left_scope() {
    let mut index = fixture();
    let topics = topics_config(3);
    train_index(&mut index, &topics, None, None, 5_000).expect("train");

    index.set_test_quality("notes/a0-1.md", "low").expect("quality");
    let report = assign_index(&mut index, &topics, 6_000).expect("assign");
    assert_eq!(report.removed_this_run, 1);
    assert_eq!(topic_of(&index, "notes/a0-1.md"), None);
    assert_eq!(report.assigned, 3 * PER_AXIS - 1);
    assert_eq!(report.in_scope, 3 * PER_AXIS - 1);
}

#[test]
fn train_with_fewer_in_scope_notes_than_k_errors_and_leaves_the_tables_unchanged() {
    let mut index = fixture();
    train_index(&mut index, &topics_config(3), None, None, 5_000).expect("train");
    let before = tables(&index);

    let err = train_index(&mut index, &topics_config(3), Some(31), None, 6_000).expect_err("n < k");
    assert!(
        err.to_string().contains("30 in-scope note(s) cannot make k=31"),
        "{err}"
    );
    assert_eq!(tables(&index), before);
}

#[test]
fn train_with_one_pending_eligible_note_errors_and_leaves_the_tables_unchanged() {
    let mut index = fixture();
    train_index(&mut index, &topics_config(3), None, None, 5_000).expect("train");
    let before = tables(&index);

    // Summary text, no summary vector yet: the embed loop still owes it one.
    index
        .insert_test_note_row("notes/pending.md", "article", 100)
        .expect("note");
    let err = train_index(&mut index, &topics_config(3), None, Some(11), 6_000).expect_err("pending");
    let message = err.to_string();
    assert!(message.contains("1 eligible note(s) are pending"), "{message}");
    assert!(message.contains("notes/pending.md"), "{message}");
    assert!(message.contains("sb cortex embed --backfill"), "{message}");
    assert_eq!(tables(&index), before);

    let report = assign_index(&mut index, &topics_config(3), 7_000).expect("assign still runs");
    assert_eq!(report.pending, 1, "the report counts it");
}

#[test]
fn a_nothing_to_embed_note_does_not_block_train() {
    let mut index = fixture();
    index
        .insert_test_note_row("notes/empty.md", "article", 100)
        .expect("note");
    index
        .mark_nothing_to_embed_batch(EmbeddingKind::Summary, MODEL, &[("notes/empty.md".to_string(), 100)])
        .expect("sentinel");

    let report = train_index(&mut index, &topics_config(3), None, None, 5_000).expect("train");
    assert_eq!(report.pending, 0);
    assert_eq!(report.eligible, 3 * PER_AXIS + 1, "settled, eligible, out of scope");
    assert_eq!(report.in_scope, 3 * PER_AXIS);
    assert_eq!(topic_of(&index, "notes/empty.md"), None);
}

#[test]
fn an_unembedded_excluded_type_or_low_quality_note_is_not_pending() {
    let mut index = fixture();
    index
        .insert_test_note_row("entities/new.md", "entity", 100)
        .expect("note");
    index
        .insert_test_note_row("notes/stub.md", "article", 100)
        .expect("note");
    index.set_test_quality("notes/stub.md", "low").expect("quality");

    let report = train_index(&mut index, &topics_config(3), None, None, 5_000).expect("train");
    assert_eq!(report.pending, 0);
}

#[test]
fn clear_empties_the_tables_and_the_next_train_is_one_generation_higher() {
    let mut index = fixture();
    let topics = topics_config(3);
    train_index(&mut index, &topics, None, None, 5_000).expect("train 1");
    train_index(&mut index, &topics, None, None, 5_100).expect("train 2");

    let cleared = clear_index(&mut index, &topics).expect("clear");
    assert_eq!(cleared.status, TopicStatus::Absent);
    assert_eq!(cleared.removed_this_run, 3 * PER_AXIS);
    assert_eq!(cleared.generation, None);
    assert!(
        cleared
            .hint
            .as_deref()
            .unwrap_or("")
            .contains("sb cortex topics --train")
    );
    let (state, centroids, notes) = tables(&index);
    assert!(centroids.is_empty() && notes.is_empty());
    assert_eq!(state, [("generation".to_string(), "2".to_string())], "counter kept");

    let absent = assign_index(&mut index, &topics, 5_200).expect("assign on absent");
    assert_eq!((absent.status, absent.assigned_this_run), (TopicStatus::Absent, 0));
    assert!(tables(&index).2.is_empty(), "absent writes nothing");

    let retrained = train_index(&mut index, &topics, None, None, 5_300).expect("train 3");
    assert_eq!(retrained.generation, Some(3));
}

#[test]
fn assign_on_a_stale_generation_writes_nothing_and_reports_stale() {
    let mut index = fixture();
    let topics = topics_config(3);
    train_index(&mut index, &topics, None, None, 5_000).expect("train");
    index.set_active_embedding("other-model", DIM).expect("repin");
    let before = tables(&index);

    let report = assign_index(&mut index, &topics, 6_000).expect("assign");
    assert_eq!(report.status, TopicStatus::Stale);
    assert_eq!((report.assigned_this_run, report.removed_this_run), (0, 0));
    assert!(
        report
            .hint
            .as_deref()
            .unwrap_or("")
            .contains("sb cortex topics --train"),
        "{:?}",
        report.hint
    );
    assert_eq!(tables(&index), before);
}

#[test]
fn the_report_prints_the_stored_k_beside_an_edited_one_and_exclude_types_drift() {
    let mut index = fixture();
    train_index(&mut index, &topics_config(3), None, None, 5_000).expect("train");

    let mut edited = topics_config(5);
    edited.exclude_types = vec![NoteType::Daily];
    let report = assign_index(&mut index, &edited, 6_000).expect("assign");
    assert_eq!((report.k, report.configured_k), (Some(3), 5));
    let drift = report.exclude_types_drift.expect("drift");
    assert_eq!(drift.configured, ["daily"]);
    assert_eq!(drift.trained, ["entity", "daily", "digest", "review", "system", "moc"]);
    // Scope is evaluated per note against the configured list: the entity
    // note is in scope now and gets a topic.
    assert_eq!(report.assigned_this_run, 1);
    assert!(topic_of(&index, "entities/hub.md").is_some());
}

#[test]
fn a_non_unit_row_is_an_error_naming_the_note() {
    let mut index = fixture();
    let mut long = blob_vector(0, 5);
    long.iter_mut().for_each(|x| *x *= 2.0);
    add_embedded(&mut index, "notes/long.md", "article", &long);
    let err = train_index(&mut index, &topics_config(3), None, None, 5_000).expect_err("non-unit");
    assert!(err.to_string().contains("notes/long.md"), "{err}");
    assert!(tables(&index).1.is_empty(), "nothing written");

    index.set_test_quality("notes/long.md", "low").expect("quality");
    train_index(&mut index, &topics_config(3), None, None, 5_000).expect("train");
    index.set_test_quality("notes/long.md", "").expect("quality");
    let before = tables(&index);
    let err = assign_index(&mut index, &topics_config(3), 6_000).expect_err("non-unit on assign");
    assert!(err.to_string().contains("notes/long.md"), "{err}");
    assert_eq!(tables(&index).2, before.2, "no assignment written");
}

#[test]
fn zero_k_or_zero_workers_is_an_error_before_any_write() {
    let mut index = fixture();
    assert!(train_index(&mut index, &topics_config(3), Some(0), None, 5_000).is_err());
    let mut no_workers = topics_config(3);
    no_workers.workers = 0;
    assert!(train_index(&mut index, &no_workers, None, None, 5_000).is_err());
    assert!(tables(&index).1.is_empty());
}

#[test]
fn per_run_k_and_seed_override_config_and_are_recorded() {
    let mut index = fixture();
    let report = train_index(&mut index, &topics_config(3), Some(4), Some(11), 5_000).expect("train");
    assert_eq!((report.k, report.configured_k), (Some(4), 3));
    let state = tables(&index).0;
    assert!(state.contains(&("seed".to_string(), "11".to_string())), "{state:?}");
    assert!(state.contains(&("e_means_version".to_string(), kmeans::E_MEANS_VERSION.to_string())));
}

#[test]
fn training_runs_on_a_pool_of_the_configured_size() {
    let (threads, name) = in_train_pool(3, || {
        (
            rayon::current_num_threads(),
            std::thread::current().name().map(str::to_string),
        )
    })
    .expect("pool");
    assert_eq!(threads, 3);
    assert!(name.unwrap_or_default().starts_with("topics-train-"));
}

#[test]
fn every_verb_takes_the_embed_lock() {
    let _env = crate::testutil::lock_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_DATA_HOME", tmp.path());
    let held = crate::embed::acquire_lock().expect("hold the lock");

    let config = Config::default();
    for opts in [
        TopicsOpts {
            train: true,
            ..TopicsOpts::default()
        },
        TopicsOpts::default(),
        TopicsOpts {
            clear: true,
            ..TopicsOpts::default()
        },
    ] {
        let err = run(&config, &opts).expect_err("lock held");
        assert!(
            err.downcast_ref::<crate::embed::EmbedLockHeld>().is_some(),
            "{opts:?}: {err}"
        );
    }
    drop(held);
}

#[test]
fn run_rejects_train_with_clear_and_overrides_without_train() {
    let config = Config::default();
    let both = TopicsOpts {
        train: true,
        clear: true,
        ..TopicsOpts::default()
    };
    assert!(run(&config, &both).unwrap_err().to_string().contains("conflict"));
    let k_alone = TopicsOpts {
        k: Some(8),
        ..TopicsOpts::default()
    };
    assert!(run(&config, &k_alone).unwrap_err().to_string().contains("require"));
}

#[test]
fn the_report_serializes_in_kebab_case_with_a_lowercase_status() {
    let mut index = fixture();
    let report = assign_index(&mut index, &topics_config(3), 5_000).expect("absent");
    let json = serde_json::to_value(&report).expect("json");
    assert_eq!(json["status"], "absent");
    for key in [
        "generation",
        "k",
        "configured-k",
        "trained-at",
        "eligible",
        "pending",
        "in-scope",
        "assigned",
        "assigned-this-run",
        "removed-this-run",
        "mean-cosine",
        "train-mean-cosine",
        "hint",
    ] {
        assert!(json.get(key).is_some(), "missing {key}: {json}");
    }
    assert!(json.get("exclude-types-drift").is_none());
}
