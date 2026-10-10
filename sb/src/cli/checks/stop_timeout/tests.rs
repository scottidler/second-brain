use super::*;
use crate::cli::checks::Severity;

fn cortex_cfg(stop_timeout_secs: u64) -> cortex::config::Config {
    let mut cfg = cortex::config::Config::default();
    cfg.daemon.stop_timeout_secs = stop_timeout_secs;
    cfg
}

#[test]
fn stop_budget_finding_errors_naming_both_keys() {
    let finding = stop_budget_finding(&cortex_cfg(100));
    assert_eq!(finding.severity, Severity::Error);
    assert!(finding.message.contains("stop-timeout-secs"), "{}", finding.message);
    assert!(finding.message.contains("fabric.timeout-secs"), "{}", finding.message);
}

#[test]
fn stop_budget_finding_is_ok_at_the_default() {
    assert_eq!(stop_budget_finding(&cortex_cfg(180)).severity, Severity::Ok);
}

#[test]
fn user_manager_stop_finding_errors_on_five_seconds() {
    let finding = user_manager_stop_finding(&cortex_cfg(180), || Ok(std::time::Duration::from_secs(5)));
    assert_eq!(finding.severity, Severity::Error);
    assert!(finding.message.contains("below"), "{}", finding.message);
}

#[test]
fn user_manager_stop_finding_errors_when_unreadable() {
    let finding = user_manager_stop_finding(&cortex_cfg(180), || Err("no bus".to_string()));
    assert_eq!(finding.severity, Severity::Error);
    assert!(finding.message.contains("cannot read"), "{}", finding.message);
}

#[test]
fn user_manager_stop_finding_passes_at_210_with_180() {
    let finding = user_manager_stop_finding(&cortex_cfg(180), || Ok(std::time::Duration::from_secs(210)));
    assert_eq!(finding.severity, Severity::Ok);
}

#[test]
fn user_manager_stop_finding_tracks_the_configured_budget() {
    let finding = user_manager_stop_finding(&cortex_cfg(300), || Ok(std::time::Duration::from_secs(210)));
    assert_eq!(finding.severity, Severity::Error, "210s no longer covers 300 + 30");
}

#[test]
fn parse_timeout_stop_usec_reads_systemd_timespans() {
    assert_eq!(parse_timeout_stop_usec("3min 30s\n").unwrap().as_secs(), 210);
    assert_eq!(parse_timeout_stop_usec("5s").unwrap().as_secs(), 5);
    assert_eq!(parse_timeout_stop_usec("infinity").unwrap(), std::time::Duration::MAX);
    assert!(parse_timeout_stop_usec("").is_err());
    assert!(parse_timeout_stop_usec("soon").is_err());
}
