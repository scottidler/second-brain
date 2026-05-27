use super::*;

#[test]
fn semantic_duplicate_filename_starts_with_kind_prefix() {
    let d = Dream::SemanticDuplicateGroup {
        gem_ids: vec![1, 2],
        canonical: 1,
    };
    let name = dream_filename(&d);
    assert!(name.starts_with("semantic-duplicate-"));
    assert!(name.ends_with(".md"));
}

#[test]
fn same_dream_yields_same_filename() {
    let a = Dream::CrossReference {
        from_gem: 1,
        to_gem: 2,
        relation: "precursor".to_string(),
    };
    let b = a.clone();
    assert_eq!(dream_filename(&a), dream_filename(&b));
}

#[test]
fn render_all_writes_one_file_per_dream() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dreams = vec![
        Dream::SemanticDuplicateGroup {
            gem_ids: vec![1, 2],
            canonical: 1,
        },
        Dream::NarrativeCandidate {
            gem_ids: vec![1, 2, 3],
            proposed_title: "X".to_string(),
            proposed_thesis: "T".to_string(),
        },
    ];
    let written = render_all(&dreams, tmp.path()).expect("write");
    assert_eq!(written.len(), 2);
    for path in &written {
        let body = std::fs::read_to_string(path).expect("read");
        assert!(body.contains("type: facet-dream"));
        assert!(body.contains("facet-dream-kind:"));
        assert!(body.contains("facet-dream-status: proposed"));
    }
}
