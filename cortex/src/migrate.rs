use eyre::{Context, Result};
use glob::Pattern;
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::{Config, MigrationConfig};
use crate::opts::MigrateOpts;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::{Note, scan_vault};

/// Top-level orchestrator for `sb cortex migrate`. Scans the vault, then runs
/// `apply_migrate` (when `opts.apply`) or `lint_migrate` (dry-run).
pub fn run(vault_root: &Path, config: &Config, opts: &MigrateOpts) -> Result<Report> {
    log::info!("starting migrate command (vault_root={})", vault_root.display());
    let notes = scan_vault(vault_root, &config.vault)?;
    if opts.apply {
        let count = apply_migrate(vault_root, &notes, &config.migrations)?;
        Ok(Report {
            applied: count,
            ..Default::default()
        })
    } else {
        Ok(lint_migrate(&notes, &config.migrations))
    }
}

/// Planned file move with optional frontmatter updates.
#[derive(Debug)]
struct PlannedMove {
    from: PathBuf,
    to: PathBuf,
    set_frontmatter: Vec<(String, serde_yaml::Value)>,
}

/// Run migration dry-run: report what would be moved and what fields would change.
pub fn lint_migrate(notes: &[Note], migrations: &[MigrationConfig]) -> Report {
    let mut report = Report::default();

    for migration in migrations {
        // Report file moves
        let moves = plan_migration(notes, migration);
        for planned in &moves {
            report.add(Violation {
                path: planned.from.clone(),
                rule: format!("migrate.{}", migration.name),
                severity: Severity::Info,
                message: format!("would move to {}", planned.to.display()),
                fix: Some(Fix::MoveFile {
                    from: planned.from.clone(),
                    to: planned.to.clone(),
                }),
            });
        }

        // Report field transforms
        lint_field_transforms(notes, migration, &mut report);

        // Report value transforms
        lint_value_transforms(notes, migration, &mut report);
    }

    log::info!("migrate lint complete: {} violation(s)", report.violations.len());
    report
}

/// Apply migrations: field transforms first, then file moves.
pub fn apply_migrate(vault_root: &Path, notes: &[Note], migrations: &[MigrationConfig]) -> Result<usize> {
    let mut total_count = 0;

    // Phase 1: Apply field transforms (renames and drops)
    for migration in migrations {
        if !migration.field_renames.is_empty() || !migration.field_drops.is_empty() {
            let count = apply_field_transforms(vault_root, notes, migration)?;
            total_count += count;
        }
    }

    // Phase 1b: Apply value transforms (value renames within fields)
    for migration in migrations {
        if !migration.value_renames.is_empty() {
            let count = apply_value_transforms(vault_root, notes, migration)?;
            total_count += count;
        }
    }

    // Phase 2: Apply file moves
    let mut all_moves: Vec<PlannedMove> = Vec::new();
    for migration in migrations {
        all_moves.extend(plan_migration(notes, migration));
    }

    if all_moves.is_empty() {
        return Ok(total_count);
    }

    let mut move_count = 0;
    let mut applied: Vec<(PathBuf, PathBuf)> = Vec::new();

    // Execute moves
    for planned in &all_moves {
        let abs_from = vault_root.join(&planned.from);
        let abs_to = vault_root.join(&planned.to);

        if let Some(parent) = abs_to.parent() {
            std::fs::create_dir_all(parent).context(format!("failed to create directory {}", parent.display()))?;
        }

        // Never clobber: a real file at the destination (routine on a
        // Syncthing'd vault) would be silently destroyed by `fs::rename`.
        if abs_to.exists() {
            log::warn!(
                "skipping migrate {} -> {}: destination already exists (would clobber)",
                planned.from.display(),
                planned.to.display()
            );
            continue;
        }

        std::fs::rename(&abs_from, &abs_to).context(format!(
            "failed to move {} to {}",
            abs_from.display(),
            abs_to.display()
        ))?;

        // Apply frontmatter updates if any
        if !planned.set_frontmatter.is_empty() {
            let content = std::fs::read_to_string(&abs_to)?;
            if let Some(new_content) = crate::scope::insert_frontmatter_fields(&content, &planned.set_frontmatter) {
                vault::note::write_atomic(&abs_to, new_content.as_bytes())?;
            }
        }

        log::info!("migrated file: {} -> {}", planned.from.display(), planned.to.display());
        applied.push((planned.from.clone(), planned.to.clone()));
        move_count += 1;
    }

    // Batch update wikilinks for the moves that actually landed.
    crate::naming::update_wikilinks_batch(vault_root, notes, &applied)?;

    Ok(total_count + move_count)
}

/// Plan all moves for a single migration config.
fn plan_migration(notes: &[Note], migration: &MigrationConfig) -> Vec<PlannedMove> {
    let mut moves = Vec::new();

    for move_rule in &migration.moves {
        let pattern = match Pattern::new(&move_rule.from) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("invalid glob pattern: {}: {e}", move_rule.from);
                continue;
            }
        };

        for note in notes {
            let path_str = note.path.to_string_lossy();
            if pattern.matches(&path_str) {
                let filename = match note.path.file_name() {
                    Some(f) => f,
                    None => continue,
                };

                let to = PathBuf::from(&move_rule.to).join(filename);

                // Don't plan a move if source == destination
                if note.path == to {
                    continue;
                }

                let set_frontmatter = move_rule
                    .set_frontmatter
                    .as_ref()
                    .map(|fm| fm.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default();

                moves.push(PlannedMove {
                    from: note.path.clone(),
                    to,
                    set_frontmatter,
                });
            }
        }
    }

    moves
}

/// Update wikilinks across the vault after file moves.
/// Report what field transforms would be applied (dry-run).
///
/// Parallel over `notes`: each note can emit multiple violations (one per matching rename/drop),
/// so we use `flat_map` instead of `filter_map`. `par_iter().flat_map().collect()` preserves
/// the input-slice order over the per-note Vec<Violation>, so the final Report sequence is
/// bit-identical to the previous sequential implementation.
fn lint_field_transforms(notes: &[Note], migration: &MigrationConfig, report: &mut Report) {
    if migration.field_renames.is_empty() && migration.field_drops.is_empty() {
        return;
    }

    let violations: Vec<Violation> = notes
        .par_iter()
        .flat_map(|note| {
            let mut out = Vec::new();
            for (old_key, new_key) in &migration.field_renames {
                if note.frontmatter.extra.contains_key(old_key) {
                    out.push(Violation {
                        path: note.path.clone(),
                        rule: format!("migrate.{}.rename", migration.name),
                        severity: Severity::Info,
                        message: format!("would rename field '{old_key}' to '{new_key}'"),
                        fix: None,
                    });
                }
            }
            for drop_key in &migration.field_drops {
                if note.frontmatter.extra.contains_key(drop_key) {
                    out.push(Violation {
                        path: note.path.clone(),
                        rule: format!("migrate.{}.drop", migration.name),
                        severity: Severity::Info,
                        message: format!("would drop field '{drop_key}'"),
                        fix: None,
                    });
                }
            }
            out
        })
        .collect();

    for v in violations {
        report.add(v);
    }
}

/// Apply field renames and drops within frontmatter blocks.
/// Operates on the raw text between `---` delimiters to preserve formatting.
///
/// Parallel over `notes` via rayon. Per-note read-modify-write is independent (each note is its
/// own file, plain `std::fs::write` with no explicit fsync), so the same lock-contention
/// argument as `apply_quality` applies. `try_reduce` aggregates the success counter and
/// short-circuits on the first error to preserve sequential fail-fast semantics.
fn apply_field_transforms(vault_root: &Path, notes: &[Note], migration: &MigrationConfig) -> Result<usize> {
    notes
        .par_iter()
        .map(|note| -> Result<usize> {
            // Quick check: does this note have any fields to transform?
            let has_rename_target = migration
                .field_renames
                .keys()
                .any(|k| note.frontmatter.extra.contains_key(k));
            let has_drop_target = migration
                .field_drops
                .iter()
                .any(|k| note.frontmatter.extra.contains_key(k));

            if !has_rename_target && !has_drop_target {
                return Ok(0);
            }

            let abs_path = vault_root.join(&note.path);
            let content =
                std::fs::read_to_string(&abs_path).context(format!("failed to read {}", abs_path.display()))?;

            let Some((fm_block, before, after)) = extract_frontmatter_block(&content) else {
                return Ok(0);
            };

            let mut lines: Vec<String> = fm_block.lines().map(String::from).collect();
            let mut changed = false;

            // Build set of existing keys for conflict detection
            let existing_keys: HashSet<String> = lines
                .iter()
                .filter_map(|l| l.split(':').next().map(|k| k.trim().to_string()))
                .collect();

            // Apply renames
            for (old_key, new_key) in &migration.field_renames {
                for line in &mut lines {
                    if line.starts_with(&format!("{old_key}:")) {
                        if existing_keys.contains(new_key) {
                            log::warn!(
                                "skipping rename: target field already exists: {} ({old_key} -> {new_key})",
                                note.path.display()
                            );
                        } else {
                            *line = line.replacen(old_key, new_key, 1);
                            changed = true;
                        }
                    }
                }
            }

            // Apply drops. Route through scope's continuation-aware
            // `remove_entry` so a multi-line list/nested-map value (column-0
            // `- bullet` or indented `  - bullet`) has its continuation lines
            // removed too. A bare `retain` on `starts_with("{dk}:")` dropped
            // only the header line and left the bullets orphaned - invalid
            // YAML the parser then read as defaults.
            let original_len = lines.len();
            for dk in &migration.field_drops {
                crate::scope::remove_entry(&mut lines, dk);
            }
            if lines.len() != original_len {
                changed = true;
            }

            if changed {
                let new_content = format!("{before}---\n{}\n---{after}", lines.join("\n"));
                vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
                log::info!("applied field transforms: {}", note.path.display());
                Ok(1)
            } else {
                Ok(0)
            }
        })
        .try_reduce(|| 0usize, |a, b| Ok(a + b))
}

/// Extract the frontmatter block from file content.
/// Returns (frontmatter_text, content_before_opening_delim, content_after_closing_delim).
fn extract_frontmatter_block(content: &str) -> Option<(&str, &str, &str)> {
    // (fm_block, after) come from the shared splitter; `before` is the leading
    // whitespace the splitter trims off (almost always empty).
    let (fm_block, after) = vault::frontmatter::split_raw(content)?;
    let before_offset = content.len() - content.trim_start().len();
    let before = &content[..before_offset];
    Some((fm_block, before, after))
}

/// Report what value transforms would be applied (dry-run).
///
/// Parallel over `notes`: pure compute, flat_map handles the multiple-violations-per-note
/// case the same way `lint_field_transforms` does. Output order matches the sequential
/// implementation bit-for-bit.
fn lint_value_transforms(notes: &[Note], migration: &MigrationConfig, report: &mut Report) {
    if migration.value_renames.is_empty() {
        return;
    }

    let violations: Vec<Violation> = notes
        .par_iter()
        .flat_map(|note| {
            let abs_path_display = note.path.display().to_string();
            let mut out = Vec::new();
            for (field_name, value_map) in &migration.value_renames {
                let current_value = match field_name.as_str() {
                    "domain" => note.frontmatter.domain.as_deref(),
                    "type" => note.frontmatter.note_type.as_deref(),
                    "origin" => note.frontmatter.origin.as_deref(),
                    "status" => note.frontmatter.status.as_deref(),
                    _ => note.frontmatter.extra.get(field_name).and_then(|v| v.as_str()),
                };

                if let Some(current) = current_value
                    && let Some(new_value) = value_map.get(current)
                {
                    out.push(Violation {
                        path: note.path.clone(),
                        rule: format!("migrate.{}.value-rename", migration.name),
                        severity: Severity::Info,
                        message: format!(
                            "would rename {field_name}: '{current}' -> '{new_value}' in {abs_path_display}"
                        ),
                        fix: None,
                    });
                }
            }
            out
        })
        .collect();

    for v in violations {
        report.add(v);
    }
}

/// Apply value renames within frontmatter fields.
/// Operates on raw text to preserve formatting, same as field transforms.
///
/// Parallel over `notes`: per-note independent read-modify-write of plain `std::fs::write`,
/// same lock-contention reasoning as `apply_field_transforms`. `try_reduce` aggregates the
/// success counter and short-circuits on the first error.
fn apply_value_transforms(vault_root: &Path, notes: &[Note], migration: &MigrationConfig) -> Result<usize> {
    notes
        .par_iter()
        .map(|note| -> Result<usize> {
            // Quick check: does this note have any values to transform?
            let mut has_target = false;
            for (field_name, value_map) in &migration.value_renames {
                let current_value = match field_name.as_str() {
                    "domain" => note.frontmatter.domain.as_deref(),
                    "type" => note.frontmatter.note_type.as_deref(),
                    "origin" => note.frontmatter.origin.as_deref(),
                    "status" => note.frontmatter.status.as_deref(),
                    _ => note.frontmatter.extra.get(field_name).and_then(|v| v.as_str()),
                };
                if let Some(current) = current_value
                    && value_map.contains_key(current)
                {
                    has_target = true;
                    break;
                }
            }

            if !has_target {
                return Ok(0);
            }

            let abs_path = vault_root.join(&note.path);
            let content =
                std::fs::read_to_string(&abs_path).context(format!("failed to read {}", abs_path.display()))?;

            let Some((fm_block, before, after)) = extract_frontmatter_block(&content) else {
                return Ok(0);
            };

            let mut lines: Vec<String> = fm_block.lines().map(String::from).collect();
            let mut changed = false;

            for (field_name, value_map) in &migration.value_renames {
                for line in &mut lines {
                    for (old_value, new_value) in value_map {
                        // Match both quoted and unquoted YAML values
                        let unquoted = format!("{field_name}: {old_value}");
                        let double_quoted = format!("{field_name}: \"{old_value}\"");
                        let single_quoted = format!("{field_name}: '{old_value}'");

                        if *line == unquoted {
                            *line = format!("{field_name}: {new_value}");
                            changed = true;
                        } else if *line == double_quoted {
                            *line = format!("{field_name}: \"{new_value}\"");
                            changed = true;
                        } else if *line == single_quoted {
                            *line = format!("{field_name}: '{new_value}'");
                            changed = true;
                        }
                    }
                }
            }

            if changed {
                let new_content = format!("{before}---\n{}\n---{after}", lines.join("\n"));
                vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
                log::info!("applied value transforms: {} ({})", note.path.display(), migration.name);
                Ok(1)
            } else {
                Ok(0)
            }
        })
        .try_reduce(|| 0usize, |a, b| Ok(a + b))
}

#[cfg(test)]
mod tests;
