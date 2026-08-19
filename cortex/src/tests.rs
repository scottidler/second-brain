use super::*;
use crate::testutil::NoteBuilder;
use std::path::PathBuf;

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
    let report = crate::linking::lint_linking(&notes, &config, &crate::stopwords::Stopwords::default());
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

/// Design doc `2026-07-05-cortex-daemon-oscillation-loop.md`, Phase 1,
/// success criterion (b): the lint apply fingerprint must exclude every
/// `fix: None` violation. `hobby-project.md` (TestVault fixture) carries a
/// single `tags.non-canonical` violation - Severity::Info, `fix: None` - and
/// no other lint issue (valid filename, all required frontmatter present,
/// tag format already lowercase-hyphenated). The lint REPORT must still flag
/// it; the apply-path fingerprint must not.
#[test]
fn lint_apply_report_excludes_fix_none_violations() {
    use crate::testutil::TestVault;

    let v = TestVault::new();
    let config = v.config();
    let opts = crate::opts::LintOpts {
        apply: true,
        format: crate::opts::LintFormat::Human,
        rule: Vec::new(),
        path: Some("hobby-project.md".to_string()),
    };

    let (report, lint_apply) = crate::lint(v.root(), &config, &opts).expect("lint apply");

    assert!(
        report
            .violations
            .iter()
            .any(|viol| viol.rule == "tags.non-canonical" && viol.fix.is_none()),
        "expected the lint report to still flag the unfixable violation: {:?}",
        report.violations
    );
    assert!(
        lint_apply.written_paths.is_empty(),
        "a note with ONLY a fix:None violation must never appear in written_paths: {:?}",
        lint_apply.written_paths
    );
    assert_eq!(lint_apply.remaining_violations, report.violations.len());
}

/// Success criterion (a): a lint pass with zero writable fixes produces an
/// empty fingerprint. Scoping the same single-violation note above through
/// the full lint dispatch (all four appliers) demonstrates the aggregate
/// `LintApplyReport.written_paths` - not just one applier's return - stays
/// empty when nothing in scope is fixable.
#[test]
fn lint_apply_zero_writable_fixes_yields_empty_fingerprint() {
    use crate::testutil::TestVault;

    let v = TestVault::new();
    let config = v.config();
    let opts = crate::opts::LintOpts {
        apply: true,
        format: crate::opts::LintFormat::Human,
        rule: Vec::new(),
        path: Some("hobby-project.md".to_string()),
    };

    let (_report, lint_apply) = crate::lint(v.root(), &config, &opts).expect("lint apply");
    assert!(lint_apply.written_paths.is_empty());
}

/// Success criterion (c): a unit test asserts `fingerprint ⊆ files whose
/// bytes changed`. Runs the full default lint apply (naming/frontmatter/
/// tags/scope) over the whole fixture vault - which contains BOTH fixable
/// violations (renames, missing frontmatter, alias tags, scope fields) and
/// unfixable ones (non-canonical tags, bad enums) - then verifies every path
/// `LintApplyReport` names actually has different bytes on disk, and that at
/// least one write really happened (the invariant is not vacuously true).
#[test]
fn lint_apply_written_paths_are_subset_of_bytes_changed() {
    use crate::testutil::TestVault;
    use std::fs;

    let v = TestVault::new();

    // Snapshot every markdown file's bytes before the apply pass.
    let mut before: std::collections::HashMap<PathBuf, Vec<u8>> = std::collections::HashMap::new();
    for note in v.scan() {
        let abs = v.root().join(&note.path);
        if let Ok(bytes) = fs::read(&abs) {
            before.insert(note.path.clone(), bytes);
        }
    }

    let config = v.config();
    let opts = crate::opts::LintOpts {
        apply: true,
        format: crate::opts::LintFormat::Human,
        rule: Vec::new(),
        path: None,
    };
    let (_report, lint_apply) = crate::lint(v.root(), &config, &opts).expect("lint apply");

    assert!(
        !lint_apply.written_paths.is_empty(),
        "expected at least one real write across the fixable violations in the fixture vault"
    );

    for written in &lint_apply.written_paths {
        let rel = PathBuf::from(written);
        let abs = v.root().join(&rel);
        let after = fs::read(&abs).unwrap_or_else(|e| panic!("written path {written} unreadable after apply: {e}"));
        let prior = before.get(&rel);
        assert_ne!(
            prior.map(|b| b.as_slice()),
            Some(after.as_slice()),
            "fingerprint named {written} but its bytes did not change"
        );
    }

    // The unfixable-only note must never appear, even though it was
    // in-scope for this unfiltered pass.
    assert!(
        !lint_apply.written_paths.iter().any(|p| p.contains("hobby-project.md")),
        "unfixable-only note leaked into the fingerprint: {:?}",
        lint_apply.written_paths
    );
}
