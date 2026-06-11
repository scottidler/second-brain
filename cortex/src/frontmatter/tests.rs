use super::*;
use crate::config::SchemaConfig;
use crate::testutil::TestVault;

fn test_schema() -> SchemaConfig {
    SchemaConfig {
        domains: vec![
            "ai",
            "tech",
            "football",
            "work",
            "writing",
            "music",
            "spanish",
            "life",
            "homelab",
            "diy",
            "resources",
            "system",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        types: vec![
            "youtube", "article", "github", "social", "book", "video", "research", "daily", "meeting", "note", "vocab",
            "moc", "link", "poem", "system",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        origins: vec!["authored", "assisted", "generated"]
            .into_iter()
            .map(String::from)
            .collect(),
        statuses: vec!["unread", "reading", "reviewed", "starred"]
            .into_iter()
            .map(String::from)
            .collect(),
        methods: vec!["http", "telegram", "clipboard", "cli", "manual"]
            .into_iter()
            .map(String::from)
            .collect(),
    }
}

#[test]
fn test_valid_notes_pass() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.frontmatter;
    let schema = test_schema();

    let report = lint_frontmatter(&notes, &config, &schema);
    // rust-guide.md has all required fields - should NOT be flagged for required
    assert!(
        !report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy() == "rust-guide.md" && v.rule.starts_with("frontmatter.required"))
    );
}

#[test]
fn test_missing_frontmatter_detected() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.frontmatter;
    let schema = test_schema();

    let report = lint_frontmatter(&notes, &config, &schema);
    // bare-note.md has no frontmatter
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy() == "bare-note.md" && v.rule == "frontmatter.missing")
    );
}

#[test]
fn test_missing_required_fields() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.frontmatter;
    let schema = test_schema();

    let report = lint_frontmatter(&notes, &config, &schema);
    // partial-frontmatter.md has title but missing date, type, tags
    let partial_violations: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.path.to_string_lossy() == "partial-frontmatter.md" && v.rule.starts_with("frontmatter.required"))
        .collect();
    assert_eq!(partial_violations.len(), 3);
}

#[test]
fn test_type_specific_fields() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.frontmatter;
    let schema = test_schema();

    let report = lint_frontmatter(&notes, &config, &schema);
    // cool-video.md is type=video but missing source, creator
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy() == "cool-video.md" && v.rule.contains("type-field.video"))
    );
}

#[test]
fn test_title_from_filename() {
    assert_eq!(title_from_filename(Path::new("hello-world.md")), "Hello World");
    assert_eq!(title_from_filename(Path::new("my-note-123.md")), "My Note 123");
}

#[test]
fn test_apply_inserts_frontmatter() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.frontmatter;
    let schema = test_schema();

    let count = apply_frontmatter(v.root(), &notes, &config, &schema).expect("apply");
    assert!(count > 0);

    // bare-note.md should now have frontmatter
    let content = v.read("bare-note.md");
    assert!(content.starts_with("---\n"));
    assert!(content.contains("title:"));
}

#[test]
fn test_enum_validation_invalid_domain() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.frontmatter;
    let schema = test_schema();

    let report = lint_frontmatter(&notes, &config, &schema);
    // bad-enums.md has domain: tech-stuff which is invalid
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy() == "bad-enums.md" && v.rule == "frontmatter.enum.domain")
    );
}

#[test]
fn test_enum_validation_invalid_type() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.frontmatter;
    let schema = test_schema();

    let report = lint_frontmatter(&notes, &config, &schema);
    // bad-enums.md has type: blogpost which is invalid
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy() == "bad-enums.md" && v.rule == "frontmatter.enum.type")
    );
}

#[test]
fn test_enum_validation_invalid_origin() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.frontmatter;
    let schema = test_schema();

    let report = lint_frontmatter(&notes, &config, &schema);
    // bad-enums.md has origin: robot which is invalid
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy() == "bad-enums.md" && v.rule == "frontmatter.enum.origin")
    );
}

#[test]
fn test_enum_validation_skipped_when_schema_empty() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.frontmatter;
    // SchemaConfig::default() is now enum-derived (non-empty); construct a
    // genuinely empty schema to exercise the skip-when-empty code path.
    let empty_schema = SchemaConfig {
        domains: vec![],
        types: vec![],
        origins: vec![],
        statuses: vec![],
        methods: vec![],
    };

    let report = lint_frontmatter(&notes, &config, &empty_schema);
    // With empty schema, no enum violations should appear
    assert!(!report.violations.iter().any(|v| v.rule.starts_with("frontmatter.enum")));
}

#[test]
fn test_daily_note_exempt_from_domain() {
    let v = TestVault::new();
    let notes = v.scan();
    let mut config = v.config().actions.frontmatter;
    config.required = vec![
        "title".to_string(),
        "date".to_string(),
        "type".to_string(),
        "domain".to_string(),
        "origin".to_string(),
        "tags".to_string(),
    ];
    config.exempt.insert("daily".to_string(), vec!["domain".to_string()]);
    let schema = test_schema();

    let report = lint_frontmatter(&notes, &config, &schema);
    // daily/2026-03-18.md is type: daily, has no domain, should NOT be flagged
    assert!(
        !report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy().contains("2026-03-18") && v.rule == "frontmatter.required.domain")
    );
}

#[test]
fn test_inbox_note_exempt_from_domain() {
    let v = TestVault::new();
    let notes = v.scan();
    let mut config = v.config().actions.frontmatter;
    config.required = vec![
        "title".to_string(),
        "date".to_string(),
        "type".to_string(),
        "domain".to_string(),
        "origin".to_string(),
        "tags".to_string(),
    ];
    config
        .path_exempt
        .insert("inbox/**".to_string(), vec!["domain".to_string()]);
    let schema = test_schema();

    let report = lint_frontmatter(&notes, &config, &schema);
    // inbox/untriaged-link.md has no domain, should NOT be flagged
    assert!(
        !report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy().contains("untriaged-link") && v.rule == "frontmatter.required.domain")
    );
}

#[test]
fn test_deprecated_field_detection() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.frontmatter;
    let schema = SchemaConfig::default();

    let report = lint_frontmatter(&notes, &config, &schema);
    // legacy-note.md has url, author, duration_min, folder
    let legacy_deprecated: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.path.to_string_lossy() == "legacy-note.md" && v.rule.starts_with("frontmatter.deprecated"))
        .collect();
    assert!(
        legacy_deprecated.len() >= 4,
        "expected at least 4 deprecated field violations, got {}",
        legacy_deprecated.len()
    );
}
