use super::*;

fn test_canonical_set() -> HashSet<String> {
    [
        "ai",
        "agents",
        "claude",
        "rust",
        "python",
        "obsidian",
        "football",
        "coaching",
        "offense",
        "drills",
        "prompt-engineering",
        "mcp",
        "llm",
        "security",
        "gaming",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn test_mapping() -> TagMapping {
    let mut m = HashMap::new();
    m.insert("ai-agents".to_string(), Some("agents".to_string()));
    m.insert("ai-coding".to_string(), Some("ai".to_string()));
    m.insert("claudecodeai".to_string(), None); // rejected
    m.insert("large-language-models".to_string(), Some("llm".to_string()));
    m.insert("prompt-engineering".to_string(), Some("prompt-engineering".to_string()));
    m
}

// ---- is_concatenated_word ----

#[test]
fn test_concatenated_word_detected() {
    let canonical = test_canonical_set();
    assert!(is_concatenated_word("claudecodeai", &canonical)); // claude + ai
    assert!(is_concatenated_word("aiagents", &canonical)); // ai + agents
    assert!(is_concatenated_word("rustpython", &canonical)); // rust + python
}

#[test]
fn test_hyphenated_tag_not_concatenated() {
    let canonical = test_canonical_set();
    assert!(!is_concatenated_word("claude-code", &canonical));
    assert!(!is_concatenated_word("ai-agents", &canonical));
}

#[test]
fn test_single_word_not_concatenated() {
    let canonical = test_canonical_set();
    assert!(!is_concatenated_word("infrastructure", &canonical));
    assert!(!is_concatenated_word("worldbuilding", &canonical));
}

#[test]
fn test_short_tag_not_concatenated() {
    let canonical = test_canonical_set();
    assert!(!is_concatenated_word("rust", &canonical));
    assert!(!is_concatenated_word("ai", &canonical));
}

// ---- match_to_canonical ----

#[test]
fn test_match_mapping_hit() {
    let canonical = test_canonical_set();
    let mapping = test_mapping();
    assert_eq!(match_to_canonical("ai-agents", &canonical, &mapping), vec!["agents"]);
}

#[test]
fn test_match_mapping_rejection() {
    let canonical = test_canonical_set();
    let mapping = test_mapping();
    let result = match_to_canonical("claudecodeai", &canonical, &mapping);
    assert!(result.is_empty());
}

#[test]
fn test_match_exact_canonical() {
    let canonical = test_canonical_set();
    let mapping = test_mapping();
    assert_eq!(match_to_canonical("rust", &canonical, &mapping), vec!["rust"]);
}

#[test]
fn test_match_segment_fuzzy() {
    let canonical = test_canonical_set();
    let mapping = test_mapping();
    // "ai-coding-agents" not in mapping, segments "ai", "coding", "agents"
    // "ai" and "agents" are canonical, "coding" is not
    let result = match_to_canonical("ai-coding-agents", &canonical, &mapping);
    assert!(result.contains(&"ai".to_string()));
    assert!(result.contains(&"agents".to_string()));
    assert!(!result.contains(&"coding".to_string()));
}

#[test]
fn test_match_no_match() {
    let canonical = test_canonical_set();
    let mapping = test_mapping();
    let result = match_to_canonical("completely-unknown-topic", &canonical, &mapping);
    assert!(result.is_empty());
}

#[test]
fn test_match_multi_word_canonical_exact() {
    let canonical = test_canonical_set();
    let mapping = test_mapping();
    // prompt-engineering is in mapping -> prompt-engineering
    assert_eq!(
        match_to_canonical("prompt-engineering", &canonical, &mapping),
        vec!["prompt-engineering"]
    );
}

// ---- filter_and_cap ----

#[test]
fn test_filter_basic() {
    let canonical = test_canonical_set();
    let mapping = test_mapping();
    let raw = vec!["ai-agents".to_string(), "rust".to_string(), "unknown-junk".to_string()];
    let result = filter_and_cap(&raw, &canonical, &mapping, 7);
    assert!(result.contains(&"agents".to_string()));
    assert!(result.contains(&"rust".to_string()));
    assert!(!result.iter().any(|t| t.contains("unknown")));
}

#[test]
fn test_filter_dedup() {
    let canonical = test_canonical_set();
    let mapping = test_mapping();
    // Both map to "agents" via mapping
    let raw = vec!["ai-agents".to_string(), "ai-coding-agents".to_string()];
    let result = filter_and_cap(&raw, &canonical, &mapping, 7);
    assert_eq!(result.iter().filter(|t| *t == "agents").count(), 1);
}

#[test]
fn test_filter_cap() {
    let canonical = test_canonical_set();
    let mapping = test_mapping();
    let raw = vec![
        "ai-agents".to_string(),
        "rust".to_string(),
        "python".to_string(),
        "obsidian".to_string(),
        "football".to_string(),
        "coaching".to_string(),
        "offense".to_string(),
        "drills".to_string(),
        "security".to_string(),
        "gaming".to_string(),
    ];
    let result = filter_and_cap(&raw, &canonical, &mapping, 7);
    assert_eq!(result.len(), 7);
}

#[test]
fn test_filter_rejected_tags_excluded() {
    let canonical = test_canonical_set();
    let mapping = test_mapping();
    let raw = vec!["claudecodeai".to_string(), "rust".to_string()];
    let result = filter_and_cap(&raw, &canonical, &mapping, 7);
    assert_eq!(result, vec!["rust"]);
}

// ---- CanonicalTagsFile ----

#[test]
fn test_parse_canonical_tags_yaml() {
    let yaml = r#"
max-per-note: 7
max-canonical: 300
tags:
  ai:
    - ai
    - claude
    - llm
  tech:
    - rust
    - python
"#;
    let file: CanonicalTagsFile = serde_yaml::from_str(yaml).expect("parse failed");
    assert_eq!(file.max_per_note, 7);
    assert_eq!(file.max_canonical, 300);
    let all = file.all_tags();
    assert_eq!(all.len(), 5);
    assert!(all.contains("ai"));
    assert!(all.contains("rust"));
}

#[test]
fn test_load_tag_mapping_yaml() {
    let yaml = "ai-agents: agents\nclaudecodeai: null\nrust: rust\n";
    let mapping: TagMapping = serde_yaml::from_str(yaml).expect("parse failed");
    assert_eq!(mapping.get("ai-agents"), Some(&Some("agents".to_string())));
    assert_eq!(mapping.get("claudecodeai"), Some(&None));
    assert_eq!(mapping.get("rust"), Some(&Some("rust".to_string())));
}

// ---- group keys <-> Domain parity ----

// The `tags:` map's keys are documentation groupings, not read by any consumer
// (`all_tags()` flattens across groups); this test is what keeps them pinned
// to `Domain` so a missing/renamed group can't drift silently again.
#[test]
fn test_canonical_tags_groups_match_domain() {
    use crate::schema::Domain;
    use std::str::FromStr;

    let yaml = include_str!("../../../config/canonical-tags.yml");
    let file: CanonicalTagsFile = serde_yaml::from_str(yaml).expect("parse failed");

    for key in file.tags.keys() {
        Domain::from_str(key).unwrap_or_else(|_| panic!("group key '{key}' is not a valid Domain variant"));
    }

    for domain in Domain::all() {
        let key = domain.as_str();
        assert!(
            file.tags.contains_key(key),
            "Domain::{domain:?} (\"{key}\") has no group key in canonical-tags.yml"
        );
    }
}
