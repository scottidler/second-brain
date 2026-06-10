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
