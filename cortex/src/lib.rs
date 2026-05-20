// Lib invariant: cortex pub fns return typed data; sb owns stdout/stderr.
// Production code emits nothing via println!/eprintln! - log::* routes
// through the logger initializer instead. Test modules that print captured
// stdout are exempted via the not(test) guard below.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

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

use eyre::Result;
use std::path::Path;

use config::Config;
use opts::{LinkOpts, LintOpts};
use report::Report;
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

pub fn link(vault_root: &Path, config: &Config, opts: &LinkOpts) -> Result<Report> {
    log::info!(
        "starting link command (vault_root={} scan={:?})",
        vault_root.display(),
        opts.scan
    );
    let all_notes = scan_vault(vault_root, &config.vault)?;
    let exclude_patterns = parse_patterns(&config.vault.exclude);
    let include_patterns = parse_patterns(&config.vault.include);
    let notes: Vec<Note> = all_notes
        .iter()
        .filter(|n| !is_excluded(n, &exclude_patterns, &include_patterns))
        .cloned()
        .collect();

    // `ScanScope::All` falls through to whatever the config holds; any other
    // variant overrides the config's `actions.linking.scan-for`. This is the
    // wiring the CLI flag was missing until v0.8.5.
    let overridden;
    let linking_config = if opts.scan == crate::opts::ScanScope::All {
        &config.actions.linking
    } else {
        overridden = crate::config::LinkingConfig {
            scan_for: opts.scan.as_config_scan_for(),
            ..config.actions.linking.clone()
        };
        &overridden
    };

    if opts.apply {
        let count = linking::apply_linking(vault_root, &notes, linking_config)?;
        Ok(Report {
            applied: count,
            ..Default::default()
        })
    } else {
        Ok(linking::lint_linking(&notes, linking_config))
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

    #[test]
    fn scan_scope_as_config_scan_for_maps_each_variant() {
        use crate::opts::ScanScope;
        assert_eq!(ScanScope::People.as_config_scan_for(), vec!["people".to_string()]);
        assert_eq!(ScanScope::Projects.as_config_scan_for(), vec!["projects".to_string()]);
        assert_eq!(ScanScope::Concepts.as_config_scan_for(), vec!["concepts".to_string()]);
        assert_eq!(ScanScope::All.as_config_scan_for(), vec!["all".to_string()]);
    }
}
