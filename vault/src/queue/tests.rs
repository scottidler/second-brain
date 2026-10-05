use super::*;

#[test]
fn idle_serializes_to_exactly_state_idle() {
    let json = serde_json::to_string(&QueueSnapshot::idle()).expect("json");
    assert_eq!(json, r#"{"state":"idle"}"#);
    let yaml = serde_yaml::to_string(&QueueSnapshot::idle()).expect("yaml");
    assert_eq!(yaml.trim(), "state: idle");
}

fn sample() -> QueueSnapshot {
    QueueSnapshot {
        state: QueueState::Draining,
        batch: Some(BatchSummary {
            id: "20261004-215943-9f01".into(),
            started: "2026-10-04T21:59:43Z".into(),
            elapsed_secs: 192,
            total: 2,
            done: 1,
            remaining: 1,
            succeeded: 0,
            failed: 1,
            queued: 0,
            processing: 1,
            wedged: 0,
        }),
        items: Some(vec![
            QueueItem {
                trace: "20261004-215950-ab12".into(),
                state: ItemState::Processing,
                source: "https://www.youtube.com/watch?v=x".into(),
                received: Some("2026-10-04T21:59:50Z".into()),
                started: Some("2026-10-04T22:01:02Z".into()),
                age_secs: Some(113),
                failure_stage: None,
                failure_reason: None,
            },
            QueueItem {
                trace: "20261004-215944-cd34".into(),
                state: ItemState::Failed,
                source: "https://www.youtube.com/watch?v=y".into(),
                received: None,
                started: None,
                age_secs: None,
                failure_stage: Some(FailureStage::FetchFailed),
                failure_reason: Some("yt-dlp HTTP 403".into()),
            },
        ]),
    }
}

#[test]
fn draining_wire_shape_is_kebab_case_and_skips_absent_item_fields() {
    let v = serde_json::to_value(sample()).expect("json");
    assert_eq!(v["state"], "draining");
    assert_eq!(v["batch"]["elapsed-secs"], 192);
    assert_eq!(v["items"][0]["state"], "processing");
    assert_eq!(v["items"][0]["age-secs"], 113);
    assert!(v["items"][0].get("failure-stage").is_none());
    assert_eq!(v["items"][1]["failure-stage"], "fetch-failed");
    assert_eq!(v["items"][1]["failure-reason"], "yt-dlp HTTP 403");
    assert!(v["items"][1].get("received").is_none());
    assert!(v["items"][1].get("age-secs").is_none());
}

#[test]
fn snapshot_round_trips_through_json() {
    let s = sample();
    let json = serde_json::to_string(&s).expect("json");
    let back: QueueSnapshot = serde_json::from_str(&json).expect("parse");
    assert_eq!(back, s);
    let idle: QueueSnapshot = serde_json::from_str(r#"{"state":"idle"}"#).expect("parse idle");
    assert_eq!(idle, QueueSnapshot::idle());
}

#[test]
fn unknown_state_is_a_parse_error() {
    assert!(serde_json::from_str::<QueueSnapshot>(r#"{"state":"busy"}"#).is_err());
}
