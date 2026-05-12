use rayon::prelude::*;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

use crate::config::QualityConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Regex to match wikilink targets in note bodies.
static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]").expect("valid wikilink regex"));

/// Patterns (case-insensitive) that indicate the note content is a Fabric
/// paraphrase of a block/error page rather than the intended article. These
/// are the "failed-fetch" quality signature.
/// Matches Gate-2's paraphrase patterns in borg's staged pipeline so the two
/// detectors stay in sync.
const FAILED_FETCH_PATTERNS: &[&str] = &[
    "only an error message",
    "no actual content",
    "error message indicating",
    "content inaccessible",
    "access to the website is blocked",
    "anonymous access to domain",
];

/// True when the note body contains a failed-fetch signature. Public so the
/// `borg migrate reingest-failed` command can reuse the same detection.
pub fn has_failed_fetch_signature(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    FAILED_FETCH_PATTERNS.iter().any(|p| lower.contains(p))
}

/// Types excluded from quality scoring (system-generated notes).
const EXCLUDED_TYPES: &[&str] = &["digest", "review", "daily", "system"];

/// Quality level based on accumulated issues.
#[derive(Debug, Clone, PartialEq, Eq)]
enum QualityLevel {
    Low,
    Medium,
    High,
}

impl std::fmt::Display for QualityLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QualityLevel::Low => write!(f, "low"),
            QualityLevel::Medium => write!(f, "medium"),
            QualityLevel::High => write!(f, "high"),
        }
    }
}

/// A quality issue found in a note.
#[derive(Debug, Clone)]
struct QualityIssue {
    name: String,
    severity: IssueSeverity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IssueSeverity {
    Critical,
    Warning,
    Info,
}

/// Run quality scoring on all notes.
///
/// Parallel over `notes` via rayon: `assess_note` is a pure function of `(&Note, &HashSet, &Config)`,
/// `compute_level` is pure, and the violation construction has no inter-iteration state. The
/// `par_iter().filter_map().collect()` pattern preserves input order, so the final report
/// matches the sequential version bit-for-bit on the same fixture.
pub fn lint_quality(notes: &[Note], config: &QualityConfig) -> Report {
    log::debug!("quality::lint_quality: notes={}", notes.len());
    let mut report = Report::default();

    // Build inbound link index
    let inbound_targets = build_inbound_index(notes);

    let violations: Vec<Violation> = notes
        .par_iter()
        .filter_map(|note| {
            // Skip system-generated note types
            if let Some(ref note_type) = note.frontmatter.note_type
                && EXCLUDED_TYPES.contains(&note_type.as_str())
            {
                return None;
            }

            let issues = assess_note(note, &inbound_targets, config);
            if issues.is_empty() {
                return None;
            }

            let level = compute_level(&issues);
            let issue_names: Vec<String> = issues.iter().map(|i| i.name.clone()).collect();

            let severity = match level {
                QualityLevel::Low => Severity::Warning,
                QualityLevel::Medium => Severity::Info,
                QualityLevel::High => return None, // Don't report high-quality notes
            };

            Some(Violation {
                path: note.path.clone(),
                rule: "quality.score".to_string(),
                severity,
                message: format!("quality: {level} (issues: {})", issue_names.join(", ")),
                fix: Some(Fix::SetCortexFields {
                    fields: vec![
                        ("cortex-quality".to_string(), level.to_string()),
                        (
                            "cortex-quality-issues".to_string(),
                            format!("[{}]", issue_names.join(", ")),
                        ),
                    ],
                }),
            })
        })
        .collect();

    for v in violations {
        report.add(v);
    }

    log::info!("quality lint complete: {} violation(s)", report.violations.len());
    report
}

/// Apply quality scoring: write cortex-quality fields to frontmatter.
/// Also clears stale fields from notes that are now high quality.
///
/// Both write loops run in parallel via rayon. Plain `std::fs::write` does no explicit
/// parent-directory fsync, so the kernel-level dirent-sync serialization that gates the borg
/// `write_atomic` paths does not gate cortex writes. Error propagation uses
/// `par_iter().try_reduce(...)` so a single write failure still aborts the apply with the same
/// `Result<usize>` semantics as the sequential implementation.
pub fn apply_quality(vault_root: &Path, notes: &[Note], config: &QualityConfig) -> eyre::Result<usize> {
    log::debug!(
        "quality::apply_quality: vault_root={} notes={}",
        vault_root.display(),
        notes.len()
    );
    let report = lint_quality(notes, config);

    let flagged_paths: HashSet<&Path> = report.violations.iter().map(|v| v.path.as_path()).collect();

    // Apply: write quality fields to flagged notes (parallel).
    let applied_count: usize = report
        .violations
        .par_iter()
        .map(|violation| -> eyre::Result<usize> {
            let Some(Fix::SetCortexFields { fields }) = &violation.fix else {
                return Ok(0);
            };
            let abs_path = vault_root.join(&violation.path);
            let content = std::fs::read_to_string(&abs_path)?;

            let already_set = fields
                .iter()
                .all(|(key, val)| content.contains(&format!("{key}: {val}")));
            if already_set {
                return Ok(0);
            }

            let yaml_fields: Vec<(String, serde_yaml::Value)> = fields
                .iter()
                .map(|(k, v)| (k.clone(), serde_yaml::Value::String(v.clone())))
                .collect();

            if let Some(new_content) = crate::scope::insert_frontmatter_fields(&content, &yaml_fields) {
                std::fs::write(&abs_path, new_content)?;
                log::info!("wrote quality fields: {}", violation.path.display());
                Ok(1)
            } else {
                Ok(0)
            }
        })
        .try_reduce(|| 0usize, |a, b| Ok(a + b))?;

    // Clear: remove quality fields from notes no longer flagged (parallel).
    let cortex_keys = vec!["cortex-quality".to_string(), "cortex-quality-issues".to_string()];
    let cleared_count: usize = notes
        .par_iter()
        .map(|note| -> eyre::Result<usize> {
            if flagged_paths.contains(note.path.as_path()) {
                return Ok(0);
            }
            let has_cortex_fields = note.frontmatter.extra.contains_key("cortex-quality")
                || note.frontmatter.extra.contains_key("cortex-quality-issues");
            if !has_cortex_fields {
                return Ok(0);
            }

            let abs_path = vault_root.join(&note.path);
            let content = std::fs::read_to_string(&abs_path)?;
            if let Some(new_content) = crate::scope::remove_frontmatter_fields(&content, &cortex_keys) {
                std::fs::write(&abs_path, new_content)?;
                log::info!("cleared stale quality fields: {}", note.path.display());
                Ok(1)
            } else {
                Ok(0)
            }
        })
        .try_reduce(|| 0usize, |a, b| Ok(a + b))?;

    Ok(applied_count + cleared_count)
}

/// Build a set of note stems/paths that are referenced by at least one wikilink.
fn build_inbound_index(notes: &[Note]) -> HashSet<String> {
    let mut targets = HashSet::new();
    for note in notes {
        for cap in WIKILINK_RE.captures_iter(&note.body) {
            if let Some(m) = cap.get(1) {
                targets.insert(m.as_str().trim().to_lowercase());
            }
        }
    }
    targets
}

/// Assess a single note for quality issues.
fn assess_note(note: &Note, inbound_targets: &HashSet<String>, config: &QualityConfig) -> Vec<QualityIssue> {
    let mut issues = Vec::new();
    let body = &note.body;

    // Empty body
    if body.trim().is_empty() {
        issues.push(QualityIssue {
            name: "empty-body".to_string(),
            severity: IssueSeverity::Critical,
        });
    }
    // Stub body
    else if body.split_whitespace().count() < config.min_word_count {
        issues.push(QualityIssue {
            name: "stub-body".to_string(),
            severity: IssueSeverity::Warning,
        });
    }

    // Failed-fetch signature: Fabric paraphrased a block/error page into a
    // "summary". Flagged Critical so the note shows up at QualityLevel::Low
    // and is easy to find with `cortex lint --json | jq '.[] |
    // select(.issues | includes("failed-fetch"))'`.
    if has_failed_fetch_signature(body) {
        issues.push(QualityIssue {
            name: "failed-fetch".to_string(),
            severity: IssueSeverity::Critical,
        });
    }

    // No inbound links (not referenced by any other note)
    let stem = note
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !stem.is_empty() && !inbound_targets.contains(&stem) {
        issues.push(QualityIssue {
            name: "no-inbound-links".to_string(),
            severity: IssueSeverity::Warning,
        });
    }

    // No outbound links
    if !WIKILINK_RE.is_match(body) {
        issues.push(QualityIssue {
            name: "no-outbound-links".to_string(),
            severity: IssueSeverity::Info,
        });
    }

    // Missing summary
    let has_summary = body.contains("## Summary") || body.contains("> [!tldr]");
    if !has_summary && body.split_whitespace().count() >= config.min_word_count {
        issues.push(QualityIssue {
            name: "missing-summary".to_string(),
            severity: IssueSeverity::Info,
        });
    }

    issues
}

/// Compute overall quality level from issues.
fn compute_level(issues: &[QualityIssue]) -> QualityLevel {
    if issues.iter().any(|i| i.severity == IssueSeverity::Critical) {
        return QualityLevel::Low;
    }
    let warning_count = issues.iter().filter(|i| i.severity == IssueSeverity::Warning).count();
    if warning_count >= 2 {
        QualityLevel::Low
    } else if warning_count >= 1 || !issues.is_empty() {
        QualityLevel::Medium
    } else {
        QualityLevel::High
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{NoteBuilder, TestVault};

    fn default_config() -> QualityConfig {
        QualityConfig { min_word_count: 50 }
    }

    #[test]
    fn test_empty_body_flagged_critical() {
        let notes = vec![
            NoteBuilder::new("empty.md")
                .title("Empty")
                .note_type("note")
                .body("")
                .build(),
            NoteBuilder::new("other.md")
                .title("Other")
                .note_type("note")
                .body("A real note with enough words to pass the stub check. More words needed here to reach fifty words total. Let's keep adding until we have enough content to avoid the stub threshold completely.")
                .build(),
        ];

        let report = lint_quality(&notes, &default_config());
        let empty_vi = report
            .violations
            .iter()
            .find(|v| v.path.to_string_lossy() == "empty.md");
        assert!(empty_vi.is_some(), "empty body should be flagged");
        assert!(empty_vi.unwrap().message.contains("empty-body"));
    }

    #[test]
    fn test_stub_body_flagged() {
        let notes = vec![
            NoteBuilder::new("stub.md")
                .title("Stub")
                .note_type("note")
                .body("Just a few words.")
                .build(),
        ];

        let report = lint_quality(&notes, &default_config());
        assert!(report.violations.iter().any(|v| v.message.contains("stub-body")));
    }

    #[test]
    fn test_no_inbound_links_flagged() {
        let notes = vec![
            NoteBuilder::new("orphan.md")
                .title("Orphan")
                .note_type("note")
                .body("A real note with enough words to pass. More words needed here to reach fifty words total. Let's keep adding until we have enough content to completely avoid the stub body threshold check.")
                .build(),
            NoteBuilder::new("other.md")
                .title("Other")
                .note_type("note")
                .body("This note links to [[something-else]] but not to orphan.")
                .build(),
        ];

        let report = lint_quality(&notes, &default_config());
        let orphan_vi = report
            .violations
            .iter()
            .find(|v| v.path.to_string_lossy() == "orphan.md");
        assert!(orphan_vi.is_some());
        assert!(orphan_vi.unwrap().message.contains("no-inbound-links"));
    }

    #[test]
    fn test_inbound_link_not_flagged() {
        let notes = vec![
            NoteBuilder::new("target.md")
                .title("Target")
                .note_type("note")
                .body("A real note with enough words to pass the stub check. More words needed here to reach fifty words total. Let's keep adding until we have enough content to completely avoid threshold. See also [[other]].")
                .build(),
            NoteBuilder::new("referrer.md")
                .title("Referrer")
                .note_type("note")
                .body("See [[target]] for details. This has enough words to pass the stub check. More words needed here to reach fifty. Let's keep adding until we have enough content to completely avoid the threshold.")
                .build(),
        ];

        let report = lint_quality(&notes, &default_config());
        let target_issues: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.path.to_string_lossy() == "target.md")
            .collect();

        assert!(
            !target_issues.iter().any(|v| v.message.contains("no-inbound-links")),
            "target should not be flagged for no inbound links"
        );
    }

    #[test]
    fn test_system_types_excluded() {
        let notes = vec![
            NoteBuilder::new("digest.md")
                .title("Daily Digest")
                .note_type("digest")
                .body("")
                .build(),
            NoteBuilder::new("review.md")
                .title("Weekly Review")
                .note_type("review")
                .body("")
                .build(),
        ];

        let report = lint_quality(&notes, &default_config());
        assert!(
            report.is_empty(),
            "system types should be excluded from quality scoring"
        );
    }

    #[test]
    fn test_quality_level_computation() {
        let critical = vec![QualityIssue {
            name: "empty-body".to_string(),
            severity: IssueSeverity::Critical,
        }];
        assert_eq!(compute_level(&critical), QualityLevel::Low);

        let two_warnings = vec![
            QualityIssue {
                name: "stub-body".to_string(),
                severity: IssueSeverity::Warning,
            },
            QualityIssue {
                name: "no-inbound-links".to_string(),
                severity: IssueSeverity::Warning,
            },
        ];
        assert_eq!(compute_level(&two_warnings), QualityLevel::Low);

        let one_warning = vec![QualityIssue {
            name: "stub-body".to_string(),
            severity: IssueSeverity::Warning,
        }];
        assert_eq!(compute_level(&one_warning), QualityLevel::Medium);

        let info_only = vec![QualityIssue {
            name: "no-outbound-links".to_string(),
            severity: IssueSeverity::Info,
        }];
        assert_eq!(compute_level(&info_only), QualityLevel::Medium);
    }

    #[test]
    fn test_apply_quality_on_vault() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = default_config();

        let count = apply_quality(v.root(), &notes, &config).expect("apply");
        assert!(count > 0, "should have applied quality fields to some notes");

        // bare-note.md has no frontmatter at all, so it won't get fields written
        // but partial-frontmatter.md has a stub body and should be flagged
        let partial = v.read("partial-frontmatter.md");
        assert!(
            partial.contains("cortex-quality:"),
            "partial-frontmatter.md should have quality field"
        );
    }

    #[test]
    fn test_failed_fetch_signature_detects_paraphrased_block() {
        assert!(has_failed_fetch_signature(
            "The provided input contains an error message indicating that access to the website is blocked."
        ));
        assert!(has_failed_fetch_signature(
            "This page contains only an error message and no real content."
        ));
        assert!(has_failed_fetch_signature(
            "Anonymous access to domain has been blocked due to suspected DDoS activity."
        ));
    }

    #[test]
    fn test_failed_fetch_signature_not_triggered_on_clean_content() {
        assert!(!has_failed_fetch_signature(
            "Docker containers provide lightweight virtualisation. The article explains seven useful \
             containers for self-hosters who want to minimise overhead."
        ));
    }

    #[test]
    fn test_failed_fetch_flagged_critical() {
        let body = "The provided input contains an error message indicating that access to the website \
                    is blocked. More words here to exceed the min_word_count threshold so the stub check \
                    does not interfere with the failed-fetch signal being the dominant issue on the note. \
                    Adding enough content to reach beyond fifty words total for the configured threshold.";
        let notes = vec![
            NoteBuilder::new("blocked.md")
                .title("Blocked")
                .note_type("note")
                .body(body)
                .build(),
        ];

        let report = lint_quality(&notes, &default_config());
        let vi = report
            .violations
            .iter()
            .find(|v| v.path.to_string_lossy() == "blocked.md")
            .expect("blocked.md should be flagged");
        assert!(vi.message.contains("failed-fetch"));
        // Critical issue → QualityLevel::Low
        assert!(vi.message.contains("low"));
    }

    #[test]
    fn test_apply_quality_clears_stale_fields() {
        let v = TestVault::new();
        v.add_note(
            "was-bad.md",
            "---\ntitle: Was Bad\ndate: 2026-03-18\ntype: note\ndomain: tech\norigin: authored\ntags:\n  - rust\ncortex-quality: low\ncortex-quality-issues: \"[empty-body]\"\n---\nNow this note has a real body with plenty of words to pass the quality checks. It has enough content to not be a stub. It also has outbound links like [[rust-guide]] and a summary section.\n\n## Summary\n\nThis note is now high quality.\n",
        );

        let notes = v.scan();
        let config = default_config();

        apply_quality(v.root(), &notes, &config).expect("apply");

        let content = v.read("was-bad.md");
        // Note: it may still have quality fields if it fails other checks (no-inbound-links),
        // but the old "empty-body" issue should not persist since it now has content
        assert!(!content.contains("empty-body"), "stale empty-body issue should be gone");
    }

    /// Phase 2a determinism guard: parallel `lint_quality` produces violations in the same order
    /// as the sequential implementation would over the same input slice. Concretely, the
    /// `path`-keyed sequence of violations must equal the sequence the input slice's path
    /// ordering would dictate.
    #[test]
    fn lint_quality_violations_ordered_by_input_slice_under_par_iter() {
        // Build 20 short-body notes whose names span the alphabet so the slice has a
        // non-trivial ordering. Each note is short enough to be flagged as a "stub".
        let mut notes = Vec::new();
        for tag in &[
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet", "kilo",
            "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
        ] {
            let filename = format!("{tag}.md");
            notes.push(
                NoteBuilder::new(&filename)
                    .title(tag)
                    .note_type("note")
                    .body("short stub")
                    .build(),
            );
        }
        let config = default_config();

        let report = lint_quality(&notes, &config);

        // Every violation has a path; that path sequence must equal the (filtered) input order.
        let violation_paths: Vec<String> = report
            .violations
            .iter()
            .map(|v| v.path.to_string_lossy().to_string())
            .collect();
        // Walk the input slice in order, retaining only entries that appear in violation_paths.
        let expected: Vec<String> = notes
            .iter()
            .map(|n| n.path.to_string_lossy().to_string())
            .filter(|p| violation_paths.contains(p))
            .collect();
        assert_eq!(
            violation_paths, expected,
            "par_iter().filter_map().collect() must preserve input-slice order"
        );
        assert!(
            !report.violations.is_empty(),
            "test fixture should produce at least one violation"
        );
    }
}
