use super::*;

#[test]
fn redirect_body_is_a_single_wikilink_line() {
    assert_eq!(redirect_body("some-survivor"), "Merged into [[some-survivor]].\n");
}

/// Pins the contract itself. Both `cortex::association::tombstone_content`
/// and `borg::dedupe::apply_group` build their output from these values, so
/// changing one here changes both writers at once - which is the point.
/// If you are here because this failed, you are changing the tombstone
/// dialect for every already-retired note in the vault.
#[test]
fn the_tombstone_contract_is_pinned() {
    assert_eq!(SUPERSEDED_BY_KEY, "superseded-by");
    assert_eq!(SLUG_KEY, "slug");
    assert_eq!(redirect_body("x"), "Merged into [[x]].\n");
}
