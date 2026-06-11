use super::*;

#[test]
fn test_to_slug_basic() {
    assert_eq!(to_slug("Hello World.md"), "hello-world");
    assert_eq!(to_slug("My_Note.md"), "my-note");
    assert_eq!(to_slug("already-valid.md"), "already-valid");
}

#[test]
fn test_to_slug_special_chars() {
    assert_eq!(to_slug("Hello World!.md"), "hello-world");
    assert_eq!(to_slug("Test (1).md"), "test-1");
    assert_eq!(to_slug("A   B   C.md"), "a-b-c");
}

#[test]
fn test_to_slug_preserves_numbers() {
    assert_eq!(to_slug("note-123.md"), "note-123");
    assert_eq!(to_slug("2026-03-16-daily.md"), "2026-03-16-daily");
}

#[test]
fn test_is_valid_slug() {
    assert!(is_valid_slug("hello-world"));
    assert!(is_valid_slug("note-123"));
    assert!(is_valid_slug("a"));

    assert!(!is_valid_slug("Hello-World"));
    assert!(!is_valid_slug("hello_world"));
    assert!(!is_valid_slug("-leading"));
    assert!(!is_valid_slug("trailing-"));
    assert!(!is_valid_slug("double--hyphen"));
    assert!(!is_valid_slug(""));
}

#[test]
fn test_lint_naming_on_vault() {
    let v = crate::testutil::TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.naming;

    let report = lint_naming(&notes, &config);
    // "My Awesome Note.md" should be flagged
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule == "naming.lowercase-hyphenated" && v.path.to_string_lossy().contains("My Awesome Note"))
    );
    // Valid slugs should NOT be flagged
    assert!(
        !report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy() == "rust-guide.md")
    );
}

#[test]
fn test_lint_naming_max_length() {
    let v = crate::testutil::TestVault::new();
    v.add_note(
        &format!("{}.md", "a".repeat(100)),
        "---\ntitle: Long\n---\nLong name.\n",
    );
    let notes = v.scan();
    let config = v.config().actions.naming;

    let report = lint_naming(&notes, &config);
    assert!(report.violations.iter().any(|v| v.rule == "naming.max-length"));
}

#[test]
fn test_lint_naming_exempt() {
    let v = crate::testutil::TestVault::new();
    v.add_note("system/Bad Name.md", "---\ntitle: Bad\n---\nExempt.\n");
    let notes = v.scan();
    let config = NamingConfig {
        style: "lowercase-hyphenated".to_string(),
        max_length: 80,
        exempt_patterns: vec!["^system/".to_string()],
    };

    let report = lint_naming(&notes, &config);
    assert!(
        !report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy().contains("system/Bad Name"))
    );
}

#[test]
fn test_apply_naming_renames_files() {
    let v = crate::testutil::TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.naming;

    let renames = apply_naming(v.root(), &notes, &config).expect("apply");
    assert!(!renames.is_empty());
    // "My Awesome Note.md" should be renamed to "my-awesome-note.md"
    assert!(v.exists("my-awesome-note.md"));
    assert!(!v.exists("My Awesome Note.md"));
}
