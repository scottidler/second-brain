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

/// Design doc `2026-07-05-cortex-daemon-oscillation-loop.md`, Phase 1,
/// success criterion (a) exercised through the real daemon seam: a note
/// whose ONLY lint violation is `frontmatter.date-format` (Severity::Warning,
/// `fix: None` - a regex check, independent of canonical-tag config) must
/// produce an empty `configured_actions` fingerprint for the `lint` action,
/// and the note's bytes must be untouched on disk. Before Phase 1 this arm
/// fingerprinted `report.violations` paths directly, so this exact case
/// (a real violation, zero real writes) would have latched oscillation
/// detection on phantom churn.
#[test]
fn configured_actions_lint_fingerprint_excludes_unfixable_violations() {
    let vault_dir = tempfile::tempdir().expect("vault tmpdir");
    let vault_root = vault_dir.path();
    let note_path = vault_root.join("only-unfixable.md");
    let original = "---\ntitle: Only Unfixable\ndate: March 2026\ntype: note\ntags:\n  - ok\n---\nBody.\n";
    std::fs::write(&note_path, original).expect("write note");

    let config = Config::default();
    let mut daemon_config = DaemonConfig::default();
    daemon_config.actions.clear();
    daemon_config
        .actions
        .insert("lint".to_string(), crate::config::DaemonAction { enable: true });

    let fingerprint = configured_actions(vault_root, &config, &daemon_config, &[]);
    assert!(
        fingerprint.is_empty(),
        "expected an empty fingerprint - the only violation present carries fix: None: {fingerprint:?}"
    );

    let after = std::fs::read_to_string(&note_path).expect("read note after cycle");
    assert_eq!(
        after, original,
        "lint detected the violation but must never have written the note"
    );
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

/// Build a `SweepConfig` whose canonical vocabulary + tag mapping do NOT
/// contain `digest` (or anything else) - mirrors the real
/// `config/canonical-tags.yml` / `tag-mapping.yml` state the design doc
/// documents (no `digest` entry anywhere), so `canonical::filter_and_cap`
/// drops the tag `intel::generate_daily_digest` hardcodes.
fn sweep_config_without_digest(assets_dir: &Path) -> crate::config::SweepConfig {
    std::fs::write(assets_dir.join("canonical-tags.yml"), "tags: {}\n").expect("write canonical-tags.yml");
    std::fs::write(assets_dir.join("tag-mapping.yml"), "{}\n").expect("write tag-mapping.yml");
    std::fs::write(assets_dir.join("tag-proposals.yml"), "proposals: []\n").expect("write tag-proposals.yml");
    crate::config::SweepConfig {
        canonical_path: assets_dir.join("canonical-tags.yml"),
        mapping_path: assets_dir.join("tag-mapping.yml"),
        proposals_path: assets_dir.join("tag-proposals.yml"),
        ..crate::config::SweepConfig::default()
    }
}

/// Phase 0 repro (design doc `2026-07-05-cortex-daemon-oscillation-loop.md`):
/// scripted two-cycle reproduction of the intel<->sweep two-writer fight on a
/// temp fixture vault. `intel::generate_daily_digest` unconditionally stamps
/// `tags: [digest]` on the daily digest note every time it runs; `digest` is
/// not in the canonical tag vocabulary, so `sweep::migrate` unconditionally
/// strips it back to `tags: []` on the very next sweep. Neither side is
/// individually buggy (both are idempotent in isolation) - the fight is two
/// writers disagreeing about the note's tags forever.
#[test]
fn intel_sweep_two_writer_fight_reproduces_across_two_cycles() {
    let vault_dir = tempfile::tempdir().expect("vault tmpdir");
    let vault_root = vault_dir.path();
    let assets_dir = tempfile::tempdir().expect("assets tmpdir");

    let config = Config {
        sweep: sweep_config_without_digest(assets_dir.path()),
        ..Config::default()
    };

    let intel_opts = crate::opts::IntelOpts {
        mode: crate::intel::IntelMode::Daily,
        output: None,
        // Fixed date with no ingested notes dated "yesterday" relative to it,
        // so `generate_daily_digest` takes the empty-input branch and never
        // calls the LLM (deterministic, network-free).
        as_of: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
    };

    // Cycle 1: intel writes the digest with the hardcoded, non-canonical tag.
    let report = crate::intel::run(vault_root, &config, &intel_opts).expect("intel cycle 1");
    let digest_path = report.output_path.clone();
    let after_intel_1 = std::fs::read_to_string(&digest_path).expect("read digest after intel cycle 1");
    assert!(
        after_intel_1.contains("tags: [digest]"),
        "cycle-1 intel must leave tags: [digest]; got:\n{after_intel_1}"
    );

    // Cycle 1: sweep's canonical-tag migration strips the non-canonical tag.
    let notes = crate::vault::scan_vault(vault_root, &config.vault).expect("scan after intel cycle 1");
    crate::sweep::migrate(vault_root, &notes, &config.sweep, false).expect("sweep migrate cycle 1");
    let after_sweep_1 = std::fs::read_to_string(&digest_path).expect("read digest after sweep cycle 1");
    assert!(
        after_sweep_1.contains("tags: []"),
        "cycle-1 sweep must rewrite to tags: []; got:\n{after_sweep_1}"
    );

    // Cycle 2: intel regenerates the digest from scratch and re-stamps
    // `tags: [digest]`, restoring exactly what cycle-1 sweep just stripped.
    crate::intel::run(vault_root, &config, &intel_opts).expect("intel cycle 2");
    let after_intel_2 = std::fs::read_to_string(&digest_path).expect("read digest after intel cycle 2");
    assert!(
        after_intel_2.contains("tags: [digest]"),
        "cycle-2 intel must restore tags: [digest]; got:\n{after_intel_2}"
    );
}

/// Phase 0/7 regression guard (design doc, "structural invariant"): two
/// consecutive periodic sweeps over an unchanged, steady-state vault must
/// eventually produce an EMPTY `SweepFingerprint`. On today's code (pre
/// Phase 1/2) it never converges: the intel<->sweep two-writer fight above
/// is a genuine, non-idempotent-across-cycles write, so the daemon's own
/// `configured_actions` fingerprint is non-empty on every single cycle,
/// forever - which is exactly what permanently latches `oscillating = true`
/// in `start_watching`.
///
/// This test PINS that current buggy (non-converging) behavior so it bites
/// now. Phase 7 inverts the assertion (`!fp2.is_empty()` -> `fp2.is_empty()`)
/// once Phases 1-2 land and re-purposes this test as the passing regression
/// guard the design doc calls for.
#[test]
fn periodic_sweep_fingerprint_does_not_converge_pre_phase1_and_2() {
    let vault_dir = tempfile::tempdir().expect("vault tmpdir");
    let vault_root = vault_dir.path();
    let assets_dir = tempfile::tempdir().expect("assets tmpdir");

    let config = Config {
        sweep: sweep_config_without_digest(assets_dir.path()),
        ..Config::default()
    };

    let mut daemon_config = DaemonConfig::default();
    daemon_config.actions.clear();
    daemon_config
        .actions
        .insert("intel".to_string(), crate::config::DaemonAction { enable: true });
    daemon_config
        .actions
        .insert("sweep".to_string(), crate::config::DaemonAction { enable: true });

    // First periodic sweep: baseline. (May be empty or non-empty depending on
    // HashMap iteration order between "intel" and "sweep" - not asserted on.)
    let fp1 = configured_actions(vault_root, &config, &daemon_config, &[]);
    // Second periodic sweep over the SAME, otherwise-unchanged vault: this is
    // the "two consecutive steady-state sweeps" the design doc's acceptance
    // criterion targets. It must be non-empty on current code regardless of
    // action-iteration order: whichever of intel/sweep ran second in cycle 1
    // leaves the digest note in a state the OTHER one rewrites in cycle 2.
    let fp2 = configured_actions(vault_root, &config, &daemon_config, &[]);

    assert!(
        !fp2.is_empty(),
        "expected today's code to phantom-oscillate via the intel<->sweep digest-tag fight \
         (fp1={fp1:?} fp2={fp2:?}); if this now passes, Phases 1-2 have landed - invert this \
         assertion to `fp2.is_empty()` per the Phase 7 plan in \
         docs/design/2026-07-05-cortex-daemon-oscillation-loop.md"
    );
}
