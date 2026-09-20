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

// ---- Phase 4: field-to-tags / tags-remove ----

fn domain_as_tag() -> MigrationConfig {
    let mut field_to_tags = std::collections::HashMap::new();
    field_to_tags.insert(
        "domain".to_string(),
        crate::config::FieldToTags {
            exclude: vec!["resources".to_string(), "system".to_string()],
        },
    );
    MigrationConfig {
        name: "v5-domain-as-tag".to_string(),
        field_to_tags,
        ..Default::default()
    }
}

fn tags_of(v: &TestVault, path: &str) -> Vec<String> {
    let content = v.read(path);
    let fm = content.split("\n---\n").next().expect("frontmatter");
    let mut out = Vec::new();
    let mut under_tags = false;
    for line in fm.lines() {
        if let Some(after_key) = line.strip_prefix("tags:") {
            under_tags = true;
            if let Some(body) = after_key.trim().strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                out.extend(body.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()));
                under_tags = false;
            }
            continue;
        }
        if under_tags {
            match line.trim().strip_prefix("- ") {
                Some(tag) => out.push(tag.trim().to_string()),
                None => under_tags = false,
            }
        }
    }
    out
}

#[test]
fn field_to_tags_writes_the_domain_value_as_a_tag() {
    let v = TestVault::new();
    let notes = v.scan();
    let count = apply_migrate(v.root(), &notes, std::slice::from_ref(&domain_as_tag())).expect("apply");
    assert!(count > 0, "expected the migration to write at least one note");

    // `rust-guide.md` is `domain: tech` with tags [rust, programming].
    let tags = tags_of(&v, "rust-guide.md");
    assert!(tags.contains(&"tech".to_string()), "domain value not added: {tags:?}");
    assert!(tags.contains(&"rust".to_string()), "existing tag lost: {tags:?}");
    assert!(tags.contains(&"programming".to_string()), "existing tag lost: {tags:?}");
}

#[test]
fn field_to_tags_is_idempotent() {
    let v = TestVault::new();
    let migration = domain_as_tag();

    let first = apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&migration)).expect("first apply");
    assert!(first > 0);
    // A second apply must write ZERO files: the value is already a tag.
    let second = apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&migration)).expect("second apply");
    assert_eq!(second, 0, "migration is not idempotent");
}

#[test]
fn field_to_tags_writes_block_form() {
    let v = TestVault::new();
    apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&domain_as_tag())).expect("apply");
    let content = v.read("rust-guide.md");
    assert!(content.contains("tags:\n  - "), "expected block form:\n{content}");
    assert!(!content.contains("tags: ["), "inline form survived:\n{content}");
}

#[test]
fn field_to_tags_skips_excluded_values() {
    let v = TestVault::new();
    std::fs::write(
        v.root().join("junk.md"),
        "---\ntitle: Junk\ndate: 2026-03-20\ntype: note\ndomain: resources\norigin: authored\ntags:\n  - rust\n---\nBody.\n",
    ).expect("write");
    apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&domain_as_tag())).expect("apply");
    let tags = tags_of(&v, "junk.md");
    assert!(
        !tags.contains(&"resources".to_string()),
        "excluded value propagated: {tags:?}"
    );
    assert_eq!(tags, vec!["rust".to_string()], "unrelated tags disturbed: {tags:?}");
}

#[test]
fn field_to_tags_strips_quotes_and_normalizes() {
    let v = TestVault::new();
    // 350 vault notes quote the value; `knowledge` is the legacy spelling of
    // `life` that `hygiene::normalize_domain` maps.
    std::fs::write(
        v.root().join("quoted.md"),
        "---\ntitle: Q\ndate: 2026-03-20\ntype: note\ndomain: \"knowledge\"\norigin: authored\ntags: []\n---\nBody.\n",
    )
    .expect("write");
    apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&domain_as_tag())).expect("apply");
    let tags = tags_of(&v, "quoted.md");
    assert_eq!(tags, vec!["life".to_string()], "got {tags:?}");
}

#[test]
fn tags_remove_strips_the_named_tags() {
    let v = TestVault::new();
    let migration = MigrationConfig {
        name: "v5-domain-as-tag-undo".to_string(),
        tags_remove: vec!["rust".to_string()],
        ..Default::default()
    };
    apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&migration)).expect("apply");
    let tags = tags_of(&v, "rust-guide.md");
    assert!(!tags.contains(&"rust".to_string()), "tag not removed: {tags:?}");
    assert!(tags.contains(&"programming".to_string()), "sibling tag lost: {tags:?}");
}

#[test]
fn lint_tag_transforms_flags_a_note_that_would_exceed_the_cap() {
    let v = TestVault::new();
    std::fs::write(
        v.root().join("full.md"),
        "---\ntitle: Full\ndate: 2026-03-20\ntype: note\ndomain: tech\norigin: authored\ntags:\n  - a\n  - b\n---\nBody.\n",
    ).expect("write");
    let notes = v.scan();
    let migration = domain_as_tag();
    let report = lint_migrate_selected(&notes, &[&migration], Some(2));
    let hit = report
        .violations
        .iter()
        .find(|vi| vi.path.to_string_lossy() == "full.md")
        .expect("expected a violation for full.md");
    assert!(
        hit.message.contains("WOULD EXCEED max-per-note"),
        "cap not flagged: {}",
        hit.message
    );
}

#[test]
fn load_plan_reads_the_shipped_undo_file() {
    // The inverse must stay parseable and must NOT be a cortex.yml migration.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/migrations/v5-domain-as-tag-undo.yml");
    let plan = load_plan(&path).expect("plan parses");
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].name, "v5-domain-as-tag-undo");
    assert_eq!(plan[0].tags_remove.len(), 10, "expected the ten migrated names");
    assert!(plan[0].field_to_tags.is_empty(), "the inverse must not re-add tags");
}
