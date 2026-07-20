use super::*;

#[test]
fn failure_stage_roundtrip() {
    for stage in FailureStage::all() {
        let s = stage.as_str();
        let parsed: FailureStage = s.parse().expect("FailureStage parse roundtrip");
        assert_eq!(*stage, parsed, "roundtrip mismatch for {s}");
    }
}

#[test]
fn failure_stage_all_seven_variants() {
    assert_eq!(FailureStage::all().len(), 7);
}

#[test]
fn failure_stage_display_matches_as_str() {
    for stage in FailureStage::all() {
        assert_eq!(format!("{stage}"), stage.as_str());
    }
}

#[test]
fn failure_stage_parse_rejects_legacy_watchdog_orphan() {
    assert!("watchdog-orphan".parse::<FailureStage>().is_err());
}

#[test]
fn failure_stage_parse_is_case_insensitive() {
    assert_eq!(
        "FETCH-FAILED".parse::<FailureStage>().expect("upper case"),
        FailureStage::FetchFailed
    );
    assert_eq!(
        "Crashed".parse::<FailureStage>().expect("mixed case"),
        FailureStage::Crashed
    );
}

#[test]
fn receipt_kind_roundtrip() {
    for kind in [
        ReceiptKind::Url,
        ReceiptKind::Text,
        ReceiptKind::Binary,
        ReceiptKind::Session,
    ] {
        let parsed: ReceiptKind = kind.as_str().parse().expect("ReceiptKind parse roundtrip");
        assert_eq!(kind, parsed);
    }
}

#[test]
fn receipt_status_roundtrip() {
    for status in [
        ReceiptStatus::Received,
        ReceiptStatus::Succeeded,
        ReceiptStatus::Failed,
        ReceiptStatus::Rejected,
    ] {
        let parsed: ReceiptStatus = status.as_str().parse().expect("ReceiptStatus parse roundtrip");
        assert_eq!(status, parsed);
    }
}

#[test]
fn receipt_kind_session_as_str() {
    assert_eq!(ReceiptKind::Session.as_str(), "session");
}

#[test]
fn receipt_status_rejected_as_str() {
    assert_eq!(ReceiptStatus::Rejected.as_str(), "rejected");
}

#[test]
fn receipts_db_path_lives_under_sb_borg() {
    let path = receipts_db_path().expect("data_local_dir resolves in test env");
    let s = path.to_string_lossy();
    assert!(s.ends_with("sb/borg/receipts.db"), "got {s}");
}

#[test]
fn receipts_dir_is_parent_of_db_path() {
    let dir = receipts_dir().expect("dir resolves");
    let path = receipts_db_path().expect("db path resolves");
    assert_eq!(path.parent().expect("db path has parent"), dir.as_path());
}
