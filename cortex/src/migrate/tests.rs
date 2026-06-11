use super::*;
use crate::config::MigrationMove;
use crate::testutil::TestVault;
use std::collections::HashMap;

#[test]
fn test_plan_migration_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();

    let migration = MigrationConfig {
        name: "flatten-projects".to_string(),
        moves: vec![MigrationMove {
            from: "projects/**".to_string(),
            to: "Notes/".to_string(),
            set_frontmatter: None,
        }],
        ..Default::default()
    };

    let moves = plan_migration(&notes, &migration);
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].to, PathBuf::from("Notes/obsidian-cortex.md"));
}

#[test]
fn test_plan_migration_no_match() {
    let v = TestVault::new();
    let notes = v.scan();

    let migration = MigrationConfig {
        name: "noop".to_string(),
        moves: vec![MigrationMove {
            from: "nonexistent/**".to_string(),
            to: "Notes/".to_string(),
            set_frontmatter: None,
        }],
        ..Default::default()
    };

    let moves = plan_migration(&notes, &migration);
    assert!(moves.is_empty());
}

#[test]
fn test_plan_migration_with_frontmatter_set() {
    let v = TestVault::new();
    let notes = v.scan();

    let mut fm_set = HashMap::new();
    fm_set.insert("scope".to_string(), serde_yaml::Value::String("work".to_string()));

    let migration = MigrationConfig {
        name: "scope-projects".to_string(),
        moves: vec![MigrationMove {
            from: "projects/**".to_string(),
            to: "Notes/".to_string(),
            set_frontmatter: Some(fm_set),
        }],
        ..Default::default()
    };

    let moves = plan_migration(&notes, &migration);
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].set_frontmatter.len(), 1);
}

#[test]
fn test_apply_migrate_moves_files() {
    let v = TestVault::new();
    let notes = v.scan();

    let migrations = vec![MigrationConfig {
        name: "flatten".to_string(),
        moves: vec![MigrationMove {
            from: "projects/**".to_string(),
            to: "Notes/".to_string(),
            set_frontmatter: None,
        }],
        ..Default::default()
    }];

    let count = apply_migrate(v.root(), &notes, &migrations).expect("apply");
    assert_eq!(count, 1);
    assert!(v.exists("Notes/obsidian-cortex.md"));
    assert!(!v.exists("projects/obsidian-cortex.md"));
}

#[test]
fn test_lint_migrate_reports_moves() {
    let v = TestVault::new();
    let notes = v.scan();

    let migrations = vec![MigrationConfig {
        name: "test".to_string(),
        moves: vec![MigrationMove {
            from: "projects/**".to_string(),
            to: "Notes/".to_string(),
            set_frontmatter: None,
        }],
        ..Default::default()
    }];

    let report = lint_migrate(&notes, &migrations);
    assert_eq!(report.violations.len(), 1);
    assert_eq!(report.violations[0].rule, "migrate.test");
}

#[test]
fn test_field_rename_applies() {
    let v = TestVault::new();
    let notes = v.scan();

    let mut renames = HashMap::new();
    renames.insert("url".to_string(), "source".to_string());
    renames.insert("author".to_string(), "creator".to_string());

    let migration = MigrationConfig {
        name: "v2-renames".to_string(),
        field_renames: renames,
        ..Default::default()
    };

    let count = apply_field_transforms(v.root(), &notes, &migration).expect("apply");
    assert!(count > 0, "expected at least one file transformed");

    // legacy-note.md had url and author, should now have source and creator
    let content = v.read("legacy-note.md");
    assert!(content.contains("source:"), "expected 'source:' after rename");
    assert!(content.contains("creator:"), "expected 'creator:' after rename");
    assert!(!content.contains("\nurl:"), "expected 'url:' to be renamed");
    assert!(!content.contains("\nauthor:"), "expected 'author:' to be renamed");
}

#[test]
fn test_field_drop_applies() {
    let v = TestVault::new();
    // Add a note with droppable fields
    v.add_note(
        "drop-test.md",
        "---\ntitle: Drop Test\ndate: 2026-01-01\ntype: note\nday: monday\ntime: 10:00\ntags: []\n---\nBody.\n",
    );
    let notes = v.scan();

    let migration = MigrationConfig {
        name: "v2-drops".to_string(),
        field_drops: vec!["day".to_string(), "time".to_string()],
        ..Default::default()
    };

    let count = apply_field_transforms(v.root(), &notes, &migration).expect("apply");
    assert!(count > 0);

    let content = v.read("drop-test.md");
    assert!(!content.contains("day:"));
    assert!(!content.contains("time:"));
    assert!(content.contains("title: Drop Test"));
}

#[test]
fn field_drop_on_block_list_does_not_orphan_bullets() {
    // Regression: dropping a key whose value is a multi-line block list
    // used to remove only the header line, orphaning the `- bullet`
    // continuation lines as invalid YAML siblings.
    let v = TestVault::new();
    v.add_note(
            "drop-list-test.md",
            "---\ntitle: Drop List\ndate: 2026-01-01\ntype: note\ncortex-quality-issues:\n- no-summary\n- no-links\ntags: []\n---\nBody.\n",
        );
    let notes = v.scan();

    let migration = MigrationConfig {
        name: "v2-drop-list".to_string(),
        field_drops: vec!["cortex-quality-issues".to_string()],
        ..Default::default()
    };

    let count = apply_field_transforms(v.root(), &notes, &migration).expect("apply");
    assert!(count > 0);

    let content = v.read("drop-list-test.md");
    let fm_block = content.split("\n---").next().expect("frontmatter");
    for line in fm_block.lines() {
        assert!(
            !line.starts_with("- "),
            "orphan bullet survived: {line:?}\nfull fm:\n{fm_block}"
        );
    }
    assert!(!content.contains("cortex-quality-issues"));
    assert!(content.contains("title: Drop List"));
}

#[test]
fn test_field_rename_skips_conflict() {
    let v = TestVault::new();
    // Note already has both 'author' and 'creator'
    v.add_note(
            "conflict-note.md",
            "---\ntitle: Conflict\ndate: 2026-01-01\ntype: note\nauthor: Old Author\ncreator: Existing Creator\ntags: []\n---\nBody.\n",
        );
    let notes = v.scan();

    let mut renames = HashMap::new();
    renames.insert("author".to_string(), "creator".to_string());

    let migration = MigrationConfig {
        name: "v2-renames".to_string(),
        field_renames: renames,
        ..Default::default()
    };

    let count = apply_field_transforms(v.root(), &notes, &migration).expect("apply");
    // Should skip due to conflict - creator already exists
    let content = v.read("conflict-note.md");
    assert!(
        content.contains("author: Old Author"),
        "author should be preserved due to conflict"
    );
    assert!(content.contains("creator: Existing Creator"));
    // The conflict note should not count as transformed since it was skipped
    // (legacy-note.md may also get transformed, so count could be > 0)
    let _ = count;
}

#[test]
fn test_lint_field_transforms_reports() {
    let v = TestVault::new();
    let notes = v.scan();

    let mut renames = HashMap::new();
    renames.insert("url".to_string(), "source".to_string());

    let migrations = vec![MigrationConfig {
        name: "v2".to_string(),
        field_renames: renames,
        field_drops: vec!["folder".to_string()],
        ..Default::default()
    }];

    let report = lint_migrate(&notes, &migrations);
    // legacy-note.md has both url and folder
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule.contains("rename") && v.path.to_string_lossy() == "legacy-note.md")
    );
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule.contains("drop") && v.path.to_string_lossy() == "legacy-note.md")
    );
}

#[test]
fn test_value_rename_applies() {
    let v = TestVault::new();
    v.add_note(
        "knowledge-note.md",
        "---\ntitle: Health Tips\ndate: 2026-01-01\ntype: note\ndomain: knowledge\ntags: []\n---\nBody.\n",
    );
    let notes = v.scan();

    let mut value_map = HashMap::new();
    value_map.insert("knowledge".to_string(), "life".to_string());
    let mut value_renames = HashMap::new();
    value_renames.insert("domain".to_string(), value_map);

    let migration = MigrationConfig {
        name: "v3-domain-expansion".to_string(),
        value_renames,
        ..Default::default()
    };

    let count = apply_value_transforms(v.root(), &notes, &migration).expect("apply");
    assert!(count > 0, "expected at least one value transform");

    let content = v.read("knowledge-note.md");
    assert!(content.contains("domain: life"), "expected domain: life after rename");
    assert!(
        !content.contains("domain: knowledge"),
        "expected knowledge to be renamed"
    );
}

#[test]
fn test_value_rename_quoted() {
    let v = TestVault::new();
    v.add_note(
        "quoted-domain.md",
        "---\ntitle: Quoted\ndate: 2026-01-01\ntype: note\ndomain: \"knowledge\"\ntags: []\n---\nBody.\n",
    );
    let notes = v.scan();

    let mut value_map = HashMap::new();
    value_map.insert("knowledge".to_string(), "life".to_string());
    let mut value_renames = HashMap::new();
    value_renames.insert("domain".to_string(), value_map);

    let migration = MigrationConfig {
        name: "v3-test".to_string(),
        value_renames,
        ..Default::default()
    };

    let count = apply_value_transforms(v.root(), &notes, &migration).expect("apply");
    assert!(count > 0);

    let content = v.read("quoted-domain.md");
    assert!(
        content.contains("domain: \"life\""),
        "expected quoted value to be renamed"
    );
}

#[test]
fn test_value_rename_no_match() {
    let v = TestVault::new();
    v.add_note(
        "ai-note.md",
        "---\ntitle: AI Note\ndate: 2026-01-01\ntype: note\ndomain: ai\ntags: []\n---\nBody.\n",
    );
    let notes = v.scan();

    let mut value_map = HashMap::new();
    value_map.insert("knowledge".to_string(), "life".to_string());
    let mut value_renames = HashMap::new();
    value_renames.insert("domain".to_string(), value_map);

    let migration = MigrationConfig {
        name: "v3-test".to_string(),
        value_renames,
        ..Default::default()
    };

    let count = apply_value_transforms(v.root(), &notes, &migration).expect("apply");
    assert_eq!(count, 0, "ai domain should not be renamed");
}

#[test]
fn test_lint_value_transforms_reports() {
    let v = TestVault::new();
    v.add_note(
        "knowledge-note.md",
        "---\ntitle: Health Tips\ndate: 2026-01-01\ntype: note\ndomain: knowledge\ntags: []\n---\nBody.\n",
    );
    let notes = v.scan();

    let mut value_map = HashMap::new();
    value_map.insert("knowledge".to_string(), "life".to_string());
    let mut value_renames = HashMap::new();
    value_renames.insert("domain".to_string(), value_map);

    let migrations = vec![MigrationConfig {
        name: "v3-test".to_string(),
        value_renames,
        ..Default::default()
    }];

    let report = lint_migrate(&notes, &migrations);
    assert!(
        report.violations.iter().any(|v| v.rule.contains("value-rename")),
        "expected value-rename violation"
    );
}

#[test]
fn test_extract_frontmatter_block() {
    let content = "---\ntitle: Test\ndate: 2026-01-01\n---\nBody here.\n";
    let (fm, before, after) = extract_frontmatter_block(content).expect("extract");
    assert!(fm.contains("title: Test"));
    assert!(fm.contains("date: 2026-01-01"));
    assert_eq!(before, "");
    assert!(after.contains("Body here."));
}
