use super::*;
use crate::config::Config;

fn documented_defaults(cfg: &ImagesConfig) {
    assert!(cfg.enabled);
    assert_eq!(cfg.max_candidates, 40);
    assert_eq!(cfg.max_download_bytes, 20_971_520);
    assert_eq!(cfg.max_per_note, 20);
    assert_eq!(cfg.max_section_bytes, 1500);
    assert_eq!(cfg.min_side_px, 200);
    assert_eq!(cfg.max_bytes, 2_097_152);
    assert_eq!(cfg.max_side_px, 2048);
    let cf = &cfg.content_filter;
    assert!(cf.enabled);
    assert_eq!(
        cf.keep,
        vec![
            SlideCategory::ArchitectureDiagram,
            SlideCategory::SequenceDiagram,
            SlideCategory::Flowchart,
            SlideCategory::Chart,
            SlideCategory::Infographic,
        ]
    );
    assert_eq!(cf.model, "");
    assert!((cf.min_confidence - 0.6).abs() < f32::EPSILON);
}

#[test]
fn omitted_images_block_yields_documented_defaults() {
    let config: Config = serde_yaml::from_str("{}").unwrap();
    documented_defaults(&config.images);
}

#[test]
fn images_block_with_only_enabled_yields_documented_defaults() {
    let config: Config = serde_yaml::from_str("images:\n  enabled: true\n").unwrap();
    documented_defaults(&config.images);
}

#[test]
fn partial_content_filter_keeps_the_other_defaults() {
    let config: Config = serde_yaml::from_str("images:\n  content-filter:\n    min-confidence: 0.8\n").unwrap();
    let cf = &config.images.content_filter;
    assert!((cf.min_confidence - 0.8).abs() < f32::EPSILON);
    assert!(cf.enabled);
    assert_eq!(cf.keep.len(), 5);
}

#[test]
fn explicit_values_override() {
    let yaml = "images:\n  enabled: false\n  max-per-note: 3\n  content-filter:\n    keep: [code, terminal]\n";
    let config: Config = serde_yaml::from_str(yaml).unwrap();
    assert!(!config.images.enabled);
    assert_eq!(config.images.max_per_note, 3);
    assert_eq!(
        config.images.content_filter.keep,
        vec![SlideCategory::Code, SlideCategory::Terminal]
    );
}

#[test]
fn unknown_keep_category_fails_config_load() {
    let err = serde_yaml::from_str::<Config>("images:\n  content-filter:\n    keep: [diagram]\n").unwrap_err();
    assert!(err.to_string().contains("unknown slide-category"), "{err}");
}
