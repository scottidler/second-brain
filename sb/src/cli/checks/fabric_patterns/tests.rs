use super::super::Severity;
use super::*;
use serial_test::serial;

/// A probe that finds no pattern file, so only `fabric -l` decides.
fn no_files(_: &str) -> bool {
    false
}

#[test]
fn fabric_patterns_present_yields_two_ok() {
    let findings = fabric_pattern_findings("summarize\nmy_weekly\nclean_text\n", Some("my_weekly"), no_files);
    assert_eq!(findings.len(), 2);
    assert!(findings.iter().all(|f| f.severity == Severity::Ok), "{findings:?}");
}

#[test]
fn fabric_patterns_missing_summarize_is_error() {
    let findings = fabric_pattern_findings("my_weekly\n", Some("my_weekly"), no_files);
    assert_eq!(findings[0].severity, Severity::Error);
    assert!(findings[0].message.contains("summarize"));
    assert_eq!(findings[1].severity, Severity::Ok);
}

#[test]
fn fabric_patterns_missing_weekly_is_warn_naming_it() {
    let findings = fabric_pattern_findings("summarize\n", Some("my_weekly"), no_files);
    assert_eq!(findings[0].severity, Severity::Ok);
    assert_eq!(findings[1].severity, Severity::Warn);
    assert!(findings[1].message.contains("my_weekly"));
}

#[test]
fn fabric_patterns_checks_the_configured_weekly_value() {
    let list = "summarize\nother_weekly\n";
    let findings = fabric_pattern_findings(list, Some("my_weekly"), no_files);
    assert_eq!(findings[1].severity, Severity::Warn);
    assert!(findings[1].message.contains("my_weekly"));
    let findings = fabric_pattern_findings("summarize\nmy_weekly\n", Some("my_weekly"), no_files);
    assert!(findings.iter().all(|f| f.severity == Severity::Ok));
}

#[test]
fn fabric_patterns_no_weekly_configured_checks_only_summarize() {
    let findings = fabric_pattern_findings("summarize\n", None, no_files);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Ok);
}

#[test]
fn fabric_patterns_match_whole_lines_not_substrings() {
    let findings = fabric_pattern_findings("summarize_paper\n", None, no_files);
    assert_eq!(findings[0].severity, Severity::Error);
}

/// The `intel.batch-weekly` default is one of the patterns sb ships, so
/// `sb bootstrap` installs it.
#[test]
fn default_weekly_pattern_is_a_shipped_pattern() {
    let default = cortex::config::IntelConfig::default().batch_weekly;
    assert_eq!(default.as_deref(), Some(cortex::config::DEFAULT_WEEKLY_PATTERN));
    let shipped = format!("{}.md", cortex::config::DEFAULT_WEEKLY_PATTERN);
    assert!(
        crate::cli::bootstrap::PATTERNS.iter().any(|(name, _)| *name == shipped),
        "{shipped} is not in bootstrap::PATTERNS"
    );
}

/// After `sb bootstrap`, the default resolves to the installed file the way
/// the weekly fallback call resolves it, and doctor reports it present even
/// though `fabric -l` (upstream patterns only) does not list it.
#[test]
#[serial(env_xdg)]
fn default_weekly_pattern_resolves_after_bootstrap_and_doctor_does_not_warn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let original = std::env::var_os("XDG_CONFIG_HOME");
    // SAFETY: env mutation serialized by `serial(env_xdg)`; restored below.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    crate::cli::bootstrap::extract_canonical_assets(false).expect("extract");

    let default = cortex::config::DEFAULT_WEEKLY_PATTERN;
    let resolved = vault::fabric::resolve_pattern(default);
    let expected = tmp.path().join("sb").join("patterns").join(format!("{default}.md"));
    let findings = fabric_pattern_findings("summarize\nclean_text\n", Some(default), pattern_file_exists);

    // SAFETY: restoring env.
    unsafe {
        match original {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
    assert_eq!(std::path::PathBuf::from(&resolved), expected);
    assert!(expected.is_file(), "bootstrap installed {}", expected.display());
    assert!(
        findings.iter().all(|f| f.severity == Severity::Ok),
        "no warning for the shipped default: {findings:?}"
    );
}
