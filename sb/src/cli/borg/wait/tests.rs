use super::*;
use vault::queue::{BatchSummary, QueueItem};

fn batch(failed: u64, wedged: u64, remaining: u64) -> BatchSummary {
    BatchSummary {
        id: "20261004-215943-9f01".to_string(),
        started: "2026-10-04T21:59:43Z".to_string(),
        elapsed_secs: 192,
        total: 6,
        done: 6 - remaining,
        remaining,
        succeeded: 6 - remaining - failed,
        failed,
        queued: 0,
        processing: remaining - wedged,
        wedged,
    }
}

fn item(state: ItemState) -> QueueItem {
    QueueItem {
        trace: format!("trace-{state:?}"),
        state,
        source: "https://www.youtube.com/watch?v=x".to_string(),
        received: None,
        started: None,
        age_secs: None,
        failure_stage: None,
        failure_reason: None,
    }
}

fn snap(state: QueueState, batch: Option<BatchSummary>, items: Vec<QueueItem>) -> QueueSnapshot {
    QueueSnapshot {
        state,
        batch,
        items: (!items.is_empty()).then_some(items),
    }
}

#[test]
fn decide_plain_idle_is_zero() {
    assert_eq!(decide(&QueueSnapshot::idle(), false), Verdict::Exit(0));
    assert_eq!(decide(&QueueSnapshot::idle(), true), Verdict::Exit(0));
}

#[test]
fn decide_pinned_batch_drained_clean_is_zero() {
    let s = snap(QueueState::Idle, Some(batch(0, 0, 0)), vec![]);
    assert_eq!(decide(&s, false), Verdict::Exit(0));
}

#[test]
fn decide_pinned_batch_drained_with_failures_is_three() {
    let s = snap(QueueState::Idle, Some(batch(1, 0, 0)), vec![item(ItemState::Failed)]);
    assert_eq!(decide(&s, false), Verdict::Exit(EXIT_FAILED));
    assert_eq!(decide(&s, true), Verdict::Exit(EXIT_FAILED));
}

#[test]
fn decide_wedged_item_is_four_even_with_failures_or_deadline() {
    let s = snap(
        QueueState::Draining,
        Some(batch(1, 1, 2)),
        vec![
            item(ItemState::Processing),
            item(ItemState::Wedged),
            item(ItemState::Failed),
        ],
    );
    assert_eq!(decide(&s, false), Verdict::Exit(EXIT_WEDGED));
    assert_eq!(decide(&s, true), Verdict::Exit(EXIT_WEDGED));
}

#[test]
fn decide_draining_past_deadline_is_five() {
    let s = snap(
        QueueState::Draining,
        Some(batch(0, 0, 3)),
        vec![item(ItemState::Processing)],
    );
    assert_eq!(decide(&s, true), Verdict::Exit(EXIT_TIMEOUT));
}

#[test]
fn decide_draining_before_deadline_keeps_waiting() {
    let s = snap(
        QueueState::Draining,
        Some(batch(1, 0, 3)),
        vec![
            item(ItemState::Queued),
            item(ItemState::Processing),
            item(ItemState::Failed),
        ],
    );
    assert_eq!(decide(&s, false), Verdict::KeepWaiting);
}
