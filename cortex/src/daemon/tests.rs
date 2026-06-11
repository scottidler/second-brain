use super::*;
use crate::config::DaemonConfig;
use chrono::Datelike;

#[test]
fn test_is_enabled_default_is_false() {
    let config = DaemonConfig::default();
    assert!(!config.is_enabled("lint"));
    assert!(!config.is_enabled("link"));
    assert!(!config.is_enabled("nonexistent"));
}

#[test]
fn test_is_enabled_explicit_true() {
    let mut config = DaemonConfig::default();
    config
        .actions
        .insert("lint".to_string(), crate::config::DaemonAction { enable: true });
    assert!(config.is_enabled("lint"));
    assert!(!config.is_enabled("link"));
}

#[test]
fn test_is_enabled_explicit_false() {
    let config = DaemonConfig::default();
    // lint is in default actions but enable defaults to false
    assert!(!config.is_enabled("lint"));
}

#[test]
fn test_configured_actions() {
    let config = DaemonConfig::default();
    let actions = config.configured_actions();
    assert!(actions.contains(&"lint"));
    assert!(actions.contains(&"broken-links"));
}

#[test]
fn test_daemon_config_deserialize_actions() {
    let yaml =
        "actions:\n  lint:\n    enable: true\n  broken-links: {}\n  link:\n    enable: false\ndebounce-secs: 10\n";
    let config: DaemonConfig = serde_yaml::from_str(yaml).expect("deserialize");
    assert_eq!(config.debounce_secs, 10);
    assert!(config.is_enabled("lint"));
    assert!(!config.is_enabled("broken-links"));
    assert!(!config.is_enabled("link"));
    assert!(!config.is_enabled("nonexistent"));
    assert_eq!(config.actions.len(), 3);
}

#[test]
fn test_sweep_fingerprint_empty_default() {
    let fp = SweepFingerprint::default();
    assert!(fp.is_empty());
}

#[test]
fn test_sweep_fingerprint_non_empty() {
    let mut fp = SweepFingerprint::default();
    fp.add("lint", vec!["a.md".to_string(), "b.md".to_string()]);
    assert!(!fp.is_empty());
}

#[test]
fn test_sweep_fingerprint_equality() {
    let mut fp1 = SweepFingerprint::default();
    fp1.add("lint", vec!["b.md".to_string(), "a.md".to_string()]);

    let mut fp2 = SweepFingerprint::default();
    fp2.add("lint", vec!["a.md".to_string(), "b.md".to_string()]);

    // Both should sort to the same order
    assert_eq!(fp1, fp2);
}

#[test]
fn test_sweep_fingerprint_different_files() {
    let mut fp1 = SweepFingerprint::default();
    fp1.add("lint", vec!["a.md".to_string()]);

    let mut fp2 = SweepFingerprint::default();
    fp2.add("lint", vec!["b.md".to_string()]);

    assert_ne!(fp1, fp2);
}

#[test]
fn test_sweep_fingerprint_empty_files_ignored() {
    let mut fp = SweepFingerprint::default();
    fp.add("lint", vec![]);
    assert!(fp.is_empty());
}

#[test]
fn test_duration_until_daily_future_today() {
    // If we ask for a time that hasn't passed yet today, it should be today (on weekdays)
    // or next Monday (on weekends)
    let now = chrono::Local::now();
    let future_hour = (now.format("%H").to_string().parse::<u32>().unwrap_or(0) + 1) % 24;
    let time_str = format!("{future_hour:02}:00");
    let dur = duration_until_next(&time_str);
    // Should be within 3 days (worst case: Saturday -> Monday)
    assert!(dur < Duration::from_secs(3 * 24 * 3600));
    assert!(dur > Duration::ZERO);
}

#[test]
fn test_duration_until_daily_already_passed() {
    // If we ask for a time that already passed, it should be next weekday
    let now = chrono::Local::now();
    let past_hour = if now.format("%H").to_string().parse::<u32>().unwrap_or(0) > 0 {
        now.format("%H").to_string().parse::<u32>().unwrap_or(0) - 1
    } else {
        23
    };
    let time_str = format!("{past_hour:02}:00");
    let dur = duration_until_next(&time_str);
    // Should be within 3 days (worst case: Friday past -> Monday)
    assert!(dur > Duration::ZERO);
    assert!(dur <= Duration::from_secs(3 * 24 * 3600));
}

#[test]
fn test_duration_until_weekday_schedule() {
    // "M-F 12:00" should always land on a weekday (Mon-Fri)
    let dur = duration_until_next("M-F 12:00");
    let now = chrono::Local::now();
    let target = now + chrono::Duration::from_std(dur).expect("valid duration");
    let weekday = target.weekday();
    assert!(
        matches!(
            weekday,
            chrono::Weekday::Mon
                | chrono::Weekday::Tue
                | chrono::Weekday::Wed
                | chrono::Weekday::Thu
                | chrono::Weekday::Fri
        ),
        "M-F schedule should only fire on weekdays, got {weekday:?}"
    );
}

#[test]
fn test_duration_until_weekly_returns_valid_duration() {
    let dur = duration_until_next("Sun 22:00");
    // Should be within 7 days
    assert!(dur <= Duration::from_secs(7 * 24 * 3600));
    assert!(dur > Duration::ZERO);
}

#[test]
fn test_duration_until_weekly_all_days() {
    for day in &["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] {
        let schedule = format!("{day} 12:00");
        let dur = duration_until_next(&schedule);
        assert!(dur <= Duration::from_secs(7 * 24 * 3600), "failed for {day}");
        assert!(dur > Duration::ZERO, "failed for {day}");
    }
}

#[test]
fn test_daemon_config_deserialize_schedule_fields() {
    let yaml = "daily-at: \"23:00\"\nweekly-at: \"Sun 22:00\"\n";
    let config: DaemonConfig = serde_yaml::from_str(yaml).expect("deserialize");
    assert_eq!(config.daily_at.as_deref(), Some("23:00"));
    assert_eq!(config.weekly_at.as_deref(), Some("Sun 22:00"));
}

#[test]
fn test_schedule_to_cron_weekdays() {
    assert_eq!(schedule_to_cron("M-F 07:00"), "00 07 * * 1-5");
    assert_eq!(schedule_to_cron("Mon-Fri 07:00"), "00 07 * * 1-5");
}

#[test]
fn test_schedule_to_cron_single_day() {
    assert_eq!(schedule_to_cron("Sun 22:00"), "00 22 * * 0");
    assert_eq!(schedule_to_cron("Mon 09:30"), "30 09 * * 1");
}

#[test]
fn test_schedule_to_cron_weekend() {
    assert_eq!(schedule_to_cron("Sat-Sun 10:00"), "00 10 * * 6,0");
}

#[test]
fn test_schedule_to_cron_bare_time() {
    assert_eq!(schedule_to_cron("07:00"), "00 07 * * *");
}

#[test]
fn test_daemon_config_default_no_schedule() {
    let config = DaemonConfig::default();
    assert!(config.daily_at.is_none());
    assert!(config.weekly_at.is_none());
}

// Phase 0 smoke test: scan_vault wrapped in tokio::task::block_in_place runs to completion
// from a multi-thread tokio runtime without panicking. This is the guardrail for the design
// doc's Phase 0 wrapping pattern.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_vault_inside_block_in_place_does_not_panic() {
    use crate::config::VaultConfig;
    use std::fs;

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    fs::write(
        root.join("a.md"),
        "---\ndomain: tools\ntype: knowledge\norigin: authored\nstatus: draft\nmethod: cli\n---\n# A\n",
    )
    .expect("write a");
    fs::write(
        root.join("b.md"),
        "---\ndomain: tools\ntype: knowledge\norigin: authored\nstatus: draft\nmethod: cli\n---\n# B\n",
    )
    .expect("write b");

    let vault_config = VaultConfig::default();
    let notes = tokio::task::block_in_place(|| crate::vault::scan_vault(root, &vault_config))
        .expect("scan_vault should succeed");
    assert_eq!(notes.len(), 2, "expected 2 notes from tempdir scan");
}
