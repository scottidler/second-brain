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
pub mod graph;
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
pub mod startup;
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
            autotag::apply_autotag(
                vault_root,
                &lintable_notes,
                &all_notes,
                &config.actions.auto_tag,
                &config.fabric,
            )?;
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

    // Build the effective linking config: start from the base (config or the
    // `--scan`-overridden scan-for), then fold in the shared `glossary.yml`
    // concepts + aliases (Phase 2). `ScanScope::All` falls through to whatever
    // the config holds; any other variant overrides `actions.linking.scan-for`.
    let mut linking_config = config.actions.linking.clone();
    if opts.scan != crate::opts::ScanScope::All {
        linking_config.scan_for = opts.scan.as_config_scan_for();
    }
    let glossary = linking::load_glossary(&::vault::paths::glossary())?;
    // Config-provided concepts (if any) plus the glossary's; glossary aliases
    // win over any config-level aliases on key collision.
    linking_config.entities.concepts.extend(glossary.concepts);
    linking_config.aliases.extend(glossary.aliases);

    if opts.apply {
        let count = linking::apply_linking(vault_root, &notes, &linking_config)?;
        Ok(Report {
            applied: count,
            ..Default::default()
        })
    } else {
        Ok(linking::lint_linking(&notes, &linking_config))
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

    /// Build a config + note fixture that should fire one violation per
    /// rule class (concept, person, project) so we can assert subset behavior
    /// across `--scan` variants.
    fn linking_fixture() -> (crate::config::LinkingConfig, Vec<Note>) {
        let config = crate::config::LinkingConfig {
            scan_for: vec!["all".to_string()],
            entities: crate::config::LinkingEntities {
                people: vec!["Alice Smith".to_string()],
                projects: vec!["ProjectAtlas".to_string()],
                concepts: Vec::new(),
            },
            targets: crate::config::LinkingTargets::default(),
            min_word_length: 4,
            aliases: std::collections::HashMap::new(),
        };
        // Concept target: a separate note with title "Distillation" lives in the
        // vault. The probe note mentions Alice Smith, ProjectAtlas, and Distillation
        // in its body so each scan_for variant has at least one mention to flag.
        let concept_note = NoteBuilder::new("notes/distillation.md").title("Distillation").build();
        let probe = NoteBuilder::new("notes/probe.md")
            .title("probe")
            .body("Alice Smith owns ProjectAtlas. The Distillation step is documented here.")
            .build();
        (config, vec![concept_note, probe])
    }

    fn run_link_scope(scan_for: Vec<String>) -> std::collections::HashSet<String> {
        let (mut config, notes) = linking_fixture();
        config.scan_for = scan_for;
        let report = crate::linking::lint_linking(&notes, &config);
        report.violations.into_iter().map(|v| v.rule).collect()
    }

    #[test]
    fn link_scan_people_is_strict_subset_of_all() {
        let all = run_link_scope(vec!["all".to_string()]);
        let people = run_link_scope(vec!["people".to_string()]);

        assert!(people.iter().all(|r| all.contains(r)), "people={people:?} all={all:?}");
        assert!(
            people.len() < all.len(),
            "people scope should be a STRICT subset: people={people:?} all={all:?}"
        );
        assert!(
            people.contains("linking.person"),
            "people scope must still flag persons"
        );
        assert!(
            !people.contains("linking.project") && !people.contains("linking.concept"),
            "people scope must not flag projects or concepts: {people:?}"
        );
    }

    #[test]
    fn link_scan_projects_is_strict_subset_of_all() {
        let all = run_link_scope(vec!["all".to_string()]);
        let projects = run_link_scope(vec!["projects".to_string()]);
        assert!(projects.iter().all(|r| all.contains(r)));
        assert!(projects.len() < all.len());
        assert!(projects.contains("linking.project"));
        assert!(!projects.contains("linking.person"));
        assert!(!projects.contains("linking.concept"));
    }

    #[test]
    fn link_scan_concepts_is_strict_subset_of_all() {
        let all = run_link_scope(vec!["all".to_string()]);
        let concepts = run_link_scope(vec!["concepts".to_string()]);
        assert!(concepts.iter().all(|r| all.contains(r)));
        assert!(concepts.len() < all.len());
        assert!(concepts.contains("linking.concept"));
        assert!(!concepts.contains("linking.person"));
        assert!(!concepts.contains("linking.project"));
    }
}
