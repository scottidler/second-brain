use super::*;
use crate::testutil::NoteBuilder;

fn note(path: &str) -> Note {
    NoteBuilder::new(path).title(path).build()
}

#[test]
fn test_not_excluded_by_default() {
    let n = note("notes/foo.md");
    assert!(!is_excluded(&n, &[], &[]));
}

#[test]
fn test_excluded_by_pattern() {
    let n = note("system/templates/link.md");
    let exclude = parse_patterns(&["system/templates/**".to_string()]);
    assert!(is_excluded(&n, &exclude, &[]));
}

#[test]
fn test_include_overrides_exclude() {
    let n = note("system/design-vault.md");
    let exclude = parse_patterns(&["system/**".to_string()]);
    let include = parse_patterns(&["system/design-*.md".to_string()]);
    assert!(!is_excluded(&n, &exclude, &include));
}

#[test]
fn test_include_does_not_affect_non_excluded() {
    let n = note("notes/foo.md");
    let exclude = parse_patterns(&["system/**".to_string()]);
    let include = parse_patterns(&["system/design-*.md".to_string()]);
    assert!(!is_excluded(&n, &exclude, &include));
}

#[test]
fn test_excluded_not_rescued_by_unmatched_include() {
    let n = note("system/templates/link.md");
    let exclude = parse_patterns(&["system/**".to_string()]);
    let include = parse_patterns(&["system/design-*.md".to_string()]);
    assert!(is_excluded(&n, &exclude, &include));
}

#[test]
fn scan_scope_as_config_scan_for_maps_each_variant() {
    use crate::opts::ScanScope;
    assert_eq!(ScanScope::People.as_config_scan_for(), vec!["people".to_string()]);
    assert_eq!(ScanScope::Projects.as_config_scan_for(), vec!["projects".to_string()]);
    assert_eq!(ScanScope::Concepts.as_config_scan_for(), vec!["concepts".to_string()]);
    assert_eq!(ScanScope::All.as_config_scan_for(), vec!["all".to_string()]);
}

/// Build a config + note fixture that should fire one violation per
/// rule class (concept, person, project) so we can assert subset behavior
/// across `--scan` variants.
fn linking_fixture() -> (crate::config::LinkingConfig, Vec<Note>) {
    let config = crate::config::LinkingConfig {
        scan_for: vec!["all".to_string()],
        entities: crate::config::LinkingEntities {
            people: vec!["Alice Smith".to_string()],
            projects: vec!["ProjectAtlas".to_string()],
            concepts: Vec::new(),
        },
        targets: crate::config::LinkingTargets::default(),
        min_word_length: 4,
        aliases: std::collections::HashMap::new(),
    };
    // Concept target: a separate note with title "Distillation" lives in the
    // vault. The probe note mentions Alice Smith, ProjectAtlas, and Distillation
    // in its body so each scan_for variant has at least one mention to flag.
    let concept_note = NoteBuilder::new("notes/distillation.md").title("Distillation").build();
    let probe = NoteBuilder::new("notes/probe.md")
        .title("probe")
        .body("Alice Smith owns ProjectAtlas. The Distillation step is documented here.")
        .build();
    (config, vec![concept_note, probe])
}

fn run_link_scope(scan_for: Vec<String>) -> std::collections::HashSet<String> {
    let (mut config, notes) = linking_fixture();
    config.scan_for = scan_for;
    let report = crate::linking::lint_linking(&notes, &config);
    report.violations.into_iter().map(|v| v.rule).collect()
}

#[test]
fn link_scan_people_is_strict_subset_of_all() {
    let all = run_link_scope(vec!["all".to_string()]);
    let people = run_link_scope(vec!["people".to_string()]);

    assert!(people.iter().all(|r| all.contains(r)), "people={people:?} all={all:?}");
    assert!(
        people.len() < all.len(),
        "people scope should be a STRICT subset: people={people:?} all={all:?}"
    );
    assert!(
        people.contains("linking.person"),
        "people scope must still flag persons"
    );
    assert!(
        !people.contains("linking.project") && !people.contains("linking.concept"),
        "people scope must not flag projects or concepts: {people:?}"
    );
}

#[test]
fn link_scan_projects_is_strict_subset_of_all() {
    let all = run_link_scope(vec!["all".to_string()]);
    let projects = run_link_scope(vec!["projects".to_string()]);
    assert!(projects.iter().all(|r| all.contains(r)));
    assert!(projects.len() < all.len());
    assert!(projects.contains("linking.project"));
    assert!(!projects.contains("linking.person"));
    assert!(!projects.contains("linking.concept"));
}

#[test]
fn link_scan_concepts_is_strict_subset_of_all() {
    let all = run_link_scope(vec!["all".to_string()]);
    let concepts = run_link_scope(vec!["concepts".to_string()]);
    assert!(concepts.iter().all(|r| all.contains(r)));
    assert!(concepts.len() < all.len());
    assert!(concepts.contains("linking.concept"));
    assert!(!concepts.contains("linking.person"));
    assert!(!concepts.contains("linking.project"));
}
