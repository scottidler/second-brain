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
        "---\ntitle: Health Tips\ndate: 2026-01-01\ntype: note\ncategory: knowledge\ntags: []\n---\nBody.\n",
    );
    let notes = v.scan();

    let mut value_map = HashMap::new();
    value_map.insert("knowledge".to_string(), "life".to_string());
    let mut value_renames = HashMap::new();
    value_renames.insert("category".to_string(), value_map);

    let migration = MigrationConfig {
        name: "v3-category-rename-test".to_string(),
        value_renames,
        ..Default::default()
    };

    let count = apply_value_transforms(v.root(), &notes, &migration).expect("apply");
    assert!(count > 0, "expected at least one value transform");

    let content = v.read("knowledge-note.md");
    assert!(
        content.contains("category: life"),
        "expected category: life after rename"
    );
    assert!(
        !content.contains("category: knowledge"),
        "expected knowledge to be renamed"
    );
}

#[test]
fn test_value_rename_quoted() {
    let v = TestVault::new();
    v.add_note(
        "quoted-category.md",
        "---\ntitle: Quoted\ndate: 2026-01-01\ntype: note\ncategory: \"knowledge\"\ntags: []\n---\nBody.\n",
    );
    let notes = v.scan();

    let mut value_map = HashMap::new();
    value_map.insert("knowledge".to_string(), "life".to_string());
    let mut value_renames = HashMap::new();
    value_renames.insert("category".to_string(), value_map);

    let migration = MigrationConfig {
        name: "v3-test".to_string(),
        value_renames,
        ..Default::default()
    };

    let count = apply_value_transforms(v.root(), &notes, &migration).expect("apply");
    assert!(count > 0);

    let content = v.read("quoted-category.md");
    assert!(
        content.contains("category: \"life\""),
        "expected quoted value to be renamed"
    );
}

#[test]
fn test_value_rename_no_match() {
    let v = TestVault::new();
    v.add_note(
        "ai-note.md",
        "---\ntitle: AI Note\ndate: 2026-01-01\ntype: note\ncategory: ai\ntags: []\n---\nBody.\n",
    );
    let notes = v.scan();

    let mut value_map = HashMap::new();
    value_map.insert("knowledge".to_string(), "life".to_string());
    let mut value_renames = HashMap::new();
    value_renames.insert("category".to_string(), value_map);

    let migration = MigrationConfig {
        name: "v3-test".to_string(),
        value_renames,
        ..Default::default()
    };

    let count = apply_value_transforms(v.root(), &notes, &migration).expect("apply");
    assert_eq!(count, 0, "ai category should not be renamed");
}

#[test]
fn test_lint_value_transforms_reports() {
    let v = TestVault::new();
    v.add_note(
        "knowledge-note.md",
        "---\ntitle: Health Tips\ndate: 2026-01-01\ntype: note\ncategory: knowledge\ntags: []\n---\nBody.\n",
    );
    let notes = v.scan();

    let mut value_map = HashMap::new();
    value_map.insert("knowledge".to_string(), "life".to_string());
    let mut value_renames = HashMap::new();
    value_renames.insert("category".to_string(), value_map);

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

fn category_as_tag() -> MigrationConfig {
    let mut field_to_tags = std::collections::HashMap::new();
    field_to_tags.insert(
        "category".to_string(),
        crate::config::FieldToTags {
            exclude: vec!["resources".to_string(), "system".to_string()],
        },
    );
    MigrationConfig {
        name: "v5-category-as-tag".to_string(),
        field_to_tags,
        ..Default::default()
    }
}

/// A vocabulary holding exactly `tags`, capped at `max_per_note`.
fn canon_with(tags: &[&str], max_per_note: usize) -> vault::canonical::CanonicalSet {
    vault::canonical::CanonicalSet {
        all: tags.iter().map(|t| (*t).to_string()).collect(),
        no_segment: Default::default(),
        no_classifier: Default::default(),
        max_per_note,
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
fn field_to_tags_writes_the_category_value_as_a_tag() {
    let v = TestVault::new();
    std::fs::write(
        v.root().join("categorized.md"),
        "---\ntitle: Categorized\ndate: 2026-03-20\ntype: note\ncategory: tech\ntags:\n  - rust\n  - programming\n---\nBody.\n",
    )
    .expect("write");
    let notes = v.scan();
    let count = apply_migrate(v.root(), &notes, std::slice::from_ref(&category_as_tag())).expect("apply");
    assert!(count > 0, "expected the migration to write at least one note");

    // `categorized.md` is `category: tech` with tags [rust, programming].
    let tags = tags_of(&v, "categorized.md");
    assert!(tags.contains(&"tech".to_string()), "category value not added: {tags:?}");
    assert!(tags.contains(&"rust".to_string()), "existing tag lost: {tags:?}");
    assert!(tags.contains(&"programming".to_string()), "existing tag lost: {tags:?}");
}

#[test]
fn field_to_tags_is_idempotent() {
    let v = TestVault::new();
    std::fs::write(
        v.root().join("categorized.md"),
        "---\ntitle: Categorized\ndate: 2026-03-20\ntype: note\ncategory: tech\ntags:\n  - rust\n---\nBody.\n",
    )
    .expect("write");
    let migration = category_as_tag();

    let first = apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&migration)).expect("first apply");
    assert!(first > 0);
    // A second apply must write ZERO files: the value is already a tag.
    let second = apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&migration)).expect("second apply");
    assert_eq!(second, 0, "migration is not idempotent");
}

#[test]
fn field_to_tags_writes_block_form() {
    let v = TestVault::new();
    apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&category_as_tag())).expect("apply");
    let content = v.read("rust-guide.md");
    assert!(content.contains("tags:\n  - "), "expected block form:\n{content}");
    assert!(!content.contains("tags: ["), "inline form survived:\n{content}");
}

#[test]
fn field_to_tags_skips_excluded_values() {
    let v = TestVault::new();
    std::fs::write(
        v.root().join("junk.md"),
        "---\ntitle: Junk\ndate: 2026-03-20\ntype: note\ncategory: resources\norigin: authored\ntags:\n  - rust\n---\nBody.\n",
    ).expect("write");
    apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&category_as_tag())).expect("apply");
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
    // 350 vault notes quote the value; the writer must strip the quotes and
    // lowercase before writing the tag. No vocabulary is loaded here, so the
    // value is written as-is; the vocabulary filter has its own test below.
    std::fs::write(
        v.root().join("quoted.md"),
        "---\ntitle: Q\ndate: 2026-03-20\ntype: note\ncategory: \"Knowledge\"\norigin: authored\ntags: []\n---\nBody.\n",
    )
    .expect("write");
    apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&category_as_tag())).expect("apply");
    let tags = tags_of(&v, "quoted.md");
    assert_eq!(tags, vec!["knowledge".to_string()], "got {tags:?}");
}

#[test]
fn tags_remove_strips_the_named_tags() {
    let v = TestVault::new();
    let migration = MigrationConfig {
        name: "v5-category-as-tag-undo".to_string(),
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
        "---\ntitle: Full\ndate: 2026-03-20\ntype: note\ncategory: tech\norigin: authored\ntags:\n  - a\n  - b\n---\nBody.\n",
    ).expect("write");
    let notes = v.scan();
    let migration = category_as_tag();
    let report = lint_migrate_selected(&notes, &[&migration], Some(&canon_with(&["tech", "a", "b"], 2)));
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
    // This is the real shipped historical-undo artifact for the P4 migration
    // that already ran against the live vault (its name and content are
    // fixed history, not a demo field name, so this test does not rename it
    // alongside the synthetic `category_as_tag` fixtures above).
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/migrations/v5-domain-as-tag-undo.yml");
    let plan = load_plan(&path).expect("plan parses");
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].name, "v5-domain-as-tag-undo");
    assert_eq!(plan[0].tags_remove.len(), 10, "expected the ten migrated names");
    assert!(plan[0].field_to_tags.is_empty(), "the inverse must not re-add tags");
}

#[test]
fn field_to_tags_normalizes_form_when_the_set_is_unchanged() {
    // The note already carries its own category value, so the tag SET does not
    // change, but its inline list is the wrong on-disk form. Without this the
    // vault keeps two spellings and P4's own success criterion fails.
    let v = TestVault::new();
    std::fs::write(
        v.root().join("already.md"),
        "---\ntitle: A\ndate: 2026-03-20\ntype: note\ncategory: tech\norigin: authored\ntags: [tech, rust]\n---\nBody.\n",
    )
    .expect("write");

    let count = apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&category_as_tag())).expect("apply");
    assert!(count > 0);

    let content = v.read("already.md");
    assert!(
        content.contains("tags:\n  - tech\n  - rust"),
        "not normalized:\n{content}"
    );
    assert!(!content.contains("tags: ["), "inline form survived:\n{content}");

    // Still idempotent: the form is right now, so a second pass writes nothing.
    let again = apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&category_as_tag())).expect("second apply");
    assert_eq!(again, 0, "form normalization broke idempotence");
}

#[test]
fn field_to_tags_leaves_empty_inline_lists_alone() {
    // `tags: []` is already "no tags"; rewriting it to a bare `tags:` swaps one
    // spelling of empty for another and churns every entities/ file.
    let v = TestVault::new();
    std::fs::write(
        v.root().join("empty.md"),
        "---\ntitle: E\ndate: 2026-03-20\ntype: note\norigin: authored\ntags: []\n---\nBody.\n",
    )
    .expect("write");

    apply_migrate(v.root(), &v.scan(), std::slice::from_ref(&category_as_tag())).expect("apply");
    assert!(v.read("empty.md").contains("tags: []"), "empty list was churned");
}

/// B9: with a vocabulary loaded, `field-to-tags` drops a value that is not a
/// canonical tag instead of writing one the next sweep would silently strip.
#[test]
fn field_to_tags_drops_a_value_outside_the_vocabulary() {
    let v = TestVault::new();
    std::fs::write(
        v.root().join("legacy.md"),
        "---\ntitle: L\ndate: 2026-03-20\ntype: note\ncategory: \"Knowledge\"\norigin: authored\ntags: []\n---\nBody.\n",
    )
    .expect("write");
    let migration = category_as_tag();

    let without = canon_with(&["tech"], 8);
    let report = lint_migrate_selected(&v.scan(), &[&migration], Some(&without));
    assert!(
        !report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "legacy.md"),
        "dry-run must not plan a non-canonical tag: {:?}",
        report.violations
    );
    apply_migrate_selected(v.root(), &v.scan(), &[&migration], Some(&without)).expect("apply");
    assert!(tags_of(&v, "legacy.md").is_empty(), "non-canonical value written");

    let with = canon_with(&["tech", "knowledge"], 8);
    apply_migrate_selected(v.root(), &v.scan(), &[&migration], Some(&with)).expect("apply");
    assert_eq!(tags_of(&v, "legacy.md"), vec!["knowledge".to_string()]);
}

/// B3: the dry-run lists a note already carrying two or more of the migrated
/// names, whether or not the transform would change it.
#[test]
fn lint_tag_transforms_flags_a_note_already_carrying_two_migrated_names() {
    let v = TestVault::new();
    // Two notes give the migration two distinct names: tech and music.
    std::fs::write(
        v.root().join("music.md"),
        "---\ntitle: M\ndate: 2026-03-20\ntype: note\ncategory: music\norigin: authored\ntags: []\n---\nBody.\n",
    )
    .expect("write");
    // Already carries both names, and its own value is already present, so
    // the transform itself is a no-op for it.
    std::fs::write(
        v.root().join("both.md"),
        "---\ntitle: B\ndate: 2026-03-20\ntype: note\ncategory: tech\norigin: authored\ntags:\n  - tech\n  - music\n---\nBody.\n",
    )
    .expect("write");
    std::fs::write(
        v.root().join("one.md"),
        "---\ntitle: O\ndate: 2026-03-20\ntype: note\ncategory: tech\norigin: authored\ntags:\n  - music\n---\nBody.\n",
    )
    .expect("write");
    let migration = category_as_tag();
    let report = lint_migrate_selected(&v.scan(), &[&migration], Some(&canon_with(&["tech", "music"], 8)));

    let both = report
        .violations
        .iter()
        .find(|vi| vi.path.to_string_lossy() == "both.md")
        .expect("both.md listed even though nothing changes");
    assert!(
        both.message.contains("ALREADY CARRIES 2 migrated names"),
        "not flagged: {}",
        both.message
    );
    let one = report
        .violations
        .iter()
        .find(|vi| vi.path.to_string_lossy() == "one.md")
        .expect("one.md gains tech");
    assert!(
        !one.message.contains("ALREADY CARRIES"),
        "one migrated name is not two: {}",
        one.message
    );
}
