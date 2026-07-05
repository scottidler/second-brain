use super::*;

/// Builds a `Config` whose canonical-tag vocabulary and mapping live in a
/// throwaway tempdir, so the test does not depend on (or mutate) the real
/// `~/.config/sb/` catalogue.
///
/// Canonical vocabulary: `rust`, `llm`. Mapping: `rustlang` -> `rust`
/// (collapses a near-miss into canonical), `javascript` -> null (explicit
/// rejection, distinct from "no match at all").
fn config_with_fixture_canonical(dir: &std::path::Path) -> Config {
    let canonical_path = dir.join("canonical-tags.yml");
    let mapping_path = dir.join("tag-mapping.yml");

    std::fs::write(
        &canonical_path,
        "max-per-note: 7\ntags:\n  ai:\n    - llm\n  systems:\n    - rust\n",
    )
    .expect("write fixture canonical-tags.yml");
    std::fs::write(&mapping_path, "rustlang: rust\njavascript: null\n").expect("write fixture tag-mapping.yml");

    Config {
        tags: crate::config::TagsConfig {
            canonical_path: canonical_path.display().to_string(),
            mapping_path: mapping_path.display().to_string(),
            reject_concatenated: true,
        },
        ..Config::default()
    }
}

/// Phase 2 success criterion: a distiller that proposes candidate tags (the
/// new pattern instruction) has those tags reach the note through the exact
/// merge-then-filter code path `pipeline.rs` runs at publish time
/// (`all_tags.extend(distilled.tags...)` at pipeline.rs:617, followed by
/// `finalize_tags`) - canonical tags survive, non-canonical/rejected ones
/// are dropped, and a mapped near-miss collapses onto its canonical target.
#[tokio::test]
async fn distiller_proposed_tags_survive_canonical_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_fixture_canonical(dir.path());

    // Mirrors pipeline.rs:614-617: tags from other sources (yt-dlp, fabric
    // generate_tags, hashtags) sanitized first, then distiller-proposed tags
    // (`distilled.tags`) unioned in and sanitized the same way.
    let other_tags = ["llm".to_string()];
    let distilled_tags = [
        "rust".to_string(),             // exact canonical hit
        "made-up-nonsense".to_string(), // no canonical match anywhere
        "RustLang".to_string(),         // mapped near-miss -> collapses to "rust"
        "javascript".to_string(),       // explicitly rejected by the mapping
    ];

    let mut all_tags: Vec<String> = other_tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.extend(distilled_tags.iter().map(|t| hygiene::sanitize_tag(t)));

    finalize_tags(&mut all_tags, &config).await;

    assert!(
        all_tags.contains(&"llm".to_string()),
        "canonical tag from other sources must survive: {all_tags:?}"
    );
    assert!(
        all_tags.contains(&"rust".to_string()),
        "canonical distiller-proposed tag must survive: {all_tags:?}"
    );
    assert!(
        !all_tags.contains(&"made-up-nonsense".to_string()),
        "non-canonical distiller-proposed tag must be dropped: {all_tags:?}"
    );
    assert!(
        !all_tags.contains(&"javascript".to_string()),
        "mapping-rejected tag must be dropped: {all_tags:?}"
    );
    assert!(
        !all_tags.contains(&"rustlang".to_string()),
        "mapped near-miss must collapse onto its canonical target, not survive raw: {all_tags:?}"
    );
    assert_eq!(
        all_tags.len(),
        2,
        "expected exactly {{llm, rust}} to survive: {all_tags:?}"
    );
}

/// A distiller that (still) proposes nothing produces no canonical tags from
/// that source - the filter has nothing to gate, not an error.
#[tokio::test]
async fn empty_distiller_tags_yield_no_canonical_tags_from_that_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_fixture_canonical(dir.path());

    let mut all_tags: Vec<String> = Vec::new();
    let distilled_tags: Vec<String> = Vec::new();
    all_tags.extend(distilled_tags.iter().map(|t| hygiene::sanitize_tag(t)));

    finalize_tags(&mut all_tags, &config).await;

    assert!(
        all_tags.is_empty(),
        "no proposed tags means nothing survives: {all_tags:?}"
    );
}
