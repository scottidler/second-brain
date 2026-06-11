use regex::Regex;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use crate::config::{LinkingConfig, LinkingFilter};
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// The concept glossary + alias table, loaded from `glossary.yml` (Phase 2 of
/// graph-augmented-memory). Mirrors `canonical-tags.yml`: kebab-case concept
/// slugs plus an `aliases` map of surface form → canonical slug.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Glossary {
    pub concepts: Vec<String>,
    pub aliases: HashMap<String, String>,
}

/// Load the glossary from `path`. A missing file yields an empty glossary (the
/// linker simply has no concepts/aliases to apply); a malformed file is a hard
/// error so a typo is not silently ignored.
pub fn load_glossary(path: &Path) -> eyre::Result<Glossary> {
    log::debug!("load_glossary: path={}", path.display());
    if !path.exists() {
        log::info!("load_glossary: {} absent; no glossary concepts/aliases", path.display());
        return Ok(Glossary::default());
    }
    let content = std::fs::read_to_string(path)?;
    let glossary: Glossary = serde_yaml::from_str(&content)?;
    log::info!(
        "load_glossary: {} concepts, {} aliases",
        glossary.concepts.len(),
        glossary.aliases.len()
    );
    Ok(glossary)
}

/// Check if a value is excluded by a filter.
/// Excluded if it matches an exclude pattern and no include pattern overrides it.
fn is_filtered(value: &str, filter: &LinkingFilter) -> bool {
    if filter.exclude.is_empty() {
        return false;
    }
    let excluded = filter.exclude.iter().any(|e| e == value);
    if !excluded {
        return false;
    }
    if !filter.include.is_empty() && filter.include.iter().any(|i| i == value) {
        return false;
    }
    true
}

/// Check if a path is excluded by a filter (glob-style prefix matching).
fn is_path_filtered(path: &Path, filter: &LinkingFilter) -> bool {
    if filter.exclude.is_empty() {
        return false;
    }
    let path_str = path.to_string_lossy();
    let excluded = filter
        .exclude
        .iter()
        .any(|e| path_str.starts_with(e.trim_end_matches('*').trim_end_matches('/')));
    if !excluded {
        return false;
    }
    if !filter.include.is_empty() {
        let included = filter
            .include
            .iter()
            .any(|i| path_str.starts_with(i.trim_end_matches('*').trim_end_matches('/')));
        if included {
            return false;
        }
    }
    true
}

/// Regex to find existing wikilinks (to avoid double-linking).
static EXISTING_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]").expect("valid wikilink regex"));

/// Run wikilink inference on all notes.
pub fn lint_linking(notes: &[Note], config: &LinkingConfig) -> Report {
    let mut report = Report::default();

    // Build entity lists from config + note titles, filtering by target rules
    let note_titles: Vec<(String, String)> = notes
        .iter()
        .filter_map(|n| {
            // Filter by type
            if let Some(ref note_type) = n.frontmatter.note_type
                && is_filtered(note_type, &config.targets.types)
            {
                return None;
            }
            // Filter by path
            if is_path_filtered(&n.path, &config.targets.paths) {
                return None;
            }
            let stem = n.path.file_stem()?.to_str()?.to_string();
            let raw_title = n.frontmatter.title.clone().unwrap_or_else(|| stem.clone());
            // Strip wikilink brackets from titles (some notes have title: "[[foo]]")
            let title = raw_title.trim_start_matches("[[").trim_end_matches("]]").to_string();
            if title.is_empty() {
                return None;
            }
            Some((stem, title))
        })
        .collect();

    let scan_for: HashSet<&str> = config.scan_for.iter().map(|s| s.as_str()).collect();

    for note in notes {
        let existing_links = extract_existing_links(&note.body);
        // Lowercase the body + build the offset map ONCE per note, reused by
        // every candidate term below (was rebuilt per (note, term) pair).
        let lowered = LoweredBody::new(&note.body);

        // Match note titles/stems in body text
        if scan_for.contains("concepts") || scan_for.contains("all") {
            for (stem, title) in &note_titles {
                // Don't self-link
                if note.path.file_stem().and_then(|s| s.to_str()) == Some(stem) {
                    continue;
                }

                // Don't suggest if already linked
                if existing_links.contains(&stem.to_lowercase()) {
                    continue;
                }

                // Check if the title or stem appears in the body (case-insensitive)
                if let Some((context, surface)) = lowered.find_mention(title, stem, config.min_word_length) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.concept".to_string(),
                        severity: Severity::Info,
                        message: format!("mention of '{title}' could be linked as [[{stem}]]"),
                        fix: Some(Fix::AddWikilink {
                            target: stem.clone(),
                            surface,
                            context,
                        }),
                    });
                }
            }
        }

        // Match known people entities
        if scan_for.contains("people") || scan_for.contains("all") {
            for person in &config.entities.people {
                if existing_links.contains(&person.to_lowercase()) {
                    continue;
                }
                if let Some((context, surface)) = lowered.find_mention(person, person, config.min_word_length) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.person".to_string(),
                        severity: Severity::Info,
                        message: format!("mention of '{person}' could be linked"),
                        fix: Some(Fix::AddWikilink {
                            target: person.clone(),
                            surface,
                            context,
                        }),
                    });
                }
            }
        }

        // Match known project entities
        if scan_for.contains("projects") || scan_for.contains("all") {
            for project in &config.entities.projects {
                if existing_links.contains(&project.to_lowercase()) {
                    continue;
                }
                if let Some((context, surface)) = lowered.find_mention(project, project, config.min_word_length) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.project".to_string(),
                        severity: Severity::Info,
                        message: format!("mention of '{project}' could be linked"),
                        fix: Some(Fix::AddWikilink {
                            target: project.clone(),
                            surface,
                            context,
                        }),
                    });
                }
            }
        }

        // Match glossary concepts (Phase 2): kebab-case slugs linked at first
        // body mention as [[slug]] (or [[slug|Surface]] when the prose case
        // differs). Plus alias surface forms -> canonical slug as piped links.
        if scan_for.contains("concepts") || scan_for.contains("all") {
            for slug in &config.entities.concepts {
                if existing_links.contains(&slug.to_lowercase()) {
                    continue;
                }
                if note.path.file_stem().and_then(|s| s.to_str()) == Some(slug.as_str()) {
                    continue; // never self-link a concept's own hub note
                }
                if let Some((context, surface)) = lowered.find_mention(slug, slug, config.min_word_length) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.glossary".to_string(),
                        severity: Severity::Info,
                        message: format!("mention of '{surface}' could be linked as [[{slug}]]"),
                        fix: Some(Fix::AddWikilink {
                            target: slug.clone(),
                            surface,
                            context,
                        }),
                    });
                }
            }

            for (alias_surface, slug) in &config.aliases {
                if existing_links.contains(&slug.to_lowercase()) {
                    continue;
                }
                if let Some((context, surface)) =
                    lowered.find_mention(alias_surface, alias_surface, config.min_word_length)
                {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.alias".to_string(),
                        severity: Severity::Info,
                        message: format!("alias '{surface}' could be linked as [[{slug}|{surface}]]"),
                        fix: Some(Fix::AddWikilink {
                            target: slug.clone(),
                            surface,
                            context,
                        }),
                    });
                }
            }
        }
    }

    log::info!("linking lint complete: {} violation(s)", report.violations.len());
    report
}

/// Apply link suggestions: insert [[wikilinks]] at first mention.
pub fn apply_linking(vault_root: &Path, notes: &[Note], config: &LinkingConfig) -> eyre::Result<usize> {
    let report = lint_linking(notes, config);
    let mut fixed_count = 0;

    // Group fixes by file, carrying (target, surface) so the apply step can
    // emit a piped link that preserves the prose wording.
    let mut fixes_by_path: std::collections::HashMap<&std::path::Path, Vec<(&str, &str)>> =
        std::collections::HashMap::new();
    for violation in &report.violations {
        if let Some(Fix::AddWikilink { target, surface, .. }) = &violation.fix {
            fixes_by_path
                .entry(violation.path.as_path())
                .or_default()
                .push((target, surface));
        }
    }

    for (path, fixes) in &fixes_by_path {
        let abs_path = vault_root.join(path);
        let content = std::fs::read_to_string(&abs_path)?;
        let mut new_content = content.clone();

        for (target, surface) in fixes {
            // Find the first mention of the surface form and wrap it in [[]].
            if let Some(new) = insert_first_wikilink(&new_content, target, surface) {
                new_content = new;
            }
        }

        if new_content != content {
            vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
            log::info!("inserted wikilinks: {}", path.display());
            fixed_count += 1;
        }
    }

    Ok(fixed_count)
}

/// Extract all existing wikilink targets from body (lowercased).
fn extract_existing_links(body: &str) -> HashSet<String> {
    EXISTING_LINK_RE
        .captures_iter(body)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().trim().to_lowercase()))
        .collect()
}

/// A note body lowercased ONCE, with a map from lowercased-byte offsets back to
/// original `body` byte offsets. Built once per note and reused across every
/// candidate term (`find_mention`). Previously the lowercase pass + offset map
/// were rebuilt on every (note, term) pair - O(notes x terms x body), and the
/// daemon runs `lint_linking` every sweep. Hoisting the per-note build out of
/// the term loop drops the inner cost to a single `find` per term.
struct LoweredBody<'a> {
    body: &'a str,
    body_lower: String,
    /// `(lower_off, body_off)` pairs, ascending, recorded at the start of every
    /// body char's lowercase expansion plus an end sentinel. `to_lowercase` can
    /// change byte length (e.g. Turkish I), so a `body_lower.find()` offset is
    /// NOT a valid index into `body`; mapping back keeps every slice on a real
    /// char boundary and yields the correct original-case span.
    map: Vec<(usize, usize)>,
}

impl<'a> LoweredBody<'a> {
    fn new(body: &'a str) -> Self {
        let mut body_lower = String::with_capacity(body.len());
        let mut map: Vec<(usize, usize)> = Vec::new();
        for (body_off, ch) in body.char_indices() {
            map.push((body_lower.len(), body_off));
            for lc in ch.to_lowercase() {
                body_lower.push(lc);
            }
        }
        map.push((body_lower.len(), body.len()));
        Self { body, body_lower, map }
    }

    fn to_body(&self, lower_off: usize) -> usize {
        match self.map.binary_search_by_key(&lower_off, |&(l, _)| l) {
            Ok(i) => self.map[i].1,
            // A match landing mid-expansion (rare) snaps back to that char's start.
            Err(i) => self.map[i.saturating_sub(1)].1,
        }
    }

    /// Find a case-insensitive mention of `title` (then `stem`) in the body.
    /// Returns `(context, surface)` where `surface` is the text actually matched
    /// (preserving original case) so the caller can emit `[[target|surface]]`.
    fn find_mention(&self, title: &str, stem: &str, min_len: usize) -> Option<(String, String)> {
        let body = self.body;
        for term in [title, stem] {
            let term_lower = term.to_lowercase();
            if term_lower.len() < min_len {
                continue;
            }

            if let Some(lpos) = self.body_lower.find(&term_lower) {
                let pos = self.to_body(lpos);
                let after_pos = self.to_body(lpos + term_lower.len());

                // Skip if match is inside an existing wikilink
                let before_slice = &body[..pos];
                if let (Some(open), Some(close)) = (before_slice.rfind("[["), before_slice.rfind("]]"))
                    && open > close
                {
                    log::debug!("skipping mention inside existing wikilink: {term}");
                    continue;
                }
                if before_slice.ends_with("[[") {
                    log::debug!("skipping mention inside existing wikilink: {term}");
                    continue;
                }

                // Verify it's a word boundary (not inside another word)
                let before_char = body[..pos].chars().last().unwrap_or(' ');
                let after_char = body[after_pos..].chars().next().unwrap_or(' ');

                if before_char.is_ascii_alphanumeric() || after_char.is_ascii_alphanumeric() {
                    continue;
                }

                // The surface form is the original-case substring at the match.
                let surface = body[pos..after_pos].to_string();
                // Context: surrounding ~20 chars, snapped to char boundaries.
                let start = body.floor_char_boundary(pos.saturating_sub(20));
                let end = body.ceil_char_boundary((after_pos + 20).min(body.len()));
                let context = body[start..end].to_string();
                return Some((context, surface));
            }
        }

        None
    }
}

/// Insert a wikilink at the first mention of `surface` in content, pointing at
/// `target`. Only searches the body (after frontmatter) to avoid corrupting
/// YAML fields. Emits a piped link `[[target|matched]]` when the matched text
/// differs from `target` (an alias surface form, or a title whose stem
/// differs); a plain `[[matched]]` otherwise.
fn insert_first_wikilink(content: &str, target: &str, surface: &str) -> Option<String> {
    // Find the end of frontmatter so we only search the body
    let body_start = if let Some(after_open) = content.strip_prefix("---") {
        after_open.find("\n---").map(|pos| 3 + pos + "\n---".len()).unwrap_or(0)
    } else {
        0
    };

    let body = &content[body_start..];
    // Match the surface form (the text that actually appears) rather than the
    // target slug, so an alias like "Retrieval-Augmented Generation" is found
    // even though it points at the slug "rag".
    let pattern = format!(r"(?i)\b{}\b", regex::escape(surface));
    let re = Regex::new(&pattern).ok()?;

    // Only replace the first occurrence in the body
    if let Some(mat) = re.find(body) {
        let abs_start = body_start + mat.start();
        let abs_end = body_start + mat.end();

        let before = &content[..abs_start];
        let matched = &content[abs_start..abs_end];

        // Don't insert if already inside a wikilink
        if before.ends_with("[[") || content[abs_end..].starts_with("]]") {
            return None;
        }

        let after = &content[abs_end..];
        // Plain link only when the matched text is exactly the slug; otherwise
        // pipe so the canonical slug is the link target while the prose wording
        // (case, alias surface form) is preserved as the display text.
        let link = if matched == target {
            format!("[[{matched}]]")
        } else {
            format!("[[{target}|{matched}]]")
        };
        Some(format!("{before}{link}{after}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
