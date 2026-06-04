use super::*;
use vault::receipts::{FailureStage, ReceiptKind, ReceiptStatus};
use vault::schema::Method;

fn fresh() -> Connection {
    open_memory().expect("open_memory")
}

#[test]
fn open_memory_applies_four_pragmas() {
    let conn = fresh();
    let p = active_pragmas(&conn).expect("active_pragmas");
    assert_eq!(
        p.journal_mode.to_lowercase(),
        "memory",
        "in-memory DB reports 'memory' journal mode"
    );
    assert_eq!(p.synchronous, 1, "synchronous=NORMAL is value 1");
    assert_eq!(p.busy_timeout, 5000);
    assert!(p.foreign_keys);
}

#[test]
fn open_at_file_applies_wal_mode() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("receipts.db");
    let conn = open_at(&path).expect("open_at");
    let p = active_pragmas(&conn).expect("active_pragmas");
    assert_eq!(p.journal_mode.to_lowercase(), "wal", "on-disk DB uses WAL");
    assert_eq!(p.busy_timeout, 5000);
    assert!(p.foreign_keys);
}

#[test]
fn record_received_inserts_row_with_received_status() {
    let conn = fresh();
    record_received(&conn, "tr-001", Method::Telegram, ReceiptKind::Url, "https://x.com/a").expect("record_received");
    let r = get(&conn, "tr-001").expect("get").expect("row present");
    assert_eq!(r.status, "received");
    assert_eq!(r.method, "telegram");
    assert_eq!(r.kind, "url");
    assert_eq!(r.raw_input, "https://x.com/a");
    assert!(r.terminal_at.is_none());
    assert!(r.note_path.is_none());
    assert!(r.failure_stage.is_none());
    assert!(r.replay_of.is_none());
}

#[test]
fn record_received_is_idempotent_on_trace_id() {
    let conn = fresh();
    record_received(&conn, "tr-002", Method::Http, ReceiptKind::Text, "first").expect("first insert");
    record_received(&conn, "tr-002", Method::Http, ReceiptKind::Text, "second-ignored")
        .expect("idempotent re-insert is OK");
    let r = get(&conn, "tr-002").expect("get").expect("row");
    assert_eq!(r.raw_input, "first", "INSERT OR IGNORE preserves the first row");
}

#[test]
fn mark_succeeded_promotes_received_to_succeeded() {
    let conn = fresh();
    record_received(&conn, "tr-s1", Method::Cli, ReceiptKind::Url, "u").expect("ins");
    let updated = mark_succeeded(&conn, "tr-s1", "inbox/foo.md").expect("mark");
    assert!(updated);
    let r = get(&conn, "tr-s1").expect("get").expect("row");
    assert_eq!(r.status, "succeeded");
    assert_eq!(r.note_path.as_deref(), Some("inbox/foo.md"));
    assert!(r.terminal_at.is_some());
}

#[test]
fn mark_succeeded_is_noop_on_already_terminal_row() {
    let conn = fresh();
    record_received(&conn, "tr-s2", Method::Cli, ReceiptKind::Url, "u").expect("ins");
    mark_failed(&conn, "tr-s2", FailureStage::FetchFailed, "yt-dlp").expect("first transition wins");
    let updated = mark_succeeded(&conn, "tr-s2", "inbox/foo.md").expect("mark");
    assert!(!updated, "succeeded should be a no-op once row is failed");
    let r = get(&conn, "tr-s2").expect("get").expect("row");
    assert_eq!(r.status, "failed", "failed status is absorbing");
    assert_eq!(r.failure_stage.as_deref(), Some("fetch-failed"));
}

#[test]
fn mark_failed_promotes_received_to_failed_with_stage() {
    let conn = fresh();
    record_received(&conn, "tr-f1", Method::Telegram, ReceiptKind::Url, "u").expect("ins");
    let updated = mark_failed(&conn, "tr-f1", FailureStage::QualityBlocked, "blocked title").expect("mark");
    assert!(updated);
    let r = get(&conn, "tr-f1").expect("get").expect("row");
    assert_eq!(r.status, "failed");
    assert_eq!(r.failure_stage.as_deref(), Some("quality-blocked"));
    assert_eq!(r.failure_reason.as_deref(), Some("blocked title"));
}

#[test]
fn mark_failed_twice_keeps_first_stage() {
    let conn = fresh();
    record_received(&conn, "tr-f2", Method::Http, ReceiptKind::Url, "u").expect("ins");
    mark_failed(&conn, "tr-f2", FailureStage::FetchFailed, "first").expect("first");
    let second = mark_failed(&conn, "tr-f2", FailureStage::PublishFailed, "second").expect("second");
    assert!(!second, "second mark_failed should be a no-op");
    let r = get(&conn, "tr-f2").expect("get").expect("row");
    assert_eq!(r.failure_stage.as_deref(), Some("fetch-failed"));
    assert_eq!(r.failure_reason.as_deref(), Some("first"));
}

#[test]
fn schema_rejects_invalid_status() {
    let conn = fresh();
    let err = conn
        .execute(
            "INSERT INTO receipts (trace_id, received_at, method, kind, raw_input, status) \
             VALUES ('bad', '2026-05-20T00:00:00Z', 'http', 'url', 'u', 'banana')",
            [],
        )
        .expect_err("CHECK constraint should reject");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("check") || msg.to_lowercase().contains("constraint"),
        "got {msg}"
    );
}

#[test]
fn schema_rejects_invalid_failure_stage() {
    let conn = fresh();
    record_received(&conn, "tr-bad-stage", Method::Cli, ReceiptKind::Url, "u").expect("ins");
    let err = conn
        .execute(
            "UPDATE receipts SET status='failed', failure_stage='not-a-real-stage' \
             WHERE trace_id='tr-bad-stage'",
            [],
        )
        .expect_err("CHECK constraint should reject");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("check") || msg.to_lowercase().contains("constraint"),
        "got {msg}"
    );
}

#[test]
fn record_replay_carries_replay_of() {
    let conn = fresh();
    record_received(&conn, "orig", Method::Cli, ReceiptKind::Url, "u").expect("orig");
    mark_failed(&conn, "orig", FailureStage::FetchFailed, "boom").expect("fail");
    record_replay(&conn, "replay-1", Method::Cli, ReceiptKind::Url, "u", "orig").expect("replay");
    let r = get(&conn, "replay-1").expect("get").expect("row");
    assert_eq!(r.status, "received");
    assert_eq!(r.replay_of.as_deref(), Some("orig"));
}

#[test]
fn promote_stale_to_crashed_moves_only_past_deadline_rows() {
    let conn = fresh();
    // Fresh row, recent: should NOT be promoted with deadline=10s.
    record_received(&conn, "fresh", Method::Http, ReceiptKind::Url, "u").expect("fresh");
    // Synthesize an old row by patching received_at to far in the past.
    record_received(&conn, "old", Method::Http, ReceiptKind::Url, "u").expect("old");
    conn.execute(
        "UPDATE receipts SET received_at='2024-01-01T00:00:00Z' WHERE trace_id='old'",
        [],
    )
    .expect("backdate");
    let promoted = promote_stale_to_crashed(&conn, 60).expect("promote");
    assert_eq!(promoted, 1, "only the old row should be promoted");
    let fresh_row = get(&conn, "fresh").expect("get").expect("row");
    assert_eq!(fresh_row.status, "received");
    let old_row = get(&conn, "old").expect("get").expect("row");
    assert_eq!(old_row.status, "failed");
    assert_eq!(old_row.failure_stage.as_deref(), Some("crashed"));
}

#[test]
fn promote_single_to_crashed_status_guard() {
    let conn = fresh();
    record_received(&conn, "tr-race", Method::Http, ReceiptKind::Url, "u").expect("ins");
    // Simulate the pipeline winning the race first.
    mark_succeeded(&conn, "tr-race", "inbox/race.md").expect("succ");
    // Watchdog tries to crash it now -> no-op.
    let crashed = promote_single_to_crashed(&conn, "tr-race", 60).expect("promote");
    assert!(!crashed);
    let r = get(&conn, "tr-race").expect("get").expect("row");
    assert_eq!(r.status, "succeeded");
}

#[test]
fn list_stale_returns_only_past_deadline_received() {
    let conn = fresh();
    record_received(&conn, "fresh", Method::Cli, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "old1", Method::Cli, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "old2", Method::Cli, ReceiptKind::Url, "u").expect("ins");
    conn.execute(
        "UPDATE receipts SET received_at='2024-01-01T00:00:00Z' WHERE trace_id IN ('old1','old2')",
        [],
    )
    .expect("backdate");
    // old2 is already terminal -> should not appear.
    mark_failed(&conn, "old2", FailureStage::FetchFailed, "x").expect("fail");
    let stale = list_stale(&conn, 60).expect("list_stale");
    let ids: Vec<&str> = stale.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["old1"]);
}

#[test]
fn query_filters_by_status() {
    let conn = fresh();
    record_received(&conn, "a", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "b", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "c", Method::Http, ReceiptKind::Url, "u").expect("ins");
    mark_succeeded(&conn, "a", "n1.md").expect("succ a");
    mark_failed(&conn, "b", FailureStage::FetchFailed, "boom").expect("fail b");
    let succ = query(
        &conn,
        &Filter {
            status: Some(ReceiptStatus::Succeeded),
            ..Default::default()
        },
    )
    .expect("query");
    assert_eq!(succ.len(), 1);
    assert_eq!(succ[0].trace_id, "a");
    let failed = query(
        &conn,
        &Filter {
            status: Some(ReceiptStatus::Failed),
            ..Default::default()
        },
    )
    .expect("query");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].trace_id, "b");
    let received = query(
        &conn,
        &Filter {
            status: Some(ReceiptStatus::Received),
            ..Default::default()
        },
    )
    .expect("query");
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].trace_id, "c");
}

#[test]
fn query_filters_by_method_and_stage() {
    let conn = fresh();
    record_received(&conn, "tg1", Method::Telegram, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "http1", Method::Http, ReceiptKind::Url, "u").expect("ins");
    mark_failed(&conn, "tg1", FailureStage::FetchFailed, "x").expect("fail");
    mark_failed(&conn, "http1", FailureStage::QualityBlocked, "y").expect("fail");
    let by_method = query(
        &conn,
        &Filter {
            method: Some(Method::Telegram),
            ..Default::default()
        },
    )
    .expect("q");
    assert_eq!(by_method.len(), 1);
    assert_eq!(by_method[0].trace_id, "tg1");
    let by_stage = query(
        &conn,
        &Filter {
            stage: Some(FailureStage::QualityBlocked),
            ..Default::default()
        },
    )
    .expect("q");
    assert_eq!(by_stage.len(), 1);
    assert_eq!(by_stage[0].trace_id, "http1");
}

#[test]
fn query_filters_by_signal_method() {
    // Phase 4: confirm Method::Signal round-trips through the receipts schema
    // and the --method query filter. Without this, `sb borg log --method signal`
    // would silently return zero rows even with Signal traffic in the DB.
    let conn = fresh();
    record_received(&conn, "sg1", Method::Signal, ReceiptKind::Url, "https://example.com").expect("ins signal");
    record_received(&conn, "tg2", Method::Telegram, ReceiptKind::Url, "https://example.org").expect("ins tg");
    let by_method = query(
        &conn,
        &Filter {
            method: Some(Method::Signal),
            ..Default::default()
        },
    )
    .expect("q");
    assert_eq!(by_method.len(), 1, "Signal filter must return exactly the Signal row");
    assert_eq!(by_method[0].trace_id, "sg1");
    assert_eq!(by_method[0].method, "signal");
}

#[test]
fn query_filters_by_source_like() {
    let conn = fresh();
    record_received(
        &conn,
        "x1",
        Method::Http,
        ReceiptKind::Url,
        "https://youtube.com/watch?v=1",
    )
    .expect("ins");
    record_received(&conn, "x2", Method::Http, ReceiptKind::Url, "https://example.com/a").expect("ins");
    let yt = query(
        &conn,
        &Filter {
            source_like: Some("%youtube.com%".to_string()),
            ..Default::default()
        },
    )
    .expect("q");
    assert_eq!(yt.len(), 1);
    assert_eq!(yt[0].trace_id, "x1");
}

#[test]
fn query_limit_caps_results() {
    let conn = fresh();
    for i in 0..5 {
        record_received(&conn, &format!("t{i}"), Method::Http, ReceiptKind::Url, "u").expect("ins");
    }
    let two = query(
        &conn,
        &Filter {
            limit: Some(2),
            ..Default::default()
        },
    )
    .expect("q");
    assert_eq!(two.len(), 2);
}

#[test]
fn query_filters_by_since_with_parsed_relative_duration() {
    // The load-bearing property: parse_since output must be in the exact same
    // fixed-width UTC format as stored received_at, so the SQL `>= ?`
    // lexicographic comparison equals chronological comparison. This test
    // fails if that format ever diverges (the original bug bound a raw "5m").
    let conn = fresh();
    record_received(&conn, "inside", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "outside", Method::Http, ReceiptKind::Url, "u").expect("ins");
    conn.execute(
        "UPDATE receipts SET received_at='2026-06-04T11:58:00Z' WHERE trace_id='inside'",
        [],
    )
    .expect("backdate inside");
    conn.execute(
        "UPDATE receipts SET received_at='2026-06-04T11:00:00Z' WHERE trace_id='outside'",
        [],
    )
    .expect("backdate outside");
    let now = Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0).unwrap();
    let since = parse_since("5m", now).expect("parse 5m");
    let rows = query(
        &conn,
        &Filter {
            since: Some(since),
            ..Default::default()
        },
    )
    .expect("query");
    let ids: Vec<&str> = rows.iter().map(|r| r.trace_id.as_str()).collect();
    assert_eq!(ids, vec!["inside"], "only the row inside the 5m window is returned");
}

#[test]
fn parse_since_relative_duration_subtracts_from_now() {
    let now = Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0).unwrap();
    assert_eq!(parse_since("5m", now).expect("5m"), "2026-06-04T11:55:00Z");
    assert_eq!(parse_since("2h", now).expect("2h"), "2026-06-04T10:00:00Z");
    assert_eq!(parse_since("7d", now).expect("7d"), "2026-05-28T12:00:00Z");
}

#[test]
fn parse_since_absolute_iso_is_normalized_to_utc() {
    let now = Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0).unwrap();
    // Already-UTC datetime passes through unchanged.
    assert_eq!(
        parse_since("2026-06-01T08:30:00Z", now).expect("iso"),
        "2026-06-01T08:30:00Z"
    );
    // An offset datetime is converted to UTC.
    assert_eq!(
        parse_since("2026-06-01T08:30:00+02:00", now).expect("iso offset"),
        "2026-06-01T06:30:00Z"
    );
}

#[test]
fn parse_since_bare_date_is_midnight_utc() {
    let now = Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0).unwrap();
    assert_eq!(parse_since("2026-06-02", now).expect("date"), "2026-06-02T00:00:00Z");
}

#[test]
fn parse_since_rejects_garbage_loudly() {
    let now = Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0).unwrap();
    let err = parse_since("not-a-time", now).expect_err("garbage must error");
    let msg = err.to_string();
    assert!(msg.contains("could not parse --since"), "got: {msg}");
}

#[test]
fn count_by_status_groups_correctly() {
    let conn = fresh();
    record_received(&conn, "r1", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "r2", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "s", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "f", Method::Http, ReceiptKind::Url, "u").expect("ins");
    mark_succeeded(&conn, "s", "n.md").expect("s");
    mark_failed(&conn, "f", FailureStage::FetchFailed, "x").expect("f");
    let (recv, succ, fail) = count_by_status(&conn).expect("count");
    assert_eq!((recv, succ, fail), (2, 1, 1));
}

#[test]
fn count_failed_by_stage_returns_each_stage() {
    let conn = fresh();
    for (id, stage) in [
        ("a", FailureStage::FetchFailed),
        ("b", FailureStage::FetchFailed),
        ("c", FailureStage::QualityBlocked),
        ("d", FailureStage::Crashed),
    ] {
        record_received(&conn, id, Method::Http, ReceiptKind::Url, "u").expect("ins");
        mark_failed(&conn, id, stage, "x").expect("fail");
    }
    let by_stage = count_failed_by_stage(&conn).expect("count");
    let mut counts = std::collections::HashMap::new();
    for (s, c) in by_stage {
        counts.insert(s, c);
    }
    assert_eq!(counts.get(&FailureStage::FetchFailed), Some(&2));
    assert_eq!(counts.get(&FailureStage::QualityBlocked), Some(&1));
    assert_eq!(counts.get(&FailureStage::Crashed), Some(&1));
}

#[test]
fn pool_connections_have_pragmas_applied() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("receipts.db");
    let pool = build_pool_at(&path).expect("pool");
    let conn = pool.get().expect("checkout");
    let p = active_pragmas(&conn).expect("pragmas");
    assert_eq!(p.journal_mode.to_lowercase(), "wal");
    assert_eq!(p.busy_timeout, 5000);
    assert!(p.foreign_keys);
}

#[test]
fn row_count_returns_total() {
    let conn = fresh();
    assert_eq!(row_count(&conn).expect("rc"), 0);
    record_received(&conn, "a", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "b", Method::Http, ReceiptKind::Url, "u").expect("ins");
    assert_eq!(row_count(&conn).expect("rc"), 2);
}

#[test]
fn open_at_is_idempotent_across_two_opens() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("receipts.db");
    {
        let conn = open_at(&path).expect("first");
        record_received(&conn, "persist", Method::Cli, ReceiptKind::Url, "u").expect("ins");
    }
    {
        let conn = open_at(&path).expect("second open does not re-fail");
        let r = get(&conn, "persist").expect("get").expect("row");
        assert_eq!(r.trace_id, "persist");
    }
}

#[test]
fn schema_version_recorded_once() {
    let conn = fresh();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))
        .expect("count schema_version");
    assert_eq!(count, 1);
    let v: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| row.get(0))
        .expect("max version");
    assert_eq!(v, SCHEMA_VERSION);
}
