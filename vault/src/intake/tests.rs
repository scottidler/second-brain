use super::*;
use tempfile::tempdir;

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
fn raw_input_path_is_under_system_intake() {
    let dir = tempdir().expect("tempdir");
    let path = raw_input_path(dir.path(), "tg-aaaaaa");
    assert!(path.ends_with("system/intake/tg-aaaaaa.txt"));
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
