pub mod autotag;
pub mod classify;
pub mod config;
pub mod daemon;
pub mod duplicates;
pub mod embed;
pub mod fabric;
pub mod frontmatter;
pub mod intel;
pub mod linking;
pub mod links;
pub mod llm;
pub mod migrate;
pub mod naming;
pub mod opts;
pub mod quality;
pub mod report;
pub mod scope;
pub mod state;
pub mod summarize;
pub mod sweep;
pub mod tags;
pub mod testutil;
pub mod vault;

use colored::Colorize;
use eyre::Result;
use std::path::Path;

use config::Config;
use opts::{LinkOpts, LintOpts, StateOpts};
use report::Report;
use state::VaultManifest;
use vault::{Note, scan_vault};

/// Check if a note's path matches any glob pattern in the list.
fn matches_any(note: &Note, patterns: &[glob::Pattern]) -> bool {
    patterns.iter().any(|pat| {
        let path_str = note.path.to_string_lossy();
        pat.matches(&path_str)
            || note
                .path
                .file_name()
                .map(|f| pat.matches(f.to_string_lossy().as_ref()))
                .unwrap_or(false)
    })
}

/// Check if a note is excluded from enforcement.
/// A note is excluded if it matches an exclude pattern AND does not match any include pattern.
/// Include overrides exclude.
fn is_excluded(note: &Note, exclude_patterns: &[glob::Pattern], include_patterns: &[glob::Pattern]) -> bool {
    if !matches_any(note, exclude_patterns) {
        return false;
    }
    // Excluded, but check if include overrides
    if !include_patterns.is_empty() && matches_any(note, include_patterns) {
        return false;
    }
    true
}

/// Parse glob pattern strings into glob::Pattern objects.
fn parse_patterns(patterns: &[String]) -> Vec<glob::Pattern> {
    patterns
        .iter()
        .filter_map(|p| match glob::Pattern::new(p) {
            Ok(pat) => Some(pat),
            Err(e) => {
                log::warn!("invalid glob pattern, skipping: {}: {e}", p);
                None
            }
        })
        .collect()
}

pub fn lint(vault_root: &Path, config: &Config, opts: &LintOpts) -> Result<Report> {
    log::info!("starting lint run (vault_root={})", vault_root.display());
    let all_notes = scan_vault(vault_root, &config.vault)?;

    // Apply --path glob filter if provided
    let all_notes: Vec<_> = if let Some(ref pattern) = opts.path {
        let glob = glob::Pattern::new(pattern).map_err(|e| eyre::eyre!("invalid glob pattern '{}': {}", pattern, e))?;
        all_notes.into_iter().filter(|n| glob.matches_path(&n.path)).collect()
    } else {
        all_notes
    };

    // Split into all_notes (for link indexes) and lintable_notes (for violations)
    let exclude_patterns = parse_patterns(&config.vault.exclude);
    let include_patterns = parse_patterns(&config.vault.include);
    let lintable_notes: Vec<Note> = all_notes
        .iter()
        .filter(|n| !is_excluded(n, &exclude_patterns, &include_patterns))
        .cloned()
        .collect();

    log::info!(
        "vault scanned: all_count={}, lintable_count={}",
        all_notes.len(),
        lintable_notes.len()
    );

    let mut report = Report::default();

    let rules: Vec<&str> = if opts.rule.is_empty() {
        vec!["naming", "frontmatter", "tags", "scope", "broken-links"]
    } else {
        opts.rule.iter().map(|s| s.as_str()).collect()
    };

    log::info!("running lint rules: {:?}", rules);

    if rules.contains(&"naming") {
        report.merge(naming::lint_naming(&lintable_notes, &config.actions.naming));
        if opts.apply {
            naming::apply_naming(vault_root, &lintable_notes, &config.actions.naming)?;
        }
    }

    if rules.contains(&"frontmatter") {
        report.merge(frontmatter::lint_frontmatter(
            &lintable_notes,
            &config.actions.frontmatter,
            &config.schema,
        ));
        if opts.apply {
            frontmatter::apply_frontmatter(vault_root, &lintable_notes, &config.actions.frontmatter, &config.schema)?;
        }
    }

    if rules.contains(&"tags") {
        report.merge(tags::lint_tags(&lintable_notes, &config.actions.tags));
        if opts.apply {
            tags::apply_tags(vault_root, &lintable_notes, &config.actions.tags)?;
        }
    }

    if rules.contains(&"scope") {
        report.merge(scope::lint_scope(&lintable_notes, &config.actions.scope));
        if opts.apply {
            scope::apply_scope(vault_root, &lintable_notes, &config.actions.scope)?;
        }
    }

    if rules.contains(&"broken-links") {
        report.merge(links::lint_broken_links(
            &lintable_notes,
            &all_notes,
            &config.actions.broken_links,
        ));
    }

    if rules.contains(&"duplicates") {
        report.merge(duplicates::lint_duplicates(&lintable_notes, &config.actions.duplicates));
        if opts.apply {
            duplicates::apply_duplicates(vault_root, &lintable_notes, &config.actions.duplicates)?;
        }
    }

    if rules.contains(&"quality") {
        report.merge(quality::lint_quality(&lintable_notes, &config.actions.quality));
        if opts.apply {
            quality::apply_quality(vault_root, &lintable_notes, &config.actions.quality)?;
        }
    }

    if rules.contains(&"auto-tag") {
        report.merge(autotag::lint_autotag(
            &lintable_notes,
            &all_notes,
            &config.actions.auto_tag,
        ));
        if opts.apply {
            autotag::apply_autotag(vault_root, &lintable_notes, &all_notes, &config.actions.auto_tag)?;
        }
    }

    // Output formatting is the caller's responsibility (see sb/src/cli/cortex.rs).
    Ok(report)
}

pub fn state(vault_root: &Path, config: &Config, opts: &StateOpts) -> Result<()> {
    log::info!("starting state command (vault_root={})", vault_root.display());
    let cache_dir = &config.state.cache_dir;
    let manifest_path = VaultManifest::manifest_path(vault_root, cache_dir);

    if opts.refresh || opts.diff {
        let current = VaultManifest::scan(vault_root, &config.vault.ignore)?;

        if opts.diff {
            if manifest_path.exists() {
                let previous = VaultManifest::load(&manifest_path)?;
                let diff = previous.diff(&current);

                if diff.has_changes() {
                    if !diff.added.is_empty() {
                        println!("{}", "Added:".green().bold());
                        for p in &diff.added {
                            println!("  + {}", p.display());
                        }
                    }
                    if !diff.removed.is_empty() {
                        println!("{}", "Removed:".red().bold());
                        for p in &diff.removed {
                            println!("  - {}", p.display());
                        }
                    }
                    if !diff.modified.is_empty() {
                        println!("{}", "Modified:".yellow().bold());
                        for p in &diff.modified {
                            println!("  ~ {}", p.display());
                        }
                    }
                    println!(
                        "\n{}: {} added, {} removed, {} modified",
                        "Summary".bold(),
                        diff.added.len(),
                        diff.removed.len(),
                        diff.modified.len()
                    );
                } else {
                    println!("{}", "No changes since last scan.".green());
                }
            } else {
                println!("{}", "No previous manifest found. Run with --refresh first.".yellow());
            }
        }

        if opts.refresh {
            current.save(&manifest_path)?;
            println!(
                "{} manifest saved ({} files)",
                "Refreshed:".green().bold(),
                current.files.len()
            );
        }
    } else {
        // Default: show current manifest info
        if manifest_path.exists() {
            let manifest = VaultManifest::load(&manifest_path)?;
            println!(
                "Last scan: {} ({} files)",
                manifest.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
                manifest.files.len()
            );
        } else {
            println!(
                "{}",
                "No manifest found. Run `cortex state --refresh` to create one.".yellow()
            );
        }
    }

    Ok(())
}

pub fn link(vault_root: &Path, config: &Config, opts: &LinkOpts) -> Result<Report> {
    log::info!("starting link command (vault_root={})", vault_root.display());
    let all_notes = scan_vault(vault_root, &config.vault)?;
    let exclude_patterns = parse_patterns(&config.vault.exclude);
    let include_patterns = parse_patterns(&config.vault.include);
    let notes: Vec<Note> = all_notes
        .iter()
        .filter(|n| !is_excluded(n, &exclude_patterns, &include_patterns))
        .cloned()
        .collect();

    if opts.apply {
        let count = linking::apply_linking(vault_root, &notes, &config.actions.linking)?;
        Ok(Report {
            applied: count,
            ..Default::default()
        })
    } else {
        Ok(linking::lint_linking(&notes, &config.actions.linking))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::NoteBuilder;

    fn note(path: &str) -> Note {
        NoteBuilder::new(path).title(path).build()
    }

    #[test]
    fn test_not_excluded_by_default() {
        let n = note("notes/foo.md");
        assert!(!is_excluded(&n, &[], &[]));
    }

    #[test]
    fn test_excluded_by_pattern() {
        let n = note("system/templates/link.md");
        let exclude = parse_patterns(&["system/templates/**".to_string()]);
        assert!(is_excluded(&n, &exclude, &[]));
    }

    #[test]
    fn test_include_overrides_exclude() {
        let n = note("system/design-vault.md");
        let exclude = parse_patterns(&["system/**".to_string()]);
        let include = parse_patterns(&["system/design-*.md".to_string()]);
        assert!(!is_excluded(&n, &exclude, &include));
    }

    #[test]
    fn test_include_does_not_affect_non_excluded() {
        let n = note("notes/foo.md");
        let exclude = parse_patterns(&["system/**".to_string()]);
        let include = parse_patterns(&["system/design-*.md".to_string()]);
        assert!(!is_excluded(&n, &exclude, &include));
    }

    #[test]
    fn test_excluded_not_rescued_by_unmatched_include() {
        let n = note("system/templates/link.md");
        let exclude = parse_patterns(&["system/**".to_string()]);
        let include = parse_patterns(&["system/design-*.md".to_string()]);
        assert!(is_excluded(&n, &exclude, &include));
    }
}
