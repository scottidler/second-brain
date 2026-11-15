//! `topic_map`, the `tag_brief` and `vault_overview` topic keys, against a
//! trained fixture index. A sibling of `tests.rs`, which is near the file
//! size limit.

use super::*;
use serde_json::{Value, json};
use vault::search::{Assignment, NewGeneration};

const MODEL: &str = "test-model";
const DIM: usize = 4;

fn json_of(result: &CallToolResult) -> Value {
    let text = result
        .content
        .first()
        .expect("a content item")
        .as_text()
        .expect("text content")
        .text
        .clone();
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .unwrap_or_default()
}

/// A pinned index with one note per entry of `tags` at `notes/{i}.md`.
fn index_with_notes(tags: &[Vec<&str>]) -> SearchIndex {
    let mut index = SearchIndex::open_memory().expect("open");
    index.set_active_embedding(MODEL, DIM).expect("pin");
    for (i, note_tags) in tags.iter().enumerate() {
        let path = format!("notes/{i}.md");
        index.insert_test_note_row(&path, "article", 1_000).expect("note");
        index.set_test_title(&path, &format!("title {i}")).expect("title");
        index.set_test_tags(&path, note_tags).expect("tags");
    }
    index
}

/// Train a `k`-topic generation; `rows` is `(note index, topic, cosine)`.
fn train(index: &mut SearchIndex, k: usize, rows: &[(usize, u32, f32)]) -> i64 {
    train_at(index, k, rows, 1_700_000_000)
}

fn train_at(index: &mut SearchIndex, k: usize, rows: &[(usize, u32, f32)], trained_at: i64) -> i64 {
    let assignments: Vec<Assignment> = rows
        .iter()
        .map(|(note, topic, cosine)| Assignment {
            note_path: format!("notes/{note}.md"),
            topic: *topic,
            cosine: *cosine,
            vector_hash: format!("planted-{note}"),
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
            trained_at,
            seed: 7,
            epochs: 5.0,
            e_means_version: "0.2.1",
            train_mean_cosine: 0.8,
            exclude_types: &[NoteType::Entity],
        })
        .expect("replace generation")
}

fn server(index: SearchIndex) -> OracleMcpServer {
    OracleMcpServer::new(Config::default(), index)
}

async fn call(server: &OracleMcpServer, tool: &str, args: Value) -> CallToolResult {
    server.dispatch(tool, args).await.expect("dispatch")
}

/// Topic 0 holds 40 notes (30 carry `rag`); topic 1 holds 360 untagged notes.
fn rag_fixture() -> OracleMcpServer {
    let mut tags: Vec<Vec<&str>> = Vec::new();
    tags.extend(vec![vec!["rag"]; 30]);
    tags.extend(vec![vec![]; 370]);
    let mut index = index_with_notes(&tags);
    let rows: Vec<(usize, u32, f32)> = (0..400)
        .map(|i| (i, if i < 40 { 0 } else { 1 }, 1.0 - i as f32 / 1000.0))
        .collect();
    train(&mut index, 2, &rows);
    server(index)
}

fn topic_labels(v: &Value, topic: u64) -> Vec<String> {
    let entry = v["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|t| t["topic"] == topic)
        .unwrap_or_else(|| panic!("topic {topic} in {v}"));
    entry["tags"]
        .as_array()
        .expect("tags")
        .iter()
        .map(|t| t["tag"].as_str().expect("tag").to_string())
        .collect()
}

#[tokio::test]
async fn topic_map_on_an_untrained_index_says_absent_and_is_not_an_error() {
    let server = server(index_with_notes(&[vec!["a"], vec!["a"]]));
    let result = call(&server, "topic_map", json!({})).await;
    assert_ne!(result.is_error, Some(true));
    let v = json_of(&result);
    assert_eq!(v["status"], "absent", "{v}");
    assert_eq!(v["count"], 0, "{v}");
    assert_eq!(v["results"], json!([]), "{v}");
    assert!(
        v["hint"].as_str().expect("hint").contains("sb cortex topics --train"),
        "{v}"
    );
}

#[tokio::test]
async fn topic_map_on_an_unpinned_empty_index_is_absent_too() {
    let server = server(SearchIndex::open_memory().expect("open"));
    let result = call(&server, "topic_map", json!({"topic": 0})).await;
    assert_ne!(result.is_error, Some(true));
    assert_eq!(json_of(&result)["status"], "absent");
}

#[tokio::test]
async fn topic_map_after_a_model_change_says_stale_with_the_hint() {
    let mut index = index_with_notes(&[vec!["a"], vec!["a"]]);
    train(&mut index, 1, &[(0, 0, 0.9), (1, 0, 0.8)]);
    index.set_active_embedding("another-model", DIM).expect("repin");
    let server = server(index);
    let v = json_of(&call(&server, "topic_map", json!({})).await);
    assert_eq!(v["status"], "stale", "{v}");
    assert_eq!(v["count"], 0);
    assert!(v["hint"].as_str().expect("hint").contains("sb cortex topics --train"));
    assert_eq!(v["generation"], 1, "the stale generation is still named: {v}");
}

#[tokio::test]
async fn topic_map_browse_returns_every_non_empty_topic_with_the_envelope() {
    // k = 4, topic 3 empty.
    let mut index = index_with_notes(&vec![vec![]; 5]);
    train(
        &mut index,
        4,
        &[(0, 0, 0.9), (1, 1, 0.9), (2, 1, 0.8), (3, 1, 0.7), (4, 2, 0.6)],
    );
    let server = server(index);
    let result = call(&server, "topic_map", json!({})).await;
    assert_ne!(result.is_error, Some(true));
    let v = json_of(&result);
    assert_eq!(v["status"], "valid");
    assert_eq!(v["generation"], 1);
    assert_eq!(v["k"], 4);
    assert_eq!(v["trained-at"], "2023-11-14T22:13:20Z");
    assert_eq!(v["count"], 3);
    let topics: Vec<u64> = v["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|t| t["topic"].as_u64().expect("topic"))
        .collect();
    assert_eq!(topics, vec![1, 0, 2], "biggest first, empty topic 3 absent");
    assert_eq!(v["results"][0]["notes"], 3);
    assert_eq!(v["results"][0]["exemplars"], json!(["title 1", "title 2", "title 3"]));
}

/// 6 notes in one topic: 2 carry only `a`, 2 only `b`, 2 both.
#[tokio::test]
async fn topic_map_tags_any_and_all_give_different_counts() {
    let mut index = index_with_notes(&[
        vec!["a"],
        vec!["a"],
        vec!["b"],
        vec!["b"],
        vec!["a", "b"],
        vec!["a", "b"],
    ]);
    train(&mut index, 1, &(0..6).map(|i| (i, 0, 0.9)).collect::<Vec<_>>());
    let server = server(index);
    let any = json_of(&call(&server, "topic_map", json!({"tags": ["a", "b"], "tags_mode": "any"})).await);
    let all = json_of(&call(&server, "topic_map", json!({"tags": ["a", "b"], "tags_mode": "all"})).await);
    let default = json_of(&call(&server, "topic_map", json!({"tags": ["a", "b"]})).await);
    assert_eq!(any["results"][0]["notes"], 6, "{any}");
    assert_eq!(all["results"][0]["notes"], 2, "{all}");
    assert_eq!(default["results"][0]["notes"], 6, "tags_mode defaults to any");
}

#[tokio::test]
async fn topic_map_label_is_identical_with_and_without_a_filter() {
    let server = rag_fixture();
    let plain = json_of(&call(&server, "topic_map", json!({})).await);
    let filtered = json_of(&call(&server, "topic_map", json!({"tags": ["rag"]})).await);
    assert_eq!(filtered["count"], 1, "topic 1 has no `rag` note: {filtered}");
    assert_eq!(filtered["results"][0]["notes"], 30);
    assert_eq!(topic_labels(&plain, 0), ["rag"]);
    assert_eq!(topic_labels(&plain, 0), topic_labels(&filtered, 0));
}

#[tokio::test]
async fn topic_map_labels_a_tag_on_30_of_40_topic_notes_and_30_of_400_assigned() {
    let server = rag_fixture();
    let v = json_of(&call(&server, "topic_map", json!({})).await);
    assert_eq!(v["results"][0]["topic"], 1, "the 360-note topic is listed first");
    let tags = &v["results"][1]["tags"];
    assert_eq!(v["results"][1]["topic"], 0);
    assert_eq!(tags[0]["tag"], "rag", "{v}");
    assert_eq!(tags[0]["notes"], 30);
    assert_eq!(tags[0]["lift"], 10.0);
    assert!(topic_labels(&v, 1).is_empty(), "the untagged topic has no label");
}

#[tokio::test]
async fn topic_map_generation_one_below_stored_is_an_error_naming_both() {
    let mut index = index_with_notes(&[vec![], vec![]]);
    train(&mut index, 2, &[(0, 0, 0.9), (1, 1, 0.9)]);
    let second = train(&mut index, 2, &[(0, 0, 0.9), (1, 1, 0.9)]);
    assert_eq!(second, 2);
    let server = server(index);

    let stale = call(&server, "topic_map", json!({"topic": 0, "generation": 1})).await;
    assert_eq!(stale.is_error, Some(true));
    let message = text_of(&stale);
    assert!(message.contains('1') && message.contains('2'), "{message}");
    assert!(
        message.contains("generation 1") && message.contains("generation is 2"),
        "{message}"
    );

    let fresh = call(&server, "topic_map", json!({"topic": 0, "generation": 2})).await;
    assert_ne!(fresh.is_error, Some(true));
    assert_eq!(json_of(&fresh)["generation"], 2);
}

#[tokio::test]
async fn topic_map_browse_and_tags_forms_check_generation_too() {
    let mut index = index_with_notes(&[vec!["a"], vec!["a"]]);
    train(&mut index, 2, &[(0, 0, 0.9), (1, 1, 0.9)]);
    train(&mut index, 2, &[(0, 0, 0.9), (1, 1, 0.9)]);
    let server = server(index);

    for args in [json!({"generation": 1}), json!({"tags": ["a"], "generation": 1})] {
        let stale = call(&server, "topic_map", args.clone()).await;
        assert_eq!(stale.is_error, Some(true), "{args}");
        let message = text_of(&stale);
        assert!(
            message.contains("generation 1") && message.contains("generation is 2"),
            "{message}"
        );
    }
    for args in [json!({"generation": 2}), json!({"tags": ["a"], "generation": 2})] {
        let fresh = call(&server, "topic_map", args.clone()).await;
        assert_ne!(fresh.is_error, Some(true), "{args}");
        assert_eq!(json_of(&fresh)["generation"], 2);
    }
}

#[tokio::test]
async fn trained_at_is_rfc3339_with_a_z_and_seconds_on_every_surface() {
    let mut index = index_with_notes(&[vec![], vec![]]);
    train(&mut index, 2, &[(0, 0, 0.9), (1, 1, 0.9)]);
    let server = server(index);
    let browse = json_of(&call(&server, "topic_map", json!({})).await);
    assert_eq!(browse["trained-at"], "2023-11-14T22:13:20Z", "{browse}");
    let overview = json_of(&call(&server, "vault_overview", json!({})).await);
    assert_eq!(overview["topics"]["trained-at"], "2023-11-14T22:13:20Z", "{overview}");
}

#[tokio::test]
async fn an_unrepresentable_trained_at_is_an_error_not_a_null() {
    let mut index = index_with_notes(&[vec![], vec![]]);
    train_at(&mut index, 2, &[(0, 0, 0.9), (1, 1, 0.9)], i64::MAX);
    let server = server(index);
    let browse = server.dispatch("topic_map", json!({})).await;
    assert!(
        browse.as_ref().map_or(true, |r| r.is_error == Some(true)),
        "browse must fail loudly: {browse:?}"
    );
    let overview = server.dispatch("vault_overview", json!({})).await;
    assert!(
        overview.as_ref().map_or(true, |r| r.is_error == Some(true)),
        "overview must fail loudly: {overview:?}"
    );
}

#[tokio::test]
async fn topic_map_topic_form_lists_notes_nearest_first_at_the_requested_detail() {
    let mut index = index_with_notes(&[vec!["a"], vec!["b"], vec!["a"], vec![]]);
    train(&mut index, 2, &[(0, 0, 0.5), (1, 0, 0.9), (2, 0, 0.7), (3, 1, 0.99)]);
    let server = server(index);

    let v = json_of(&call(&server, "topic_map", json!({"topic": 0})).await);
    assert_eq!(v["topic"], 0);
    assert_eq!(v["count"], 3);
    let paths: Vec<&str> = v["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|n| n["path"].as_str().expect("path"))
        .collect();
    assert_eq!(paths, ["notes/1.md", "notes/2.md", "notes/0.md"]);
    assert!(v["results"][0].get("trace").is_some(), "notes carry the trace block");
    assert!(v["results"][0].get("tldr").is_none(), "default detail is metadata");

    let tldr = json_of(&call(&server, "topic_map", json!({"topic": 0, "detail": "tldr", "limit": 1})).await);
    assert_eq!(tldr["count"], 1);
    assert!(tldr["results"][0].get("tldr").is_some());

    let tagged = json_of(&call(&server, "topic_map", json!({"topic": 0, "tags": ["a"]})).await);
    assert_eq!(tagged["count"], 2);
}

#[tokio::test]
async fn topic_map_unknown_topic_and_unknown_arguments_fail_loudly() {
    let mut index = index_with_notes(&[vec![], vec![]]);
    train(&mut index, 2, &[(0, 0, 0.9), (1, 1, 0.9)]);
    let server = server(index);
    let unknown = call(&server, "topic_map", json!({"topic": 7})).await;
    assert_eq!(unknown.is_error, Some(true));
    assert!(text_of(&unknown).contains("0 to 1"), "{}", text_of(&unknown));

    let err = server
        .dispatch("topic_map", json!({"cluster": 1}))
        .await
        .expect_err("unknown field must fail loudly");
    assert!(err.message.contains("unknown field"), "{}", err.message);
}

#[tokio::test]
async fn tag_brief_carries_the_top_topics_for_the_tag() {
    // 10 topics: topic t holds t+1 notes tagged `x`.
    let mut tags: Vec<Vec<&str>> = Vec::new();
    let mut rows: Vec<(usize, u32, f32)> = Vec::new();
    for topic in 0..10_u32 {
        for _ in 0..=topic {
            rows.push((tags.len(), topic, 0.9));
            tags.push(vec!["x"]);
        }
    }
    let mut index = index_with_notes(&tags);
    train(&mut index, 10, &rows);
    let server = server(index);

    let v = json_of(&call(&server, "tag_brief", json!({"tag": "x"})).await);
    assert_eq!(v["topics"]["generation"], 1, "{v}");
    let results = v["topics"]["results"].as_array().expect("topic results");
    assert_eq!(results.len(), 8, "top 8 only");
    assert_eq!(results[0]["topic"], 9);
    assert_eq!(results[0]["notes"], 10);
    assert!(results[0].get("tags").is_some());
    assert!(results[0].get("exemplars").is_none());
    assert!(v.get("topics-status").is_none());
}

#[tokio::test]
async fn tag_brief_topics_is_null_with_a_status_when_there_is_no_valid_generation() {
    let server = server(index_with_notes(&[vec!["x"]]));
    let v = json_of(&call(&server, "tag_brief", json!({"tag": "x"})).await);
    assert!(v["topics"].is_null(), "{v}");
    assert_eq!(v["topics-status"], "absent");
    assert_eq!(v["total_notes"], 1, "the rest of the brief is unchanged");
}

#[tokio::test]
async fn vault_overview_carries_a_topics_status_block() {
    let absent = server(index_with_notes(&[vec!["a"]]));
    let v = json_of(&call(&absent, "vault_overview", json!({})).await);
    assert_eq!(v["topics"]["status"], "absent", "{v}");
    assert!(v["topics"]["generation"].is_null());
    assert_eq!(v["topics"]["assigned"], 0);
    assert!(
        v.get("total_notes").is_some() || v.get("total").is_some(),
        "stats still serialized: {v}"
    );

    let mut index = index_with_notes(&[vec!["a"], vec!["a"], vec!["a"]]);
    train(&mut index, 2, &[(0, 0, 0.9), (1, 1, 0.9)]);
    let valid = server(index);
    let v = json_of(&call(&valid, "vault_overview", json!({})).await);
    assert_eq!(v["topics"]["status"], "valid");
    assert_eq!(v["topics"]["generation"], 1);
    assert_eq!(v["topics"]["k"], 2);
    assert_eq!(v["topics"]["assigned"], 2);
    assert_eq!(v["topics"]["trained-at"], "2023-11-14T22:13:20Z");
}
