use super::*;

fn test_canonical_hashset() -> HashSet<String> {
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

fn test_canonical_set() -> CanonicalSet {
    CanonicalSet {
        all: test_canonical_hashset(),
        no_segment: HashSet::new(),
        no_classifier: HashSet::new(),
        max_per_note: 7,
    }
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
    let canonical = test_canonical_hashset();
    assert!(is_concatenated_word("claudecodeai", &canonical)); // claude + ai
    assert!(is_concatenated_word("aiagents", &canonical)); // ai + agents
    assert!(is_concatenated_word("rustpython", &canonical)); // rust + python
}

#[test]
fn test_hyphenated_tag_not_concatenated() {
    let canonical = test_canonical_hashset();
    assert!(!is_concatenated_word("claude-code", &canonical));
    assert!(!is_concatenated_word("ai-agents", &canonical));
}

#[test]
fn test_single_word_not_concatenated() {
    let canonical = test_canonical_hashset();
    assert!(!is_concatenated_word("infrastructure", &canonical));
    assert!(!is_concatenated_word("worldbuilding", &canonical));
}

#[test]
fn test_short_tag_not_concatenated() {
    let canonical = test_canonical_hashset();
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
    let result = filter_and_cap(&raw, &canonical, &mapping);
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
    let result = filter_and_cap(&raw, &canonical, &mapping);
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
    let result = filter_and_cap(&raw, &canonical, &mapping);
    assert_eq!(result.len(), 7);
}

#[test]
fn test_filter_rejected_tags_excluded() {
    let canonical = test_canonical_set();
    let mapping = test_mapping();
    let raw = vec!["claudecodeai".to_string(), "rust".to_string()];
    let result = filter_and_cap(&raw, &canonical, &mapping);
    assert_eq!(result, vec!["rust"]);
}

// ---- no-segment-match guard (Phase 1) ----

#[test]
fn segment_match_skips_no_segment_tags_in_both_matchers() {
    let canonical = CanonicalSet {
        all: ["work", "life", "rust", "cli"].iter().map(|s| s.to_string()).collect(),
        no_segment: ["work", "life"].iter().map(|s| s.to_string()).collect(),
        no_classifier: HashSet::new(),
        max_per_note: 7,
    };
    let mapping = TagMapping::new();

    // Guarded segments never mint, in either matcher.
    assert_eq!(
        match_to_canonical("work-life-balance", &canonical, &mapping),
        Vec::<String>::new()
    );
    let raw = vec!["work-life-balance".to_string()];
    assert_eq!(filter_and_cap(&raw, &canonical, &mapping), Vec::<String>::new());

    // Unguarded segments still mint as today.
    let mut result = match_to_canonical("rust-cli-tooling", &canonical, &mapping);
    result.sort();
    assert_eq!(result, vec!["cli".to_string(), "rust".to_string()]);
    let raw = vec!["rust-cli-tooling".to_string()];
    let mut result = filter_and_cap(&raw, &canonical, &mapping);
    result.sort();
    assert_eq!(result, vec!["cli".to_string(), "rust".to_string()]);
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

// ---- protect-aware capping (implementation audit r1, M1) ----

fn protecting_set(protected: &[&str], max_per_note: usize) -> CanonicalSet {
    CanonicalSet {
        all: ["ai", "claude", "llm", "rust", "work"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        no_segment: HashSet::new(),
        no_classifier: protected.iter().map(|s| (*s).to_string()).collect(),
        max_per_note,
    }
}

/// `filter_and_cap` sorts within-tier alphabetically and used to truncate with
/// no knowledge of the protect list, so a cortex sweep dropped `work` off the
/// end of an over-cap note. `work` sorts last here on purpose.
#[test]
fn filter_and_cap_keeps_a_protected_tag_over_the_cap() {
    let canonical = protecting_set(&["work"], 3);
    let mapping = TagMapping::new();
    let raw: Vec<String> = ["ai", "claude", "llm", "work"].iter().map(|s| s.to_string()).collect();

    let result = filter_and_cap(&raw, &canonical, &mapping);
    assert_eq!(result.len(), 3, "the cap is still hard: {result:?}");
    assert!(
        result.contains(&"work".to_string()),
        "protected tag dropped: {result:?}"
    );
    assert_eq!(
        result,
        vec!["ai".to_string(), "claude".to_string(), "work".to_string()],
        "priority order survives the protect-aware cap"
    );
}

/// Under the cap the helper is the identity, so no note is reordered and no
/// vault-wide rewrite churn is introduced.
#[test]
fn cap_protecting_is_the_identity_under_the_cap() {
    let protected: HashSet<String> = ["work".to_string()].into_iter().collect();
    let tags: Vec<String> = ["ai", "work", "rust"].iter().map(|s| s.to_string()).collect();
    assert_eq!(cap_protecting(tags.clone(), 8, &protected), tags);
}

/// Edge: more protected tags than the cap allows. The cap wins - a note over
/// `max-per-note` fails `tags.cap` lint either way.
#[test]
fn cap_protecting_keeps_the_cap_hard_when_every_tag_is_protected() {
    let protected: HashSet<String> = ["work", "life", "diy"].iter().map(|s| (*s).to_string()).collect();
    let tags: Vec<String> = ["work", "life", "diy"].iter().map(|s| (*s).to_string()).collect();
    let result = cap_protecting(tags, 2, &protected);
    assert_eq!(result, vec!["work".to_string(), "life".to_string()]);
}

// ---- shipped vocabulary + strict parsing (review fold C7, C8) ----

/// The shipped file, byte for byte. Path is relative to this source file
/// (`vault/src/canonical/tests.rs` -> `config/canonical-tags.yml`).
const SHIPPED_CANONICAL_TAGS: &str = include_str!("../../../config/canonical-tags.yml");

#[test]
fn default_max_per_note_matches_the_design_and_the_shipped_file() {
    let file: CanonicalTagsFile = serde_yaml::from_str("tags: {}").expect("minimal file parses");
    assert_eq!(file.max_per_note, 8, "default is the design's cap");
    let shipped: CanonicalTagsFile = serde_yaml::from_str(SHIPPED_CANONICAL_TAGS).expect("shipped parses");
    assert_eq!(shipped.max_per_note, file.max_per_note, "default and shipped agree");
}

/// A mistyped protect-list key must be a parse error, not an empty protect
/// list that silently lets every capping path drop `work`/`life`/...
#[test]
fn canonical_tags_file_rejects_an_unknown_key() {
    let yaml = "max-per-note: 8\nno-classifer-tags:\n  - work\ntags: {}\n";
    let err = serde_yaml::from_str::<CanonicalTagsFile>(yaml).expect_err("typo must not parse");
    assert!(
        format!("{err}").contains("no-classifer-tags"),
        "error names the offending key: {err}"
    );
}

#[test]
fn shipped_canonical_tags_file_parses_and_holds_its_invariants() {
    let file: CanonicalTagsFile = serde_yaml::from_str(SHIPPED_CANONICAL_TAGS).expect("shipped file parses");
    assert_eq!(file.max_per_note, 8);
    assert_eq!(file.no_segment_match.len(), 5, "{:?}", file.no_segment_match);
    assert_eq!(file.no_classifier_tags.len(), 5, "{:?}", file.no_classifier_tags);

    let mut flat: Vec<&String> = file.tags.values().flatten().collect();
    assert_eq!(flat.len(), 117, "117 canonical tags");
    let before = flat.len();
    flat.sort();
    flat.dedup();
    assert_eq!(flat.len(), before, "no tag appears under two groups");

    let is_kebab = |t: &str| {
        !t.is_empty()
            && t.split('-')
                .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
    };
    for tag in &flat {
        assert!(is_kebab(tag), "tag {tag:?} is not ^[a-z0-9]+(-[a-z0-9]+)*$");
    }

    let all = file.all_tags();
    for guarded in file.no_segment_match.iter().chain(&file.no_classifier_tags) {
        assert!(
            all.contains(guarded),
            "guard-list tag {guarded:?} is not in the vocabulary"
        );
    }
}
