use super::*;

#[test]
fn parse_single_auto_block() {
    let input = "<!-- facet:auto:begin x -->\nbody\n<!-- facet:auto:end x -->\n";
    let blocks = parse(input);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
        Block::Auto { id, body } => {
            assert_eq!(id, "x");
            assert_eq!(body, "body\n");
        }
        _ => panic!("expected Auto"),
    }
}

#[test]
fn parse_preserves_operator_chunks_between_autos() {
    let input = "<!-- facet:auto:begin a -->\nA\n<!-- facet:auto:end a -->\n\nmiddle text\n\n<!-- facet:auto:begin b -->\nB\n<!-- facet:auto:end b -->\n";
    let blocks = parse(input);
    assert_eq!(blocks.len(), 3);
    match &blocks[1] {
        Block::Operator(s) => assert!(s.contains("middle text")),
        _ => panic!("expected Operator in middle"),
    }
}

#[test]
fn emit_round_trips() {
    let input = "<!-- facet:auto:begin a -->\nA\n<!-- facet:auto:end a -->\nleading op\n";
    let blocks = parse(input);
    let out = emit(&blocks);
    assert_eq!(out, input);
}

#[test]
fn merge_replaces_auto_body_preserves_operator() {
    let existing = "<!-- facet:auto:begin x -->\nold body\n<!-- facet:auto:end x -->\noperator only line\n";
    let template = "<!-- facet:auto:begin x -->\nnew body\n<!-- facet:auto:end x -->\n";
    let out = merge(existing, template);
    assert!(out.contains("new body"));
    assert!(!out.contains("old body"));
    assert!(out.contains("operator only line"), "operator content lost: {out}");
}

#[test]
fn merge_appends_new_template_only_blocks() {
    let existing = "<!-- facet:auto:begin x -->\nX\n<!-- facet:auto:end x -->\n";
    let template = "<!-- facet:auto:begin x -->\nX\n<!-- facet:auto:end x -->\n<!-- facet:auto:begin y -->\nY\n<!-- facet:auto:end y -->\n";
    let out = merge(existing, template);
    assert!(out.contains("<!-- facet:auto:begin y -->"));
    assert!(out.contains("Y"));
}

#[test]
fn merge_empties_block_when_template_drops_id() {
    let existing = "<!-- facet:auto:begin x -->\nfull\n<!-- facet:auto:end x -->\n";
    let template = "<!-- facet:auto:begin other -->\n<!-- facet:auto:end other -->\n";
    let out = merge(existing, template);
    // x stays as an empty fencepost
    assert!(out.contains("<!-- facet:auto:begin x -->"));
    assert!(out.contains("<!-- facet:auto:end x -->"));
    assert!(!out.contains("full"));
    assert!(out.contains("<!-- facet:auto:begin other -->"));
}

#[test]
fn unmatched_begin_marker_demotes_remainder_to_operator() {
    // Mistakenly deleted end marker: the remainder of the file is treated as operator content
    let input = "before\n<!-- facet:auto:begin x -->\nbody no end marker\n";
    let blocks = parse(input);
    assert_eq!(blocks.len(), 2, "got {blocks:#?}");
    match &blocks[1] {
        Block::Operator(s) => assert!(s.contains("body no end marker")),
        _ => panic!("expected operator demotion"),
    }
}
