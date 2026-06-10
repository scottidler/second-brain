use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;

use crate::config::{AutoTagConfig, FabricConfig};
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Lint notes that could benefit from auto-tagging.
///
/// Parallel over `notes` via rayon: each per-note assessment is pure CPU work over independent
/// inputs. `par_iter().filter_map().collect()` preserves input order, so the final `Report`
/// is bit-identical to the sequential version on the same fixture.
pub fn lint_autotag(notes: &[Note], all_notes: &[Note], config: &AutoTagConfig) -> Report {
    log::debug!(
        "autotag::lint_autotag: notes={} all_notes={} enabled={} min_tags_threshold={}",
        notes.len(),
        all_notes.len(),
        config.enabled,
        config.min_tags_threshold
    );
    let mut report = Report::default();

    if !config.enabled {
        return report;
    }

    // Build canonical tag set: from config or auto-derived from vault
    let canonical = build_canonical_tags(all_notes, config);

    let violations: Vec<Violation> = notes
        .par_iter()
        .filter_map(|note| {
            // Skip already processed
            if note.frontmatter.extra.contains_key("cortex-tagged") {
                return None;
            }

            // Only process notes with few tags
            let tag_count = note.frontmatter.tags.as_ref().map(|t| t.len()).unwrap_or(0);
            if tag_count >= config.min_tags_threshold {
                return None;
            }

            // Only process freshly ingested notes. ("processed" is a legacy
            // status with no enum variant - tolerated on read, not in schema.)
            let status = note.frontmatter.status.as_deref();
            let dominated = status == Some(vault::schema::Status::Unread.as_str())
                || status == Some("processed")
                || note.frontmatter.origin.as_deref() == Some(vault::schema::Origin::Assisted.as_str());
            if !dominated {
                return None;
            }

            // Skip empty bodies
            if note.body.trim().is_empty() {
                return None;
            }

            // Suggest tags based on content keywords matching canonical set
            let suggested = suggest_tags_deterministic(&note.body, &canonical, note);
            if suggested.is_empty() {
                return None;
            }

            let suggested_str = suggested.join(", ");
            Some(Violation {
                path: note.path.clone(),
                rule: "autotag.suggestion".to_string(),
                severity: Severity::Info,
                message: format!("suggested tags: {suggested_str}"),
                fix: Some(Fix::SetCortexFields {
                    fields: vec![
                        ("cortex-suggested-tags".to_string(), format!("[{suggested_str}]")),
                        ("cortex-tagged".to_string(), "true".to_string()),
                    ],
                }),
            })
        })
        .collect();

    for v in violations {
        report.add(v);
    }

    log::info!("autotag lint complete: {} violation(s)", report.violations.len());
    report
}

/// Apply auto-tagging: write suggested tags and run Fabric if available.
///
/// The first phase (deterministic-suggestion writes) parallelizes via rayon: writes target
/// distinct files and use plain `std::fs::write`, so the same lock-contention argument as
/// `apply_quality` applies. The optional Fabric phase below stays sequential because each
/// iteration shells out to an external LLM-style binary.
pub fn apply_autotag(
    vault_root: &Path,
    notes: &[Note],
    all_notes: &[Note],
    config: &AutoTagConfig,
    fabric: &FabricConfig,
) -> eyre::Result<Vec<String>> {
    log::debug!(
        "autotag::apply_autotag: vault_root={} notes={} all_notes={}",
        vault_root.display(),
        notes.len(),
        all_notes.len()
    );
    let report = lint_autotag(notes, all_notes, config);

    // Real changed-path list (not just a count) for the daemon's oscillation
    // fingerprint. Each parallel task yields the changed path or None.
    let applied: Vec<String> = report
        .violations
        .par_iter()
        .map(|violation| -> eyre::Result<Option<String>> {
            let Some(Fix::SetCortexFields { fields }) = &violation.fix else {
                return Ok(None);
            };
            let abs_path = vault_root.join(&violation.path);
            let content = std::fs::read_to_string(&abs_path)?;

            // Check if already set
            if content.contains("cortex-tagged:") {
                return Ok(None);
            }

            let yaml_fields: Vec<(String, serde_yaml::Value)> = fields
                .iter()
                .map(|(k, v)| (k.clone(), serde_yaml::Value::String(v.clone())))
                .collect();

            if let Some(new_content) = crate::scope::insert_frontmatter_fields(&content, &yaml_fields) {
                vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
                log::info!("wrote suggested tags: {}", violation.path.display());
                Ok(Some(violation.path.to_string_lossy().to_string()))
            } else {
                Ok(None)
            }
        })
        .collect::<eyre::Result<Vec<Option<String>>>>()?
        .into_iter()
        .flatten()
        .collect();
    let mut changed = applied;

    // If Fabric is available and a pattern is configured, enhance suggestions
    if let Some(ref pattern) = config.fabric_pattern
        && crate::fabric::is_available(&fabric.binary)
    {
        for note in notes {
            if note.frontmatter.extra.contains_key("cortex-tagged") {
                continue;
            }
            if note.frontmatter.status.as_deref() != Some(vault::schema::Status::Unread.as_str()) {
                continue;
            }
            if note.body.trim().is_empty() {
                continue;
            }

            let input = crate::fabric::truncate_input(&note.body, config.max_input_tokens);
            match crate::fabric::run_pattern(fabric, pattern, input, config.fabric_timeout_secs) {
                Ok(output) => {
                    let canonical = build_canonical_tags(all_notes, config);
                    let fabric_tags = extract_tags_from_output(&output, &canonical);
                    if !fabric_tags.is_empty() {
                        let abs_path = vault_root.join(&note.path);
                        let content = std::fs::read_to_string(&abs_path)?;
                        let suggested_str = fabric_tags.join(", ");
                        let fields = vec![
                            (
                                "cortex-suggested-tags".to_string(),
                                serde_yaml::Value::String(format!("[{suggested_str}]")),
                            ),
                            (
                                "cortex-tagged".to_string(),
                                serde_yaml::Value::String("true".to_string()),
                            ),
                        ];
                        if let Some(new_content) = crate::scope::insert_frontmatter_fields(&content, &fields) {
                            vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
                            log::info!("wrote fabric-enhanced tags: {}", note.path.display());
                            changed.push(note.path.to_string_lossy().to_string());
                        }
                    }
                }
                Err(e) => {
                    log::warn!("fabric autotag failed: {}: {e}", note.path.display());
                }
            }
        }
    }

    Ok(changed)
}

/// Build canonical tag set from config or auto-derive from vault.
fn build_canonical_tags(notes: &[Note], config: &AutoTagConfig) -> Vec<String> {
    if !config.canonical_tags.is_empty() {
        return config.canonical_tags.clone();
    }

    // Auto-derive: collect all tags, rank by frequency, take top N
    let mut tag_counts: HashMap<String, usize> = HashMap::new();
    for note in notes {
        if let Some(ref tags) = note.frontmatter.tags {
            for tag in tags {
                *tag_counts.entry(tag.clone()).or_insert(0) += 1;
            }
        }
    }

    let mut sorted: Vec<(String, usize)> = tag_counts.into_iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.1));
    sorted
        .into_iter()
        .take(config.auto_derive_top_n)
        .map(|(tag, _)| tag)
        .collect()
}

/// Suggest tags deterministically by matching canonical tags against note content.
fn suggest_tags_deterministic(body: &str, canonical: &[String], note: &Note) -> Vec<String> {
    let body_lower = body.to_lowercase();
    let existing: Vec<String> = note
        .frontmatter
        .tags
        .as_ref()
        .map(|t| t.iter().map(|s| s.to_lowercase()).collect())
        .unwrap_or_default();

    canonical
        .iter()
        .filter(|tag| {
            let tag_lower = tag.to_lowercase();
            // Skip if already tagged
            if existing.contains(&tag_lower) {
                return false;
            }
            // Check if tag appears as a word in the body
            let pattern = format!(r"\b{}\b", regex::escape(&tag_lower));
            regex::Regex::new(&pattern)
                .map(|re| re.is_match(&body_lower))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Extract tag-like words from Fabric output that match canonical tags.
fn extract_tags_from_output(output: &str, canonical: &[String]) -> Vec<String> {
    let output_lower = output.to_lowercase();
    canonical
        .iter()
        .filter(|tag| output_lower.contains(&tag.to_lowercase()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{NoteBuilder, TestVault};

    fn default_config() -> AutoTagConfig {
        AutoTagConfig {
            enabled: true,
            min_tags_threshold: 3,
            canonical_tags: vec![
                "rust".to_string(),
                "python".to_string(),
                "automation".to_string(),
                "cli-tools".to_string(),
            ],
            fabric_pattern: None,
            auto_derive_top_n: 50,
            max_input_tokens: 50000,
            fabric_timeout_secs: 30,
        }
    }

    #[test]
    fn test_suggest_tags_deterministic() {
        let canonical = vec!["rust".to_string(), "python".to_string(), "automation".to_string()];
        let note = NoteBuilder::new("test.md")
            .title("Test")
            .tags(&["python"])
            .body("This is about rust and automation tools.")
            .build();

        let suggestions = suggest_tags_deterministic(&note.body, &canonical, &note);
        assert!(suggestions.contains(&"rust".to_string()));
        assert!(suggestions.contains(&"automation".to_string()));
        assert!(
            !suggestions.contains(&"python".to_string()),
            "existing tag should not be suggested"
        );
    }

    #[test]
    fn test_lint_autotag_on_vault() {
        let v = TestVault::new();
        // Add a note with few tags and origin: assisted
        v.add_note(
            "needs-tags.md",
            "---\ntitle: Needs Tags\ndate: 2026-03-18\ntype: note\ndomain: tech\norigin: assisted\nstatus: unread\ntags: []\n---\nThis article discusses rust programming and python scripting for automation.\n",
        );
        let notes = v.scan();
        let config = default_config();

        let report = lint_autotag(&notes, &notes, &config);
        let vi = report
            .violations
            .iter()
            .find(|v| v.path.to_string_lossy().contains("needs-tags"));
        assert!(vi.is_some(), "should suggest tags for note with few tags");
        assert!(vi.unwrap().message.contains("rust"));
    }

    #[test]
    fn test_lint_autotag_skips_already_tagged() {
        let notes = vec![
            NoteBuilder::new("tagged.md")
                .title("Tagged")
                .note_type("note")
                .origin("assisted")
                .status("unread")
                .extra("cortex-tagged", serde_yaml::Value::Bool(true))
                .body("This is about rust programming.")
                .build(),
        ];
        let config = default_config();

        let report = lint_autotag(&notes, &notes, &config);
        assert!(report.is_empty(), "should skip already tagged notes");
    }

    #[test]
    fn test_lint_autotag_skips_notes_with_enough_tags() {
        let notes = vec![
            NoteBuilder::new("enough.md")
                .title("Enough")
                .note_type("note")
                .origin("assisted")
                .status("unread")
                .tags(&["rust", "programming", "cli"])
                .body("This is about rust programming.")
                .build(),
        ];
        let config = default_config();

        let report = lint_autotag(&notes, &notes, &config);
        assert!(report.is_empty(), "should skip notes with enough tags");
    }

    #[test]
    fn test_build_canonical_from_config() {
        let config = default_config();
        let notes = vec![];
        let canonical = build_canonical_tags(&notes, &config);
        assert_eq!(canonical, config.canonical_tags);
    }

    #[test]
    fn test_build_canonical_auto_derive() {
        let config = AutoTagConfig {
            canonical_tags: vec![], // Empty - should auto-derive
            auto_derive_top_n: 3,
            ..default_config()
        };
        let notes = vec![
            NoteBuilder::new("a.md").tags(&["rust", "python"]).build(),
            NoteBuilder::new("b.md").tags(&["rust", "cli"]).build(),
            NoteBuilder::new("c.md").tags(&["rust", "python", "ai"]).build(),
        ];

        let canonical = build_canonical_tags(&notes, &config);
        assert_eq!(canonical.len(), 3);
        assert_eq!(canonical[0], "rust"); // Most frequent
    }

    #[test]
    fn test_apply_autotag_on_vault() {
        let v = TestVault::new();
        v.add_note(
            "tag-me.md",
            "---\ntitle: Tag Me\ndate: 2026-03-18\ntype: note\ndomain: tech\norigin: assisted\nstatus: unread\ntags: []\n---\nThis discusses rust programming and python automation heavily.\n",
        );
        let notes = v.scan();
        let config = default_config();

        let fabric = FabricConfig::default();
        let changed = apply_autotag(v.root(), &notes, &notes, &config, &fabric).expect("apply");
        assert!(!changed.is_empty());

        let content = v.read("tag-me.md");
        assert!(content.contains("cortex-suggested-tags:"));
        assert!(content.contains("cortex-tagged:"));
    }

    #[test]
    fn test_disabled_config() {
        let config = AutoTagConfig {
            enabled: false,
            ..default_config()
        };
        let notes = vec![
            NoteBuilder::new("test.md")
                .title("Test")
                .origin("assisted")
                .status("unread")
                .body("rust programming")
                .build(),
        ];

        let report = lint_autotag(&notes, &notes, &config);
        assert!(report.is_empty());
    }
}
