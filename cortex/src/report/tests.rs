use super::*;

fn violation(rule: &str) -> Violation {
    Violation {
        path: PathBuf::from("note.md"),
        rule: rule.to_string(),
        severity: Severity::Warning,
        message: String::new(),
        fix: None,
    }
}

#[test]
fn count_by_rule_prefix_groups_by_suffix_and_ignores_other_prefixes() {
    let report = Report {
        violations: vec![
            violation("frontmatter.required.tags"),
            violation("frontmatter.required.tags"),
            violation("frontmatter.required.origin"),
            violation("tags.non-canonical"),
        ],
        applied: 0,
        applied_paths: Vec::new(),
    };

    let required = report.count_by_rule_prefix("frontmatter.required.");
    let expected: BTreeMap<String, u64> = [("tags".to_string(), 2), ("origin".to_string(), 1)]
        .into_iter()
        .collect();
    assert_eq!(required, expected);

    let enums = report.count_by_rule_prefix("frontmatter.enum.");
    assert!(enums.is_empty());
}
