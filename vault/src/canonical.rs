use eyre::{Result, WrapErr};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// `canonical-tags.yml`. Unknown keys are an error: a mistyped
/// `no-classifer-tags:` would otherwise parse as an absent (empty) protect
/// list and every capping path would drop the tags it was meant to keep.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CanonicalTagsFile {
    #[serde(default = "default_max_per_note")]
    pub max_per_note: usize,
    #[serde(default = "default_max_canonical")]
    pub max_canonical: usize,
    /// Tags tier-2 segment matching may never mint on its own (e.g. `work` and
    /// `life` out of raw `work-life-balance`). Absent in files written before
    /// this field existed, hence the default.
    #[serde(default)]
    pub no_segment_match: Vec<String>,
    /// Tags no capping path may drop: the classifier recovers 0 of 17 of them
    /// from note text (design doc P0b). Reaches every consumer through
    /// `CanonicalSet::no_classifier`.
    #[serde(default)]
    pub no_classifier_tags: Vec<String>,
    pub tags: HashMap<String, Vec<String>>,
}

/// The loaded canonical-tag vocabulary shared by borg, cortex, and (from
/// Phase 2) distillers: the flattened tag set, the segment-guard list, the
/// protect list, and the per-note cap. Absorbs borg's former private
/// `CanonicalState` (`borg/src/pipeline.rs`) so every caller loads one
/// vocabulary shape.
#[derive(Debug, Clone)]
pub struct CanonicalSet {
    pub all: HashSet<String>,
    pub no_segment: HashSet<String>,
    /// `no-classifier-tags`: tags no capping path may drop. Carried on the
    /// snapshot rather than passed alongside it because every consumer that
    /// caps (`filter_and_cap` here, `distillers::tags::apply_retag`) already
    /// holds a `CanonicalSet`, and a protect list held next to the vocabulary
    /// instead of in it is how sweep came to cap without one.
    pub no_classifier: HashSet<String>,
    pub max_per_note: usize,
}

/// Matches `max-per-note` in the shipped `config/canonical-tags.yml`
/// (`shipped_canonical_tags_file_parses_and_holds_its_invariants` pins both).
fn default_max_per_note() -> usize {
    8
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

    /// Build the shared `CanonicalSet` snapshot every matcher takes.
    pub fn canonical_set(&self) -> CanonicalSet {
        CanonicalSet {
            all: self.all_tags(),
            no_segment: self.no_segment_match.iter().cloned().collect(),
            no_classifier: self.no_classifier_tags.iter().cloned().collect(),
            max_per_note: self.max_per_note,
        }
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
pub fn match_to_canonical(raw_tag: &str, canon: &CanonicalSet, mapping: &TagMapping) -> Vec<String> {
    // 1. Mapping file lookup
    if let Some(mapped) = mapping.get(raw_tag) {
        return match mapped {
            Some(canonical) => vec![canonical.clone()],
            None => vec![], // explicitly rejected
        };
    }

    // 2. Exact canonical match
    if canon.all.contains(raw_tag) {
        return vec![raw_tag.to_string()];
    }

    // 3. Segment fuzzy match (single-word canonical tags only), skipping any
    // segment on the no-segment-match guard list.
    let segments: Vec<&str> = raw_tag.split('-').collect();
    let matches: Vec<String> = segments
        .iter()
        .filter(|seg| !seg.is_empty())
        .filter_map(|seg| {
            if canon.all.contains(*seg) && !canon.no_segment.contains(*seg) {
                Some(seg.to_string())
            } else {
                None
            }
        })
        .collect();

    if !matches.is_empty() {
        return matches;
    }

    // 4. No match
    vec![]
}

/// True when `raw_tag` carries an explicit `null` entry in the mapping file,
/// i.e. a human already rejected it. `match_to_canonical` returns an empty
/// vec for BOTH this and "no match at all"; a proposal scanner must not
/// conflate them or it re-proposes every rejection forever.
pub fn is_rejected(raw_tag: &str, mapping: &TagMapping) -> bool {
    matches!(mapping.get(raw_tag), Some(None))
}

/// Filter raw tags through canonical vocabulary and cap the result.
///
/// Returns deduplicated canonical tags, capped at max_per_note.
/// Tags from mapping hits come first, then exact matches, then segment matches.
pub fn filter_and_cap(raw_tags: &[String], canon: &CanonicalSet, mapping: &TagMapping) -> Vec<String> {
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
        if canon.all.contains(raw.as_str()) {
            exact_hits.push(raw.clone());
            continue;
        }

        // Segment fuzzy match, skipping any segment on the no-segment-match
        // guard list (this is the duplicate of `match_to_canonical`'s tier-2
        // loop that borg and sweep call through this function).
        let segments: Vec<&str> = raw.split('-').collect();
        for seg in segments {
            if !seg.is_empty() && canon.all.contains(seg) && !canon.no_segment.contains(seg) {
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
    let ordered: Vec<String> = tiered.into_iter().map(|(_, tag)| tag).collect();
    cap_protecting(ordered, canon.max_per_note, &canon.no_classifier)
}

/// Cap a tag list at `max_per_note` without dropping a protected tag.
///
/// The input order is the caller's priority order and survives untouched;
/// only *which* tags survive changes. Over the cap, protected tags
/// (`no-classifier-tags`) claim slots first, then the rest in order. Under
/// the cap this is the identity, so no vault-wide reordering churn.
///
/// The cap stays hard: protected tags beyond `max_per_note` are still
/// dropped, because a note over the cap fails `tags.cap` lint either way.
pub fn cap_protecting(tags: Vec<String>, max_per_note: usize, protected: &HashSet<String>) -> Vec<String> {
    if tags.len() <= max_per_note {
        return tags;
    }
    let mut keep = vec![false; tags.len()];
    let mut slots = max_per_note;
    for pass_protected in [true, false] {
        for (i, tag) in tags.iter().enumerate() {
            if slots == 0 {
                break;
            }
            if keep[i] || protected.contains(tag) != pass_protected {
                continue;
            }
            keep[i] = true;
            slots -= 1;
        }
    }
    let dropped: Vec<&String> = tags.iter().zip(&keep).filter(|(_, k)| !**k).map(|(t, _)| t).collect();
    log::debug!(
        "canonical::cap_protecting: {} -> {max_per_note} tags, dropped {dropped:?}",
        tags.len()
    );
    tags.into_iter().zip(keep).filter_map(|(t, k)| k.then_some(t)).collect()
}

#[cfg(test)]
mod tests;
