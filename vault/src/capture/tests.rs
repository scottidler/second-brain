use super::*;

#[test]
fn captures_a_warn_and_filters_by_needle() {
    install();
    log::warn!("capture-test-needle-xyz happened");
    log::info!("capture-test-needle-xyz info is below the cutoff");
    let hits = warns_containing("capture-test-needle-xyz");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(warns_containing("capture-test-no-such-line").is_empty());
}
