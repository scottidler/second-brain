use std::collections::HashMap;
use std::path::Path;

use crate::config::DuplicatesConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Check if a note path matches any of the given glob patterns.
fn matches_exclude(note: &Note, patterns: &[glob::Pattern]) -> bool {
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

/// Parse glob pattern strings into glob::Pattern objects.
fn parse_exclude_patterns(patterns: &[String]) -> Vec<glob::Pattern> {
    patterns
        .iter()
        .filter_map(|p| match glob::Pattern::new(p) {
            Ok(pat) => Some(pat),
            Err(e) => {
                log::warn!("invalid duplicates exclude pattern, skipping: {}: {e}", p);
                None
            }
        })
        .collect()
}

/// Run duplicate detection on all notes.
pub fn lint_duplicates(notes: &[Note], config: &DuplicatesConfig) -> Report {
    let mut report = Report::default();

    // Filter out excluded paths and the user's own authored notes (never
    // duplicate-flag work notes / journals - that's the user's space).
    let exclude_patterns = parse_exclude_patterns(&config.exclude);
    let eligible: Vec<usize> = notes
        .iter()
        .enumerate()
        .filter(|(_, n)| !matches_exclude(n, &exclude_patterns) && !crate::scope::is_authored(n))
        .map(|(i, _)| i)
        .collect();

    // Phase 1: exact content hash duplicates
    let mut hash_groups: HashMap<u64, Vec<usize>> = HashMap::new();
    for &i in &eligible {
        let note = &notes[i];
        // Skip empty/whitespace-only bodies to avoid false exact duplicates
        if note.body.trim().is_empty() {
            continue;
        }
        let hash = simple_hash(&note.body);
        hash_groups.entry(hash).or_default().push(i);
    }

    for indices in hash_groups.values() {
        if indices.len() < 2 {
            continue;
        }

        // Check same-type-only constraint
        if config.same_type_only {
            let types: Vec<Option<&str>> = indices
                .iter()
                .map(|&i| notes[i].frontmatter.note_type.as_deref())
                .collect();
            if !types.windows(2).all(|w| w[0] == w[1]) {
                continue;
            }
        }

        // Use content hash as group ID for exact duplicates
        let group_hash = format!("dup-{:x}", simple_hash(&notes[indices[0]].body));
        let first = &notes[indices[0]];
        for &idx in &indices[1..] {
            let dupe = &notes[idx];
            report.add(Violation {
                path: dupe.path.clone(),
                rule: "duplicates.exact".to_string(),
                severity: Severity::Warning,
                message: format!("exact duplicate of {}", first.path.display()),
                fix: Some(Fix::SetCortexFields {
                    fields: vec![
                        ("cortex-duplicate".to_string(), "true".to_string()),
                        ("cortex-duplicate-group".to_string(), group_hash.clone()),
                    ],
                }),
            });
        }
        // Also tag the first note in the group
        report.add(Violation {
            path: first.path.clone(),
            rule: "duplicates.exact".to_string(),
            severity: Severity::Warning,
            message: format!("exact duplicate of {}", notes[indices[1]].path.display()),
            fix: Some(Fix::SetCortexFields {
                fields: vec![
                    ("cortex-duplicate".to_string(), "true".to_string()),
                    ("cortex-duplicate-group".to_string(), group_hash.clone()),
                ],
            }),
        });
    }

    // Phase 2: fuzzy similarity (TF-IDF based)
    // Build a sub-slice of eligible notes for TF-IDF comparison
    let eligible_notes: Vec<Note> = eligible.iter().map(|&i| notes[i].clone()).collect();
    if eligible_notes.len() > 1 && config.threshold < 1.0 {
        let similarities = find_similar_notes(&eligible_notes, config.threshold, config.same_type_only);
        for (ei, ej, score) in similarities {
            // Map back to original notes indices
            let i = eligible[ei];
            let j = eligible[ej];
            // Skip if already reported as exact duplicate
            let already_reported = report
                .violations
                .iter()
                .any(|v| v.path == notes[j].path && v.rule == "duplicates.exact");
            if already_reported {
                continue;
            }

            // Use oldest note stem as group anchor for fuzzy matches
            let stem_i = notes[i].path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
            let stem_j = notes[j].path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
            let group_anchor = if stem_i <= stem_j { stem_i } else { stem_j };
            let group_hash = format!("dup-{group_anchor}");

            report.add(Violation {
                path: notes[j].path.clone(),
                rule: "duplicates.similar".to_string(),
                severity: Severity::Info,
                message: format!("similar to {} (score: {:.2})", notes[i].path.display(), score),
                fix: Some(Fix::SetCortexFields {
                    fields: vec![
                        ("cortex-duplicate".to_string(), "true".to_string()),
                        ("cortex-duplicate-group".to_string(), group_hash.clone()),
                    ],
                }),
            });

            // Also tag the paired note
            let already_tagged = report
                .violations
                .iter()
                .any(|v| v.path == notes[i].path && v.rule.starts_with("duplicates."));
            if !already_tagged {
                report.add(Violation {
                    path: notes[i].path.clone(),
                    rule: "duplicates.similar".to_string(),
                    severity: Severity::Info,
                    message: format!("similar to {} (score: {:.2})", notes[j].path.display(), score),
                    fix: Some(Fix::SetCortexFields {
                        fields: vec![
                            ("cortex-duplicate".to_string(), "true".to_string()),
                            ("cortex-duplicate-group".to_string(), group_hash.clone()),
                        ],
                    }),
                });
            }
        }
    }

    log::info!("duplicates lint complete: {} violation(s)", report.violations.len());
    report
}

/// Apply duplicate surfacing: write cortex-duplicate fields to frontmatter.
/// Also clears stale duplicate fields from notes no longer flagged.
pub fn apply_duplicates(vault_root: &Path, notes: &[Note], config: &DuplicatesConfig) -> eyre::Result<Vec<String>> {
    let report = lint_duplicates(notes, config);
    // Real changed-path list (not just a count) so the daemon's oscillation
    // fingerprint can compare consecutive sweeps by file.
    let mut changed = Vec::new();

    // Collect paths that ARE duplicates in this run
    let duplicate_paths: std::collections::HashSet<&Path> =
        report.violations.iter().map(|v| v.path.as_path()).collect();

    // Apply: write cortex-duplicate fields to flagged notes
    for violation in &report.violations {
        if let Some(Fix::SetCortexFields { fields }) = &violation.fix {
            let abs_path = vault_root.join(&violation.path);
            // Per-note errors WARN and skip rather than `?`-aborting the run.
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("skipping duplicate fields for {}: {e}", violation.path.display());
                    continue;
                }
            };

            // Check if fields already set correctly
            let already_set = fields
                .iter()
                .all(|(key, val)| content.contains(&format!("{key}: {val}")));
            if already_set {
                continue;
            }

            let yaml_fields: Vec<(String, serde_yaml::Value)> = fields
                .iter()
                .map(|(k, v)| (k.clone(), serde_yaml::Value::String(v.clone())))
                .collect();

            if let Some(new_content) = crate::scope::insert_frontmatter_fields(&content, &yaml_fields) {
                if let Err(e) = vault::note::write_atomic(&abs_path, new_content.as_bytes()) {
                    log::warn!("skipping duplicate fields for {}: {e}", violation.path.display());
                    continue;
                }
                log::info!("wrote duplicate fields: {}", violation.path.display());
                changed.push(violation.path.to_string_lossy().to_string());
            }
        }
    }

    // Clear: remove cortex-duplicate fields from notes that are no longer duplicates
    let cortex_keys = vec!["cortex-duplicate".to_string(), "cortex-duplicate-group".to_string()];
    for note in notes {
        if duplicate_paths.contains(note.path.as_path()) {
            continue; // Still a duplicate, don't clear
        }

        // Check if this note has cortex-duplicate fields that need clearing
        let has_cortex_fields = note.frontmatter.extra.contains_key("cortex-duplicate")
            || note.frontmatter.extra.contains_key("cortex-duplicate-group");
        if !has_cortex_fields {
            continue;
        }

        let abs_path = vault_root.join(&note.path);
        // Per-note errors WARN and skip rather than `?`-aborting the run.
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("skipping stale-duplicate clear for {}: {e}", note.path.display());
                continue;
            }
        };
        if let Some(new_content) = crate::scope::remove_frontmatter_fields(&content, &cortex_keys) {
            if let Err(e) = vault::note::write_atomic(&abs_path, new_content.as_bytes()) {
                log::warn!("skipping stale-duplicate clear for {}: {e}", note.path.display());
                continue;
            }
            log::info!("cleared stale duplicate fields: {}", note.path.display());
            changed.push(note.path.to_string_lossy().to_string());
        }
    }

    Ok(changed)
}

/// Simple non-cryptographic hash for body content comparison.
fn simple_hash(content: &str) -> u64 {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return 0;
    }
    // FNV-1a hash
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in trimmed.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Build a simple TF-IDF model and find similar note pairs.
fn find_similar_notes(notes: &[Note], threshold: f64, same_type_only: bool) -> Vec<(usize, usize, f64)> {
    // Filter out empty bodies for TF-IDF
    let valid_indices: Vec<usize> = notes
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.body.trim().is_empty())
        .map(|(i, _)| i)
        .collect();

    let tokenized: Vec<HashMap<&str, usize>> = valid_indices.iter().map(|&i| tokenize(&notes[i].body)).collect();

    // Document frequency
    let mut df: HashMap<&str, usize> = HashMap::new();
    for tokens in &tokenized {
        for term in tokens.keys() {
            *df.entry(term).or_insert(0) += 1;
        }
    }

    let n = valid_indices.len() as f64;

    // TF-IDF vectors (sparse)
    let tfidf_vecs: Vec<HashMap<&str, f64>> = tokenized
        .iter()
        .map(|tokens| {
            let total: usize = tokens.values().sum();
            if total == 0 {
                return HashMap::new();
            }
            tokens
                .iter()
                .map(|(term, &count)| {
                    let tf = count as f64 / total as f64;
                    let idf = (n / (*df.get(term).unwrap_or(&1)) as f64).ln() + 1.0;
                    (*term, tf * idf)
                })
                .collect()
        })
        .collect();

    let mut results = Vec::new();

    for vi in 0..valid_indices.len() {
        for vj in (vi + 1)..valid_indices.len() {
            let i = valid_indices[vi];
            let j = valid_indices[vj];
            if same_type_only && notes[i].frontmatter.note_type != notes[j].frontmatter.note_type {
                continue;
            }

            let score = cosine_similarity(&tfidf_vecs[vi], &tfidf_vecs[vj]);
            if score >= threshold {
                results.push((i, j, score));
            }
        }
    }

    results
}

/// Tokenize text into word frequency map.
fn tokenize(text: &str) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric());
        if word.len() >= 2 {
            *counts.entry(word).or_insert(0) += 1;
        }
    }
    counts
}

/// Cosine similarity between two sparse TF-IDF vectors.
fn cosine_similarity(a: &HashMap<&str, f64>, b: &HashMap<&str, f64>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let dot: f64 = a
        .iter()
        .filter_map(|(term, val_a)| b.get(term).map(|val_b| val_a * val_b))
        .sum();

    let mag_a: f64 = a.values().map(|v| v * v).sum::<f64>().sqrt();
    let mag_b: f64 = b.values().map(|v| v * v).sum::<f64>().sqrt();

    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }

    dot / (mag_a * mag_b)
}

#[cfg(test)]
mod tests;
