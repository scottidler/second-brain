use eyre::{Result, WrapErr};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CanonicalTagsFile {
    #[serde(default = "default_max_per_note")]
    pub max_per_note: usize,
    #[serde(default = "default_max_canonical")]
    pub max_canonical: usize,
    pub tags: HashMap<String, Vec<String>>,
}

fn default_max_per_note() -> usize {
    7
}

fn default_max_canonical() -> usize {
    300
}

impl CanonicalTagsFile {
    pub fn load(path: &Path) -> Result<Self> {
        let expanded = shellexpand::tilde(&path.to_string_lossy()).to_string();
        let content = std::fs::read_to_string(&expanded)
            .wrap_err_with(|| format!("failed to read canonical tags from {expanded}"))?;
        let file: Self = serde_yaml::from_str(&content).wrap_err("failed to parse canonical tags YAML")?;
        Ok(file)
    }

    pub fn all_tags(&self) -> HashSet<String> {
        self.tags.values().flatten().cloned().collect()
    }
}

pub type TagMapping = HashMap<String, Option<String>>;

pub fn load_tag_mapping(path: &Path) -> Result<TagMapping> {
    let expanded = shellexpand::tilde(&path.to_string_lossy()).to_string();
    let content =
        std::fs::read_to_string(&expanded).wrap_err_with(|| format!("failed to read tag mapping from {expanded}"))?;
    let mapping: TagMapping = serde_yaml::from_str(&content).wrap_err("failed to parse tag mapping YAML")?;
    Ok(mapping)
}

/// Check if a tag is a concatenated word (no hyphens, contains 2+ canonical substrings).
pub fn is_concatenated_word(tag: &str, canonical_set: &HashSet<String>) -> bool {
    if tag.contains('-') {
        return false;
    }

    // Build substring dictionary from canonical tags (strip hyphens), sorted
    // longest-first so the longest substrings match first. (Built from the
    // `canonical_set` parameter, so it can't be a module `LazyLock`; the
    // redundant clone the old code made before sorting is dropped.)
    let mut substrs: Vec<String> = canonical_set
        .iter()
        .map(|t| t.replace('-', ""))
        .filter(|s| s.len() >= 2) // skip very short ones to avoid false matches
        .collect();
    substrs.sort_by_key(|b| std::cmp::Reverse(b.len()));

    // Count non-overlapping substring matches
    let mut matches = 0;
    let mut remaining = tag.to_string();

    for substr in &substrs {
        if remaining.contains(substr.as_str()) {
            remaining = remaining.replacen(substr.as_str(), "", 1);
            matches += 1;
            if matches >= 2 {
                return true;
            }
        }
    }

    false
}

/// Match a raw tag to canonical tag(s).
///
/// Returns zero, one, or multiple canonical tags.
/// Priority: mapping file -> exact canonical match -> segment fuzzy match.
pub fn match_to_canonical(raw_tag: &str, canonical_set: &HashSet<String>, mapping: &TagMapping) -> Vec<String> {
    // 1. Mapping file lookup
    if let Some(mapped) = mapping.get(raw_tag) {
        return match mapped {
            Some(canonical) => vec![canonical.clone()],
            None => vec![], // explicitly rejected
        };
    }

    // 2. Exact canonical match
    if canonical_set.contains(raw_tag) {
        return vec![raw_tag.to_string()];
    }

    // 3. Segment fuzzy match (single-word canonical tags only)
    let segments: Vec<&str> = raw_tag.split('-').collect();
    let matches: Vec<String> = segments
        .iter()
        .filter(|seg| !seg.is_empty())
        .filter_map(
            |seg| {
                if canonical_set.contains(*seg) { Some(seg.to_string()) } else { None }
            },
        )
        .collect();

    if !matches.is_empty() {
        return matches;
    }

    // 4. No match
    vec![]
}

/// Filter raw tags through canonical vocabulary and cap the result.
///
/// Returns deduplicated canonical tags, capped at max_per_note.
/// Tags from mapping hits come first, then exact matches, then segment matches.
pub fn filter_and_cap(
    raw_tags: &[String],
    canonical_set: &HashSet<String>,
    mapping: &TagMapping,
    max_per_note: usize,
) -> Vec<String> {
    let mut mapping_hits = Vec::new();
    let mut exact_hits = Vec::new();
    let mut segment_hits = Vec::new();

    for raw in raw_tags {
        // Check mapping first
        if let Some(mapped) = mapping.get(raw.as_str()) {
            if let Some(canonical) = mapped {
                mapping_hits.push(canonical.clone());
            }
            continue;
        }

        // Exact canonical match
        if canonical_set.contains(raw.as_str()) {
            exact_hits.push(raw.clone());
            continue;
        }

        // Segment fuzzy match
        let segments: Vec<&str> = raw.split('-').collect();
        for seg in segments {
            if !seg.is_empty() && canonical_set.contains(seg) {
                segment_hits.push(seg.to_string());
            }
        }
    }

    // Combine in priority order, dedup (keep the highest-priority occurrence
    // of each tag). Tag the tier so the sort below is WITHIN-tier only: a plain
    // `result.sort()` reorders the whole list alphabetically, so `truncate`
    // would keep the alphabetically-first tags instead of the highest-priority
    // ones - destroying the documented mapping > exact > segment priority.
    let mut seen = HashSet::new();
    let mut tiered: Vec<(u8, String)> = Vec::new();
    for (tier, tag) in mapping_hits
        .into_iter()
        .map(|t| (0u8, t))
        .chain(exact_hits.into_iter().map(|t| (1u8, t)))
        .chain(segment_hits.into_iter().map(|t| (2u8, t)))
    {
        if seen.insert(tag.clone()) {
            tiered.push((tier, tag));
        }
    }

    // (tier, tag): tiers stay ordered, within-tier is alphabetical (deterministic).
    tiered.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    tiered.truncate(max_per_note);
    tiered.into_iter().map(|(_, tag)| tag).collect()
}

#[cfg(test)]
mod tests {
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
}
