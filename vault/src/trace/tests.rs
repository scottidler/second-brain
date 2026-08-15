use super::*;

#[test]
fn test_generate_format() {
    let id = generate(Method::Telegram);
    let re = regex::Regex::new(r"^[a-z]{2}-[0-9a-f]{8}$").expect("valid regex");
    assert!(re.is_match(&id), "trace ID '{id}' does not match expected format");
}

/// Widening the trace field from 24 to 32 bits (design doc
/// `2026-08-15-harvest-note-identity-trace-keyed-replace.md`, Phase 1) must
/// not retroactively invalidate any already-minted 6-hex-char trace id: the
/// two widths coexist in the vault as opaque strings (receipts TEXT key,
/// exact-equality lookups, no fixed-width parsing anywhere but this test file
/// itself, per the doc's "exactly ONE length assumption" audit). This asserts
/// a pre-widening-shaped id still matches the general trace-id shape a
/// consumer would use to sanity-check one (prefix + lowercase hex), just not
/// the new-mint-only 8-char regex above.
#[test]
fn test_legacy_six_hex_trace_id_still_matches_general_shape() {
    let legacy_id = "hv-e5d240";
    let re = regex::Regex::new(r"^[a-z]{2}-[0-9a-f]+$").expect("valid regex");
    assert!(
        re.is_match(legacy_id),
        "legacy 6-hex trace id '{legacy_id}' should still match the general opaque-string shape"
    );
    assert_ne!(
        legacy_id.split('-').nth(1).expect("hex part").len(),
        8,
        "sanity: this fixture is deliberately the OLD 6-char width"
    );
}

#[test]
fn test_method_prefixes() {
    assert_eq!(method_prefix(Method::Telegram), "tg");
    assert_eq!(method_prefix(Method::Discord), "dc");
    assert_eq!(method_prefix(Method::Http), "ht");
    assert_eq!(method_prefix(Method::Clipboard), "cb");
    assert_eq!(method_prefix(Method::Cli), "cl");
    assert_eq!(method_prefix(Method::Ntfy), "nf");
    assert_eq!(method_prefix(Method::Signal), "sg");
    assert_eq!(method_prefix(Method::Manual), "mn");
    assert_eq!(method_prefix(Method::Harvest), "hv");
}

#[test]
fn test_sequential_uniqueness() {
    let id1 = generate(Method::Cli);
    let id2 = generate(Method::Cli);
    assert_ne!(id1, id2, "two sequential trace IDs should differ");
}

#[test]
fn test_different_methods_different_prefix() {
    let tg = generate(Method::Telegram);
    let dc = generate(Method::Discord);
    assert!(tg.starts_with("tg-"));
    assert!(dc.starts_with("dc-"));
}
