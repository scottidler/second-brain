use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

use crate::config::BrokenLinksConfig;
use crate::report::{Report, Severity, Violation};
use crate::vault::Note;

/// Regex to match [[wikilinks]] and [[wikilinks|display text]].
static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]").expect("valid wikilink regex"));

/// Asset file extensions that indicate a genuinely broken embed/reference
/// when the target file is missing (as opposed to an aspirational note link).
const ASSET_EXTENSIONS: &[&str] = &[
    // Images
    ".png",
    ".jpg",
    ".jpeg",
    ".gif",
    ".svg",
    ".webp",
    ".bmp",
    ".tiff",
    // Documents
    ".pdf",
    // Media
    ".mp4",
    ".mp3",
    ".wav",
    ".webm",
    ".ogg",
    ".m4a",
    // Other Obsidian embed types
    ".csv",
    ".excalidraw",
];

/// Check if a wikilink target refers to an asset (image, PDF, media, etc.)
/// based on its file extension.
fn is_asset_reference(target: &str) -> bool {
    let lower = target.to_lowercase();
    ASSET_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

/// Strip fenced code blocks from markdown body so that wikilinks inside
/// code blocks are not extracted. Uses line-by-line state tracking.
fn strip_fenced_code_blocks(body: &str) -> String {
    let mut result = String::with_capacity(body.len());
    let mut in_fence = false;

    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            result.push('\n');
            continue;
        }
        if in_fence {
            result.push('\n');
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

/// Run broken link detection.
/// `lintable_notes` are checked for violations; `all_notes` are used to build
/// the resolution indexes (so excluded files still count as valid link targets).
pub fn lint_broken_links(lintable_notes: &[Note], all_notes: &[Note], config: &BrokenLinksConfig) -> Report {
    let mut report = Report::default();

    if !config.check_wikilinks {
        return report;
    }

    // Build indexes from ALL notes (including excluded) so that links to
    // excluded files still resolve correctly.

    // Stem index: file stems in lowercase
    let note_stems: HashSet<String> = all_notes
        .iter()
        .filter_map(|n| n.path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_lowercase()))
        .collect();

    // Path index: full paths with extension stripped, lowercased
    let note_paths: HashSet<String> = all_notes
        .iter()
        .map(|n| {
            let path = n.path.with_extension("");
            path.to_string_lossy().to_lowercase()
        })
        .collect();

    // Title index: lowercased frontmatter titles for exact title match
    let title_set: HashSet<String> = all_notes
        .iter()
        .filter_map(|n| n.frontmatter.title.as_ref())
        .map(|t| t.to_lowercase())
        .collect();

    // Only check lintable notes for violations
    for note in lintable_notes {
        let clean_body = strip_fenced_code_blocks(&note.body);
        let links = extract_wikilinks(&clean_body);

        for link in links {
            let target_lower = link.to_lowercase();
            let target_slug = crate::naming::to_slug(&link);

            // Resolution order: stem -> path -> title -> slug-of-target
            let exists = note_stems.contains(&target_lower)
                || note_paths.contains(&target_lower.replace('\\', "/"))
                || title_set.contains(&target_lower)
                || note_stems.contains(&target_slug);

            if !exists {
                // Classify unresolved links by type
                let (rule, severity) = if is_asset_reference(&link) {
                    ("broken-links.asset", Severity::Error)
                } else if link.ends_with('/') {
                    ("broken-links.folder", Severity::Error)
                } else {
                    ("broken-links.unresolved", Severity::Info)
                };

                report.add(Violation {
                    path: note.path.clone(),
                    rule: rule.to_string(),
                    severity,
                    message: format!("broken wikilink: [[{link}]]"),
                    fix: None,
                });
            }
        }
    }

    log::info!("broken links lint complete: {} violation(s)", report.violations.len());
    report
}

/// Extract all wikilink targets from a note body.
fn extract_wikilinks(body: &str) -> Vec<String> {
    WIKILINK_RE
        .captures_iter(body)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().trim().to_string()))
        .collect()
}

#[cfg(test)]
mod tests;
