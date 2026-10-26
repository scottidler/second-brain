//! `PublishPolicy`'s per-run answers: the effective config, the refusal,
//! and the inflight key.

use super::*;

#[test]
fn effective_config_passes_vault_through_and_points_scratch_staging_under_root() {
    let mut config = crate::config::Config::default();
    config.staging.enabled = false;
    config.staging.root = PathBuf::from("/shared/stages");
    config.staging.layout = crate::config::StagingLayout::PerStage;

    let vault = PublishPolicy::Vault.effective_config(&config);
    assert!(matches!(vault, std::borrow::Cow::Borrowed(_)), "Vault does not clone");
    assert_eq!(vault.staging.root, PathBuf::from("/shared/stages"));

    let scratch = PublishPolicy::Scratch {
        root: PathBuf::from("/distill/cl-1"),
    };
    let effective = scratch.effective_config(&config);
    assert_eq!(effective.staging.root, PathBuf::from("/distill/cl-1/stages"));
    assert!(effective.staging.enabled);
    assert_eq!(effective.staging.layout, crate::config::StagingLayout::PerTrace);
    assert_eq!(effective.staging.retention_days, config.staging.retention_days);
    assert_eq!(
        config.staging.root,
        PathBuf::from("/shared/stages"),
        "the live config is untouched"
    );
}

#[test]
fn only_scratch_refuses_and_only_non_urls() {
    let scratch = PublishPolicy::Scratch {
        root: PathBuf::from("/distill/cl-1"),
    };
    let url = ContentKind::Url {
        url: "https://example.com/a".to_string(),
        note: None,
    };
    let text = ContentKind::Text("hello".to_string());
    assert_eq!(PublishPolicy::Vault.refusal(&text), None);
    assert_eq!(PublishPolicy::Vault.refusal(&url), None);
    assert_eq!(scratch.refusal(&url), None);
    let reason = scratch.refusal(&text).expect("a text input is refused");
    assert!(reason.contains("URL only"), "{reason}");
}

#[test]
fn inflight_key_splits_scratch_into_its_own_namespace() {
    let canonical = "https://example.com/a";
    let scratch = PublishPolicy::Scratch {
        root: PathBuf::from("/distill/cl-1"),
    };
    assert_eq!(PublishPolicy::Vault.inflight_key(canonical), canonical);
    assert_eq!(scratch.inflight_key(canonical), "distill:https://example.com/a");
}
