use super::*;
use chrono::TimeZone;
use vault::receipts::ReceiptKind;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 4, 22, 0, 0).single().expect("valid now")
}

fn mins_ago(m: i64) -> DateTime<Utc> {
    now() - TimeDelta::minutes(m)
}

fn secs_ago(s: i64) -> DateTime<Utc> {
    now() - TimeDelta::seconds(s)
}

fn row(trace: &str, received: DateTime<Utc>, status: RowStatus) -> QueueRow {
    QueueRow {
        trace_id: trace.to_string(),
        received_at: received,
        started_at: None,
        terminal_at: None,
        status,
        raw_input: format!("https://www.youtube.com/watch?v={trace}"),
        failure_stage: None,
        failure_reason: None,
    }
}

fn started(mut r: QueueRow, at: DateTime<Utc>) -> QueueRow {
    r.started_at = Some(at);
    r
}

fn finished(mut r: QueueRow, at: DateTime<Utc>) -> QueueRow {
    r.terminal_at = Some(at);
    r
}

fn cfg() -> QueueConfig {
    QueueConfig::default()
}

fn batch(s: &QueueSnapshot) -> &BatchSummary {
    s.batch.as_ref().expect("batch present")
}

fn item_state(s: &QueueSnapshot, trace: &str) -> Option<ItemState> {
    s.items
        .as_ref()
        .expect("items present")
        .iter()
        .find(|i| i.trace == trace)
        .map(|i| i.state)
}

#[test]
fn snapshot_idle_when_nothing_in_flight() {
    let rows = vec![
        finished(row("a", mins_ago(10), RowStatus::Succeeded), mins_ago(8)),
        finished(row("b", mins_ago(9), RowStatus::Failed), mins_ago(7)),
    ];
    let snap = snapshot(&rows, now(), &cfg(), None).expect("plain snapshot is always Some");
    assert_eq!(serde_json::to_string(&snap).expect("json"), r#"{"state":"idle"}"#);
    let empty = snapshot(&[], now(), &cfg(), None).expect("plain snapshot is always Some");
    assert_eq!(serde_json::to_string(&empty).expect("json"), r#"{"state":"idle"}"#);
}

#[test]
fn snapshot_partitions_and_ages() {
    let mut failed = finished(row("failed", mins_ago(20), RowStatus::Failed), mins_ago(19));
    failed.failure_stage = Some(FailureStage::FetchFailed);
    failed.failure_reason = Some("x".repeat(500));
    let rows = vec![
        failed,
        finished(row("succeeded", mins_ago(19), RowStatus::Succeeded), mins_ago(18)),
        // started_at 16m old -> wedged even though received recently-ish.
        started(row("wedged-started", mins_ago(17), RowStatus::Received), mins_ago(16)),
        // started_at NULL, received_at 16m old -> wedged (ages from receipt).
        row("wedged-queued", mins_ago(16), RowStatus::Received),
        started(row("processing", mins_ago(3), RowStatus::Received), mins_ago(2)),
        row("queued", mins_ago(1), RowStatus::Received),
    ];
    let snap = snapshot(&rows, now(), &cfg(), None).expect("some");
    assert_eq!(snap.state, QueueState::Draining);
    let b = batch(&snap);
    assert_eq!(b.id, "failed");
    assert_eq!(b.started, "2026-10-04T21:40:00Z");
    assert_eq!(b.elapsed_secs, 20 * 60);
    assert_eq!(
        (b.total, b.succeeded, b.failed, b.queued, b.processing, b.wedged),
        (6, 1, 1, 1, 1, 2)
    );
    assert_eq!(b.done, 2);
    assert_eq!(b.remaining, 4);

    assert_eq!(item_state(&snap, "wedged-started"), Some(ItemState::Wedged));
    assert_eq!(item_state(&snap, "wedged-queued"), Some(ItemState::Wedged));
    assert_eq!(item_state(&snap, "processing"), Some(ItemState::Processing));
    assert_eq!(item_state(&snap, "queued"), Some(ItemState::Queued));
    assert_eq!(item_state(&snap, "failed"), Some(ItemState::Failed));
    assert_eq!(item_state(&snap, "succeeded"), None, "succeeded items are not listed");

    let items = snap.items.as_ref().expect("items");
    let processing = items.iter().find(|i| i.trace == "processing").expect("processing item");
    assert_eq!(processing.received.as_deref(), Some("2026-10-04T21:57:00Z"));
    assert_eq!(processing.started.as_deref(), Some("2026-10-04T21:58:00Z"));
    assert_eq!(processing.age_secs, Some(120));
    let queued = items.iter().find(|i| i.trace == "queued").expect("queued item");
    assert_eq!(queued.started, None);
    assert_eq!(queued.age_secs, Some(60));
    let f = items.iter().find(|i| i.trace == "failed").expect("failed item");
    assert_eq!(f.failure_stage, Some(FailureStage::FetchFailed));
    assert_eq!(
        f.failure_reason.as_deref().map(|r| r.chars().count()),
        Some(FIELD_MAX_CHARS)
    );
    assert_eq!(f.received, None);
}

#[test]
fn snapshot_wedged_boundary_is_strictly_greater() {
    // age == wedged-after exactly is still processing; one second more is wedged.
    let rows = vec![
        started(row("at", mins_ago(20), RowStatus::Received), secs_ago(15 * 60)),
        started(row("over", mins_ago(20), RowStatus::Received), secs_ago(15 * 60 + 1)),
    ];
    let snap = snapshot(&rows, now(), &cfg(), None).expect("some");
    assert_eq!(item_state(&snap, "at"), Some(ItemState::Processing));
    assert_eq!(item_state(&snap, "over"), Some(ItemState::Wedged));
}

#[test]
fn snapshot_source_truncated_to_200_chars() {
    let mut r = row("long", mins_ago(1), RowStatus::Received);
    r.raw_input = "é".repeat(300);
    let snap = snapshot(&[r], now(), &cfg(), None).expect("some");
    let items = snap.items.expect("items");
    assert_eq!(items[0].source.chars().count(), FIELD_MAX_CHARS);
}

#[test]
fn snapshot_unknown_batch_id_is_none() {
    let rows = vec![row("a", mins_ago(1), RowStatus::Received)];
    assert_eq!(snapshot(&rows, now(), &cfg(), Some("nope")), None);
    // Any member finds its batch; the reported id stays the earliest member.
    let rows = vec![
        row("a", mins_ago(2), RowStatus::Received),
        row("b", mins_ago(1), RowStatus::Received),
    ];
    let by_member = snapshot(&rows, now(), &cfg(), Some("b")).expect("member finds batch");
    assert_eq!(batch(&by_member).id, "a");
    assert!(snapshot(&rows, now(), &cfg(), Some("a")).is_some());
}

#[test]
fn snapshot_pinned_id_survives_same_second_arrival_sorting_ahead() {
    // 1s timestamps tie and break by trace id, so a later arrival in the
    // same second can become the earliest member. The pinned id must still
    // resolve, or `wait` would 404 mid-drain.
    let t = secs_ago(30);
    let pinned = vec![row("ht-ffffffff", t, RowStatus::Received)];
    let first = snapshot(&pinned, now(), &cfg(), None).expect("plain");
    assert_eq!(batch(&first).id, "ht-ffffffff");

    let rows = vec![
        row("ht-ffffffff", t, RowStatus::Received),
        row("ht-00000000", t, RowStatus::Received),
    ];
    let snap = snapshot(&rows, now(), &cfg(), Some("ht-ffffffff")).expect("pinned id still resolves");
    assert_eq!(batch(&snap).total, 2);
}

#[test]
fn snapshot_plain_picks_batch_of_newest_in_flight_row() {
    // Old batch with a wedged straggler would merge everything after it (its
    // interval runs to now), so isolate: an old FINISHED batch, then a new one.
    let rows = vec![
        finished(row("old", mins_ago(60), RowStatus::Succeeded), mins_ago(58)),
        row("new", mins_ago(5), RowStatus::Received),
    ];
    let snap = snapshot(&rows, now(), &cfg(), None).expect("some");
    assert_eq!(batch(&snap).id, "new");
    assert_eq!(batch(&snap).total, 1);
}

fn insert(conn: &Connection, r: &QueueRow, method: Method) {
    let status = match r.status {
        RowStatus::Received => "received",
        RowStatus::Succeeded => "succeeded",
        RowStatus::Failed => "failed",
    };
    let fmt = |t: DateTime<Utc>| t.format(TIMESTAMP_FMT).to_string();
    conn.execute(
        "INSERT INTO receipts (trace_id, received_at, method, kind, raw_input, status, \
                               terminal_at, failure_stage, failure_reason, started_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            r.trace_id,
            fmt(r.received_at),
            method.as_str(),
            ReceiptKind::Url.as_str(),
            r.raw_input,
            status,
            r.terminal_at.map(fmt),
            r.failure_stage.map(|s| s.as_str()),
            r.failure_reason,
            r.started_at.map(fmt),
        ],
    )
    .expect("insert receipt");
}

fn db() -> Connection {
    crate::receipts::open_memory().expect("open_memory")
}

#[test]
fn snapshot_batch_is_stable_and_ignores_harvest() {
    // --- An early-finished row stays counted as later rows finish. ---
    let conn = db();
    insert(
        &conn,
        &finished(row("b1", mins_ago(6), RowStatus::Succeeded), mins_ago(5)),
        Method::Http,
    );
    insert(
        &conn,
        &started(row("b2", mins_ago(5), RowStatus::Received), mins_ago(4)),
        Method::Http,
    );
    insert(&conn, &row("b3", mins_ago(4), RowStatus::Received), Method::Cli);
    let before = load(&conn, now(), &cfg(), None).expect("load").expect("some");
    assert_eq!(batch(&before).id, "b1");
    assert_eq!((batch(&before).total, batch(&before).done), (3, 1));

    // b2 finishes; b1 (finished first) must still be a member.
    conn.execute(
        "UPDATE receipts SET status='succeeded', terminal_at=? WHERE trace_id='b2'",
        params![mins_ago(1).format(TIMESTAMP_FMT).to_string()],
    )
    .expect("finish b2");
    let after = load(&conn, now(), &cfg(), None).expect("load").expect("some");
    assert_eq!(batch(&after).id, "b1", "batch id is stable");
    assert_eq!(
        (batch(&after).total, batch(&after).done, batch(&after).remaining),
        (3, 2, 1)
    );

    // --- 100 harvest rows inside the window change nothing. ---
    for i in 0..100 {
        let mut h = row(&format!("h{i:03}"), secs_ago(400 - i), RowStatus::Received);
        if i % 2 == 0 {
            h = finished(h, secs_ago(399 - i));
            h.status = RowStatus::Succeeded;
        }
        insert(&conn, &h, Method::Harvest);
    }
    conn.execute(
        "INSERT INTO receipts (trace_id, received_at, method, kind, raw_input, status, terminal_at) \
         VALUES ('h-rej', ?, ?, 'session', 'x', 'rejected', ?)",
        params![
            mins_ago(2).format(TIMESTAMP_FMT).to_string(),
            Method::Harvest.as_str(),
            mins_ago(2).format(TIMESTAMP_FMT).to_string()
        ],
    )
    .expect("insert rejected harvest row");
    let with_harvest = load(&conn, now(), &cfg(), None).expect("load").expect("some");
    assert_eq!(with_harvest, after, "harvest rows must not change the snapshot");

    // --- `batch=<id>` returns a finished batch with state idle and its counts. ---
    conn.execute(
        "UPDATE receipts SET status='failed', failure_stage='fetch-failed', failure_reason='403', terminal_at=? \
         WHERE trace_id='b3'",
        params![secs_ago(30).format(TIMESTAMP_FMT).to_string()],
    )
    .expect("fail b3");
    let plain = load(&conn, now(), &cfg(), None).expect("load").expect("some");
    assert_eq!(serde_json::to_string(&plain).expect("json"), r#"{"state":"idle"}"#);
    let pinned = load(&conn, now(), &cfg(), Some("b1"))
        .expect("load")
        .expect("batch b1 exists");
    assert_eq!(pinned.state, QueueState::Idle);
    let b = batch(&pinned);
    assert_eq!((b.total, b.done, b.remaining, b.succeeded, b.failed), (3, 3, 0, 2, 1));
    assert_eq!(b.elapsed_secs, 6 * 60 - 30, "idle elapsed = last terminal - started");
    assert_eq!(item_state(&pinned, "b3"), Some(ItemState::Failed));
    assert_eq!(load(&conn, now(), &cfg(), Some("nope")).expect("load"), None);

    // --- A row whose gap to the batch is 3m is a separate batch. ---
    let conn = db();
    insert(
        &conn,
        &finished(row("g1", mins_ago(10), RowStatus::Succeeded), mins_ago(8)),
        Method::Http,
    );
    insert(&conn, &row("g2", mins_ago(5), RowStatus::Received), Method::Http);
    let split = load(&conn, now(), &cfg(), None).expect("load").expect("some");
    assert_eq!(batch(&split).id, "g2");
    assert_eq!(batch(&split).total, 1);
    let first = load(&conn, now(), &cfg(), Some("g1"))
        .expect("load")
        .expect("g1 is its own batch");
    assert_eq!((first.state, batch(&first).total), (QueueState::Idle, 1));
    // ...while a 2m gap joins.
    insert(
        &conn,
        &finished(row("j1", mins_ago(40), RowStatus::Succeeded), mins_ago(38)),
        Method::Http,
    );
    insert(
        &conn,
        &finished(row("j2", mins_ago(36), RowStatus::Succeeded), mins_ago(35)),
        Method::Http,
    );
    let joined = load(&conn, now(), &cfg(), Some("j1")).expect("load").expect("j1");
    assert_eq!(batch(&joined).total, 2);

    // --- A `received` row 30h old is still visible (finished history is trimmed). ---
    let conn = db();
    insert(
        &conn,
        &finished(
            row("ancient-done", now() - TimeDelta::hours(30), RowStatus::Succeeded),
            now() - TimeDelta::hours(30),
        ),
        Method::Http,
    );
    insert(
        &conn,
        &row("ancient", now() - TimeDelta::hours(30), RowStatus::Received),
        Method::Http,
    );
    let old = load(&conn, now(), &cfg(), None).expect("load").expect("some");
    assert_eq!(old.state, QueueState::Draining);
    assert_eq!(batch(&old).id, "ancient");
    assert_eq!(batch(&old).total, 1, "finished rows past 24h are trimmed");
    assert_eq!(item_state(&old, "ancient"), Some(ItemState::Wedged));
}

#[test]
fn load_errors_on_unparseable_timestamp_instead_of_reporting_idle() {
    let conn = db();
    conn.execute(
        "INSERT INTO receipts (trace_id, received_at, method, kind, raw_input, status) \
         VALUES ('bad', 'not-a-time', ?, 'url', 'x', 'received')",
        params![Method::Http.as_str()],
    )
    .expect("insert");
    let err = load(&conn, now(), &cfg(), None).expect_err("bad timestamp must error");
    assert!(format!("{err:#}").contains("received_at"), "{err:#}");
}
