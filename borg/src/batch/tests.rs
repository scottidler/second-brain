#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use super::notifier::{self, Sink, SinkOutcome};
use super::*;
use crate::notify::MessageSink;
use crate::receipts;
use crate::types::IngestMethod;
use vault::receipts::{FailureStage, ReceiptKind};

// ---- helpers --------------------------------------------------------------------

fn member(conn: &Connection, batch_id: &str, trace_id: &str) {
    create_if_absent(conn, batch_id).unwrap();
    receipts::record_received_in_batch(
        conn,
        trace_id,
        IngestMethod::Cli,
        ReceiptKind::Url,
        "https://x",
        None,
        batch_id,
    )
    .unwrap();
}

fn ids(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("tr-{i}")).collect()
}

fn backdate_batch(conn: &Connection, batch_id: &str) {
    conn.execute(
        "UPDATE receipts SET received_at='2024-01-01T00:00:00Z' WHERE batch_id=?",
        [batch_id],
    )
    .unwrap();
}

/// A sink that records every message instead of sending it.
#[derive(Default)]
struct Counting {
    sent: Mutex<Vec<String>>,
    fail: bool,
}

impl Counting {
    fn texts(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl MessageSink for Counting {
    async fn send(&self, text: &str) -> std::result::Result<(), String> {
        self.sent.lock().unwrap().push(text.to_string());
        if self.fail { Err("boom".to_string()) } else { Ok(()) }
    }
}

/// A notifier over counting sinks with the disable gate open. Returns the
/// sender, the sinks, and the task to join once every sender is dropped.
fn counting_notifier() -> (SummarySender, Arc<Counting>, Arc<Counting>, tokio::task::JoinHandle<()>) {
    let desktop = Arc::new(Counting::default());
    let telegram = Arc::new(Counting::default());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(notifier::run_with(
        rx,
        Some(desktop.clone() as Sink),
        Some(telegram.clone() as Sink),
        false,
    ));
    (tx, desktop, telegram, task)
}

fn summaries_in(texts: &[String]) -> usize {
    texts.iter().filter(|t| t.starts_with("borg bulk ")).count()
}

// ---- text and ids ---------------------------------------------------------------

#[test]
fn summary_text_is_the_documented_line() {
    let mut s = BatchSummary {
        batch_id: "b1".into(),
        succeeded: 5,
        degraded: 1,
        failed: 1,
        crashed: 1,
        total: 6,
        unknown: 0,
        seal_missing: false,
    };
    assert_eq!(
        s.text(),
        "borg bulk b1: 5 succeeded (1 degraded), 1 failed (1 crashed) of 6 URL items"
    );
    s.seal_missing = true;
    s.unknown = 2;
    assert_eq!(
        s.text(),
        "borg bulk b1: 5 succeeded (1 degraded), 1 failed (1 crashed) of 6 URL items, seal missing, 2 unknown"
    );
}

#[test]
fn valid_id_takes_a_uuid_and_refuses_blank_long_or_path_shaped_ids() {
    assert!(valid_id(&uuid::Uuid::new_v4().to_string()));
    assert!(!valid_id(""));
    assert!(!valid_id(&"a".repeat(65)));
    assert!(!valid_id("../x"));
    assert!(!valid_id("a b"));
}

// ---- seal -----------------------------------------------------------------------

#[test]
fn seal_is_unknown_first_identical_and_conflicting_by_set() {
    let conn = receipts::open_memory().unwrap();
    assert_eq!(seal(&conn, "nope", &ids(1)).unwrap(), SealOutcome::UnknownBatch);
    member(&conn, "b1", "tr-1");
    member(&conn, "b1", "tr-2");
    assert_eq!(seal(&conn, "b1", &ids(2)).unwrap(), SealOutcome::Sealed);
    let reordered = vec!["tr-2".to_string(), "tr-1".to_string(), "tr-1".to_string()];
    assert_eq!(seal(&conn, "b1", &reordered).unwrap(), SealOutcome::AlreadySealed);
    assert_eq!(seal(&conn, "b1", &ids(1)).unwrap(), SealOutcome::Conflict);
}

// ---- drained predicate ----------------------------------------------------------

#[test]
fn an_unsealed_batch_never_drains() {
    let conn = receipts::open_memory().unwrap();
    member(&conn, "b1", "tr-1");
    receipts::mark_succeeded(&conn, "tr-1", "n.md", false).unwrap();
    assert!(!drained(&conn, "b1").unwrap());
    assert_eq!(maybe_summarize(&conn, "b1").unwrap(), None);
}

#[test]
fn a_member_still_received_holds_the_drain() {
    let conn = receipts::open_memory().unwrap();
    member(&conn, "b1", "tr-1");
    member(&conn, "b1", "tr-2");
    seal(&conn, "b1", &ids(2)).unwrap();
    receipts::mark_succeeded(&conn, "tr-1", "n.md", false).unwrap();
    assert!(!drained(&conn, "b1").unwrap());
    receipts::mark_failed(&conn, "tr-2", FailureStage::FetchFailed, "x").unwrap();
    assert!(drained(&conn, "b1").unwrap());
}

/// A sealed id the daemon has no row for yet (the client saw it queued) holds
/// the drain even though every row present is terminal.
#[test]
fn a_sealed_id_with_no_row_holds_the_drain() {
    let conn = receipts::open_memory().unwrap();
    member(&conn, "b1", "tr-1");
    seal(&conn, "b1", &ids(2)).unwrap();
    receipts::mark_succeeded(&conn, "tr-1", "n.md", false).unwrap();
    assert!(!drained(&conn, "b1").unwrap(), "tr-2 is sealed but has no row");
    assert_eq!(maybe_summarize(&conn, "b1").unwrap(), None);
    member(&conn, "b1", "tr-2");
    receipts::mark_succeeded(&conn, "tr-2", "n.md", false).unwrap();
    assert!(drained(&conn, "b1").unwrap());
}

// ---- claim ----------------------------------------------------------------------

#[test]
fn the_summary_counts_every_member_and_is_claimed_once() {
    let conn = receipts::open_memory().unwrap();
    for id in ids(4) {
        member(&conn, "b1", &id);
    }
    seal(&conn, "b1", &ids(4)).unwrap();
    receipts::mark_succeeded(&conn, "tr-1", "n.md", false).unwrap();
    receipts::mark_succeeded(&conn, "tr-2", "n.md", true).unwrap();
    receipts::mark_failed(&conn, "tr-3", FailureStage::FetchFailed, "x").unwrap();
    receipts::mark_failed(&conn, "tr-4", FailureStage::Crashed, "x").unwrap();
    let summary = after_terminal(&conn, "tr-4").unwrap().expect("drained");
    assert_eq!(
        summary.text(),
        "borg bulk b1: 2 succeeded (1 degraded), 2 failed (1 crashed) of 4 URL items"
    );
    assert_eq!(maybe_summarize(&conn, "b1").unwrap(), None, "claimed once");
    assert_eq!(after_terminal(&conn, "tr-1").unwrap(), None);
}

#[test]
fn an_unbatched_trace_summarizes_nothing() {
    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, "solo", IngestMethod::Http, ReceiptKind::Url, "u").unwrap();
    receipts::mark_succeeded(&conn, "solo", "n.md", false).unwrap();
    assert_eq!(batch_of(&conn, "solo").unwrap(), None);
    assert_eq!(after_terminal(&conn, "solo").unwrap(), None);
}

/// Eight callers on their own connections race one drained batch: exactly
/// one wins the claim.
#[test]
fn the_claim_is_exactly_once_under_concurrent_callers() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("receipts.db");
    {
        let conn = receipts::open_at(&path).unwrap();
        for id in ids(3) {
            member(&conn, "b1", &id);
            receipts::mark_succeeded(&conn, &id, "n.md", false).unwrap();
        }
        seal(&conn, "b1", &ids(3)).unwrap();
    }
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let conn = receipts::open_at(&path).unwrap();
                barrier.wait();
                maybe_summarize(&conn, "b1").unwrap()
            })
        })
        .collect();
    let wins = handles
        .into_iter()
        .filter_map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(wins.len(), 1, "{wins:?}");
    assert_eq!(wins[0].total, 3);
}

/// The claim re-checks the drain itself: a member whose intake lands after
/// `drained` said yes but before the claim (a lost-response member) fails the
/// claim, and the batch summarizes only once that member is terminal.
#[test]
fn a_member_received_between_the_drain_check_and_the_claim_blocks_the_claim() {
    let conn = receipts::open_memory().unwrap();
    member(&conn, "b1", "tr-1");
    seal(&conn, "b1", &ids(1)).unwrap();
    receipts::mark_succeeded(&conn, "tr-1", "n.md", false).unwrap();
    assert!(drained(&conn, "b1").unwrap(), "drained before the late intake");

    member(&conn, "b1", "tr-late");
    assert_eq!(
        claim(&conn, "b1", false).unwrap(),
        None,
        "claimed past a received member"
    );

    receipts::mark_succeeded(&conn, "tr-late", "n.md", false).unwrap();
    let summary = maybe_summarize(&conn, "b1").unwrap().expect("drained now");
    assert_eq!(summary.total, 2);
}

/// The same guard covers the sealed-id half of the drain predicate, and the
/// backstop claim refuses a batch with a member still `received`.
#[test]
fn the_claim_refuses_a_missing_sealed_row_and_the_backstop_claim_a_received_member() {
    let conn = receipts::open_memory().unwrap();
    member(&conn, "b1", "tr-1");
    seal(&conn, "b1", &ids(2)).unwrap();
    receipts::mark_succeeded(&conn, "tr-1", "n.md", false).unwrap();
    assert_eq!(
        claim(&conn, "b1", false).unwrap(),
        None,
        "tr-2 is sealed but has no row"
    );

    member(&conn, "b2", "tr-9");
    assert_eq!(claim(&conn, "b2", true).unwrap(), None, "tr-9 is still received");
    receipts::mark_succeeded(&conn, "tr-9", "n.md", false).unwrap();
    assert!(claim(&conn, "b2", true).unwrap().expect("drained").seal_missing);
}

/// `reconcile_sealed` summarizes a sealed, drained, unclaimed batch once and
/// leaves unsealed and undrained batches alone.
#[test]
fn reconcile_sealed_claims_only_sealed_drained_batches_once() {
    let conn = receipts::open_memory().unwrap();
    member(&conn, "done", "tr-1");
    seal(&conn, "done", &ids(1)).unwrap();
    receipts::mark_succeeded(&conn, "tr-1", "n.md", false).unwrap();
    member(&conn, "open", "tr-2");
    receipts::mark_succeeded(&conn, "tr-2", "n.md", false).unwrap();
    member(&conn, "busy", "tr-3");
    seal(&conn, "busy", &["tr-3".to_string()]).unwrap();

    let won = reconcile_sealed(&conn).unwrap();
    assert_eq!(
        won.iter().map(|s| s.batch_id.as_str()).collect::<Vec<_>>(),
        vec!["done"]
    );
    assert!(reconcile_sealed(&conn).unwrap().is_empty(), "claimed once");
}

// ---- backstop -------------------------------------------------------------------

#[test]
fn the_backstop_summarizes_an_idle_unsealed_batch_as_seal_missing() {
    let conn = receipts::open_memory().unwrap();
    for id in ids(2) {
        member(&conn, "b1", &id);
        receipts::mark_succeeded(&conn, &id, "n.md", false).unwrap();
    }
    assert!(
        backstop(&conn, 60, Utc::now()).unwrap().is_empty(),
        "newest member is fresh"
    );
    backdate_batch(&conn, "b1");
    let won = backstop(&conn, 60, Utc::now()).unwrap();
    assert_eq!(won.len(), 1);
    assert!(won[0].seal_missing);
    assert!(
        won[0].text().ends_with("of 2 URL items, seal missing"),
        "{}",
        won[0].text()
    );
    assert!(backstop(&conn, 60, Utc::now()).unwrap().is_empty(), "claimed once");
}

#[test]
fn the_backstop_waits_for_a_received_member() {
    let conn = receipts::open_memory().unwrap();
    member(&conn, "b1", "tr-1");
    member(&conn, "b1", "tr-2");
    receipts::mark_succeeded(&conn, "tr-1", "n.md", false).unwrap();
    backdate_batch(&conn, "b1");
    assert!(backstop(&conn, 60, Utc::now()).unwrap().is_empty());
}

/// A sealed batch belongs to its own drain check: the backstop never claims
/// it, drained or not.
#[test]
fn the_backstop_never_touches_a_sealed_batch() {
    let conn = receipts::open_memory().unwrap();
    member(&conn, "b1", "tr-1");
    member(&conn, "b1", "tr-2");
    seal(&conn, "b1", &ids(3)).unwrap();
    receipts::mark_succeeded(&conn, "tr-1", "n.md", false).unwrap();
    receipts::mark_succeeded(&conn, "tr-2", "n.md", false).unwrap();
    backdate_batch(&conn, "b1");
    assert!(backstop(&conn, 60, Utc::now()).unwrap().is_empty(), "tr-3 still owed");
}

// ---- notifier -------------------------------------------------------------------

#[tokio::test]
async fn deliver_reports_sent_failed_and_disabled_per_sink() {
    let summary = BatchSummary {
        batch_id: "b1".into(),
        succeeded: 1,
        degraded: 0,
        failed: 0,
        crashed: 0,
        total: 1,
        unknown: 0,
        seal_missing: false,
    };
    let ok: Sink = Arc::new(Counting::default());
    let bad: Sink = Arc::new(Counting {
        fail: true,
        ..Counting::default()
    });
    let d = notifier::deliver(&summary, Some(&ok), Some(&bad), false).await;
    assert_eq!((d.desktop, d.telegram), (SinkOutcome::Sent, SinkOutcome::Failed));
    let d = notifier::deliver(&summary, None, Some(&ok), false).await;
    assert_eq!((d.desktop, d.telegram), (SinkOutcome::Disabled, SinkOutcome::Sent));

    let gated = Arc::new(Counting::default());
    let d = notifier::deliver(&summary, Some(&(gated.clone() as Sink)), None, true).await;
    assert_eq!(d.desktop, SinkOutcome::Disabled);
    assert!(gated.texts().is_empty(), "the disable gate sends nothing");
}

// ---- the success criteria -------------------------------------------------------

/// A sealed batch of 6 CLI items, once every item is terminal, puts exactly 1
/// desktop and 1 Telegram message on the summary sinks. A 7th member whose
/// POST response was lost (a row the client never sealed) holds the summary
/// until it too is terminal. The per-item half (0 acks for a batched CLI
/// ingest) is proved through the real `/ingest` route in `server/tests.rs`
/// (`a_batched_cli_ingest_through_the_route_attempts_no_per_item_ping`).
#[tokio::test]
async fn a_sealed_bulk_of_six_sends_one_summary_per_sink() {
    let conn = receipts::open_memory().unwrap();
    let (tx, desktop, telegram, task) = counting_notifier();

    let sealed = ids(6);
    for id in &sealed {
        member(&conn, "bulk", id);
    }
    // The lost response: the daemon wrote the row, the client never saw it.
    member(&conn, "bulk", "tr-lost");
    assert_eq!(seal(&conn, "bulk", &sealed).unwrap(), SealOutcome::Sealed);
    assert_eq!(maybe_summarize(&conn, "bulk").unwrap(), None, "nothing is terminal yet");

    for id in &sealed {
        receipts::mark_succeeded(&conn, id, "n.md", false).unwrap();
        if let Some(s) = after_terminal(&conn, id).unwrap() {
            send(&tx, s);
        }
    }
    assert!(!drained(&conn, "bulk").unwrap(), "tr-lost is still received");

    receipts::mark_failed(&conn, "tr-lost", FailureStage::Crashed, "x").unwrap();
    let summary = after_terminal(&conn, "tr-lost")
        .unwrap()
        .expect("the last member drains it");
    assert_eq!(
        summary.text(),
        "borg bulk bulk: 6 succeeded (0 degraded), 1 failed (1 crashed) of 7 URL items"
    );
    send(&tx, summary);

    drop(tx);
    task.await.unwrap();
    let (d, t) = (desktop.texts(), telegram.texts());
    assert_eq!((d.len(), t.len()), (1, 1), "desktop={d:?} telegram={t:?}");
    assert_eq!((summaries_in(&d), summaries_in(&t)), (1, 1));
}

/// An unrelated unbatched ingest during an open batch leaves the batch alone.
/// After the summary is claimed, a second seal, a late terminal write of a new
/// member, and a backstop tick each send nothing. (That the unrelated ingest
/// keeps its own ack is proved through the real route in `server/tests.rs`.)
#[tokio::test]
async fn an_unbatched_ingest_leaves_the_batch_alone_and_nothing_after_the_claim_sends_again() {
    let conn = receipts::open_memory().unwrap();
    let (tx, desktop, telegram, task) = counting_notifier();
    let forward = |s: Option<BatchSummary>| {
        if let Some(s) = s {
            send(&tx, s);
        }
    };

    for id in ids(2) {
        member(&conn, "bulk", &id);
    }
    seal(&conn, "bulk", &ids(2)).unwrap();

    // The unrelated http ingest: its terminal write touches no batch.
    receipts::record_received(&conn, "ht-1", IngestMethod::Http, ReceiptKind::Url, "u").unwrap();
    receipts::mark_succeeded(&conn, "ht-1", "n.md", false).unwrap();
    forward(after_terminal(&conn, "ht-1").unwrap());

    for id in ids(2) {
        receipts::mark_succeeded(&conn, &id, "n.md", false).unwrap();
        forward(after_terminal(&conn, &id).unwrap());
    }

    // After the claim: a second seal ...
    assert_eq!(seal(&conn, "bulk", &ids(2)).unwrap(), SealOutcome::AlreadySealed);
    forward(maybe_summarize(&conn, "bulk").unwrap());
    // ... a late member's terminal write ...
    member(&conn, "bulk", "tr-late");
    receipts::mark_succeeded(&conn, "tr-late", "n.md", false).unwrap();
    forward(after_terminal(&conn, "tr-late").unwrap());
    // ... and a backstop tick on the sealed batch.
    backdate_batch(&conn, "bulk");
    for s in backstop(&conn, 60, Utc::now()).unwrap() {
        forward(Some(s));
    }

    drop(tx);
    task.await.unwrap();
    let (d, t) = (desktop.texts(), telegram.texts());
    assert_eq!((d.len(), t.len()), (1, 1), "desktop={d:?} telegram={t:?}");
    assert_eq!((summaries_in(&d), summaries_in(&t)), (1, 1));
}

// ---- schema v9 ------------------------------------------------------------------

/// A v8 DB (no `batch_id`, no `batches`) gains both on open, keeps its rows
/// unbatched, and indexes `batch_id`.
#[test]
fn v9_migration_adds_batch_id_its_index_and_the_batches_table() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("receipts.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE receipts (
               trace_id TEXT NOT NULL PRIMARY KEY, received_at TEXT NOT NULL, method TEXT NOT NULL,
               kind TEXT NOT NULL CHECK (kind IN ('url', 'text', 'binary', 'session')),
               raw_input TEXT NOT NULL,
               status TEXT NOT NULL CHECK (status IN ('received', 'succeeded', 'failed', 'rejected')),
               terminal_at TEXT, note_path TEXT, failure_stage TEXT, failure_reason TEXT, replay_of TEXT,
               degraded INTEGER NOT NULL DEFAULT 0, lease_owner_pid INTEGER DEFAULT NULL,
               lease_until TEXT DEFAULT NULL, started_at TEXT DEFAULT NULL,
               replay_origin TEXT DEFAULT NULL, scratch INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE schema_version (version INTEGER NOT NULL PRIMARY KEY, applied_at TEXT NOT NULL);
             INSERT INTO schema_version (version, applied_at) VALUES (8, '2026-10-25T00:00:00Z');
             INSERT INTO receipts (trace_id, received_at, method, kind, raw_input, status)
               VALUES ('v8-a', '2026-10-25T00:00:00Z', 'cli', 'url', 'https://a.example', 'succeeded');",
        )
        .unwrap();
    }
    let conn = receipts::open_at(&path).unwrap();
    assert_eq!(batch_of(&conn, "v8-a").unwrap(), None);
    let index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_receipts_batch_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(index, 1);
    create_if_absent(&conn, "b1").unwrap();
    let v: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 9);
    drop(conn);
    receipts::open_at(&path).expect("a second open is idempotent");
}
