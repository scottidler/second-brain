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
pub mod entities;
pub mod fabric;
pub mod frontmatter;
pub mod graph;
pub mod hub;
pub mod intel;
pub mod linking;
pub mod links;
pub mod llm;
pub mod memgraph;
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
use report::{LintApplyReport, Report};
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

pub fn lint(vault_root: &Path, config: &Config, opts: &LintOpts) -> Result<(Report, LintApplyReport)> {
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
    // Written-paths accumulator for the apply path ONLY - fed exclusively by
    // each rule's `apply_*` return value (real byte-changed paths), never by
    // `report.violations` (which includes every `fix: None` violation - the
    // permanently-unfixable majority - and would fingerprint identically
    // every cycle). This is the seam `LintApplyReport` surfaces to callers
    // like the daemon's oscillation detector.
    let mut written_paths: Vec<String> = Vec::new();

    let rules: Vec<&str> = if opts.rule.is_empty() {
        vec!["naming", "frontmatter", "tags", "scope", "broken-links"]
    } else {
        opts.rule.iter().map(|s| s.as_str()).collect()
    };

    log::info!("running lint rules: {:?}", rules);

    if rules.contains(&"naming") {
        report.merge(naming::lint_naming(&lintable_notes, &config.actions.naming));
        if opts.apply {
            written_paths.extend(naming::apply_naming(
                vault_root,
                &lintable_notes,
                &config.actions.naming,
            )?);
        }
    }

    if rules.contains(&"frontmatter") {
        report.merge(frontmatter::lint_frontmatter(
            &lintable_notes,
            &config.actions.frontmatter,
            &config.schema,
        ));
        if opts.apply {
            written_paths.extend(frontmatter::apply_frontmatter(
                vault_root,
                &lintable_notes,
                &config.actions.frontmatter,
                &config.schema,
            )?);
        }
    }

    if rules.contains(&"tags") {
        report.merge(tags::lint_tags(&lintable_notes, &config.actions.tags));
        if opts.apply {
            written_paths.extend(tags::apply_tags(vault_root, &lintable_notes, &config.actions.tags)?);
        }
    }

    if rules.contains(&"scope") {
        report.merge(scope::lint_scope(&lintable_notes, &config.actions.scope));
        if opts.apply {
            written_paths.extend(scope::apply_scope(vault_root, &lintable_notes, &config.actions.scope)?);
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
            written_paths.extend(duplicates::apply_duplicates(
                vault_root,
                &lintable_notes,
                &config.actions.duplicates,
            )?);
        }
    }

    if rules.contains(&"quality") {
        report.merge(quality::lint_quality(&lintable_notes, &config.actions.quality));
        if opts.apply {
            written_paths.extend(quality::apply_quality(
                vault_root,
                &lintable_notes,
                &config.actions.quality,
            )?);
        }
    }

    if rules.contains(&"auto-tag") {
        report.merge(autotag::lint_autotag(
            &lintable_notes,
            &all_notes,
            &config.actions.auto_tag,
        ));
        if opts.apply {
            written_paths.extend(autotag::apply_autotag(
                vault_root,
                &lintable_notes,
                &all_notes,
                &config.actions.auto_tag,
                &config.fabric,
            )?);
        }
    }

    written_paths.sort();
    written_paths.dedup();
    let lint_apply = LintApplyReport {
        written_paths,
        remaining_violations: report.violations.len(),
    };
    report.applied = lint_apply.written_paths.len();
    report.applied_paths = lint_apply.written_paths.clone();

    log::debug!(
        "lib::lint: apply={} written={} remaining_violations={}",
        opts.apply,
        lint_apply.written_paths.len(),
        lint_apply.remaining_violations
    );

    // Output formatting is the caller's responsibility (see sb/src/cli/cortex.rs).
    Ok((report, lint_apply))
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
        // `applied_paths` carries the real, byte-changed paths - the ONLY
        // thing a caller (the daemon's oscillation fingerprint) may use.
        // `applied` (a plain count) stays for CLI text that only needs "N
        // file(s)"; both derive from the same `apply_linking` return.
        let written = linking::apply_linking(vault_root, &notes, &linking_config)?;
        Ok(Report {
            applied: written.len(),
            applied_paths: written,
            ..Default::default()
        })
    } else {
        Ok(linking::lint_linking(&notes, &linking_config))
    }
}

#[cfg(test)]
mod tests;
