use super::*;
use chrono::Duration;
use vault::intake::IntakeKind;

fn make_intake_row(trace: &str, ago: Duration) -> ParsedIntakeRow {
    let when = Local::now() - ago;
    ParsedIntakeRow {
        date: when.format("%Y-%m-%d").to_string(),
        time: when.format("%H:%M").to_string(),
        method: "telegram".to_string(),
        origin_ctx: "chat-1".to_string(),
        kind: IntakeKind::Url.as_str().to_string(),
        preview: "https://example.com".to_string(),
        trace_id: trace.to_string(),
    }
}

#[test]
fn intake_age_secs_handles_past_timestamps() {
    let row = make_intake_row("tg-aaaaaa", Duration::seconds(3600));
    let age = intake_age_secs(&row).expect("parses");
    assert!((3500..=3700).contains(&age), "expected ~1 hour, got {age}s");
}

#[test]
fn intake_age_secs_returns_none_for_bogus_timestamps() {
    let row = ParsedIntakeRow {
        date: "garbage".to_string(),
        time: "nope".to_string(),
        method: "telegram".to_string(),
        origin_ctx: "x".to_string(),
        kind: "url".to_string(),
        preview: "x".to_string(),
        trace_id: "tg-xxxxxx".to_string(),
    };
    assert!(intake_age_secs(&row).is_none());
}
