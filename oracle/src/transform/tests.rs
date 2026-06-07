use super::*;

#[test]
fn hyde_uses_output_as_single_variant() {
    let out = parse_transform_output(TransformMethod::Hyde, "what is rust", "Rust is a systems language.", 3);
    assert_eq!(out, vec!["Rust is a systems language.".to_string()]);
}

#[test]
fn hyde_empty_output_falls_back_to_query() {
    let out = parse_transform_output(TransformMethod::Hyde, "what is rust", "   \n  ", 3);
    assert_eq!(
        out,
        vec!["what is rust".to_string()],
        "empty HyDE must not retrieve on an empty string"
    );
}

#[test]
fn multi_query_keeps_original_then_rewrites_capped_at_variants() {
    let raw = "rust memory safety\nrust ownership model\nrust borrow checker\nrust lifetimes";
    let out = parse_transform_output(TransformMethod::MultiQuery, "rust safety", raw, 2);
    assert_eq!(
        out,
        vec![
            "rust safety".to_string(), // original first
            "rust memory safety".to_string(),
            "rust ownership model".to_string(),
        ],
        "original kept, rewrites capped at variants=2",
    );
}

#[test]
fn multi_query_skips_blank_lines() {
    let raw = "\nrust ownership\n\n\nrust lifetimes\n";
    let out = parse_transform_output(TransformMethod::MultiQuery, "rust", raw, 5);
    assert_eq!(
        out,
        vec![
            "rust".to_string(),
            "rust ownership".to_string(),
            "rust lifetimes".to_string()
        ]
    );
}

#[test]
fn union_lists_dedups_preserving_first_seen_order() {
    let lists = vec![
        vec!["a".to_string(), "b".to_string()],
        vec!["b".to_string(), "c".to_string()],
        vec!["a".to_string(), "d".to_string()],
    ];
    let out = union_lists(lists);
    assert_eq!(
        out,
        vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()]
    );
}

#[test]
fn union_lists_single_list_is_identity() {
    let lists = vec![vec!["x".to_string(), "y".to_string(), "z".to_string()]];
    assert_eq!(
        union_lists(lists),
        vec!["x".to_string(), "y".to_string(), "z".to_string()]
    );
}

#[test]
fn union_lists_empty_is_empty() {
    assert!(union_lists(vec![]).is_empty());
}
