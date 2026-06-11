use super::*;
use crate::testutil::TestVault;

#[test]
fn find_mention_handles_length_changing_lowercase() {
    // 'İ' (2 bytes) lowercases to "i̇" (3 bytes), so a `body.to_lowercase()`
    // match offset is NOT a valid index into `body`. The old code sliced
    // `body` with that offset and could panic / extract a shifted span.
    let body = "İstanbul notes mention Rust here.";
    let (context, surface) = LoweredBody::new(body).find_mention("Rust", "rust", 3).expect("match");
    // Surface must be the original-case word, not a byte-shifted slice.
    assert_eq!(surface, "Rust");
    assert!(context.contains("Rust"));
}

#[test]
fn find_mention_no_panic_on_multibyte_before_match() {
    // Many length-changing chars before the match must not panic.
    let body = format!("{} discusses Rust extensively", "İ".repeat(50));
    let result = LoweredBody::new(&body).find_mention("Rust", "rust", 3);
    assert_eq!(result.map(|(_, s)| s), Some("Rust".to_string()));
}

#[test]
fn test_concept_linking_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.linking;

    let report = lint_linking(&notes, &config);
    // rust-guide.md body mentions "Python Guide" - should suggest linking
    assert!(
        report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "rust-guide.md"
                && vi.rule == "linking.concept"
                && vi.message.contains("Python Guide"))
    );
}

#[test]
fn test_person_entity_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.linking;

    let report = lint_linking(&notes, &config);
    // daily-standup.md mentions "John Smith"
    assert!(
        report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "daily-standup.md" && vi.rule == "linking.person")
    );
}

#[test]
fn test_already_linked_not_suggested() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.linking;

    let report = lint_linking(&notes, &config);
    // python-guide.md already has [[rust-guide]] - should NOT suggest it again
    assert!(
        !report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "python-guide.md"
                && vi.rule == "linking.concept"
                && vi.message.contains("rust-guide"))
    );
}

#[test]
fn test_insert_first_wikilink() {
    let content = "Working on obsidian-cortex and obsidian-cortex improvements.";
    let result = insert_first_wikilink(content, "obsidian-cortex", "obsidian-cortex");
    assert!(result.is_some());
    let result = result.expect("should have result");
    assert!(result.starts_with("Working on [[obsidian-cortex]]"));
    assert_eq!(result.matches("[[").count(), 1);
}

#[test]
fn test_insert_first_wikilink_skips_frontmatter() {
    let content = "---\ntitle: i replaced commands with one python script\ntype: article\n---\n\nThis article about python is great.";
    let result = insert_first_wikilink(content, "python", "python");
    assert!(result.is_some());
    let result = result.expect("should have result");
    // Must NOT modify frontmatter title
    assert!(result.contains("title: i replaced commands with one python script"));
    // Must wrap the body occurrence
    assert!(result.contains("about [[python]] is"));
}

#[test]
fn test_insert_first_wikilink_no_frontmatter() {
    let content = "Just a body with python mentioned.";
    let result = insert_first_wikilink(content, "python", "python");
    assert!(result.is_some());
    assert!(result.unwrap().contains("[[python]]"));
}

#[test]
fn test_extract_existing_links() {
    let body = "See [[note-a]] and [[note-b|display]].";
    let links = extract_existing_links(body);
    assert!(links.contains("note-a"));
    assert!(links.contains("note-b"));
}

// --- Phase 2: glossary concepts + piped alias links ---

#[test]
fn insert_first_wikilink_pipes_alias_to_slug() {
    let content = "We rely on Retrieval-Augmented Generation here.";
    let result = insert_first_wikilink(content, "rag", "Retrieval-Augmented Generation").expect("link");
    assert!(
        result.contains("[[rag|Retrieval-Augmented Generation]]"),
        "piped link preserves prose surface; got {result}"
    );
}

#[test]
fn insert_first_wikilink_pipes_when_only_case_differs() {
    let content = "We use LangChain daily.";
    // surface "LangChain" differs from slug "langchain" only in case -> piped.
    let result = insert_first_wikilink(content, "langchain", "LangChain").expect("link");
    assert!(result.contains("[[langchain|LangChain]]"), "got {result}");
}

#[test]
fn insert_first_wikilink_plain_when_surface_equals_target() {
    let content = "About python here.";
    let result = insert_first_wikilink(content, "python", "python").expect("link");
    assert!(result.contains("[[python]]"));
    assert!(!result.contains('|'), "no pipe when surface == target");
}

fn glossary_config(concepts: &[&str], aliases: &[(&str, &str)]) -> LinkingConfig {
    let mut cfg = LinkingConfig {
        scan_for: vec!["concepts".to_string()],
        min_word_length: 3,
        ..Default::default()
    };
    cfg.entities.concepts = concepts.iter().map(|s| s.to_string()).collect();
    cfg.aliases = aliases.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    cfg
}

fn note_with_body(path: &str, body: &str) -> Note {
    Note {
        path: std::path::PathBuf::from(path),
        frontmatter: crate::vault::Frontmatter::default(),
        body: body.to_string(),
        raw: body.to_string(),
    }
}

#[test]
fn glossary_concept_is_flagged_for_linking() {
    let cfg = glossary_config(&["langchain"], &[]);
    let notes = vec![note_with_body("notes/x.md", "We use LangChain in production.")];
    let report = lint_linking(&notes, &cfg);
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule == "linking.glossary" && v.message.contains("langchain")),
        "glossary concept mention flagged"
    );
}

#[test]
fn alias_is_flagged_as_piped_link() {
    let cfg = glossary_config(&[], &[("Retrieval-Augmented Generation", "rag")]);
    let notes = vec![note_with_body(
        "notes/x.md",
        "Retrieval-Augmented Generation is everywhere.",
    )];
    let report = lint_linking(&notes, &cfg);
    let v = report
        .violations
        .iter()
        .find(|v| v.rule == "linking.alias")
        .expect("alias violation");
    match &v.fix {
        Some(Fix::AddWikilink { target, surface, .. }) => {
            assert_eq!(target, "rag");
            assert_eq!(surface, "Retrieval-Augmented Generation");
        }
        other => panic!("expected AddWikilink, got {other:?}"),
    }
}

#[test]
fn glossary_does_not_double_link_existing() {
    let cfg = glossary_config(&["langchain"], &[]);
    // Body already links it -> no new violation.
    let notes = vec![note_with_body("notes/x.md", "We use [[langchain]] here.")];
    let report = lint_linking(&notes, &cfg);
    assert!(
        !report.violations.iter().any(|v| v.rule == "linking.glossary"),
        "already-linked concept is not re-flagged"
    );
}

#[test]
fn glossary_does_not_self_link_hub_note() {
    let cfg = glossary_config(&["langchain"], &[]);
    // The note IS the langchain hub note (stem == slug) -> never self-link.
    let notes = vec![note_with_body("notes/langchain.md", "LangChain is a framework.")];
    let report = lint_linking(&notes, &cfg);
    assert!(
        !report.violations.iter().any(|v| v.rule == "linking.glossary"),
        "a concept's own hub note is never self-linked"
    );
}

#[test]
fn load_glossary_missing_file_is_empty() {
    let g = load_glossary(std::path::Path::new("/nonexistent/glossary.yml")).expect("ok");
    assert!(g.concepts.is_empty());
    assert!(g.aliases.is_empty());
}
