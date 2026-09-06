use colored::Colorize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Error => write!(f, "ERROR"),
            Severity::Warning => write!(f, "WARN"),
            Severity::Info => write!(f, "INFO"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum Fix {
    RenameFile {
        from: PathBuf,
        to: PathBuf,
    },
    SetFrontmatter {
        key: String,
        value: serde_yaml::Value,
    },
    ReplaceTag {
        old: String,
        new: String,
    },
    AddWikilink {
        target: String,
        /// The text actually matched in the body. Emitted as the display half
        /// of a piped link `[[target|surface]]` when it differs from `target`
        /// (e.g. an alias surface form, or a title whose stem differs).
        surface: String,
        context: String,
    },
    MoveFile {
        from: PathBuf,
        to: PathBuf,
    },
    SetCortexFields {
        fields: Vec<(String, String)>,
    },
    RemoveCortexFields {
        keys: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Violation {
    pub path: PathBuf,
    pub rule: String,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub violations: Vec<Violation>,
    /// Set by `apply` paths to indicate how many files were modified.
    /// Zero when the run was lint-only.
    #[serde(default, skip_serializing_if = "usize_is_zero")]
    pub applied: usize,
    /// The real, byte-changed paths an `apply` path wrote - `applied` is
    /// this list's length. Empty when the run was lint-only or nothing
    /// wrote. Consumers that need a write-only fingerprint (the daemon's
    /// oscillation detector) MUST use this field, never `violations` paths
    /// (those include every unappliable suggestion / `fix: None` entry).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_paths: Vec<String>,
}

fn usize_is_zero(n: &usize) -> bool {
    *n == 0
}

/// Aggregated result of the lint apply path (`lib::lint` with `opts.apply`).
///
/// `written_paths` is the ONLY thing the daemon's oscillation fingerprint may
/// use for the lint action: it is the union of the real, byte-changed paths
/// each of the four appliers (`apply_naming`/`apply_frontmatter`/`apply_tags`/
/// `apply_scope`) returned - never the lint report's `violations` paths,
/// which include every `fix: None` violation (non-canonical tags, orphan
/// tags, date-format, enum, deprecated-field) that is permanently unfixable
/// and would otherwise fingerprint identically every cycle, latching the
/// daemon's oscillation detector forever. `remaining_violations` is a
/// diagnostic count (total violations still reported after the apply pass)
/// for logging, not part of the fingerprint invariant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LintApplyReport {
    pub written_paths: Vec<String>,
    pub remaining_violations: usize,
}

impl Report {
    pub fn add(&mut self, violation: Violation) {
        self.violations.push(violation);
    }

    pub fn merge(&mut self, other: Report) {
        self.violations.extend(other.violations);
    }

    pub fn error_count(&self) -> usize {
        self.violations.iter().filter(|v| v.severity == Severity::Error).count()
    }

    pub fn warning_count(&self) -> usize {
        self.violations
            .iter()
            .filter(|v| v.severity == Severity::Warning)
            .count()
    }

    pub fn info_count(&self) -> usize {
        self.violations.iter().filter(|v| v.severity == Severity::Info).count()
    }

    pub fn is_empty(&self) -> bool {
        self.violations.is_empty()
    }

    /// Render the human-readable report into a vector of lines for sb to
    /// print. Keeps the lib stdout-clean per the lib-returns-data invariant.
    pub fn format_human(&self, applied: bool) -> Vec<String> {
        if self.is_empty() {
            return vec![format!("{}", "No violations found.".green())];
        }

        let fixable_count = self.violations.iter().filter(|v| v.fix.is_some()).count();
        let mut lines: Vec<String> = Vec::new();

        for v in &self.violations {
            let severity_str = match v.severity {
                Severity::Error => format!("{}", v.severity).red().bold(),
                Severity::Warning => format!("{}", v.severity).yellow(),
                Severity::Info => format!("{}", v.severity).blue(),
            };
            lines.push(format!(
                "{} [{}] {}: {}",
                severity_str,
                v.rule,
                v.path.display(),
                v.message
            ));
            if let Some(ref fix) = v.fix {
                let fix_desc = match fix {
                    Fix::RenameFile { from, to } => {
                        format!("rename {} -> {}", from.display(), to.display())
                    }
                    Fix::SetFrontmatter { key, value } => {
                        format!("set {key}: {value:?}")
                    }
                    Fix::ReplaceTag { old, new } => format!("replace tag {old} -> {new}"),
                    Fix::AddWikilink { target, .. } => format!("add link [[{target}]]"),
                    Fix::MoveFile { from, to } => {
                        format!("move {} -> {}", from.display(), to.display())
                    }
                    Fix::SetCortexFields { fields } => {
                        let pairs: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
                        format!("set {}", pairs.join(", "))
                    }
                    Fix::RemoveCortexFields { keys } => {
                        format!("remove {}", keys.join(", "))
                    }
                };
                let prefix = if applied { "applied:" } else { "fix:" };
                lines.push(format!("  {} {}", prefix.dimmed(), fix_desc.dimmed()));
            }
        }

        lines.push(String::new());
        let mode = if applied {
            format!(" (applied {} fix(es))", fixable_count)
        } else {
            String::new()
        };
        lines.push(format!(
            "{}",
            format!(
                "Total: {} error(s), {} warning(s), {} info(s){}",
                self.error_count(),
                self.warning_count(),
                self.info_count(),
                mode,
            )
            .bold()
        ));
        lines
    }

    /// Render the report as a JSON string for sb to print.
    pub fn format_json(&self) -> eyre::Result<String> {
        Ok(serde_json::to_string_pretty(&self.violations)?)
    }

    /// Count violations whose `rule` starts with `prefix`, keyed by the
    /// remainder of the rule string after the prefix (e.g. `prefix =
    /// "frontmatter.required."` on a rule `"frontmatter.required.domain"`
    /// keys the count under `"domain"`). Lets a caller (doctor) report
    /// per-field/per-enum tallies from cortex's own policy engine without a
    /// second copy of the rule-name convention.
    pub fn count_by_rule_prefix(&self, prefix: &str) -> BTreeMap<String, u64> {
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for violation in &self.violations {
            if let Some(suffix) = violation.rule.strip_prefix(prefix) {
                *counts.entry(suffix.to_string()).or_insert(0) += 1;
            }
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violation(rule: &str) -> Violation {
        Violation {
            path: PathBuf::from("note.md"),
            rule: rule.to_string(),
            severity: Severity::Warning,
            message: String::new(),
            fix: None,
        }
    }

    #[test]
    fn count_by_rule_prefix_groups_by_suffix_and_ignores_other_prefixes() {
        let report = Report {
            violations: vec![
                violation("frontmatter.required.domain"),
                violation("frontmatter.required.domain"),
                violation("frontmatter.required.origin"),
                violation("tags.non-canonical"),
            ],
            applied: 0,
            applied_paths: Vec::new(),
        };

        let required = report.count_by_rule_prefix("frontmatter.required.");
        let expected: BTreeMap<String, u64> = [("domain".to_string(), 2), ("origin".to_string(), 1)]
            .into_iter()
            .collect();
        assert_eq!(required, expected);

        let enums = report.count_by_rule_prefix("frontmatter.enum.");
        assert!(enums.is_empty());
    }
}
