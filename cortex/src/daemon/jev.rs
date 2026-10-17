//! The daemon-start jev key check (`validate_daemon_start`).

use super::*;
use crate::config::Config;

/// An empty jev key WARNs at daemon start and the daemon still starts: the
/// other classifier slot still tags notes.
#[test]
fn daemon_start_warns_and_starts_when_jev_key_is_empty() {
    ::vault::capture::install();
    for (marker, classifier, fallback) in [
        (
            "SB_TEST_CORTEX_JEV_PRIMARY_KEY",
            distillers::tags::ClassifierKind::Jev,
            distillers::tags::ClassifierKind::Deterministic,
        ),
        (
            "SB_TEST_CORTEX_JEV_FALLBACK_KEY",
            distillers::tags::ClassifierKind::Deterministic,
            distillers::tags::ClassifierKind::Jev,
        ),
    ] {
        let mut config = Config::default();
        config.tags.classifier.classifier = classifier;
        config.tags.classifier.fallback = fallback;
        config.tags.classifier.api_key = marker.to_string();
        validate_daemon_start(&config).unwrap_or_else(|e| panic!("an empty jev key must not stop the daemon: {e}"));
        assert_eq!(::vault::capture::warns_containing(marker).len(), 1, "{marker}");
    }
}

#[test]
fn daemon_start_is_quiet_when_jev_is_unused() {
    ::vault::capture::install();
    let mut config = Config::default();
    config.tags.classifier.api_key = "SB_TEST_CORTEX_JEV_UNUSED_KEY".to_string();
    validate_daemon_start(&config).expect("starts");
    assert!(::vault::capture::warns_containing("SB_TEST_CORTEX_JEV_UNUSED_KEY").is_empty());
}
