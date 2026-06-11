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
mod tests;
