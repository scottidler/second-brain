use super::*;

#[test]
fn semantic_duplicate_group_roundtrips_through_json() {
    let d = Dream::SemanticDuplicateGroup {
        gem_ids: vec![1, 2, 3],
        canonical: 2,
    };
    let json = serde_json::to_string(&d).expect("serialize dream");
    let back: Dream = serde_json::from_str(&json).expect("deserialize dream");
    assert_eq!(d, back);
}

#[test]
fn cross_reference_roundtrips_through_json() {
    let d = Dream::CrossReference {
        from_gem: 7,
        to_gem: 9,
        relation: "precursor".to_string(),
    };
    let json = serde_json::to_string(&d).expect("serialize dream");
    let back: Dream = serde_json::from_str(&json).expect("deserialize dream");
    assert_eq!(d, back);
}

#[test]
fn stale_spectrum_roundtrips_through_json() {
    let d = Dream::StaleSpectrum {
        narrative_id: 42,
        new_gem_ids_since: vec![100, 101, 102],
    };
    let json = serde_json::to_string(&d).expect("serialize dream");
    let back: Dream = serde_json::from_str(&json).expect("deserialize dream");
    assert_eq!(d, back);
}

#[test]
fn narrative_candidate_roundtrips_through_json() {
    let d = Dream::NarrativeCandidate {
        gem_ids: vec![5, 6, 7],
        proposed_title: "Three Wrong Migrations".to_string(),
        proposed_thesis: "Each rejection saved an hour.".to_string(),
    };
    let json = serde_json::to_string(&d).expect("serialize dream");
    let back: Dream = serde_json::from_str(&json).expect("deserialize dream");
    assert_eq!(d, back);
}

#[test]
fn dream_serializes_with_kebab_case_tag() {
    let d = Dream::SemanticDuplicateGroup {
        gem_ids: vec![1],
        canonical: 1,
    };
    let json = serde_json::to_string(&d).expect("serialize dream");
    assert!(json.contains("\"kind\":\"semantic-duplicate-group\""));
}

#[test]
fn dream_deserializes_kebab_case_tag() {
    let raw = r#"{"kind":"cross-reference","from_gem":1,"to_gem":2,"relation":"follow-up"}"#;
    let d: Dream = serde_json::from_str(raw).expect("deserialize dream");
    match d {
        Dream::CrossReference {
            from_gem,
            to_gem,
            relation,
        } => {
            assert_eq!(from_gem, 1);
            assert_eq!(to_gem, 2);
            assert_eq!(relation, "follow-up");
        }
        _ => panic!("expected CrossReference variant"),
    }
}
