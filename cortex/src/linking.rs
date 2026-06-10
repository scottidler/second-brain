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
                if let Some((context, surface)) = find_mention(&note.body, title, stem, config.min_word_length) {
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
                if let Some((context, surface)) = find_mention(&note.body, person, person, config.min_word_length) {
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
                if let Some((context, surface)) = find_mention(&note.body, project, project, config.min_word_length) {
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
                if let Some((context, surface)) = find_mention(&note.body, slug, slug, config.min_word_length) {
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
                    find_mention(&note.body, alias_surface, alias_surface, config.min_word_length)
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

/// Find a case-insensitive mention of a term in body text. Returns
/// `(context, surface)` where `surface` is the text actually matched in the
/// body (preserving its original case) so the caller can emit a piped link
/// `[[target|surface]]` that keeps the prose wording.
fn find_mention(body: &str, title: &str, stem: &str, min_len: usize) -> Option<(String, String)> {
    // Build the lowercased body alongside a map from lowercased-byte offsets
    // back to original `body` byte offsets, recorded at the start of every
    // body char's lowercase expansion (plus an end sentinel). `to_lowercase`
    // can change byte length (e.g. Turkish I), so a `body_lower.find()` offset
    // is NOT a valid index into `body`; mapping back keeps every slice on a
    // real char boundary in `body` and yields the correct original-case span.
    let mut body_lower = String::with_capacity(body.len());
    let mut map: Vec<(usize, usize)> = Vec::new(); // (lower_off, body_off), ascending
    for (body_off, ch) in body.char_indices() {
        map.push((body_lower.len(), body_off));
        for lc in ch.to_lowercase() {
            body_lower.push(lc);
        }
    }
    map.push((body_lower.len(), body.len()));
    let to_body = |lower_off: usize| -> usize {
        match map.binary_search_by_key(&lower_off, |&(l, _)| l) {
            Ok(i) => map[i].1,
            // A match landing mid-expansion (rare) snaps back to that char's start.
            Err(i) => map[i.saturating_sub(1)].1,
        }
    };

    // Try title first, then stem
    for term in [title, stem] {
        let term_lower = term.to_lowercase();
        if term_lower.len() < min_len {
            continue;
        }

        if let Some(lpos) = body_lower.find(&term_lower) {
            let pos = to_body(lpos);
            let after_pos = to_body(lpos + term_lower.len());

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
            // Extract context (surrounding ~20 chars, snapped to char boundaries)
            let start = body.floor_char_boundary(pos.saturating_sub(20));
            let end = body.ceil_char_boundary((after_pos + 20).min(body.len()));
            let context = body[start..end].to_string();
            return Some((context, surface));
        }
    }

    None
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
mod tests {
    use super::*;
    use crate::testutil::TestVault;

    #[test]
    fn find_mention_handles_length_changing_lowercase() {
        // 'İ' (2 bytes) lowercases to "i̇" (3 bytes), so a `body.to_lowercase()`
        // match offset is NOT a valid index into `body`. The old code sliced
        // `body` with that offset and could panic / extract a shifted span.
        let body = "İstanbul notes mention Rust here.";
        let (context, surface) = find_mention(body, "Rust", "rust", 3).expect("match");
        // Surface must be the original-case word, not a byte-shifted slice.
        assert_eq!(surface, "Rust");
        assert!(context.contains("Rust"));
    }

    #[test]
    fn find_mention_no_panic_on_multibyte_before_match() {
        // Many length-changing chars before the match must not panic.
        let body = format!("{} discusses Rust extensively", "İ".repeat(50));
        let result = find_mention(&body, "Rust", "rust", 3);
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
}
