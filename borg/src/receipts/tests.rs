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
    let updated = mark_succeeded(&conn, "tr-s1", "inbox/foo.md", false).expect("mark");
    assert!(updated);
    let r = get(&conn, "tr-s1").expect("get").expect("row");
    assert_eq!(r.status, "succeeded");
    assert_eq!(r.note_path.as_deref(), Some("inbox/foo.md"));
    assert!(r.terminal_at.is_some());
    assert!(!r.degraded, "non-degraded publish defaults degraded=false");
}

#[test]
fn mark_succeeded_records_degraded_and_query_filters_on_it() {
    let conn = fresh();
    record_received(&conn, "clean", Method::Cli, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "deg", Method::Cli, ReceiptKind::Url, "u").expect("ins");
    mark_succeeded(&conn, "clean", "inbox/clean.md", false).expect("mark clean");
    mark_succeeded(&conn, "deg", "inbox/deg.md", true).expect("mark degraded");

    assert!(!get(&conn, "clean").expect("get").expect("row").degraded);
    assert!(get(&conn, "deg").expect("get").expect("row").degraded);

    // `sb borg log --degraded` => Filter { degraded: Some(true) }.
    let degraded_only = query(
        &conn,
        &Filter {
            degraded: Some(true),
            ..Default::default()
        },
    )
    .expect("query degraded");
    assert_eq!(degraded_only.len(), 1);
    assert_eq!(degraded_only[0].trace_id, "deg");
}

#[test]
fn migration_adds_degraded_column_to_pre_v2_db() {
    // Simulate a v1 DB: a `receipts` table WITHOUT the `degraded` column.
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(
        "CREATE TABLE receipts (
            trace_id TEXT NOT NULL PRIMARY KEY,
            received_at TEXT NOT NULL,
            method TEXT NOT NULL,
            kind TEXT NOT NULL,
            raw_input TEXT NOT NULL,
            status TEXT NOT NULL,
            terminal_at TEXT, note_path TEXT, failure_stage TEXT,
            failure_reason TEXT, replay_of TEXT
        );",
    )
    .expect("create v1 table");
    assert!(!has_column(&conn, "receipts", "degraded").expect("probe"));
    run_migrations(&conn).expect("migrate");
    assert!(
        has_column(&conn, "receipts", "degraded").expect("probe"),
        "v2 migration must add the degraded column"
    );
    // Idempotent: a second run does not error.
    run_migrations(&conn).expect("migrate again");
}

#[test]
fn mark_succeeded_is_noop_on_already_terminal_row() {
    let conn = fresh();
    record_received(&conn, "tr-s2", Method::Cli, ReceiptKind::Url, "u").expect("ins");
    mark_failed(&conn, "tr-s2", FailureStage::FetchFailed, "yt-dlp").expect("first transition wins");
    let updated = mark_succeeded(&conn, "tr-s2", "inbox/foo.md", false).expect("mark");
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
fn list_stale_selects_only_past_deadline_rows() {
    let conn = fresh();
    // Fresh row, recent: should NOT be selected as stale.
    record_received(&conn, "fresh", Method::Http, ReceiptKind::Url, "u").expect("fresh");
    // Synthesize an old row by patching received_at to far in the past.
    record_received(&conn, "old", Method::Http, ReceiptKind::Url, "u").expect("old");
    conn.execute(
        "UPDATE receipts SET received_at='2024-01-01T00:00:00Z' WHERE trace_id='old'",
        [],
    )
    .expect("backdate");
    // The watchdog selects stale candidates, then promotes each survivor
    // individually (the mass promote_stale_to_crashed was deleted as dead).
    let now = Utc::now();
    let stale = list_stale(&conn, 60, now).expect("list_stale");
    assert_eq!(stale.len(), 1, "only the old row is stale");
    assert_eq!(stale[0].0, "old");
    let promoted = promote_single_to_crashed(&conn, "old", 60, now).expect("promote");
    assert!(promoted);
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
    mark_succeeded(&conn, "tr-race", "inbox/race.md", false).expect("succ");
    // Watchdog tries to crash it now -> no-op.
    let crashed = promote_single_to_crashed(&conn, "tr-race", 60, Utc::now()).expect("promote");
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
    let stale = list_stale(&conn, 60, Utc::now()).expect("list_stale");
    let ids: Vec<&str> = stale.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["old1"]);
}

#[test]
fn query_filters_by_status() {
    let conn = fresh();
    record_received(&conn, "a", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "b", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "c", Method::Http, ReceiptKind::Url, "u").expect("ins");
    mark_succeeded(&conn, "a", "n1.md", false).expect("succ a");
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
fn watchdog_crash_promotion_is_queryable_by_stage_crashed() {
    // The receipts-only replacement for borg-orphans.md: a stale `received`
    // row is promoted to failed/crashed, and `sb borg log --stage crashed`
    // (a query with stage=Crashed) returns it.
    let conn = fresh();
    record_received(&conn, "stale", Method::Http, ReceiptKind::Url, "u").expect("ins");
    conn.execute(
        "UPDATE receipts SET received_at='2024-01-01T00:00:00Z' WHERE trace_id='stale'",
        [],
    )
    .expect("backdate");
    let promoted = promote_single_to_crashed(&conn, "stale", 60, Utc::now()).expect("promote");
    assert!(promoted);
    let crashed = query(
        &conn,
        &Filter {
            stage: Some(FailureStage::Crashed),
            ..Default::default()
        },
    )
    .expect("query --stage crashed");
    assert_eq!(crashed.len(), 1);
    assert_eq!(crashed[0].trace_id, "stale");
    assert_eq!(crashed[0].failure_stage.as_deref(), Some("crashed"));
}

#[test]
fn count_since_helpers_window_on_terminal_at() {
    let conn = fresh();
    // recent crashed + recent fetch-failed (terminal_at = now)
    record_received(&conn, "c1", Method::Http, ReceiptKind::Url, "u").expect("ins");
    mark_failed(&conn, "c1", FailureStage::Crashed, "x").expect("fail");
    record_received(&conn, "f1", Method::Http, ReceiptKind::Url, "u").expect("ins");
    mark_failed(&conn, "f1", FailureStage::FetchFailed, "x").expect("fail");
    // old crashed: received recently but terminal_at far in the past
    record_received(&conn, "c0", Method::Http, ReceiptKind::Url, "u").expect("ins");
    mark_failed(&conn, "c0", FailureStage::Crashed, "x").expect("fail");
    conn.execute(
        "UPDATE receipts SET terminal_at='2024-01-01T00:00:00Z' WHERE trace_id='c0'",
        [],
    )
    .expect("backdate terminal_at");

    let since = hours_ago_iso(24);
    assert_eq!(
        count_failed_since(&conn, &since).expect("failed_since"),
        2,
        "c1 + f1 in window; c0 old"
    );
    assert_eq!(
        count_crashed_since(&conn, &since).expect("crashed_since"),
        1,
        "only c1 crashed in window"
    );
}

#[test]
fn count_kind_since_windows_on_received_at_regardless_of_status() {
    let conn = fresh();
    // Recent: one session receipt still `received`, one rejected. Both
    // TOUCHED the pipeline in the window even though neither succeeded.
    record_received(&conn, "s1", Method::Cli, ReceiptKind::Session, "body").expect("ins");
    record_received(&conn, "s2", Method::Cli, ReceiptKind::Session, "body").expect("ins");
    mark_rejected(&conn, "s2", "below bar").expect("reject");
    // A non-session receipt in the same window must not be counted.
    record_received(&conn, "u1", Method::Http, ReceiptKind::Url, "u").expect("ins");
    // An old session receipt (received long ago) must not be counted either.
    record_received(&conn, "s0", Method::Cli, ReceiptKind::Session, "body").expect("ins");
    conn.execute(
        "UPDATE receipts SET received_at='2024-01-01T00:00:00Z' WHERE trace_id='s0'",
        [],
    )
    .expect("backdate received_at");

    let since = hours_ago_iso(24);
    assert_eq!(
        count_kind_since(&conn, ReceiptKind::Session, &since).expect("count_kind_since"),
        2,
        "s1 + s2 (rejected still counts - it TOUCHED the pipeline); s0 too old, u1 wrong kind"
    );
}

#[test]
fn count_by_status_groups_correctly() {
    let conn = fresh();
    record_received(&conn, "r1", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "r2", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "s", Method::Http, ReceiptKind::Url, "u").expect("ins");
    record_received(&conn, "f", Method::Http, ReceiptKind::Url, "u").expect("ins");
    mark_succeeded(&conn, "s", "n.md", false).expect("s");
    mark_failed(&conn, "f", FailureStage::FetchFailed, "x").expect("f");
    let (recv, succ, fail, rejected) = count_by_status(&conn).expect("count");
    assert_eq!((recv, succ, fail, rejected), (2, 1, 1, 0));
}

#[test]
fn count_by_status_includes_rejected() {
    let conn = fresh();
    record_received(&conn, "r1", Method::Cli, ReceiptKind::Session, "body").expect("ins");
    mark_rejected(&conn, "r1", "below selection bar").expect("reject");
    let (recv, succ, fail, rejected) = count_by_status(&conn).expect("count");
    assert_eq!((recv, succ, fail, rejected), (0, 0, 0, 1));
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
fn session_rejected_row_roundtrips() {
    let conn = fresh();
    record_received(
        &conn,
        "hv-001",
        Method::Cli,
        ReceiptKind::Session,
        "human: hi\nassistant: hello",
    )
    .expect("record_received");
    mark_rejected(&conn, "hv-001", "below selection bar").expect("mark_rejected");
    let r = get(&conn, "hv-001").expect("get").expect("row present");
    assert_eq!(r.kind, "session");
    assert_eq!(r.status, "rejected");
    assert_eq!(r.failure_reason.as_deref(), Some("below selection bar"));
    assert!(r.terminal_at.is_some());
}

#[test]
fn mark_rejected_is_noop_on_already_terminal_row() {
    let conn = fresh();
    record_received(&conn, "hv-002", Method::Cli, ReceiptKind::Session, "body").expect("record_received");
    mark_succeeded(&conn, "hv-002", "n.md", false).expect("mark_succeeded");
    let promoted = mark_rejected(&conn, "hv-002", "too late").expect("mark_rejected no-op");
    assert!(!promoted, "already-succeeded row must not flip to rejected");
    let r = get(&conn, "hv-002").expect("get").expect("row present");
    assert_eq!(r.status, "succeeded");
}

#[test]
fn v3_migration_widens_check_constraint_on_legacy_db() {
    // Simulate a pre-v3 DB: create the table with the ORIGINAL (narrower)
    // CHECK constraints, insert a row, then run migrations and confirm the
    // widened constraint accepts 'session'/'rejected' and the pre-existing
    // row survives the rebuild untouched.
    let conn = Connection::open_in_memory().expect("open_in_memory");
    apply_pragmas(&conn).expect("pragmas");
    conn.execute_batch(
        "CREATE TABLE receipts (
           trace_id        TEXT NOT NULL PRIMARY KEY,
           received_at     TEXT NOT NULL,
           method          TEXT NOT NULL,
           kind            TEXT NOT NULL
                            CHECK (kind IN ('url', 'text', 'binary')),
           raw_input       TEXT NOT NULL,
           status          TEXT NOT NULL
                            CHECK (status IN ('received', 'succeeded', 'failed')),
           terminal_at     TEXT,
           note_path       TEXT,
           failure_stage   TEXT,
           failure_reason  TEXT,
           replay_of       TEXT,
           degraded        INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE schema_version (version INTEGER NOT NULL PRIMARY KEY, applied_at TEXT NOT NULL);
         INSERT INTO schema_version (version, applied_at) VALUES (2, '2026-01-01T00:00:00Z');
         INSERT INTO receipts (trace_id, received_at, method, kind, raw_input, status)
           VALUES ('legacy-1', '2026-01-01T00:00:00Z', 'http', 'url', 'https://x.com', 'succeeded');",
    )
    .expect("seed legacy schema");

    run_migrations(&conn).expect("migrate legacy db to v3");

    // The pre-existing row survived the rebuild.
    let r = get(&conn, "legacy-1").expect("get").expect("legacy row present");
    assert_eq!(r.status, "succeeded");
    assert_eq!(r.kind, "url");

    // The widened constraint now accepts 'session'/'rejected'.
    record_received(&conn, "hv-legacy", Method::Cli, ReceiptKind::Session, "body").expect("session insert accepted");
    mark_rejected(&conn, "hv-legacy", "below bar").expect("rejected status accepted");

    // Re-running migrations on the now-v3 DB is a no-op (idempotent).
    run_migrations(&conn).expect("second migrate is a no-op");
    let row_count_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
        .expect("count");
    assert_eq!(row_count_after, 2, "rerun must not duplicate or drop rows");
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

#[test]
fn fresh_db_has_lease_columns() {
    // A brand-new DB created straight from the baseline schema.sql (v4)
    // must have both lease columns without any migration running.
    let conn = fresh();
    assert!(
        has_column(&conn, "receipts", "lease_owner_pid").expect("probe"),
        "fresh DB must have lease_owner_pid"
    );
    assert!(
        has_column(&conn, "receipts", "lease_until").expect("probe"),
        "fresh DB must have lease_until"
    );
}

#[test]
fn v4_migration_adds_lease_columns_surviving_v3_rebuild_from_pre_v3_db() {
    // Simulate a v1 DB (no `degraded` column, narrower CHECK constraints, no
    // `lease_*` columns) seeded at a real file path, then drive it through
    // the real `open_at()` entry point -- which chains v2 (add degraded), v3
    // (rebuild, widen CHECK), then v4 (add lease columns) in that order.
    // Proves the v4 columns survive the v3 rebuild's fixed 12-column
    // INSERT...SELECT (they are added AFTER it, per the migration-ordering
    // resolved decision) and that re-opening is idempotent.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("receipts.db");
    {
        let conn = Connection::open(&path).expect("open raw v1 db");
        apply_pragmas(&conn).expect("pragmas");
        conn.execute_batch(
            "CREATE TABLE receipts (
               trace_id        TEXT NOT NULL PRIMARY KEY,
               received_at     TEXT NOT NULL,
               method          TEXT NOT NULL,
               kind            TEXT NOT NULL
                                CHECK (kind IN ('url', 'text', 'binary')),
               raw_input       TEXT NOT NULL,
               status          TEXT NOT NULL
                                CHECK (status IN ('received', 'succeeded', 'failed')),
               terminal_at     TEXT,
               note_path       TEXT,
               failure_stage   TEXT,
               failure_reason  TEXT,
               replay_of       TEXT
             );
             CREATE TABLE schema_version (version INTEGER NOT NULL PRIMARY KEY, applied_at TEXT NOT NULL);
             INSERT INTO schema_version (version, applied_at) VALUES (1, '2026-01-01T00:00:00Z');
             INSERT INTO receipts (trace_id, received_at, method, kind, raw_input, status)
               VALUES ('legacy-1', '2026-01-01T00:00:00Z', 'http', 'url', 'https://x.com', 'succeeded');",
        )
        .expect("seed pre-v3 (v1) schema");
    }

    // First open: chains v2 -> v3 -> v4. Confirm both new columns exist and
    // the pre-existing row survived the v3 rebuild untouched.
    {
        let conn = open_at(&path).expect("first open migrates v1 all the way to v4");
        assert!(
            has_column(&conn, "receipts", "lease_owner_pid").expect("probe"),
            "lease_owner_pid must survive the v3 rebuild"
        );
        assert!(
            has_column(&conn, "receipts", "lease_until").expect("probe"),
            "lease_until must survive the v3 rebuild"
        );
        let r = get(&conn, "legacy-1").expect("get").expect("legacy row present");
        assert_eq!(r.status, "succeeded");
        assert_eq!(r.kind, "url");
    }

    // Second open: idempotent, no error, columns still present.
    {
        let conn = open_at(&path).expect("second open does not re-fail");
        assert!(has_column(&conn, "receipts", "lease_owner_pid").expect("probe"));
        assert!(has_column(&conn, "receipts", "lease_until").expect("probe"));
        let r = get(&conn, "legacy-1").expect("get").expect("legacy row still present");
        assert_eq!(r.status, "succeeded");
    }
}

#[test]
fn migrations_do_not_panic_on_a_newer_stored_schema_version() {
    // A DB stamped with a schema_version newer than the code's SCHEMA_VERSION
    // (a v4 DB opened by an older binary, or a hypothetical future version)
    // must not panic and must not be downgraded. The `v >= SCHEMA_VERSION`
    // match arm is the rollback-safety guard; this test bites if that arm is
    // ever removed or narrowed to exact equality.
    let conn = fresh();
    let future_version = SCHEMA_VERSION + 95;
    conn.execute(
        "UPDATE schema_version SET version=? WHERE version=?",
        params![future_version, SCHEMA_VERSION],
    )
    .expect("bump stored version to a future value");
    run_migrations(&conn).expect("must not panic on a newer stored version");
    let v: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| row.get(0))
        .expect("max version");
    assert_eq!(v, future_version, "stored version must not be downgraded");
    // Column probes are unconditional, so they still hold on the "future" DB.
    assert!(has_column(&conn, "receipts", "lease_owner_pid").expect("probe"));
    assert!(has_column(&conn, "receipts", "lease_until").expect("probe"));
}

/// Format `now + secs` (or `now - secs` for a negative offset) as a
/// [`TIMESTAMP_FMT`] string, matching how `lease_until` is stored.
fn lease_at(now: DateTime<Utc>, offset_secs: i64) -> String {
    (now + chrono::Duration::seconds(offset_secs))
        .format(TIMESTAMP_FMT)
        .to_string()
}

#[test]
fn write_lease_sets_pid_and_expiry() {
    let conn = fresh();
    record_received(&conn, "tr-lease1", Method::Http, ReceiptKind::Url, "u").expect("ins");
    let now = Utc::now();
    let until = lease_at(now, 1800);
    write_lease(&conn, "tr-lease1", 4242, &until).expect("write_lease");
    let owner_pid: i64 = conn
        .query_row(
            "SELECT lease_owner_pid FROM receipts WHERE trace_id='tr-lease1'",
            [],
            |r| r.get(0),
        )
        .expect("read lease_owner_pid");
    let lease_until: String = conn
        .query_row("SELECT lease_until FROM receipts WHERE trace_id='tr-lease1'", [], |r| {
            r.get(0)
        })
        .expect("read lease_until");
    assert_eq!(owner_pid, 4242);
    assert_eq!(lease_until, until);
}

#[test]
fn write_lease_is_noop_on_terminal_row() {
    // A lease write against an already-terminal row must not resurrect it or
    // stamp a lease that would never be cleared (mark_succeeded/mark_failed
    // are the only lease-clearing sites, and they never run again on an
    // absorbing state).
    let conn = fresh();
    record_received(&conn, "tr-lease-term", Method::Http, ReceiptKind::Url, "u").expect("ins");
    mark_succeeded(&conn, "tr-lease-term", "n.md", false).expect("mark_succeeded");
    write_lease(&conn, "tr-lease-term", 999, &lease_at(Utc::now(), 1800)).expect("write_lease no-op");
    let r = get(&conn, "tr-lease-term").expect("get").expect("row");
    assert_eq!(r.status, "succeeded", "already-terminal row is untouched");
}

#[test]
fn renew_lease_updates_expiry_only() {
    let conn = fresh();
    record_received(&conn, "tr-renew", Method::Http, ReceiptKind::Url, "u").expect("ins");
    let now = Utc::now();
    write_lease(&conn, "tr-renew", 111, &lease_at(now, 1800)).expect("write_lease");
    let renewed_until = lease_at(now, 3600);
    renew_lease(&conn, "tr-renew", &renewed_until).expect("renew_lease");
    let owner_pid: i64 = conn
        .query_row(
            "SELECT lease_owner_pid FROM receipts WHERE trace_id='tr-renew'",
            [],
            |r| r.get(0),
        )
        .expect("read lease_owner_pid");
    let lease_until: String = conn
        .query_row("SELECT lease_until FROM receipts WHERE trace_id='tr-renew'", [], |r| {
            r.get(0)
        })
        .expect("read lease_until");
    assert_eq!(owner_pid, 111, "renew never touches lease_owner_pid");
    assert_eq!(lease_until, renewed_until, "renew re-stamps lease_until");
}

#[test]
fn mark_succeeded_nulls_lease_columns_in_one_update() {
    let conn = fresh();
    record_received(&conn, "tr-succ-lease", Method::Http, ReceiptKind::Url, "u").expect("ins");
    write_lease(&conn, "tr-succ-lease", 555, &lease_at(Utc::now(), 1800)).expect("write_lease");
    mark_succeeded(&conn, "tr-succ-lease", "n.md", false).expect("mark_succeeded");
    let (owner_pid, lease_until): (Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT lease_owner_pid, lease_until FROM receipts WHERE trace_id='tr-succ-lease'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("read lease columns");
    assert!(owner_pid.is_none(), "mark_succeeded must NULL lease_owner_pid");
    assert!(lease_until.is_none(), "mark_succeeded must NULL lease_until");
}

#[test]
fn mark_failed_nulls_lease_columns_in_one_update() {
    let conn = fresh();
    record_received(&conn, "tr-fail-lease", Method::Http, ReceiptKind::Url, "u").expect("ins");
    write_lease(&conn, "tr-fail-lease", 555, &lease_at(Utc::now(), 1800)).expect("write_lease");
    mark_failed(&conn, "tr-fail-lease", FailureStage::FetchFailed, "boom").expect("mark_failed");
    let (owner_pid, lease_until): (Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT lease_owner_pid, lease_until FROM receipts WHERE trace_id='tr-fail-lease'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("read lease columns");
    assert!(owner_pid.is_none(), "mark_failed must NULL lease_owner_pid");
    assert!(lease_until.is_none(), "mark_failed must NULL lease_until");
}

#[test]
fn fresh_lease_excludes_row_from_list_stale_and_promotion_despite_past_deadline() {
    // Backdated well past the received_at deadline, but the lease is still
    // FRESH (in the future) - a live cross-process owner is still renewing
    // it. Both the SELECT and the atomic promotion UPDATE must agree the row
    // is not a reap candidate. This is the bite check for the whole feature:
    // dropping the lease predicate from either list_stale or the promotion
    // UPDATE's WHERE clause would flip one or both of these assertions.
    let conn = fresh();
    record_received(&conn, "leased", Method::Http, ReceiptKind::Url, "u").expect("ins");
    conn.execute(
        "UPDATE receipts SET received_at='2024-01-01T00:00:00Z' WHERE trace_id='leased'",
        [],
    )
    .expect("backdate");
    let now = Utc::now();
    write_lease(&conn, "leased", 777, &lease_at(now, 1800)).expect("write_lease fresh");

    let stale = list_stale(&conn, 60, now).expect("list_stale");
    assert!(
        stale.iter().all(|(id, _)| id != "leased"),
        "a row with a fresh lease must not be a stale candidate"
    );

    let promoted = promote_single_to_crashed(&conn, "leased", 60, now).expect("promote");
    assert!(
        !promoted,
        "the promotion UPDATE must match 0 rows when the lease is still fresh"
    );
    let r = get(&conn, "leased").expect("get").expect("row");
    assert_eq!(r.status, "received", "fresh-leased row is untouched");
}

#[test]
fn expired_lease_is_stale_and_promoted_with_lease_expired_reason() {
    let conn = fresh();
    record_received(&conn, "expired", Method::Http, ReceiptKind::Url, "u").expect("ins");
    conn.execute(
        "UPDATE receipts SET received_at='2024-01-01T00:00:00Z' WHERE trace_id='expired'",
        [],
    )
    .expect("backdate");
    let now = Utc::now();
    // Lease existed but expired 30 minutes ago.
    write_lease(&conn, "expired", 888, &lease_at(now, -1800)).expect("write_lease expired");

    let stale = list_stale(&conn, 60, now).expect("list_stale");
    assert!(
        stale.iter().any(|(id, _)| id == "expired"),
        "a row with an expired lease is a stale candidate"
    );

    let promoted = promote_single_to_crashed(&conn, "expired", 60, now).expect("promote");
    assert!(promoted, "an expired lease must be reaped");
    let r = get(&conn, "expired").expect("get").expect("row");
    assert_eq!(r.status, "failed");
    assert_eq!(r.failure_stage.as_deref(), Some("crashed"));
    assert_eq!(
        r.failure_reason.as_deref(),
        Some("lease-expired"),
        "an expired (not absent) lease gets the distinct lease-expired reason"
    );
}

#[test]
fn null_lease_is_stale_and_promoted_with_generic_reason() {
    // A row that never held a lease (legacy pre-Phase-4 row, or a trace whose
    // owning process died before ever writing one) keeps the pre-existing
    // generic "no terminal event within Ns" reason - only an EXPIRED (not
    // absent) lease gets the distinct lease-expired reason.
    let conn = fresh();
    record_received(&conn, "nulllease", Method::Http, ReceiptKind::Url, "u").expect("ins");
    conn.execute(
        "UPDATE receipts SET received_at='2024-01-01T00:00:00Z' WHERE trace_id='nulllease'",
        [],
    )
    .expect("backdate");
    let now = Utc::now();

    let stale = list_stale(&conn, 60, now).expect("list_stale");
    assert!(stale.iter().any(|(id, _)| id == "nulllease"));

    let promoted = promote_single_to_crashed(&conn, "nulllease", 60, now).expect("promote");
    assert!(promoted);
    let r = get(&conn, "nulllease").expect("get").expect("row");
    assert_eq!(r.failure_reason.as_deref(), Some("no terminal event within 60s"));
}

#[test]
fn query_filters_by_harvest_method() {
    // Phase 6 observability: `sb borg log --method harvest` maps to this
    // filter. A harvest session row and a non-harvest row coexist; the filter
    // returns only the harvest one (proves Method::Harvest from Phase 1 flows
    // through the query end to end).
    let conn = fresh();
    record_received(&conn, "hv-1", Method::Harvest, ReceiptKind::Session, "clyde://abc").expect("ins harvest");
    record_received(&conn, "cli-1", Method::Cli, ReceiptKind::Url, "https://x/y").expect("ins cli");
    let filter = Filter {
        status: None,
        method: Some(Method::Harvest),
        stage: None,
        since: None,
        source_like: None,
        degraded: None,
        limit: None,
    };
    let rows = query(&conn, &filter).expect("query");
    assert_eq!(rows.len(), 1, "only the harvest row matches method=harvest");
    assert_eq!(rows[0].method, "harvest");
    assert_eq!(rows[0].kind, "session");
    assert_eq!(rows[0].raw_input, "clyde://abc");
}
