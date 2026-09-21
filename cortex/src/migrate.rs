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

    // `--plan <file>` reads its migrations from a standalone YAML file instead
    // of `cortex.yml`. The inverse of a migration lives in such a file rather
    // than in the config, so `migrate --apply` can never run it by accident;
    // it was parsed but ignored before this phase.
    let from_plan;
    let migrations: &[MigrationConfig] = match &opts.plan {
        Some(path) => {
            from_plan = load_plan(path)?;
            &from_plan
        }
        None => &config.migrations,
    };

    // `--only <name>` narrows to one migration. `apply_migrate` runs every
    // configured migration otherwise, which is exactly what makes a two-step
    // rollout (v5 now, v6 seven phases later) unsafe without it.
    let selected: Vec<&MigrationConfig> = match &opts.only {
        Some(name) => {
            let hit: Vec<&MigrationConfig> = migrations.iter().filter(|m| m.name == *name).collect();
            if hit.is_empty() {
                let known: Vec<&str> = migrations.iter().map(|m| m.name.as_str()).collect();
                return Err(eyre::eyre!(
                    "no migration named {name:?}; available migrations are {known:?}"
                ));
            }
            hit
        }
        None => migrations.iter().collect(),
    };

    // The dry-run flags notes a tag transform would push over `max-per-note`.
    // Best-effort: a missing or unreadable vocabulary file means no cap column,
    // not a failed dry-run.
    let cap = vault::canonical::CanonicalTagsFile::load(&config.sweep.canonical_path)
        .map(|f| f.max_per_note)
        .ok();

    if opts.apply {
        let count = apply_migrate_selected(vault_root, &notes, &selected)?;
        Ok(Report {
            applied: count,
            ..Default::default()
        })
    } else {
        Ok(lint_migrate_selected(&notes, &selected, cap))
    }
}

/// Planned file move with optional frontmatter updates.
#[derive(Debug)]
struct PlannedMove {
    from: PathBuf,
    to: PathBuf,
    set_frontmatter: Vec<(String, serde_yaml::Value)>,
}

/// Read migrations from a standalone plan file (`migrate --plan <file>`).
/// The file is a YAML sequence of the same shape `cortex.yml`'s `migrations:`
/// holds. An inverse migration lives here, never in the config, so a routine
/// `migrate --apply` cannot run it.
pub fn load_plan(path: &Path) -> Result<Vec<MigrationConfig>> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("failed to read migration plan {}", path.display()))?;
    serde_yaml::from_str(&raw).with_context(|| format!("failed to parse migration plan {}", path.display()))
}

/// Run migration dry-run: report what would be moved and what fields would change.
pub fn lint_migrate(notes: &[Note], migrations: &[MigrationConfig]) -> Report {
    lint_migrate_selected(notes, &migrations.iter().collect::<Vec<_>>(), None)
}

/// Apply migrations: field transforms first, then file moves.
pub fn apply_migrate(vault_root: &Path, notes: &[Note], migrations: &[MigrationConfig]) -> Result<usize> {
    apply_migrate_selected(vault_root, notes, &migrations.iter().collect::<Vec<_>>())
}

/// `lint_migrate` over an already-narrowed selection (`--only`).
pub fn lint_migrate_selected(notes: &[Note], migrations: &[&MigrationConfig], cap: Option<usize>) -> Report {
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

        // Report field-to-tags and tags-remove
        lint_tag_transforms(notes, migration, cap, &mut report);
    }

    log::info!("migrate lint complete: {} violation(s)", report.violations.len());
    report
}

/// `apply_migrate` over an already-narrowed selection (`--only`).
pub fn apply_migrate_selected(vault_root: &Path, notes: &[Note], migrations: &[&MigrationConfig]) -> Result<usize> {
    let mut total_count = 0;

    // Phase 0: tag transforms. They run FIRST so a later field-drop in the
    // same run cannot remove the source field before its value is copied.
    for migration in migrations {
        if !migration.field_to_tags.is_empty() || !migration.tags_remove.is_empty() {
            total_count += apply_tag_transforms(vault_root, notes, migration)?;
        }
    }

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

/// True when the note's frontmatter carries a NON-EMPTY inline `tags: [a, b]`
/// list, the form P4 normalizes away. `pub(crate)` so `tags::lint_tags` /
/// `tags::apply_tags` (P9) share the same on-disk-form detector rather than
/// growing a second copy of this parse.
pub(crate) fn has_inline_tag_list(content: &str) -> bool {
    let Some((fm, _body)) = vault::frontmatter::split_raw(content) else {
        return false;
    };
    fm.lines().filter_map(|line| line.strip_prefix("tags:")).any(|rest| {
        let rest = rest.trim();
        rest.starts_with('[') && rest != "[]"
    })
}

/// A migration source field's raw value, quote-stripped, or `None` when the
/// note does not carry the field or carries it empty. Exclusion is NOT applied
/// here: callers that care about the value apply it, callers that only ask
/// "is this note in scope" do not.
fn source_field_value(note: &Note, field: &str) -> Option<String> {
    let raw = match field {
        "type" => note.frontmatter.note_type.as_deref(),
        "origin" => note.frontmatter.origin.as_deref(),
        "status" => note.frontmatter.status.as_deref(),
        _ => note.frontmatter.extra.get(field).and_then(|v| v.as_str()),
    }?;
    let value = raw.trim().trim_matches(['"', '\'']).trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

/// Read a note's current tag list out of its parsed frontmatter.
fn note_tags(note: &Note) -> Vec<String> {
    note.frontmatter.tags.clone().unwrap_or_default()
}

/// The canonical tag a source field's value becomes, or `None` when the value
/// is excluded, empty, or the note does not carry the field.
///
/// Values are quote-stripped and lowercased; a migration whose source field
/// needs richer normalization (emoji folder paths, legacy aliases) applies
/// its own before configuring `field-to-tags`.
fn field_to_tag_value(note: &Note, field: &str, cfg: &crate::config::FieldToTags) -> Option<String> {
    let value = source_field_value(note, field)?;
    let normalized = value.to_lowercase();
    if cfg.exclude.iter().any(|e| e == &normalized || e == &value) {
        return None;
    }
    Some(normalized)
}

/// Dry-run for `field-to-tags` and `tags-remove`.
fn lint_tag_transforms(notes: &[Note], migration: &MigrationConfig, cap: Option<usize>, report: &mut Report) {
    for note in notes {
        let current = note_tags(note);
        let mut would: Vec<String> = Vec::new();

        for (field, cfg) in &migration.field_to_tags {
            if let Some(tag) = field_to_tag_value(note, field, cfg)
                && !current.contains(&tag)
            {
                would.push(tag);
            }
        }
        for tag in &migration.tags_remove {
            if current.contains(tag) {
                would.push(format!("-{tag}"));
            }
        }
        if would.is_empty() {
            continue;
        }

        // Surface the two conditions the design doc asks the dry-run to list:
        // a note that would exceed the cap, and a note already carrying two or
        // more of the migrated names before the run.
        let mut message = format!("would set tags {would:?}");
        let added = would.iter().filter(|t| !t.starts_with('-')).count();
        if let Some(cap) = cap
            && current.len() + added > cap
        {
            message.push_str(&format!(
                " (WOULD EXCEED max-per-note: {} tags against a cap of {cap})",
                current.len() + added
            ));
        }
        report.add(Violation {
            path: note.path.clone(),
            rule: format!("migrate.{}", migration.name),
            severity: Severity::Info,
            message,
            fix: None,
        });
    }
}

/// Apply `field-to-tags` and `tags-remove`.
///
/// Idempotent: a value already present as a tag is not appended twice, so a
/// second `--apply` writes zero files. Every visited note's tag block is
/// rewritten in canonical block form, which is also how a pre-P4 inline list
/// gets normalized.
fn apply_tag_transforms(vault_root: &Path, notes: &[Note], migration: &MigrationConfig) -> Result<usize> {
    notes
        .par_iter()
        .map(|note| -> Result<usize> {
            let current = note_tags(note);
            let mut next = current.clone();

            for (field, cfg) in &migration.field_to_tags {
                if let Some(tag) = field_to_tag_value(note, field, cfg)
                    && !next.contains(&tag)
                {
                    next.push(tag);
                }
            }
            if !migration.tags_remove.is_empty() {
                next.retain(|t| !migration.tags_remove.contains(t));
            }
            // A note whose tag SET is unchanged may still be in the wrong
            // on-disk FORM. `field-to-tags` is the pass that establishes one
            // form for the field it migrates (G4), so an in-scope note with a
            // non-empty inline list is normalized to block even when nothing
            // is added. Without this the vault keeps two spellings forever:
            // the notes that already carried their own source-field value as
            // a tag are exactly the ones the set-comparison skips.
            // Carrying the source FIELD is what puts a note in scope for form
            // normalization, independent of whether its VALUE is excluded.
            // `exclude` says "do not propagate this value as a tag"; it does
            // not say "leave this note in the old spelling". The 38 `resources`
            // and 14 `system` notes are the ones that difference decides.
            let in_scope = migration
                .field_to_tags
                .keys()
                .any(|field| source_field_value(note, field).is_some());
            let form_only = next == current && in_scope && !current.is_empty();
            if next == current && !form_only {
                return Ok(0);
            }

            let abs_path = vault_root.join(&note.path);
            let content =
                std::fs::read_to_string(&abs_path).context(format!("failed to read {}", abs_path.display()))?;

            // Empty lists are left alone: rewriting `tags: []` to a bare
            // `tags:` swaps one spelling of "no tags" for another and churns
            // 915 `entities/` files for nothing.
            if form_only && !has_inline_tag_list(&content) {
                return Ok(0);
            }
            let Some(updated) = crate::tags::replace_tags_in_frontmatter(&content, &next) else {
                log::warn!("tag transform skipped (no frontmatter): {}", note.path.display());
                return Ok(0);
            };
            vault::note::write_atomic(&abs_path, updated.as_bytes())?;
            log::info!(
                "applied tag transform: {} ({}) {:?} -> {:?}",
                note.path.display(),
                migration.name,
                current,
                next
            );
            Ok(1)
        })
        .try_reduce(|| 0usize, |a, b| Ok(a + b))
}

#[cfg(test)]
mod tests;
