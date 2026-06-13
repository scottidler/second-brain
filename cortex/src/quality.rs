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

/// Types excluded from quality scoring (system-generated notes). Enum-derived
/// so the list can never drift from the real `NoteType` strings.
fn is_excluded_type(note_type: &str) -> bool {
    use std::str::FromStr;
    use vault::schema::NoteType;
    matches!(
        NoteType::from_str(note_type),
        Ok(NoteType::Digest | NoteType::Review | NoteType::Daily | NoteType::System)
    )
}

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
                && is_excluded_type(note_type)
            {
                return None;
            }

            // Never grade the user's own writing (work notes, journals, home.md).
            if crate::scope::is_authored(note) {
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
pub fn apply_quality(vault_root: &Path, notes: &[Note], config: &QualityConfig) -> eyre::Result<Vec<String>> {
    log::debug!(
        "quality::apply_quality: vault_root={} notes={}",
        vault_root.display(),
        notes.len()
    );
    let report = lint_quality(notes, config);

    let flagged_paths: HashSet<&Path> = report.violations.iter().map(|v| v.path.as_path()).collect();

    // Apply: write quality fields to flagged notes (parallel). Each task yields
    // the changed vault-relative path (or None) so callers get the real changed
    // file list, not just a count (the daemon's oscillation fingerprint).
    let applied: Vec<String> = report
        .violations
        .par_iter()
        .map(|violation| -> eyre::Result<Option<String>> {
            let Some(Fix::SetCortexFields { fields }) = &violation.fix else {
                return Ok(None);
            };
            let abs_path = vault_root.join(&violation.path);
            let content = std::fs::read_to_string(&abs_path)?;

            let already_set = fields
                .iter()
                .all(|(key, val)| content.contains(&format!("{key}: {val}")));
            if already_set {
                return Ok(None);
            }

            let yaml_fields: Vec<(String, serde_yaml::Value)> = fields
                .iter()
                .map(|(k, v)| (k.clone(), serde_yaml::Value::String(v.clone())))
                .collect();

            if let Some(new_content) = crate::scope::insert_frontmatter_fields(&content, &yaml_fields) {
                vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
                log::info!("wrote quality fields: {}", violation.path.display());
                Ok(Some(violation.path.to_string_lossy().to_string()))
            } else {
                Ok(None)
            }
        })
        .collect::<eyre::Result<Vec<Option<String>>>>()?
        .into_iter()
        .flatten()
        .collect();

    // Clear: remove quality fields from notes no longer flagged (parallel).
    let cortex_keys = vec!["cortex-quality".to_string(), "cortex-quality-issues".to_string()];
    let cleared: Vec<String> = notes
        .par_iter()
        .map(|note| -> eyre::Result<Option<String>> {
            if flagged_paths.contains(note.path.as_path()) {
                return Ok(None);
            }
            let has_cortex_fields = note.frontmatter.extra.contains_key("cortex-quality")
                || note.frontmatter.extra.contains_key("cortex-quality-issues");
            if !has_cortex_fields {
                return Ok(None);
            }

            let abs_path = vault_root.join(&note.path);
            let content = std::fs::read_to_string(&abs_path)?;
            if let Some(new_content) = crate::scope::remove_frontmatter_fields(&content, &cortex_keys) {
                vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
                log::info!("cleared stale quality fields: {}", note.path.display());
                Ok(Some(note.path.to_string_lossy().to_string()))
            } else {
                Ok(None)
            }
        })
        .collect::<eyre::Result<Vec<Option<String>>>>()?
        .into_iter()
        .flatten()
        .collect();

    let mut changed = applied;
    changed.extend(cleared);
    Ok(changed)
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
mod tests;
