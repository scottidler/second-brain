use super::*;

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn empty_by_default_suppresses_nothing() {
    let stop = Stopwords::default();
    assert!(stop.is_empty());
    assert!(!stop.contains("every"), "the code default must suppress nothing");
}

#[test]
fn matches_case_insensitively() {
    let stop = Stopwords::new(&words(&["every"]));
    assert!(stop.contains("every"));
    assert!(stop.contains("Every"), "the auto-linker writes [[Every]]");
    assert!(stop.contains("EVERY"));
}

#[test]
fn matches_whole_target_only() {
    let stop = Stopwords::new(&words(&["every"]));
    assert!(!stop.contains("everyone"), "a substring is a different note");
    assert!(!stop.contains("every-thing"));
    assert!(!stop.contains("every#heading"), "a heading ref is a different target");
}

#[test]
fn trims_both_sides() {
    let stop = Stopwords::new(&words(&["  every  "]));
    assert!(stop.contains("every"));
    assert!(stop.contains("[[ every ]]".trim_matches(['[', ']'])));
    assert_eq!(stop.len(), 1);
}

#[test]
fn blank_entries_are_dropped_not_matched_against_everything() {
    // A stray `- ""` in YAML must not turn into a match-all: `contains("")`
    // would otherwise be true and the linker would stop writing any link.
    let stop = Stopwords::new(&words(&["", "   ", "every"]));
    assert_eq!(stop.len(), 1);
    assert!(!stop.contains(""));
    assert!(stop.contains("every"));
}

#[test]
fn iter_reports_the_operators_own_spelling() {
    let stop = Stopwords::new(&words(&["Every", "brief"]));
    assert_eq!(stop.iter().collect::<Vec<_>>(), vec!["Every", "brief"]);
}
