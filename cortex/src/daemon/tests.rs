use super::*;
use crate::config::{Config, DaemonConfig, EnvBootstrapConfig};
use chrono::Datelike;
use std::cell::Cell;
use std::rc::Rc;

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
fn test_daemon_config_default_omits_rayon_and_bootstrap() {
    // Fail-closed defaults: a host with no explicit config gets no rayon cap
    // and no secret bootstrap, never a value baked into Rust source.
    let config = DaemonConfig::default();
    assert_eq!(config.rayon_threads, 0);
    assert!(config.env_bootstrap.is_none());
}

#[test]
fn test_daemon_config_deserialize_rayon_and_bootstrap() {
    let yaml = "rayon-threads: 8\n\
                env-bootstrap:\n  \
                command: manifest age decrypt /path/.secrets -f env\n  \
                env-file: /run/user/1000/cortex.env\n";
    let config: DaemonConfig = serde_yaml::from_str(yaml).expect("deserialize");
    assert_eq!(config.rayon_threads, 8);
    let bootstrap = config.env_bootstrap.expect("env-bootstrap must deserialize");
    assert_eq!(bootstrap.command, "manifest age decrypt /path/.secrets -f env");
    assert_eq!(
        bootstrap.env_file,
        std::path::PathBuf::from("/run/user/1000/cortex.env")
    );
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

/// Phase 2 (design doc `2026-07-05-cortex-daemon-oscillation-loop.md`):
/// the inversion of the Phase 0 repro. Phase 0 pinned the intel<->sweep
/// two-writer fight - intel stamped `tags: [digest]`, `sweep::migrate` stripped
/// it, forever. Phase 2 ends the fight at its source: intel emits NO tag on the
/// digest (`digest` is a `NoteType`, not a canonical tag) and is input-side
/// idempotent. This test asserts the fight NO LONGER reproduces: the digest is
/// tagless, `sweep::migrate` never touches it (criterion b: 0 for digest notes
/// across two runs), and the second intel run is a byte-for-byte no-op.
#[test]
fn intel_sweep_two_writer_fight_no_longer_reproduces() {
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

    // Cycle 1: intel writes the digest WITHOUT any tag.
    let report = crate::intel::run(vault_root, &config, &intel_opts).expect("intel cycle 1");
    let digest_path = report.output_path.clone();
    let after_intel_1 = std::fs::read_to_string(&digest_path).expect("read digest after intel cycle 1");
    assert!(
        !after_intel_1.contains("tags:"),
        "cycle-1 intel must emit NO tags on the digest; got:\n{after_intel_1}"
    );
    assert!(
        after_intel_1.contains("intel-input-hash:"),
        "cycle-1 intel must persist the input hash; got:\n{after_intel_1}"
    );

    // Cycle 1: sweep's canonical-tag migration has nothing to strip - the
    // digest is tagless, so it is NOT among the migrated paths (criterion b).
    let notes = crate::vault::scan_vault(vault_root, &config.vault).expect("scan after intel cycle 1");
    let migrated_1 = crate::sweep::migrate(vault_root, &notes, &config.sweep, false).expect("sweep migrate cycle 1");
    let digest_rel = digest_path
        .strip_prefix(vault_root)
        .unwrap_or(&digest_path)
        .to_string_lossy()
        .to_string();
    assert!(
        !migrated_1.iter().any(|p| p == &digest_rel),
        "sweep::migrate must report 0 for the digest note; migrated={migrated_1:?}"
    );
    let after_sweep_1 = std::fs::read_to_string(&digest_path).expect("read digest after sweep cycle 1");
    assert_eq!(
        after_intel_1, after_sweep_1,
        "sweep must leave the tagless digest byte-for-byte unchanged"
    );

    // Cycle 2: intel sees unchanged inputs (persisted input-hash matches) and
    // skips regeneration entirely - the digest is byte-for-byte identical, so
    // there is nothing left for sweep to fight over.
    crate::intel::run(vault_root, &config, &intel_opts).expect("intel cycle 2");
    let after_intel_2 = std::fs::read_to_string(&digest_path).expect("read digest after intel cycle 2");
    assert_eq!(
        after_intel_1, after_intel_2,
        "cycle-2 intel must be a no-op on unchanged inputs; got:\n{after_intel_2}"
    );

    // Cycle 2 sweep: still nothing to migrate.
    let notes2 = crate::vault::scan_vault(vault_root, &config.vault).expect("scan after intel cycle 2");
    let migrated_2 = crate::sweep::migrate(vault_root, &notes2, &config.sweep, false).expect("sweep migrate cycle 2");
    assert!(
        !migrated_2.iter().any(|p| p == &digest_rel),
        "sweep::migrate must report 0 for the digest note across two runs; migrated={migrated_2:?}"
    );
}

/// Phase 0/7 regression guard (design doc, "structural invariant"): two
/// consecutive periodic sweeps over an unchanged, steady-state vault must
/// eventually produce an EMPTY `SweepFingerprint`.
///
/// This fixture enables ONLY `intel` + `sweep` - the exact two writers whose
/// fight Phase 2 ends. Before Phase 2 it never converged (intel re-stamped
/// `tags: [digest]`, sweep re-stripped it, so cycle-2's fingerprint was always
/// non-empty). As of Phase 2 the digest is tagless and intel is input-side
/// idempotent, so the fight is gone and both cycles converge to an EMPTY
/// fingerprint.
///
/// NOTE: the doc's Phase 7 plan schedules this inversion (`!fp2.is_empty()` ->
/// `fp2.is_empty()`) after Phases 1-4, on the reasoning that the FULL
/// action-set fixture (lint/link/etc.) only converges once Phase 4 reconciles
/// the link matchers. This narrow intel+sweep-only fixture, however, converges
/// as soon as Phase 2 removes the digest-tag fight - the mandated tag removal
/// leaves nothing for cycle-2's fingerprint. So the inversion is forced here in
/// Phase 2. Phase 7 still owns the full-action-set empty-fingerprint invariant.
#[test]
fn periodic_sweep_fingerprint_converges_after_phase2() {
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

    // First periodic sweep: baseline (writes the tagless digest once).
    let fp1 = configured_actions(vault_root, &config, &daemon_config, &[]);
    // Second periodic sweep over the SAME, otherwise-unchanged vault: intel now
    // skips regeneration (unchanged inputs) and the tagless digest gives sweep
    // nothing to migrate, so the fingerprint is EMPTY - the daemon's oscillation
    // detector will never latch on this steady state.
    let fp2 = configured_actions(vault_root, &config, &daemon_config, &[]);

    assert!(
        fp2.is_empty(),
        "expected the intel<->sweep steady state to converge to an EMPTY fingerprint after Phase 2 \
         (fp1={fp1:?} fp2={fp2:?})"
    );
}

/// Phase 2 success criterion (c) (design doc
/// `2026-07-05-cortex-daemon-oscillation-loop.md`): a scheduled-intel write
/// performed under the `applying` guard (as the daemon's daily/weekly arms now
/// do) must NOT clear a latched `oscillating` state. The guard makes the
/// watcher callback drop the write's own events while `applying` is true; once
/// the flag flips false, no further event is delivered, so the latch - which is
/// only cleared by a delivered watcher event - stays set.
///
/// This exercises the real `VaultWatcher` wired exactly as the daemon wires it
/// (shared `applying` `AtomicBool`), a real `intel::run` write, and models the
/// daemon's watcher arm (`oscillating = false` on a delivered `VaultChange`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduled_intel_write_under_applying_guard_does_not_clear_latch() {
    let vault_dir = tempfile::tempdir().expect("vault tmpdir");
    let vault_root = vault_dir.path();
    let assets_dir = tempfile::tempdir().expect("assets tmpdir");
    let config = Config {
        sweep: sweep_config_without_digest(assets_dir.path()),
        ..Config::default()
    };

    // Mirror the daemon: an `applying` flag shared with the watcher, and a
    // latched oscillation state that a delivered watcher event would clear.
    let applying = Arc::new(AtomicBool::new(false));
    let watcher_config = WatcherConfig {
        debounce_secs: 1,
        ignore_dirs: config.vault.ignore.clone(),
    };
    let (watcher, mut watch_rx) =
        VaultWatcher::start(vault_root, watcher_config, Some(Arc::clone(&applying))).expect("start watcher");
    let mut oscillating = true;

    let intel_opts = crate::opts::IntelOpts {
        mode: crate::intel::IntelMode::Daily,
        output: None,
        // Empty-input branch: deterministic, no LLM/network call.
        as_of: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
    };

    // Scheduled intel write, wrapped in the guard exactly like the daemon's
    // daily/weekly arms. We hold `applying` true across the write AND briefly
    // after so the write's inotify events flush to the (dropping) callback
    // while the flag is still set - mirroring `block_in_place` holding the flag
    // for the entire duration of `intel::run`.
    applying.store(true, Ordering::Relaxed);
    crate::intel::run(vault_root, &config, &intel_opts).expect("scheduled intel run");
    tokio::time::sleep(Duration::from_millis(300)).await;
    applying.store(false, Ordering::Relaxed);

    // Wait past the debounce window: any event that had slipped through would
    // be emitted now. Model the daemon's watcher arm - a delivered event clears
    // the latch.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    while watch_rx.try_recv().is_ok() {
        oscillating = false;
    }

    assert!(
        oscillating,
        "a scheduled-intel write under the `applying` guard must NOT clear the oscillation latch"
    );
    drop(watcher);
}

/// Phase 5 (design doc `2026-07-05-cortex-daemon-oscillation-loop.md`), success
/// criterion (a): a cycle in which no action mutates the vault performs
/// exactly ONE `scan_vault` call. Injects a counting fake through
/// `configured_actions_with_scanner` (the Phase 5 seam) in place of the real
/// scanner, over an empty vault with every scan-consuming action enabled in
/// report-only mode (`enable: false` -> `is_enabled` false -> `apply`/`auto`
/// false in every arm that checks it), which guarantees zero writes
/// regardless of what a scan would find. Before Phase 5 this cycle issued one
/// independent `scan_vault` call per scanning action (classify, lint, link,
/// duplicates, auto-tag, quality, sweep - broken-links included) every time it
/// ran; the shared cache collapses that to exactly one call.
#[test]
fn configured_actions_no_mutation_scans_vault_exactly_once() {
    let vault_dir = tempfile::tempdir().expect("vault tmpdir");
    let vault_root = vault_dir.path();

    let config = Config::default();
    let mut daemon_config = DaemonConfig::default();
    daemon_config.actions.clear();
    for name in [
        "classify",
        "lint",
        "link",
        "broken-links",
        "duplicates",
        "auto-tag",
        "quality",
        "sweep",
    ] {
        daemon_config
            .actions
            .insert(name.to_string(), crate::config::DaemonAction { enable: false });
    }

    let calls = Rc::new(Cell::new(0usize));
    let calls_clone = Rc::clone(&calls);
    let counting_scan = move |root: &Path, vault_config: &VaultConfig| {
        calls_clone.set(calls_clone.get() + 1);
        crate::vault::scan_vault(root, vault_config)
    };

    let fingerprint = configured_actions_with_scanner(vault_root, &config, &daemon_config, &[], counting_scan);

    assert!(
        fingerprint.is_empty(),
        "expected zero writes on an empty vault with every action in report-only mode: {fingerprint:?}"
    );
    assert_eq!(
        calls.get(),
        1,
        "expected exactly one scan_vault call for a cycle where no action mutates the vault"
    );
}

/// Phase 5 (design doc `2026-07-05-cortex-daemon-oscillation-loop.md`), success
/// criterion (b): a cycle with a mutation rescans exactly at the defined
/// boundary - right before the next action that reads the shared note list,
/// never before an action that does not need fresher state. `classify` runs
/// first by design and MOVES an inbox note here (a real, on-disk mutation);
/// `lint` is the only other configured action and runs in report-only mode
/// (`enable: false`) so it cannot itself write and confound the count. Total
/// scan_vault calls must be exactly 2: the single scan at the top of the
/// cycle, plus the one rescan the design doc mandates after
/// "classify-with-promotions", before lint consumes the (now stale) cache.
#[test]
fn configured_actions_rescans_once_after_classify_promotion() {
    let vault_dir = tempfile::tempdir().expect("vault tmpdir");
    let vault_root = vault_dir.path();
    let inbox_dir = vault_root.join("inbox");
    std::fs::create_dir_all(&inbox_dir).expect("mkdir inbox");
    std::fs::write(
        inbox_dir.join("thing.md"),
        "---\ntags:\n  - rust\n---\nSome inbox content about rust tooling.\n",
    )
    .expect("write inbox note");

    let config = Config::default();
    let mut daemon_config = DaemonConfig::default();
    daemon_config.actions.clear();
    daemon_config
        .actions
        .insert("classify".to_string(), crate::config::DaemonAction { enable: true });
    daemon_config
        .actions
        .insert("lint".to_string(), crate::config::DaemonAction { enable: false });

    let calls = Rc::new(Cell::new(0usize));
    let calls_clone = Rc::clone(&calls);
    let counting_scan = move |root: &Path, vault_config: &VaultConfig| {
        calls_clone.set(calls_clone.get() + 1);
        crate::vault::scan_vault(root, vault_config)
    };

    let fingerprint = configured_actions_with_scanner(vault_root, &config, &daemon_config, &[], counting_scan);

    assert!(
        fingerprint.results.iter().any(|(action, _)| action == "classify"),
        "expected classify to have promoted the inbox note: {fingerprint:?}"
    );
    assert!(
        !vault_root.join("inbox/thing.md").exists(),
        "expected the inbox note to have been moved out of inbox/"
    );
    assert!(
        vault_root.join("notes/thing.md").exists(),
        "expected the promoted note at notes/thing.md"
    );
    assert_eq!(
        calls.get(),
        2,
        "expected exactly 2 scan_vault calls: the initial scan + one rescan boundary after classify's promotion \
         (fingerprint={fingerprint:?})"
    );
}

/// Success criterion (a): a config with the secret bootstrap, rayon cap, and
/// `log-level: info` set must render a unit string containing every one of
/// those directives plus `--log-level info`.
#[test]
fn test_render_systemd_unit_includes_bootstrap_rayon_and_log_level() {
    let config = Config {
        log_level: "info".to_string(),
        daemon: DaemonConfig {
            rayon_threads: 8,
            env_bootstrap: Some(EnvBootstrapConfig {
                command: "manifest age decrypt /home/user/secrets/.secrets -f env".to_string(),
                env_file: std::path::PathBuf::from("/run/user/1000/cortex.env"),
            }),
            ..DaemonConfig::default()
        },
        ..Config::default()
    };

    let home = std::path::Path::new("/home/user");
    let binary = std::path::Path::new("/home/user/.cargo/bin/sb");
    let vault_root = std::path::Path::new("/home/user/vault");

    let unit = render_systemd_unit(home, binary, vault_root, &config);

    assert!(
        unit.contains(
            "ExecStartPre=/bin/sh -c 'manifest age decrypt /home/user/secrets/.secrets -f env > /run/user/1000/cortex.env'"
        ),
        "missing secret ExecStartPre:\n{unit}"
    );
    assert!(
        unit.contains("EnvironmentFile=-/run/user/1000/cortex.env"),
        "missing EnvironmentFile:\n{unit}"
    );
    assert!(
        unit.contains("Environment=\"RAYON_NUM_THREADS=8\""),
        "missing rayon cap:\n{unit}"
    );
    assert!(unit.contains("--log-level info"), "missing --log-level info:\n{unit}");
}

/// Success criterion (b), the "no debug" half: with `log-level: info`
/// configured, the generated unit must never contain `--log-level debug`.
#[test]
fn test_render_systemd_unit_excludes_debug_when_config_is_info() {
    // Config::default() already carries log_level="info" (config.rs);
    // asserted explicitly here rather than reassigned, to avoid a
    // no-op field-reassign-with-default clippy hit.
    let config = Config::default();
    assert_eq!(config.log_level, "info");

    let home = std::path::Path::new("/home/user");
    let binary = std::path::Path::new("/home/user/.cargo/bin/sb");
    let vault_root = std::path::Path::new("/home/user/vault");

    let unit = render_systemd_unit(home, binary, vault_root, &config);

    assert!(
        !unit.contains("--log-level debug"),
        "unit must not contain --log-level debug when config.log_level=info:\n{unit}"
    );
    assert!(unit.contains("--log-level info"), "expected --log-level info:\n{unit}");
}

/// Success criterion (b), the "still there when configured" half: bootstrap
/// and rayon cap survive even with a non-default log level.
#[test]
fn test_render_systemd_unit_keeps_bootstrap_and_rayon_regardless_of_log_level() {
    let config = Config {
        log_level: "warn".to_string(),
        daemon: DaemonConfig {
            rayon_threads: 4,
            env_bootstrap: Some(EnvBootstrapConfig {
                command: "manifest age decrypt /path/.secrets -f env".to_string(),
                env_file: std::path::PathBuf::from("/run/user/1000/cortex.env"),
            }),
            ..DaemonConfig::default()
        },
        ..Config::default()
    };

    let home = std::path::Path::new("/home/user");
    let binary = std::path::Path::new("/home/user/.cargo/bin/sb");
    let vault_root = std::path::Path::new("/home/user/vault");

    let unit = render_systemd_unit(home, binary, vault_root, &config);

    assert!(
        unit.contains(
            "ExecStartPre=/bin/sh -c 'manifest age decrypt /path/.secrets -f env > /run/user/1000/cortex.env'"
        )
    );
    assert!(unit.contains("EnvironmentFile=-/run/user/1000/cortex.env"));
    assert!(unit.contains("Environment=\"RAYON_NUM_THREADS=4\""));
}

/// Absent config (no rayon cap, no bootstrap) must still render a valid unit
/// with neither directive - environments with no secret bootstrap are not
/// left with a broken/incomplete unit.
#[test]
fn test_render_systemd_unit_omits_bootstrap_and_rayon_when_unset() {
    let config = Config::default();

    let home = std::path::Path::new("/home/user");
    let binary = std::path::Path::new("/home/user/.cargo/bin/sb");
    let vault_root = std::path::Path::new("/home/user/vault");

    let unit = render_systemd_unit(home, binary, vault_root, &config);

    assert!(!unit.contains("ExecStartPre"), "no bootstrap configured:\n{unit}");
    assert!(!unit.contains("EnvironmentFile"), "no bootstrap configured:\n{unit}");
    assert!(!unit.contains("RAYON_NUM_THREADS"), "no rayon cap configured:\n{unit}");
    // Still a complete, well-formed unit.
    assert!(unit.contains("[Unit]"));
    assert!(unit.contains("[Service]"));
    assert!(unit.contains("[Install]"));
    assert!(unit.contains("ExecStart="));
}
