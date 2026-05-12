use super::*;
use tempfile::tempdir;

fn entry(trace: &str, stage: DlqStage) -> DlqEntry {
    DlqEntry {
        date: "2026-05-11".to_string(),
        time: "19:07".to_string(),
        method: Method::Telegram,
        stage,
        reason: "test".to_string(),
        preview: "[sticker: party-parrot]".to_string(),
        retries: 0,
        status: DlqStatus::Pending,
        trace_id: trace.to_string(),
        replay_of: None,
    }
}

#[test]
fn ensure_dlq_exists_creates_file() {
    let dir = tempdir().expect("tempdir");
    let path = dlq_path(dir.path());
    ensure_dlq_exists(&path).expect("create");
    assert!(path.exists());
    let content = fs::read_to_string(&path).expect("read");
    assert!(content.contains("# Borg Dead Letter Queue"));
    assert!(content.contains("Replay-Of"));
}

#[test]
fn append_and_parse_round_trip() {
    let dir = tempdir().expect("tempdir");
    let path = dlq_path(dir.path());
    let mut e = entry("tg-aaaaaa", DlqStage::IntakeReject);
    e.reason = "unsupported media: sticker".to_string();
    append_entry(&path, &e).expect("append");

    let rows = parse_entries(&path).expect("parse");
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.trace_id, "tg-aaaaaa");
    assert_eq!(r.stage, "intake-reject");
    assert_eq!(r.reason, "unsupported media: sticker");
    assert_eq!(r.method, "telegram");
    assert_eq!(r.status, "pending");
    assert_eq!(r.retries, 0);
    assert!(r.replay_of.is_none());
}

#[test]
fn newest_first_ordering() {
    let dir = tempdir().expect("tempdir");
    let path = dlq_path(dir.path());
    append_entry(&path, &entry("tg-aaaaaa", DlqStage::IntakeReject)).expect("a");
    append_entry(&path, &entry("tg-bbbbbb", DlqStage::FetchFailed)).expect("b");

    let content = fs::read_to_string(&path).expect("read");
    let data_lines: Vec<&str> = content
        .lines()
        .filter(|l| l.starts_with('|') && l.contains("tg-"))
        .collect();
    assert!(data_lines[0].contains("tg-bbbbbb"));
    assert!(data_lines[1].contains("tg-aaaaaa"));
}

#[test]
fn replay_of_column_round_trips() {
    let dir = tempdir().expect("tempdir");
    let path = dlq_path(dir.path());
    let mut e = entry("tg-cccccc", DlqStage::FetchFailed);
    e.replay_of = Some("tg-aaaaaa".to_string());
    append_entry(&path, &e).expect("append");

    let rows = parse_entries(&path).expect("parse");
    assert_eq!(rows[0].replay_of.as_deref(), Some("tg-aaaaaa"));
}

#[test]
fn update_status_transitions_pending_to_resolved() {
    let dir = tempdir().expect("tempdir");
    let path = dlq_path(dir.path());
    append_entry(&path, &entry("tg-aaaaaa", DlqStage::IntakeReject)).expect("a");

    let changed = update_status(&path, "tg-aaaaaa", DlqStatus::Resolved).expect("update");
    assert!(changed);

    let rows = parse_entries(&path).expect("parse");
    assert_eq!(rows[0].status, "resolved");
}

#[test]
fn update_status_returns_false_for_missing_trace() {
    let dir = tempdir().expect("tempdir");
    let path = dlq_path(dir.path());
    append_entry(&path, &entry("tg-aaaaaa", DlqStage::IntakeReject)).expect("a");

    let changed = update_status(&path, "tg-nope000", DlqStatus::Resolved).expect("update");
    assert!(!changed);
}

#[test]
fn find_by_trace_returns_the_row() {
    let dir = tempdir().expect("tempdir");
    let path = dlq_path(dir.path());
    append_entry(&path, &entry("tg-aaaaaa", DlqStage::QualityBlocked)).expect("a");

    let found = find_by_trace(&path, "tg-aaaaaa").expect("find");
    assert_eq!(found.expect("present").stage, "quality-blocked");
}

#[test]
fn stage_round_trip() {
    for s in [
        DlqStage::IntakeReject,
        DlqStage::ClassifyFailed,
        DlqStage::FetchFailed,
        DlqStage::QualityBlocked,
        DlqStage::PipelineTimedOut,
        DlqStage::PublishFailed,
        DlqStage::WatchdogOrphan,
    ] {
        let parsed: DlqStage = s.as_str().parse().expect("parse");
        assert_eq!(s, parsed);
    }
}

#[test]
fn status_round_trip() {
    for s in [
        DlqStatus::Pending,
        DlqStatus::Retried,
        DlqStatus::Abandoned,
        DlqStatus::Resolved,
    ] {
        let parsed: DlqStatus = s.as_str().parse().expect("parse");
        assert_eq!(s, parsed);
    }
}

#[test]
fn reason_with_pipes_is_preserved() {
    let dir = tempdir().expect("tempdir");
    let path = dlq_path(dir.path());
    let mut e = entry("tg-aaaaaa", DlqStage::FetchFailed);
    e.reason = "yt-dlp returned: code=1 | stderr=...".to_string();
    append_entry(&path, &e).expect("append");

    let rows = parse_entries(&path).expect("parse");
    assert_eq!(rows[0].reason, "yt-dlp returned: code=1 | stderr=...");
}
