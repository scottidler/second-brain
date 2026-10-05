use std::collections::HashSet;

use crate::config::BrokenLinksConfig;
use crate::report::{Report, Severity, Violation};
use crate::vault::Note;
use vault::wikilink::Resolver;

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

    // Path/stem resolution is `Resolver`'s (Obsidian's rules), built once.
    let resolver = Resolver::new(all_notes.iter().map(|n| n.path.to_string_lossy().replace('\\', "/")));

    // Title index: lowercased frontmatter titles for exact title match
    let title_set: HashSet<String> = all_notes
        .iter()
        .filter_map(|n| n.frontmatter.title.as_ref())
        .map(|t| t.to_lowercase())
        .collect();

    // Only check lintable notes for violations
    for note in lintable_notes {
        // A same-note link (`[[#h]]`, empty target) has nothing to resolve.
        for wikilink in vault::wikilink::parse(&note.body).filter(|l| !l.target.is_empty()) {
            let link = wikilink.target;
            let target_lower = link.to_lowercase().replace('\\', "/");
            let target_slug = crate::naming::to_slug(link);

            // Resolution order: path/stem -> title -> slug-of-target. The title
            // and slug fallbacks are this lint's deliberate leniency; no other
            // consumer gets them.
            let exists = resolver.resolve(&target_lower).next().is_some()
                || title_set.contains(&target_lower)
                || resolver.resolve(&target_slug).next().is_some();

            if !exists {
                // Classify unresolved links by type
                let (rule, severity) = if is_asset_reference(link) {
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

#[cfg(test)]
mod tests;
