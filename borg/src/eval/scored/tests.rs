use super::*;
use crate::eval::scores::ScoreRow;
use vault::distilled::{Claim, Distilled};

fn stage(root: &Path, trace: &str, claim: &str) {
    let dir = root.join(trace);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("envelope.yml"), "kind: article-url\n").unwrap();
    let distilled = Distilled {
        summary: format!("summary of {trace}"),
        claims: vec![Claim {
            text: claim.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::write(dir.join("distilled.yml"), serde_yaml::to_string(&distilled).unwrap()).unwrap();
    std::fs::write(dir.join("transcript.md"), format!("source for {trace}: {claim}")).unwrap();
}

/// Record a baseline row for the staged `trace` at its current hash.
fn score(store: &ScoreStore, root: &Path, trace: &str, scored_at: &str) {
    let fixture = load_staged_trace(root, trace).unwrap().unwrap();
    store
        .put(&ScoreRow {
            trace: trace.to_string(),
            run_kind: RUN_KIND_BASELINE.to_string(),
            run_id: String::new(),
            kind: fixture.kind.clone(),
            judge_model: String::new(),
            check_name: "note-size".to_string(),
            rubric_version: "v1".to_string(),
            content_hash: judged_input(&fixture).content_hash,
            scored_at: scored_at.to_string(),
            metrics_json: "{}".to_string(),
            error: None,
        })
        .unwrap();
}

fn traces(selection: &ScoredSelection) -> Vec<&str> {
    selection.fixtures.iter().map(|f| f.slug.as_str()).collect()
}

#[test]
fn the_most_recently_scored_traces_come_first_up_to_the_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ScoreStore::open_memory().unwrap();
    for (trace, at) in [
        ("t-old", "2026-10-15T00:00:00Z"),
        ("t-new", "2026-10-17T00:00:00Z"),
        ("t-mid", "2026-10-16T00:00:00Z"),
    ] {
        stage(tmp.path(), trace, trace);
        score(&store, tmp.path(), trace, at);
    }
    let selection = select_scored(&store, tmp.path(), 2).unwrap();
    assert_eq!(traces(&selection), vec!["t-new", "t-mid"]);
}

#[test]
fn a_swept_trace_is_counted_and_a_re_distilled_one_is_named_and_neither_is_selected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ScoreStore::open_memory().unwrap();
    for (trace, at) in [
        ("t-swept", "2026-10-17T03:00:00Z"),
        ("t-moved", "2026-10-17T02:00:00Z"),
        ("t-ok", "2026-10-17T01:00:00Z"),
    ] {
        stage(tmp.path(), trace, trace);
        score(&store, tmp.path(), trace, at);
    }
    std::fs::remove_dir_all(tmp.path().join("t-swept")).unwrap();
    stage(tmp.path(), "t-moved", "re-distilled into something else");

    let selection = select_scored(&store, tmp.path(), 10).unwrap();
    assert_eq!(traces(&selection), vec!["t-ok"]);
    assert_eq!(selection.not_staged, 1);
    assert_eq!(selection.moved_on, vec!["t-moved".to_string()]);
}

#[test]
fn a_trace_with_only_an_error_row_is_not_scored() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ScoreStore::open_memory().unwrap();
    stage(tmp.path(), "t1", "claim");
    score(&store, tmp.path(), "t1", "2026-10-17T00:00:00Z");
    let mut row = store.rows_for("t1").unwrap().remove(0);
    row.error = Some("boom".to_string());
    let errored = ScoreStore::open_memory().unwrap();
    errored.put(&row).unwrap();
    assert!(select_scored(&errored, tmp.path(), 10).unwrap().fixtures.is_empty());
}
