use super::*;
use tempfile::tempdir;

fn entry(trace: &str, kind: IntakeKind, preview: &str) -> IntakeEntry {
    IntakeEntry {
        date: "2026-05-11".to_string(),
        time: "19:07".to_string(),
        method: Method::Telegram,
        origin_ctx: "chat-1".to_string(),
        kind,
        preview: preview.to_string(),
        trace_id: trace.to_string(),
    }
}

#[test]
fn ensure_intake_exists_creates_file_with_header() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("borg-intake.md");
    ensure_intake_exists(&path).expect("create");
    assert!(path.exists());
    let content = fs::read_to_string(&path).expect("read");
    assert!(content.contains("# Borg Intake"));
    assert!(content.contains("| Date | Time | Method | Origin | Kind | Preview | Trace |"));
}

#[test]
fn ensure_intake_exists_is_idempotent() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("borg-intake.md");
    ensure_intake_exists(&path).expect("first");
    let first = fs::read_to_string(&path).expect("read");
    ensure_intake_exists(&path).expect("second");
    let second = fs::read_to_string(&path).expect("read");
    assert_eq!(first, second);
}

#[test]
fn append_entry_writes_row_at_top() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("borg-intake.md");

    append_entry(&path, &entry("tg-aaaaaa", IntakeKind::Url, "https://example.com")).expect("append");
    append_entry(&path, &entry("tg-bbbbbb", IntakeKind::Text, "hello world")).expect("append");

    let content = fs::read_to_string(&path).expect("read");
    let data_lines: Vec<&str> = content
        .lines()
        .filter(|l| l.starts_with('|') && l.contains("tg-"))
        .collect();
    assert_eq!(data_lines.len(), 2);
    assert!(data_lines[0].contains("tg-bbbbbb"), "newest first: {}", data_lines[0]);
    assert!(data_lines[1].contains("tg-aaaaaa"));
}

#[test]
fn parse_entries_recovers_via_column_names() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("borg-intake.md");
    append_entry(&path, &entry("tg-aaaaaa", IntakeKind::Url, "https://example.com")).expect("append");
    append_entry(
        &path,
        &entry("tg-bbbbbb", IntakeKind::Sticker, "[sticker: party-parrot]"),
    )
    .expect("append");

    let rows = parse_entries(&path).expect("parse");
    assert_eq!(rows.len(), 2);
    let by_trace: std::collections::HashMap<_, _> = rows.iter().map(|r| (r.trace_id.as_str(), r)).collect();
    let aaa = by_trace.get("tg-aaaaaa").expect("aaa present");
    assert_eq!(aaa.kind, "url");
    assert_eq!(aaa.preview, "https://example.com");
    assert_eq!(aaa.method, "telegram");
    let bbb = by_trace.get("tg-bbbbbb").expect("bbb present");
    assert_eq!(bbb.kind, "sticker");
}

#[test]
fn find_by_trace_returns_correct_row() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("borg-intake.md");
    append_entry(&path, &entry("tg-aaaaaa", IntakeKind::Url, "https://x")).expect("a");
    append_entry(&path, &entry("tg-bbbbbb", IntakeKind::Text, "hello")).expect("b");

    let found = find_by_trace(&path, "tg-aaaaaa").expect("find");
    assert!(found.is_some());
    assert_eq!(found.expect("present").preview, "https://x");

    let missing = find_by_trace(&path, "tg-nope000").expect("find");
    assert!(missing.is_none());
}

#[test]
fn preview_with_pipes_round_trips() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("borg-intake.md");
    append_entry(&path, &entry("tg-aaaaaa", IntakeKind::Text, "a | b | c")).expect("append");

    let rows = parse_entries(&path).expect("parse");
    assert_eq!(rows[0].preview, "a | b | c");
}

#[test]
fn write_raw_input_creates_sidecar() {
    let dir = tempdir().expect("tempdir");
    let trace = "tg-aaaaaa";
    write_raw_input(dir.path(), trace, b"hello world").expect("write");
    let path = raw_input_path(dir.path(), trace);
    assert!(path.exists());
    let body = fs::read_to_string(&path).expect("read");
    assert_eq!(body, "hello world");
}

#[test]
fn intake_kind_round_trip() {
    for k in [
        IntakeKind::Url,
        IntakeKind::Text,
        IntakeKind::Photo,
        IntakeKind::Voice,
        IntakeKind::Audio,
        IntakeKind::Document,
        IntakeKind::Sticker,
        IntakeKind::Video,
        IntakeKind::Animation,
        IntakeKind::Poll,
        IntakeKind::Location,
        IntakeKind::Contact,
        IntakeKind::Empty,
        IntakeKind::Unknown,
    ] {
        let parsed: IntakeKind = k.as_str().parse().expect("parse");
        assert_eq!(k, parsed);
    }
}

#[test]
fn header_drift_repair_restores_canonical() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("borg-intake.md");
    let drifted = "---\ntitle: Borg Intake\n---\n\n# Borg Intake\n\n\
        | Date | Time | Method | Origin | Foo | Preview | Trace |\n\
        |------|------|--------|--------|-----|---------|-------|\n";
    fs::write(&path, drifted).expect("write");

    append_entry(&path, &entry("tg-aaaaaa", IntakeKind::Url, "https://x")).expect("append");

    let after = fs::read_to_string(&path).expect("read");
    assert!(after.contains("| Date | Time | Method | Origin | Kind | Preview | Trace |"));
    assert!(!after.contains(" Foo "));
}
