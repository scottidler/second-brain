#![allow(clippy::unwrap_used)]
use super::*;

const TEMPLATE: &str =
    "---\ntype: glean\n---\n\n# T\n\n<!-- glean:fencepost-start -->\nNEW BODY\n<!-- glean:fencepost-end -->\n";

#[test]
fn merges_preserves_existing_preamble_and_postamble() {
    let existing = "---\ntype: glean\ncontent-hash: abc\n---\n\nOPERATOR PREAMBLE TEXT\n\n<!-- glean:fencepost-start -->\nOLD BODY\n<!-- glean:fencepost-end -->\n\n## Operator Postamble\n\nNotes here.\n";
    let merged = merge(existing, TEMPLATE);
    assert!(merged.contains("OPERATOR PREAMBLE TEXT"));
    assert!(merged.contains("NEW BODY"));
    assert!(merged.contains("Operator Postamble"));
    assert!(merged.contains("Notes here"));
    assert!(!merged.contains("OLD BODY"));
}

#[test]
fn merge_with_no_existing_fenceposts_appends_body() {
    let existing = "raw operator text without any markers\n";
    let merged = merge(existing, TEMPLATE);
    assert!(merged.starts_with("raw operator text"));
    assert!(merged.contains("NEW BODY"));
    assert!(merged.contains(FENCEPOST_START));
    assert!(merged.contains(FENCEPOST_END));
}

#[test]
fn merge_returns_template_verbatim_when_template_has_no_fenceposts() {
    let bad_template = "no fenceposts at all\n";
    let existing = "---\ntype: glean\n---\n\n<!-- glean:fencepost-start -->\nold\n<!-- glean:fencepost-end -->\n";
    let merged = merge(existing, bad_template);
    assert_eq!(merged, bad_template);
}

#[test]
fn round_trip_with_self_is_byte_identical() {
    let merged = merge(TEMPLATE, TEMPLATE);
    assert_eq!(merged, TEMPLATE);
}
