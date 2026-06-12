//! Migration from legacy ~/.config/{borg,cortex,obsidian-cortex,oracle,second-brain}
//! into the unified ~/.config/sb/ layout.
//!
//! Invariants:
//! - Never deletes a legacy file. A future `sb bootstrap --prune-legacy-config`
//!   verb (out of scope here) is the cleanup.
//! - Idempotent: a marker file `.migrated-to-sb` is dropped in each migrated
//!   legacy directory so a re-run noops.
//! - Conflict-safe: if the new path exists and content differs from the legacy
//!   source, refuse and surface a diff hint. The user resolves manually.
//! - YAML-aware: after copying, any value matching a legacy path literal inside
//!   the yaml is rewritten to the new sb/ location. Users who customised these
//!   paths to point somewhere off-tree are left alone.

use eyre::{Context, Result};
use std::path::{Path, PathBuf};

const MARKER: &str = ".migrated-to-sb";

const LEGACY_DIRS: &[&str] = &["borg", "cortex", "obsidian-cortex", "oracle", "second-brain"];

#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
    pub had_conflicts: bool,
}

/// Anything in `~/.config/{borg,cortex,obsidian-cortex,oracle,second-brain}` looks
/// like a legacy layout. We do not check for specific files - the directories
/// alone are evidence.
pub fn legacy_detected() -> bool {
    let Some(config_root) = vault::paths::xdg_config_dir() else {
        return false;
    };
    LEGACY_DIRS.iter().any(|name| {
        let dir = config_root.join(name);
        // If the marker is present we have already migrated this dir; skip it
        // for detection purposes.
        if dir.join(MARKER).exists() {
            return false;
        }
        dir.exists() && has_relevant_content(&dir)
    })
}

fn has_relevant_content(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.ends_with(".yml") || name.ends_with(".yaml") {
                return true;
            }
        } else if path.is_dir() && path.file_name().and_then(|s| s.to_str()) == Some("patterns") {
            return true;
        }
    }
    false
}

pub fn migrate_legacy_layout() -> Result<Report> {
    let Some(config_root) = vault::paths::xdg_config_dir() else {
        eyre::bail!("xdg_config_dir() returned None");
    };
    let sb_root = vault::paths::config_root();
    std::fs::create_dir_all(&sb_root).with_context(|| format!("create {}", sb_root.display()))?;

    let mut report = Report::default();

    // Map of legacy_path -> new_path for individual files
    let plans: Vec<(PathBuf, PathBuf)> = vec![
        (config_root.join("borg").join("borg.yml"), vault::paths::borg_config()),
        (
            config_root.join("cortex").join("cortex.yml"),
            vault::paths::cortex_config(),
        ),
        (
            config_root.join("obsidian-cortex").join("obsidian-cortex.yml"),
            vault::paths::cortex_config(),
        ),
        (
            config_root.join("oracle").join("oracle.yml"),
            vault::paths::oracle_config(),
        ),
        (
            config_root.join("second-brain").join("canonical-tags.yml"),
            vault::paths::canonical_tags(),
        ),
        (
            config_root.join("second-brain").join("tag-mapping.yml"),
            vault::paths::tag_mapping(),
        ),
        (
            config_root.join("second-brain").join("tag-proposals.yml"),
            vault::paths::tag_proposals(),
        ),
    ];

    for (src, dst) in &plans {
        copy_file_with_rewrite(src, dst, &mut report)?;
    }

    let patterns_src = config_root.join("borg").join("patterns");
    let patterns_dst = vault::paths::patterns_dir();
    copy_patterns_dir(&patterns_src, &patterns_dst, &mut report)?;

    // Drop markers in the legacy directories so subsequent runs noop.
    for legacy_dir in LEGACY_DIRS {
        let dir = config_root.join(legacy_dir);
        if !dir.exists() {
            continue;
        }
        let marker = dir.join(MARKER);
        if !marker.exists() {
            // best-effort; if we can't write the marker, the next run just retries
            let _ = std::fs::write(&marker, b"sb bootstrap migrated this directory\n");
        }
    }

    Ok(report)
}

fn copy_file_with_rewrite(src: &Path, dst: &Path, report: &mut Report) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    let src_content = std::fs::read(src).with_context(|| format!("read {}", src.display()))?;
    let rewritten = rewrite_legacy_paths_in_yaml(src, &src_content);

    if dst.exists() {
        let dst_content = std::fs::read(dst).with_context(|| format!("read {}", dst.display()))?;
        if dst_content == rewritten {
            report
                .lines
                .push(format!("noop: {} already matches {}", dst.display(), src.display()));
        } else {
            report.had_conflicts = true;
            report.lines.push(format!(
                "conflict: {} differs from migrated form of {}; resolve manually before rerunning",
                dst.display(),
                src.display()
            ));
        }
        return Ok(());
    }

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(dst, &rewritten).with_context(|| format!("write {}", dst.display()))?;
    report
        .lines
        .push(format!("migrated: {} -> {}", src.display(), dst.display()));
    Ok(())
}

fn copy_patterns_dir(src: &Path, dst: &Path, report: &mut Report) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    if dst.exists() {
        // Compare every file; if anything differs, conflict. If all match, noop.
        let mut all_match = true;
        for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
            let entry = entry?;
            let name = entry.file_name();
            let dst_file = dst.join(&name);
            if !dst_file.exists() {
                all_match = false;
                break;
            }
            let a = std::fs::read(entry.path())?;
            let b = std::fs::read(&dst_file)?;
            if a != b {
                all_match = false;
                break;
            }
        }
        if all_match {
            report.lines.push(format!(
                "noop: patterns dir {} already matches {}",
                dst.display(),
                src.display()
            ));
        } else {
            report.had_conflicts = true;
            report.lines.push(format!(
                "conflict: patterns dir {} differs from {}; resolve manually before rerunning",
                dst.display(),
                src.display()
            ));
        }
        return Ok(());
    }
    std::fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let dst_file = dst.join(entry.file_name());
        std::fs::copy(entry.path(), &dst_file)
            .with_context(|| format!("copy {} -> {}", entry.path().display(), dst_file.display()))?;
    }
    report
        .lines
        .push(format!("migrated patterns dir: {} -> {}", src.display(), dst.display()));
    Ok(())
}

/// Rewrite legacy `~/.config/{borg,second-brain,obsidian-cortex,oracle,cortex}` path
/// substrings inside YAML to the new `~/.config/sb/` location. Operates on raw
/// bytes so user-edited comments and anchors are preserved verbatim. Only literal
/// matches are rewritten - a custom user path stays untouched.
fn rewrite_legacy_paths_in_yaml(src: &Path, content: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(content) else {
        return content.to_vec();
    };
    let mut text = text.to_string();
    let pairs: &[(&str, &str)] = &[
        (
            "~/.config/second-brain/canonical-tags.yml",
            "~/.config/sb/canonical-tags.yml",
        ),
        ("~/.config/second-brain/tag-mapping.yml", "~/.config/sb/tag-mapping.yml"),
        (
            "~/.config/second-brain/tag-proposals.yml",
            "~/.config/sb/tag-proposals.yml",
        ),
        ("~/.config/borg/patterns/", "~/.config/sb/patterns/"),
        ("~/.config/borg/patterns", "~/.config/sb/patterns"),
    ];
    for (from, to) in pairs {
        text = text.replace(from, to);
    }
    log::debug!(
        "rewrite_legacy_paths_in_yaml: src={} bytes_in={} bytes_out={}",
        src.display(),
        content.len(),
        text.len()
    );
    text.into_bytes()
}

#[cfg(test)]
mod tests;
