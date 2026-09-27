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
    pub tags: HashMap<String, Vec<TagEntry>>,
}

/// The longest description a tag may carry: classifier.dev's documented
/// long-context label cap, applied everywhere so one rule holds.
pub const MAX_DESCRIPTION_CHARS: usize = 200;

/// One vocabulary entry: a bare slug (`- rust`) or a one-key map
/// (`- homelab: "self-hosted servers ..."`). Both forms are legal in one file.
///
/// The entry rules (one key, a string description of at most
/// `MAX_DESCRIPTION_CHARS`, no `"`) run inside `Deserialize` through
/// `try_from`, not in a `load()` wrapper, because `promote_tags` parses the
/// file with raw `serde_yaml::from_str` and would bypass a wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawTagEntry")]
pub struct TagEntry {
    pub slug: String,
    pub description: Option<String>,
}

/// `Scalar` catches a bare slug YAML resolves to a non-string (`123`,
/// `true`, `null`): the untagged buffer has already typed it, so `Bare`
/// cannot take it, and v0.15.4's `Vec<String>` read those as slugs
/// (implementation audit r1, P3).
#[derive(Deserialize)]
#[serde(untagged)]
enum RawTagEntry {
    Bare(String),
    Described(serde_yaml::Mapping),
    Scalar(serde_yaml::Value),
}

/// A plain scalar YAML typed as something other than a string, read back as
/// the slug it was written as. A float is refused rather than guessed at:
/// `1e3` resolves to `1000.0`, and no stringification recovers `1e3`.
fn scalar_slug(value: &serde_yaml::Value) -> std::result::Result<String, String> {
    match value {
        serde_yaml::Value::String(s) => Ok(s.clone()),
        serde_yaml::Value::Bool(b) => Ok(b.to_string()),
        serde_yaml::Value::Null => Ok("null".to_string()),
        serde_yaml::Value::Number(n) if n.is_u64() || n.is_i64() => Ok(n.to_string()),
        other => Err(format!(
            "tag entry {other:?} is not a slug YAML reads back unchanged; quote it (`- \"...\"`)"
        )),
    }
}

/// Whether `slug` written unquoted would read back as some other YAML type
/// or string, so `yaml_line` must quote it.
fn needs_quoting(slug: &str) -> bool {
    !matches!(serde_yaml::from_str::<serde_yaml::Value>(slug), Ok(serde_yaml::Value::String(s)) if s == slug)
}

impl TryFrom<RawTagEntry> for TagEntry {
    type Error = String;

    fn try_from(raw: RawTagEntry) -> std::result::Result<Self, Self::Error> {
        let map = match raw {
            RawTagEntry::Bare(slug) => return Ok(Self::bare(slug)),
            RawTagEntry::Scalar(value) => return scalar_slug(&value).map(Self::bare),
            RawTagEntry::Described(map) => map,
        };
        if map.len() != 1 {
            let keys: Vec<String> = map
                .keys()
                .map(|k| k.as_str().map_or_else(|| format!("{k:?}"), str::to_string))
                .collect();
            return Err(format!(
                "a tag entry is a bare slug or a one-key `slug: \"description\"` map; got {} keys {keys:?}",
                map.len()
            ));
        }
        let (key, value) = map.into_iter().next().expect("one key, checked above");
        let slug = scalar_slug(&key).map_err(|e| format!("tag entry key: {e}"))?;
        let serde_yaml::Value::String(description) = value else {
            return Err(format!(
                "tag {slug:?}: description must be a string, got {value:?} (write a bare `- {slug}` for no description)"
            ));
        };
        validate_description(&slug, &description)?;
        Ok(Self {
            slug,
            description: Some(description),
        })
    }
}

/// The description rules, shared by parsing and by `TagEntry::described`.
/// Empty and multi-line descriptions are refused too: an empty one would send
/// `slug: ` as a label, and a line break would split the label's line in the
/// file and in the classifier question.
fn validate_description(slug: &str, description: &str) -> std::result::Result<(), String> {
    if description.trim().is_empty() {
        return Err(format!(
            "tag {slug:?}: description is empty (write a bare `- {slug}` for no description)"
        ));
    }
    let chars = description.chars().count();
    if chars > MAX_DESCRIPTION_CHARS {
        return Err(format!(
            "tag {slug:?}: description is {chars} chars, over the {MAX_DESCRIPTION_CHARS}-char cap"
        ));
    }
    if description.contains('"') {
        return Err(format!(
            "tag {slug:?}: description contains `\"`, which classifier.dev uses to quote the label"
        ));
    }
    if description.chars().any(char::is_control) {
        return Err(format!(
            "tag {slug:?}: description contains a control character (line break or tab); keep it on one line"
        ));
    }
    Ok(())
}

impl TagEntry {
    pub fn bare(slug: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            description: None,
        }
    }

    /// A described entry, held to the same rules parsing enforces.
    pub fn described(slug: impl Into<String>, description: impl Into<String>) -> Result<Self> {
        let slug = slug.into();
        let description = description.into();
        validate_description(&slug, &description).map_err(|e| eyre::eyre!(e))?;
        Ok(Self {
            slug,
            description: Some(description),
        })
    }

    /// The entry as it is written after `    - ` in `canonical-tags.yml`:
    /// `slug`, or `slug: "description"`. A validated description holds no
    /// `"` or control character, so escaping `\` is all a double-quoted YAML
    /// scalar needs to read back the same string. A slug YAML would type as
    /// a number, bool, or null (`123`, `true`) is written quoted, so the
    /// line reads back as the same slug without leaning on `scalar_slug`.
    pub fn yaml_line(&self) -> String {
        let slug = if needs_quoting(&self.slug) {
            format!("\"{}\"", self.slug)
        } else {
            self.slug.clone()
        };
        match &self.description {
            None => slug,
            Some(d) => format!("{slug}: \"{}\"", d.replace('\\', "\\\\")),
        }
    }
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
    /// slug -> description, for described tags only. An undescribed tag is
    /// in `all` and absent here.
    pub descriptions: HashMap<String, String>,
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
        self.tags.values().flatten().map(|e| e.slug.clone()).collect()
    }

    /// Every entry across all groups, sorted by slug.
    pub fn sorted_entries(&self) -> Vec<TagEntry> {
        let mut entries: Vec<TagEntry> = self.tags.values().flatten().cloned().collect();
        entries.sort_by(|a, b| a.slug.cmp(&b.slug));
        entries
    }

    fn descriptions(&self) -> HashMap<String, String> {
        self.tags
            .values()
            .flatten()
            .filter_map(|e| e.description.clone().map(|d| (e.slug.clone(), d)))
            .collect()
    }

    /// Build the shared `CanonicalSet` snapshot every matcher takes.
    pub fn canonical_set(&self) -> CanonicalSet {
        CanonicalSet {
            all: self.all_tags(),
            no_segment: self.no_segment_match.iter().cloned().collect(),
            no_classifier: self.no_classifier_tags.iter().cloned().collect(),
            max_per_note: self.max_per_note,
            descriptions: self.descriptions(),
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

/// True when `raw_tag` matches `^[a-z0-9]+(-[a-z0-9]+)*$`, the shape every
/// tag in the shipped vocabulary holds.
///
/// NOT `sanitize_tag(t) == t`: `sanitize_tag("")` is `""`, so the empty tag
/// passes that gate, and `sanitize_slug` keeps non-ASCII alphanumerics the
/// shipped-file test rejects. A promotion path needs the strict predicate.
pub fn is_kebab_tag(raw_tag: &str) -> bool {
    !raw_tag.is_empty()
        && raw_tag
            .split('-')
            .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
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
