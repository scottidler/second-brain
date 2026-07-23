#![allow(clippy::unwrap_used)]
use super::*;
use crate::harvest::contract::{EnrichStatus, SessionRecord};

fn rec(id: &str, cwd: &str, branch: Option<&str>, created: &str, modified: &str, n_msgs: i64) -> SessionRecord {
    SessionRecord {
        session_id: id.to_string(),
        host: "desk".to_string(),
        scope: "work".to_string(),
        cwd: Some(cwd.to_string()),
        project_dir: None,
        repo: Some("org/repo".to_string()),
        git_branch: branch.map(str::to_string),
        created: Some(created.to_string()),
        modified: modified.to_string(),
        updated_at: None,
        duration_secs: None,
        dormant: true,
        title: Some("t".to_string()),
        first_prompt: Some("p".to_string()),
        n_msgs,
        model: None,
        summary: None,
        tags: vec![],
        enrich_status: Some(EnrichStatus::Ok),
        redaction_count: 0,
        transcript_path: None,
        staged_path: None,
        archived: false,
        repos_touched: None,
        files_touched: None,
        body: None,
        body_truncated: false,
        body_error: None,
    }
}

fn window_2h() -> Duration {
    Duration::hours(2)
}

#[test]
fn single_session_is_a_thread_of_one() {
    let records = vec![rec(
        "a",
        "/c",
        Some("main"),
        "2026-07-01T00:00:00+00:00",
        "2026-07-01T00:30:00+00:00",
        40,
    )];
    let threads = cluster_threads(&records, window_2h()).unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].members.len(), 1);
    assert_eq!(threads[0].primary().session_id, "a");
}

#[test]
fn same_key_within_window_merges() {
    // 15s apart, same (cwd, branch) -> one thread. Mirrors the golden arc.
    let records = vec![
        rec(
            "a",
            "/c",
            Some("main"),
            "2026-07-02T04:51:21+00:00",
            "2026-07-02T06:08:39+00:00",
            486,
        ),
        rec(
            "b",
            "/c",
            Some("main"),
            "2026-07-02T06:08:54+00:00",
            "2026-07-03T03:10:20+00:00",
            320,
        ),
    ];
    let threads = cluster_threads(&records, window_2h()).unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].member_ids(), vec!["a", "b"]);
    // primary = most messages
    assert_eq!(threads[0].primary().session_id, "a");
    assert_eq!(threads[0].total_msgs(), 806);
}

#[test]
fn same_key_beyond_window_does_not_merge() {
    // ~4h apart -> two threads (the same-cwd-unrelated case).
    let records = vec![
        rec(
            "a",
            "/c",
            Some("main"),
            "2026-07-05T09:00:00+00:00",
            "2026-07-05T09:45:00+00:00",
            40,
        ),
        rec(
            "b",
            "/c",
            Some("main"),
            "2026-07-05T14:00:00+00:00",
            "2026-07-05T15:00:00+00:00",
            55,
        ),
    ];
    let threads = cluster_threads(&records, window_2h()).unwrap();
    assert_eq!(threads.len(), 2);
}

#[test]
fn different_branch_does_not_merge() {
    let records = vec![
        rec(
            "a",
            "/c",
            Some("main"),
            "2026-07-05T09:00:00+00:00",
            "2026-07-05T09:10:00+00:00",
            40,
        ),
        rec(
            "b",
            "/c",
            Some("feature"),
            "2026-07-05T09:11:00+00:00",
            "2026-07-05T09:20:00+00:00",
            55,
        ),
    ];
    let threads = cluster_threads(&records, window_2h()).unwrap();
    assert_eq!(threads.len(), 2);
}

#[test]
fn different_cwd_does_not_merge() {
    let records = vec![
        rec(
            "a",
            "/c1",
            Some("main"),
            "2026-07-05T09:00:00+00:00",
            "2026-07-05T09:10:00+00:00",
            40,
        ),
        rec(
            "b",
            "/c2",
            Some("main"),
            "2026-07-05T09:11:00+00:00",
            "2026-07-05T09:20:00+00:00",
            55,
        ),
    ];
    let threads = cluster_threads(&records, window_2h()).unwrap();
    assert_eq!(threads.len(), 2);
}

#[test]
fn branchless_sessions_cluster_on_cwd_alone() {
    let records = vec![
        rec(
            "a",
            "/c",
            None,
            "2026-07-05T09:00:00+00:00",
            "2026-07-05T09:10:00+00:00",
            40,
        ),
        rec(
            "b",
            "/c",
            None,
            "2026-07-05T09:11:00+00:00",
            "2026-07-05T09:20:00+00:00",
            55,
        ),
    ];
    let threads = cluster_threads(&records, window_2h()).unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].members.len(), 2);
}

#[test]
fn output_is_deterministic() {
    let records = vec![
        rec(
            "z",
            "/c2",
            Some("main"),
            "2026-07-05T12:00:00+00:00",
            "2026-07-05T12:10:00+00:00",
            40,
        ),
        rec(
            "a",
            "/c1",
            Some("main"),
            "2026-07-05T09:00:00+00:00",
            "2026-07-05T09:10:00+00:00",
            40,
        ),
    ];
    let first = cluster_threads(&records, window_2h()).unwrap();
    let second = cluster_threads(&records, window_2h()).unwrap();
    assert_eq!(first, second);
    // ordered by primary created time
    assert_eq!(first[0].primary().session_id, "a");
    assert_eq!(first[1].primary().session_id, "z");
}

#[test]
fn unparseable_timestamp_is_a_loud_error() {
    let records = vec![rec(
        "a",
        "/c",
        Some("main"),
        "not-a-timestamp",
        "2026-07-05T09:10:00+00:00",
        40,
    )];
    let err = cluster_threads(&records, window_2h()).unwrap_err();
    assert!(format!("{err:#}").contains("unparseable"), "{err:#}");
}

// ---- harvest-completion Phase 6: created-guard bite, second layer. A null
// `created` is normally rejected at the SELECTION stage (`select.rs`) so it
// never reaches `cluster_threads`. This test proves the fail-loud backstop
// still exists here too: if a null-`created` record ever bypassed selection
// (a caller wiring bug, or the selection guard itself being reverted), the
// WHOLE batch errors loudly rather than clustering on a fabricated timestamp
// or silently dropping the record. Removing `select.rs`'s created guard would
// let exactly this shape reach `cluster_threads` in production.
#[test]
fn null_created_bypassing_selection_is_a_loud_backstop_error() {
    let mut r = rec(
        "a",
        "/c",
        Some("main"),
        "2026-07-05T09:00:00+00:00",
        "2026-07-05T09:10:00+00:00",
        40,
    );
    r.created = None;
    let err = cluster_threads(&[r], window_2h()).unwrap_err();
    assert!(format!("{err:#}").contains("null created timestamp"), "{err:#}");
}
