use super::*;
use crate::testutil::{NoteBuilder, TestVault};

fn default_config() -> AutoTagConfig {
    AutoTagConfig {
        enabled: true,
        min_tags_threshold: 3,
        canonical_tags: vec![
            "rust".to_string(),
            "python".to_string(),
            "automation".to_string(),
            "cli-tools".to_string(),
        ],
        fabric_pattern: None,
        auto_derive_top_n: 50,
        max_input_tokens: 50000,
        fabric_timeout_secs: 30,
    }
}

#[test]
fn test_suggest_tags_deterministic() {
    let canonical = vec!["rust".to_string(), "python".to_string(), "automation".to_string()];
    let note = NoteBuilder::new("test.md")
        .title("Test")
        .tags(&["python"])
        .body("This is about rust and automation tools.")
        .build();

    let suggestions = suggest_tags_deterministic(&note.body, &canonical, &note);
    assert!(suggestions.contains(&"rust".to_string()));
    assert!(suggestions.contains(&"automation".to_string()));
    assert!(
        !suggestions.contains(&"python".to_string()),
        "existing tag should not be suggested"
    );
}

#[test]
fn test_lint_autotag_on_vault() {
    let v = TestVault::new();
    // Add a note with few tags and origin: assisted
    v.add_note(
            "needs-tags.md",
            "---\ntitle: Needs Tags\ndate: 2026-03-18\ntype: note\ndomain: tech\norigin: assisted\nstatus: unread\ntags: []\n---\nThis article discusses rust programming and python scripting for automation.\n",
        );
    let notes = v.scan();
    let config = default_config();

    let report = lint_autotag(&notes, &notes, &config);
    let vi = report
        .violations
        .iter()
        .find(|v| v.path.to_string_lossy().contains("needs-tags"));
    assert!(vi.is_some(), "should suggest tags for note with few tags");
    assert!(vi.unwrap().message.contains("rust"));
}

#[test]
fn test_lint_autotag_skips_already_tagged() {
    let notes = vec![
        NoteBuilder::new("tagged.md")
            .title("Tagged")
            .note_type("note")
            .origin("assisted")
            .status("unread")
            .extra("cortex-tagged", serde_yaml::Value::Bool(true))
            .body("This is about rust programming.")
            .build(),
    ];
    let config = default_config();

    let report = lint_autotag(&notes, &notes, &config);
    assert!(report.is_empty(), "should skip already tagged notes");
}

#[test]
fn test_lint_autotag_skips_notes_with_enough_tags() {
    let notes = vec![
        NoteBuilder::new("enough.md")
            .title("Enough")
            .note_type("note")
            .origin("assisted")
            .status("unread")
            .tags(&["rust", "programming", "cli"])
            .body("This is about rust programming.")
            .build(),
    ];
    let config = default_config();

    let report = lint_autotag(&notes, &notes, &config);
    assert!(report.is_empty(), "should skip notes with enough tags");
}

#[test]
fn test_build_canonical_from_config() {
    let config = default_config();
    let notes = vec![];
    let canonical = build_canonical_tags(&notes, &config);
    assert_eq!(canonical, config.canonical_tags);
}

#[test]
fn test_build_canonical_auto_derive() {
    let config = AutoTagConfig {
        canonical_tags: vec![], // Empty - should auto-derive
        auto_derive_top_n: 3,
        ..default_config()
    };
    let notes = vec![
        NoteBuilder::new("a.md").tags(&["rust", "python"]).build(),
        NoteBuilder::new("b.md").tags(&["rust", "cli"]).build(),
        NoteBuilder::new("c.md").tags(&["rust", "python", "ai"]).build(),
    ];

    let canonical = build_canonical_tags(&notes, &config);
    assert_eq!(canonical.len(), 3);
    assert_eq!(canonical[0], "rust"); // Most frequent
}

#[test]
fn test_apply_autotag_on_vault() {
    let v = TestVault::new();
    v.add_note(
            "tag-me.md",
            "---\ntitle: Tag Me\ndate: 2026-03-18\ntype: note\ndomain: tech\norigin: assisted\nstatus: unread\ntags: []\n---\nThis discusses rust programming and python automation heavily.\n",
        );
    let notes = v.scan();
    let config = default_config();

    let fabric = FabricConfig::default();
    let changed = apply_autotag(v.root(), &notes, &notes, &config, &fabric).expect("apply");
    assert!(!changed.is_empty());

    let content = v.read("tag-me.md");
    assert!(content.contains("cortex-suggested-tags:"));
    assert!(content.contains("cortex-tagged:"));
}

#[test]
fn test_disabled_config() {
    let config = AutoTagConfig {
        enabled: false,
        ..default_config()
    };
    let notes = vec![
        NoteBuilder::new("test.md")
            .title("Test")
            .origin("assisted")
            .status("unread")
            .body("rust programming")
            .build(),
    ];

    let report = lint_autotag(&notes, &notes, &config);
    assert!(report.is_empty());
}
