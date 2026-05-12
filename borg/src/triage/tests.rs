use super::*;
use chrono::Duration;
use vault::intake::IntakeKind;

fn row(date: &str, time: &str, trace: &str) -> ParsedIntakeRow {
    ParsedIntakeRow {
        date: date.to_string(),
        time: time.to_string(),
        method: "telegram".to_string(),
        origin_ctx: "chat-1".to_string(),
        kind: IntakeKind::Url.as_str().to_string(),
        preview: "https://x".to_string(),
        trace_id: trace.to_string(),
    }
}

#[test]
fn intake_age_secs_returns_positive_for_past() {
    let past = Local::now() - Duration::hours(2);
    let r = row(
        &past.format("%Y-%m-%d").to_string(),
        &past.format("%H:%M").to_string(),
        "tg-aaaaaa",
    );
    let age = intake_age_secs(&r).expect("parseable");
    // 2 hours +/- a bit (because seconds were dropped).
    assert!((7100..=7300).contains(&age), "got {age}s");
}

#[test]
fn intake_age_secs_returns_none_for_garbage() {
    let r = row("not-a-date", "nope", "tg-aaaaaa");
    assert!(intake_age_secs(&r).is_none());
}

#[test]
fn write_orphans_md_emits_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orphans.md");
    let rows = [row("2026-05-11", "19:07", "tg-aaaaaa")];
    let refs: Vec<&ParsedIntakeRow> = rows.iter().collect();
    write_orphans_md(&path, &refs).expect("write");
    let content = std::fs::read_to_string(&path).expect("read");
    assert!(content.contains("# Borg Orphans"));
    assert!(content.contains("tg-aaaaaa"));
    assert!(content.contains("| Date | Time | Method | Origin | Kind | Preview | Trace |"));
}
