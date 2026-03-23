//! Classify inbox notes by domain and promote to notes/.
//!
//! Tier 1: Deterministic classification via tag-to-domain map and source URL patterns.
//! Tier 2: LLM classification with vault context (future phase).
//! Tier 3: Hold for review if no tier produces high confidence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eyre::Result;
use serde::Deserialize;

use crate::report::{Fix, Report, Severity, Violation};
use crate::scope::insert_frontmatter_fields;
use crate::vault::Note;
use ::vault::schema::Domain;
use ::vault::search::SearchIndex;

/// Classification configuration from cortex.yml
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ClassifyConfig {
    pub confidence_threshold: f64,
    pub fabric_pattern: String,
    pub fabric_timeout_secs: u64,
    pub max_input_tokens: usize,
    pub similar_notes_limit: usize,
    pub tag_domain_map: HashMap<String, Vec<String>>,
    pub source_domain_map: HashMap<String, Vec<String>>,
}

impl Default for ClassifyConfig {
    fn default() -> Self {
        Self {
            confidence_threshold: 0.7,
            fabric_pattern: "cortex_classify".to_string(),
            fabric_timeout_secs: 30,
            max_input_tokens: 8000,
            similar_notes_limit: 5,
            tag_domain_map: default_tag_domain_map(),
            source_domain_map: HashMap::new(),
        }
    }
}

fn default_tag_domain_map() -> HashMap<String, Vec<String>> {
    let mut m = HashMap::new();
    m.insert(
        "ai".into(),
        vec![
            "ai",
            "claude",
            "llm",
            "gpt",
            "anthropic",
            "openai",
            "agents",
            "prompting",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    );
    m.insert(
        "tech".into(),
        vec![
            "rust",
            "python",
            "nix",
            "cli",
            "devops",
            "obsidian",
            "neovim",
            "linux",
            "programming",
            "gemini",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    );
    m.insert(
        "football".into(),
        vec!["football", "offense", "defense", "coaching", "drills", "plays"]
            .into_iter()
            .map(String::from)
            .collect(),
    );
    m.insert(
        "work".into(),
        vec!["tatari", "sre", "infrastructure", "kubernetes", "platform"]
            .into_iter()
            .map(String::from)
            .collect(),
    );
    m.insert(
        "writing".into(),
        vec!["writing", "fiction", "plot", "worldbuilding", "publishing"]
            .into_iter()
            .map(String::from)
            .collect(),
    );
    m.insert(
        "music".into(),
        vec!["music", "synth", "production", "ableton", "electronic"]
            .into_iter()
            .map(String::from)
            .collect(),
    );
    m.insert(
        "spanish".into(),
        vec!["spanish", "espanol", "vocab", "grammar", "conjugation"]
            .into_iter()
            .map(String::from)
            .collect(),
    );
    m.insert(
        "life".into(),
        vec![
            "health",
            "exercise",
            "learning",
            "vocabulary",
            "productivity",
            "motivation",
            "fitness",
            "psychology",
            "mindset",
            "habits",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    );
    m.insert(
        "homelab".into(),
        vec![
            "homelab",
            "selfhosted",
            "plex",
            "unifi",
            "pfsense",
            "proxmox",
            "nas",
            "pihole",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    );
    m.insert(
        "diy".into(),
        vec![
            "diy",
            "woodworking",
            "building",
            "knots",
            "construction",
            "makeover",
            "furniture",
            "timber",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    );
    m.insert(
        "resources".into(),
        vec!["book", "reference", "tools"]
            .into_iter()
            .map(String::from)
            .collect(),
    );
    m
}

/// Result of classifying a single note
#[derive(Debug)]
pub struct ClassifyResult {
    pub domain: Domain,
    pub confidence: ClassifyConfidence,
    pub method: ClassifyMethod,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassifyConfidence {
    High,
    Medium,
    Low,
}

impl ClassifyConfidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassifyMethod {
    Deterministic,
    Llm,
}

impl ClassifyMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::Llm => "llm",
        }
    }
}

/// CLI options for the classify command
#[derive(Debug, clap::Parser)]
pub struct ClassifyOpts {
    /// Move notes (default: dry-run showing planned moves)
    #[arg(long)]
    pub apply: bool,

    /// Process specific files
    #[arg(long)]
    pub path: Option<String>,

    /// Reclassify notes that already have cortex-classified: true
    #[arg(long)]
    pub force: bool,

    /// Only process notes with cortex-needs-review: true
    #[arg(long)]
    pub review_only: bool,

    /// Reclassify all notes with this domain (e.g., --reclassify-domain resources)
    #[arg(long)]
    pub reclassify_domain: Option<String>,
}

/// Dry-run: returns planned classifications as violations in a Report
pub fn lint_classify(notes: &[Note], config: &ClassifyConfig, search_index: Option<&SearchIndex>) -> Report {
    let mut report = Report::default();
    let inbox_notes = filter_inbox_notes(notes, false, false);

    for note in &inbox_notes {
        match classify_note(note, config, search_index) {
            Some(result) if result.confidence != ClassifyConfidence::Low => {
                report.add(Violation {
                    path: note.path.clone(),
                    rule: "classify".to_string(),
                    severity: Severity::Info,
                    message: format!(
                        "would classify as domain={}, confidence={}, method={}: {}",
                        result.domain.as_str(),
                        result.confidence.as_str(),
                        result.method.as_str(),
                        result.reason,
                    ),
                    fix: Some(Fix::MoveFile {
                        from: note.path.clone(),
                        to: PathBuf::from("notes").join(note.path.file_name().unwrap_or_default()),
                    }),
                });
            }
            Some(result) => {
                report.add(Violation {
                    path: note.path.clone(),
                    rule: "classify".to_string(),
                    severity: Severity::Warning,
                    message: format!("low confidence, would hold for review: {}", result.reason),
                    fix: None,
                });
            }
            None => {
                report.add(Violation {
                    path: note.path.clone(),
                    rule: "classify".to_string(),
                    severity: Severity::Warning,
                    message: "no classification signal, would hold for review".to_string(),
                    fix: None,
                });
            }
        }
    }

    log::info!("classify lint complete: {} note(s) in inbox", inbox_notes.len());
    report
}

/// Apply: classify and move notes from inbox/ to notes/
/// If `reclassify_domain` is set, reclassifies notes already in notes/ that have that domain.
pub fn apply_classify(
    vault_root: &Path,
    notes: &[Note],
    config: &ClassifyConfig,
    force: bool,
    review_only: bool,
    reclassify_domain: Option<&str>,
    search_index: Option<&SearchIndex>,
) -> Result<Report> {
    let target_notes: Vec<&Note> = if let Some(domain) = reclassify_domain {
        filter_domain_notes(notes, domain)
    } else {
        filter_inbox_notes(notes, force, review_only)
    };
    let is_reclassify = reclassify_domain.is_some();
    let mut report = Report::default();
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();

    for note in &target_notes {
        let result = match classify_note(note, config, search_index) {
            Some(r) => r,
            None => {
                if !is_reclassify {
                    mark_needs_review(vault_root, note)?;
                }
                log::info!("held for review (no signal): {}", note.path.display());
                continue;
            }
        };

        if result.confidence == ClassifyConfidence::Low {
            if !is_reclassify {
                mark_needs_review(vault_root, note)?;
            }
            log::info!("held for review (low confidence): {}", note.path.display());
            continue;
        }

        // For reclassify: update domain in place, no file move
        if is_reclassify {
            let abs_path = vault_root.join(&note.path);
            let content = std::fs::read_to_string(&abs_path)?;
            let fields = build_enrichment_fields(&result);
            if let Some(new_content) = insert_frontmatter_fields(&content, &fields) {
                std::fs::write(&abs_path, new_content)?;
            }

            report.add(Violation {
                path: note.path.clone(),
                rule: "classify".to_string(),
                severity: Severity::Info,
                message: format!(
                    "reclassified domain={} (method={})",
                    result.domain.as_str(),
                    result.method.as_str(),
                ),
                fix: None,
            });

            log::info!(
                "reclassified {} (domain={}, confidence={}, method={})",
                note.path.display(),
                result.domain.as_str(),
                result.confidence.as_str(),
                result.method.as_str(),
            );
            continue;
        }

        // Enrich frontmatter and promote (inbox -> notes/)
        let mut enrichment_fields = build_enrichment_fields(&result);
        ensure_origin(&mut enrichment_fields, note);
        let enrichment_fields = enrichment_fields;
        let abs_path = vault_root.join(&note.path);
        let content = std::fs::read_to_string(&abs_path)?;

        if let Some(new_content) = insert_frontmatter_fields(&content, &enrichment_fields) {
            std::fs::write(&abs_path, new_content)?;
        }

        // Move from inbox/ to notes/
        let filename = note.path.file_name().unwrap_or_default();
        let dest_relative = PathBuf::from("notes").join(filename);
        let dest_abs = vault_root.join(&dest_relative);

        // Handle filename collision - pass source URL so we can detect
        // reingest replacements (same source = overwrite, not -2 suffix)
        let source_url = note.frontmatter.source.as_deref();
        let dest_abs = resolve_collision(&dest_abs, source_url);
        let dest_relative = dest_abs.strip_prefix(vault_root).unwrap_or(&dest_abs).to_path_buf();

        if let Some(parent) = dest_abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&abs_path, &dest_abs)?;

        moves.push((note.path.clone(), dest_relative.clone()));

        report.add(Violation {
            path: note.path.clone(),
            rule: "classify".to_string(),
            severity: Severity::Info,
            message: format!(
                "promoted to {} (domain={}, method={})",
                dest_relative.display(),
                result.domain.as_str(),
                result.method.as_str(),
            ),
            fix: None,
        });

        log::info!(
            "promoted {} -> {} (domain={}, confidence={}, method={})",
            note.path.display(),
            dest_relative.display(),
            result.domain.as_str(),
            result.confidence.as_str(),
            result.method.as_str(),
        );
    }

    // Update wikilinks across vault for moved files
    if !moves.is_empty() {
        let all_notes = crate::vault::scan_vault(vault_root, &crate::config::VaultConfig::default())?;
        update_wikilinks_for_moves(vault_root, &all_notes, &moves)?;
    }

    report.print_human(true);

    Ok(report)
}

/// Classify a single note using the tiered pipeline
fn classify_note(note: &Note, config: &ClassifyConfig, search_index: Option<&SearchIndex>) -> Option<ClassifyResult> {
    // Tier 1: Deterministic classification
    if let Some(result) = classify_by_tags(note, config) {
        return Some(result);
    }

    if let Some(result) = classify_by_source(note, config) {
        return Some(result);
    }

    // Tier 2: LLM classification with vault context
    if let Some(index) = search_index
        && let Some(result) = classify_by_llm(note, config, index)
    {
        return Some(result);
    }

    // Tier 3: No signal
    None
}

/// Tier 2: LLM classification using Fabric with vault context from SearchIndex
fn classify_by_llm(note: &Note, config: &ClassifyConfig, index: &SearchIndex) -> Option<ClassifyResult> {
    if !crate::fabric::is_available() {
        log::debug!("fabric not available, skipping LLM classification");
        return None;
    }

    // Build vault context
    let context = build_llm_context(note, config, index);
    let input = crate::fabric::truncate_input(&context, config.max_input_tokens);

    // Call Fabric pattern
    match crate::fabric::run_pattern(&config.fabric_pattern, input, config.fabric_timeout_secs) {
        Ok(output) => parse_llm_result(&output, config.confidence_threshold),
        Err(e) => {
            log::warn!("LLM classification failed: {e}");
            None
        }
    }
}

/// Build the LLM context string with vault search results
fn build_llm_context(note: &Note, config: &ClassifyConfig, index: &SearchIndex) -> String {
    let title = note.frontmatter.title.as_deref().unwrap_or("Untitled");
    let tags = note.frontmatter.tags.as_ref().map(|t| t.join(", ")).unwrap_or_default();

    // Find similar notes via FTS5
    let similar_text = match index.find_similar(&note.body, config.similar_notes_limit) {
        Ok(results) if !results.is_empty() => {
            let lines: Vec<String> = results
                .iter()
                .map(|r| format!("- \"{}\" (domain: {})", r.title, r.domain))
                .collect();
            lines.join("\n")
        }
        _ => "No similar notes found.".to_string(),
    };

    // Get tag-domain correlations for this note's tags
    let tag_correlations = match (index.tag_domain_map(), &note.frontmatter.tags) {
        (Ok(tdm), Some(note_tags)) => {
            let lines: Vec<String> = note_tags
                .iter()
                .filter_map(|tag| {
                    tdm.get(tag).map(|domains| {
                        let domain_list: Vec<String> = domains.iter().map(|(d, c)| format!("{d}:{c}")).collect();
                        format!("- tag \"{tag}\" appears in: {}", domain_list.join(", "))
                    })
                })
                .collect();
            if lines.is_empty() {
                "No tag-domain correlations found.".to_string()
            } else {
                lines.join("\n")
            }
        }
        _ => "No tag-domain correlations available.".to_string(),
    };

    // Truncate body for LLM input
    let body_chars: String = note.body.chars().take(4000).collect();

    format!(
        "Title: {title}\n\n\
         Tags: {tags}\n\n\
         Similar notes in vault:\n{similar_text}\n\n\
         Tag-domain correlations:\n{tag_correlations}\n\n\
         Content:\n{body_chars}"
    )
}

/// Parse LLM JSON output into a ClassifyResult
fn parse_llm_result(output: &str, confidence_threshold: f64) -> Option<ClassifyResult> {
    let json_str = ::vault::fabric::extract_json(output);

    #[derive(Deserialize)]
    struct LlmOutput {
        domain: String,
        confidence: f64,
        #[serde(default)]
        reasoning: String,
        #[serde(default)]
        suggested_tags: Vec<String>,
    }

    let parsed: LlmOutput = match serde_json::from_str(&json_str) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("Failed to parse LLM classification JSON: {e}");
            return None;
        }
    };

    let domain = match parsed.domain.parse::<Domain>() {
        Ok(d) => d,
        Err(_) => {
            log::warn!("LLM returned invalid domain: {}", parsed.domain);
            return None;
        }
    };

    let confidence = if parsed.confidence >= confidence_threshold {
        if parsed.confidence >= 0.85 {
            ClassifyConfidence::High
        } else {
            ClassifyConfidence::Medium
        }
    } else {
        ClassifyConfidence::Low
    };

    let _ = parsed.suggested_tags; // Available for future tag enrichment

    Some(ClassifyResult {
        domain,
        confidence,
        method: ClassifyMethod::Llm,
        reason: parsed.reasoning,
    })
}

/// Tier 1a: Tag-to-domain mapping
fn classify_by_tags(note: &Note, config: &ClassifyConfig) -> Option<ClassifyResult> {
    let note_tags = note.frontmatter.tags.as_ref()?;
    if note_tags.is_empty() {
        return None;
    }

    // Count matches per domain
    let mut domain_scores: HashMap<&str, usize> = HashMap::new();
    let mut matched_tags: HashMap<&str, Vec<&str>> = HashMap::new();

    for (domain, trigger_tags) in &config.tag_domain_map {
        for note_tag in note_tags {
            let lower_tag = note_tag.to_lowercase();
            if trigger_tags.iter().any(|t| {
                let t_lower = t.to_lowercase();
                lower_tag == t_lower || lower_tag.split('-').any(|segment| segment == t_lower)
            }) {
                *domain_scores.entry(domain.as_str()).or_insert(0) += 1;
                matched_tags.entry(domain.as_str()).or_default().push(note_tag.as_str());
            }
        }
    }

    if domain_scores.is_empty() {
        return None;
    }

    // Find domain with most matching tags
    let mut sorted: Vec<_> = domain_scores.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));

    let (top_domain, top_score) = sorted[0];

    // If there's a tie, this is ambiguous - fall through to Tier 2
    if sorted.len() > 1 && sorted[1].1 == top_score {
        return None;
    }

    let domain = Domain::from_str(top_domain).ok()?;
    let tags = matched_tags.get(top_domain).map(|t| t.join(", ")).unwrap_or_default();

    Some(ClassifyResult {
        domain,
        confidence: ClassifyConfidence::High,
        method: ClassifyMethod::Deterministic,
        reason: format!("tag match: {tags}"),
    })
}

/// Tier 1b: Source URL pattern matching
fn classify_by_source(note: &Note, config: &ClassifyConfig) -> Option<ClassifyResult> {
    let source = note.frontmatter.source.as_ref()?;
    let lower_source = source.to_lowercase();

    for (domain, patterns) in &config.source_domain_map {
        for pattern in patterns {
            if lower_source.contains(&pattern.to_lowercase()) {
                let domain = Domain::from_str(domain).ok()?;
                return Some(ClassifyResult {
                    domain,
                    confidence: ClassifyConfidence::High,
                    method: ClassifyMethod::Deterministic,
                    reason: format!("source URL match: {pattern}"),
                });
            }
        }
    }

    None
}

/// Filter notes to those matching a specific domain value (for reclassification)
fn filter_domain_notes<'a>(notes: &'a [Note], domain: &str) -> Vec<&'a Note> {
    notes
        .iter()
        .filter(|n| n.frontmatter.domain.as_deref() == Some(domain))
        .collect()
}

/// Filter notes to inbox-only, respecting force and review-only flags
fn filter_inbox_notes(notes: &[Note], force: bool, review_only: bool) -> Vec<&Note> {
    notes
        .iter()
        .filter(|n| {
            let path_str = n.path.to_string_lossy();
            path_str.starts_with("inbox/") || path_str.starts_with("inbox\\")
        })
        .filter(|n| {
            // Skip already-classified unless force
            if !force {
                let classified = n.frontmatter.extra.get("cortex-classified");
                if classified == Some(&serde_yaml::Value::Bool(true)) {
                    return false;
                }
            }
            true
        })
        .filter(|n| {
            // If review_only, only process notes with cortex-needs-review
            if review_only {
                let needs_review = n.frontmatter.extra.get("cortex-needs-review");
                return needs_review == Some(&serde_yaml::Value::Bool(true));
            }
            true
        })
        .collect()
}

/// Build frontmatter fields to set during enrichment
fn build_enrichment_fields(result: &ClassifyResult) -> Vec<(String, serde_yaml::Value)> {
    vec![
        (
            "domain".to_string(),
            serde_yaml::Value::String(result.domain.as_str().to_string()),
        ),
        ("status".to_string(), serde_yaml::Value::String("unread".to_string())),
        ("cortex-classified".to_string(), serde_yaml::Value::Bool(true)),
        (
            "cortex-classified-by".to_string(),
            serde_yaml::Value::String(result.method.as_str().to_string()),
        ),
        (
            "cortex-confidence".to_string(),
            serde_yaml::Value::String(result.confidence.as_str().to_string()),
        ),
    ]
}

/// Set origin: assisted if missing
fn ensure_origin(fields: &mut Vec<(String, serde_yaml::Value)>, note: &Note) {
    if note.frontmatter.origin.is_none() {
        fields.push(("origin".to_string(), serde_yaml::Value::String("assisted".to_string())));
    }
}

/// Mark a note as needing manual review
fn mark_needs_review(vault_root: &Path, note: &Note) -> Result<()> {
    let abs_path = vault_root.join(&note.path);
    let content = std::fs::read_to_string(&abs_path)?;

    let fields = vec![("cortex-needs-review".to_string(), serde_yaml::Value::Bool(true))];

    if let Some(new_content) = insert_frontmatter_fields(&content, &fields) {
        std::fs::write(&abs_path, new_content)?;
    }

    Ok(())
}

/// Resolve filename collision. If the existing note has the same source URL,
/// this is a reingest replacement - return the original path (overwrite in place).
/// Only append a numeric suffix for genuinely different notes with the same slug.
fn resolve_collision(path: &Path, source_url: Option<&str>) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    // Same source URL means reingest replacement - overwrite, don't create -2
    if let Some(source) = source_url
        && existing_note_has_source(path, source)
    {
        log::info!(
            "collision is a reingest replacement (same source), overwriting: {}",
            path.display()
        );
        return path.to_path_buf();
    }

    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("note");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("md");
    let parent = path.parent().unwrap_or(Path::new("."));

    for i in 2..100 {
        let candidate = parent.join(format!("{stem}-{i}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }

    // Extremely unlikely - fall back to original
    path.to_path_buf()
}

/// Check if an existing note file contains a matching source URL in its frontmatter.
fn existing_note_has_source(path: &Path, source_url: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let mut buf = vec![0u8; 2048];
    let mut reader = std::io::BufReader::new(file);
    let n = reader.read(&mut buf).unwrap_or(0);
    let header = String::from_utf8_lossy(&buf[..n]);
    let needle = format!("source: \"{source_url}\"");
    header.contains(&needle)
}

/// Update wikilinks across vault after file moves
fn update_wikilinks_for_moves(vault_root: &Path, notes: &[Note], renames: &[(PathBuf, PathBuf)]) -> Result<()> {
    if renames.is_empty() {
        return Ok(());
    }

    let rename_map: Vec<(String, String)> = renames
        .iter()
        .filter_map(|(from, to)| {
            let old_stem = from.file_stem()?.to_str()?.to_string();
            let new_stem = to.file_stem()?.to_str()?.to_string();
            if old_stem == new_stem {
                None // Same filename, no wikilink update needed
            } else {
                Some((old_stem, new_stem))
            }
        })
        .collect();

    if rename_map.is_empty() {
        return Ok(());
    }

    for note in notes {
        let abs_path = vault_root.join(&note.path);
        let content = std::fs::read_to_string(&abs_path)?;
        let mut new_content = content.clone();

        for (old_stem, new_stem) in &rename_map {
            // Replace [[old_stem]] with [[new_stem]] and [[old_stem|alias]] with [[new_stem|alias]]
            let old_link = format!("[[{old_stem}]]");
            let new_link = format!("[[{new_stem}]]");
            new_content = new_content.replace(&old_link, &new_link);

            let old_alias_prefix = format!("[[{old_stem}|");
            let new_alias_prefix = format!("[[{new_stem}|");
            new_content = new_content.replace(&old_alias_prefix, &new_alias_prefix);
        }

        if new_content != content {
            std::fs::write(&abs_path, new_content)?;
            log::debug!("updated wikilinks in {}", note.path.display());
        }
    }

    Ok(())
}

/// Trait needed for Domain::from_str since vault uses custom FromStr
trait FromStrExt: Sized {
    fn from_str(s: &str) -> Result<Self, String>;
}

impl FromStrExt for Domain {
    fn from_str(s: &str) -> Result<Self, String> {
        s.parse::<Domain>().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::NoteBuilder;

    fn test_config() -> ClassifyConfig {
        ClassifyConfig::default()
    }

    #[test]
    fn test_classify_by_tags_single_domain() {
        let note = NoteBuilder::new("inbox/test-note.md")
            .title("Test Note")
            .tags(&["rust", "cli"])
            .build();

        let config = test_config();
        let result = classify_by_tags(&note, &config);
        assert!(result.is_some());
        let result = result.expect("should classify");
        assert_eq!(result.domain, Domain::Tech);
        assert_eq!(result.confidence, ClassifyConfidence::High);
        assert_eq!(result.method, ClassifyMethod::Deterministic);
    }

    #[test]
    fn test_classify_by_tags_no_match() {
        let note = NoteBuilder::new("inbox/test-note.md")
            .title("Test Note")
            .tags(&["random-tag", "unrelated"])
            .build();

        let config = test_config();
        let result = classify_by_tags(&note, &config);
        assert!(result.is_none());
    }

    #[test]
    fn test_classify_by_tags_ambiguous_tie() {
        // Tags matching two domains equally
        let note = NoteBuilder::new("inbox/test-note.md")
            .title("Test Note")
            .tags(&["rust", "claude"]) // rust=tech, claude=ai
            .build();

        let config = test_config();
        let result = classify_by_tags(&note, &config);
        // Should return None on a tie (fall through to Tier 2)
        assert!(result.is_none());
    }

    #[test]
    fn test_classify_by_source() {
        let note = NoteBuilder::new("inbox/test-note.md")
            .title("Test Note")
            .source("https://docs.rs/some-crate")
            .build();

        let mut config = test_config();
        config.source_domain_map.insert("tech".into(), vec!["docs.rs".into()]);

        let result = classify_by_source(&note, &config);
        assert!(result.is_some());
        let result = result.expect("should classify");
        assert_eq!(result.domain, Domain::Tech);
    }

    #[test]
    fn test_classify_by_source_no_match() {
        let note = NoteBuilder::new("inbox/test-note.md")
            .title("Test Note")
            .source("https://random-site.example.com")
            .build();

        let config = test_config();
        let result = classify_by_source(&note, &config);
        assert!(result.is_none());
    }

    #[test]
    fn test_filter_inbox_notes() {
        let inbox_note = NoteBuilder::new("inbox/test.md").title("Test").build();
        let notes_note = NoteBuilder::new("notes/other.md").title("Other").build();
        let notes = vec![inbox_note, notes_note];

        let filtered = filter_inbox_notes(&notes, false, false);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].path.to_string_lossy(), "inbox/test.md");
    }

    #[test]
    fn test_filter_skips_already_classified() {
        let mut note = NoteBuilder::new("inbox/test.md").title("Test").build();
        note.frontmatter
            .extra
            .insert("cortex-classified".to_string(), serde_yaml::Value::Bool(true));
        let notes = vec![note];

        let filtered = filter_inbox_notes(&notes, false, false);
        assert_eq!(filtered.len(), 0);

        // With force=true, should include it
        let filtered = filter_inbox_notes(&notes, true, false);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn test_resolve_collision_no_conflict() {
        let path = PathBuf::from("/tmp/nonexistent-classify-test-12345.md");
        assert_eq!(resolve_collision(&path, None), path);
    }

    #[test]
    fn test_build_enrichment_fields() {
        let result = ClassifyResult {
            domain: Domain::Ai,
            confidence: ClassifyConfidence::High,
            method: ClassifyMethod::Deterministic,
            reason: "test".to_string(),
        };

        let fields = build_enrichment_fields(&result);
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "domain" && v == &serde_yaml::Value::String("ai".to_string()))
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "status" && v == &serde_yaml::Value::String("unread".to_string()))
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "cortex-classified" && v == &serde_yaml::Value::Bool(true))
        );
    }

    #[test]
    fn test_classify_by_tags_compound_segment_match() {
        // Compound tags like "ai-agents" should match via segment "agents"
        let note = NoteBuilder::new("inbox/test-note.md")
            .title("AI Agents Article")
            .tags(&["ai-agents", "ai-strategy"])
            .build();

        let config = test_config();
        let result = classify_by_tags(&note, &config);
        assert!(result.is_some());
        let result = result.expect("should classify");
        assert_eq!(result.domain, Domain::Ai);
        assert_eq!(result.confidence, ClassifyConfidence::High);
    }

    #[test]
    fn test_classify_by_tags_compound_claude_code() {
        // "claude-code" should match via segment "claude" -> ai domain
        let note = NoteBuilder::new("inbox/test-note.md")
            .title("Claude Code Tips")
            .tags(&["claude-code", "claudecode"])
            .build();

        let config = test_config();
        let result = classify_by_tags(&note, &config);
        assert!(result.is_some());
        let result = result.expect("should classify");
        assert_eq!(result.domain, Domain::Ai);
    }

    #[test]
    fn test_classify_by_tags_compound_no_false_positive() {
        // Tags with no segment matching any trigger should still return None
        let note = NoteBuilder::new("inbox/test-note.md")
            .title("Random Article")
            .tags(&["career-advice", "hiring-trends"])
            .build();

        let config = test_config();
        let result = classify_by_tags(&note, &config);
        assert!(result.is_none());
    }

    #[test]
    fn test_classify_by_tags_single_word_still_works() {
        // Exact single-word matches should still work
        let note = NoteBuilder::new("inbox/test-note.md")
            .title("Rust Article")
            .tags(&["rust"])
            .build();

        let config = test_config();
        let result = classify_by_tags(&note, &config);
        assert!(result.is_some());
        assert_eq!(result.expect("should classify").domain, Domain::Tech);
    }

    #[test]
    fn test_classify_by_tags_multi_segment_tag() {
        // "ai-coding-agents" has segments ["ai", "coding", "agents"]
        // Both "ai" and "agents" are triggers for ai domain - should give ai +1 (not +2)
        let note = NoteBuilder::new("inbox/test-note.md")
            .title("AI Coding Agents")
            .tags(&["ai-coding-agents"])
            .build();

        let config = test_config();
        let result = classify_by_tags(&note, &config);
        assert!(result.is_some());
        assert_eq!(result.expect("should classify").domain, Domain::Ai);
    }

    #[test]
    fn test_classify_note_tags_win_over_source() {
        let note = NoteBuilder::new("inbox/test-note.md")
            .title("AI Article on GitHub")
            .tags(&["claude", "llm", "anthropic"])
            .source("https://github.com/anthropics/claude")
            .build();

        let mut config = test_config();
        config
            .source_domain_map
            .insert("tech".into(), vec!["github.com".into()]);

        // Tags say ai (3 matches), source says tech - tags should win because
        // classify_note tries tags first
        let result = classify_note(&note, &config, None);
        assert!(result.is_some());
        assert_eq!(result.expect("should classify").domain, Domain::Ai);
    }
}
