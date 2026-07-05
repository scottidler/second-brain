use super::*;

#[test]
fn empty_allowlist_denies_all() {
    // Fail-closed: the serde default (empty list) must deny every chat, not
    // accept everyone (the previous fail-open behavior).
    assert!(!chat_allowed(&[], 12345));
    assert!(!chat_allowed(&[], -100200300));
    assert!(!chat_allowed(&[], 0));
}

#[test]
fn populated_allowlist_admits_only_listed() {
    let allowed = [111_i64, 222];
    assert!(chat_allowed(&allowed, 111));
    assert!(chat_allowed(&allowed, 222));
    assert!(!chat_allowed(&allowed, 333));
}

#[test]
fn telegram_prose_and_url_captures_note() {
    // Phase 8 (telegram transport capture-note fixture): the telegram text
    // handler builds its URL content via `router::url_content_from_text`, so a
    // prose+URL message lands the prose as the capture note.
    let (content, display) =
        crate::router::url_content_from_text("fixes borg's linker: https://example.com/post").expect("url present");
    assert_eq!(display, "https://example.com/post");
    match content {
        crate::types::ContentKind::Url { url, note } => {
            assert_eq!(url, "https://example.com/post");
            assert_eq!(note.as_deref(), Some("fixes borg's linker:"));
        }
        other => panic!("expected Url, got {other:?}"),
    }
}
