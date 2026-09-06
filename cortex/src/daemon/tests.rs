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

/// Phase 5 (2026-07-24 cortex-association-sweep design): `is_enabled` is a
/// generic lookup over `daemon.actions`, so "association" needs no dedicated
/// gate function - it defaults off exactly like every other action, because
/// `DaemonConfig::default()`'s action map never registers it.
#[test]
fn test_is_enabled_default_omits_association() {
    let config = DaemonConfig::default();
    assert!(!config.is_enabled("association"), "association is OFF by default");
}

#[test]
fn test_is_enabled_explicit_true_for_association() {
    let mut config = DaemonConfig::default();
    config
        .actions
        .insert("association".to_string(), crate::config::DaemonAction { enable: true });
    assert!(config.is_enabled("association"));
}

/// The on-change dispatch loop must be a deliberate NO-OP for "association"
/// (its real work runs on the separate periodic `association_interval` tick,
/// never per-change) - registering it in `daemon.actions` must not fall
/// through to the `unknown daemon action` catch-all, and must never write.
#[test]
fn configured_actions_association_is_a_no_op_never_a_per_change_write() {
    let vault_dir = tempfile::tempdir().expect("vault tmpdir");
    let vault_root = vault_dir.path();
    std::fs::write(
        vault_root.join("a.md"),
        "---\ntitle: a\ndate: 2026-07-01\ntype: session\nslug: foo\n---\nbody\n",
    )
    .expect("write note");
    std::fs::write(
        vault_root.join("b.md"),
        "---\ntitle: b\ndate: 2026-07-10\ntype: session\nslug: foo\n---\nbody\n",
    )
    .expect("write note");

    let config = Config::default();
    let mut daemon_config = DaemonConfig::default();
    daemon_config.actions.clear();
    daemon_config
        .actions
        .insert("association".to_string(), crate::config::DaemonAction { enable: true });

    let fingerprint =
        configured_actions_with_scanner(vault_root, &config, &daemon_config, &[], crate::vault::scan_vault);

    assert!(
        fingerprint.is_empty(),
        "the on-change loop's association arm never writes, regardless of same-slug notes present: {fingerprint:?}"
    );
    // The files are untouched - proof the on-change path took the no-op arm,
    // not some accidental write.
    let a = std::fs::read_to_string(vault_root.join("a.md")).unwrap();
    let b = std::fs::read_to_string(vault_root.join("b.md")).unwrap();
    assert!(a.contains("slug: foo") && b.contains("slug: foo"), "notes untouched");
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
    // `intel::run` and `sweep::migrate` both call
    // `crate::startup::validate_canonical_assets()`, which resolves the REAL
    // `XDG_CONFIG_HOME`-relative canonical-tags/tag-mapping files (not this
    // test's own `sweep_config_without_digest` assets) - acquire the
    // suite-wide lock so this can't race `startup/tests.rs`'s env mutation
    // under parallel `cargo test` (2026-07-05 cortex-daemon-oscillation-loop
    // design doc, Phase 1/7).
    let _lock = crate::testutil::lock_env();
    // Provision a private XDG_CONFIG_HOME: `intel::run` and `sweep::*`
    // call `validate_canonical_assets`, which would otherwise resolve the
    // developer's real ~/.config/sb/ and fail in any clean checkout.
    let _cfg = crate::testutil::hermetic_config_home();
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
    // See the lock comment on `intel_sweep_two_writer_fight_no_longer_reproduces` -
    // this fixture's "intel" and "sweep" arms both hit
    // `validate_canonical_assets()` against the REAL env.
    let _lock = crate::testutil::lock_env();
    // Provision a private XDG_CONFIG_HOME: `intel::run` and `sweep::*`
    // call `validate_canonical_assets`, which would otherwise resolve the
    // developer's real ~/.config/sb/ and fail in any clean checkout.
    let _cfg = crate::testutil::hermetic_config_home();
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

/// Phase 7 (design doc `2026-07-05-cortex-daemon-oscillation-loop.md`), the
/// structural guard the whole doc exists to enforce: two consecutive
/// periodic sweeps with the FULL default action set enabled - classify,
/// link, duplicates, intel, auto-tag, sweep, broken-links, lint, state,
/// quality - must produce an EMPTY `SweepFingerprint` on the second sweep.
///
/// `periodic_sweep_fingerprint_converges_after_phase2` (above) only proves
/// this for the narrow intel+sweep fixture; that test's own doc comment
/// explains why the FULL action-set invariant needed Phase 4 (link
/// detection/mutation reconciliation) before it could converge too, and
/// defers ownership of that broader claim to Phase 7. This is that test.
///
/// The fixture deliberately exercises a REAL fixable violation per action
/// where the action can produce one (classify promotes an inbox note; lint's
/// naming/frontmatter/tags rules rename/fill-title/alias-rewrite; link
/// inserts a glossary-concept wikilink; duplicates/quality/auto-tag stamp
/// idempotent `cortex-*` frontmatter fields; sweep strips one deliberately
/// non-canonical, non-aliased tag). `broken-links` and `state` never
/// contribute to the fingerprint by design (the first is read-only, the
/// second never touches vault notes) and are included only because the
/// design doc names them among the default action set - see `cortex/AGENTS.md`
/// and the design doc's Background section for the full list.
///
/// BITES (documented per the task's explicit ask, since `cargo test` has no
/// mechanism to assert "this test used to fail"): reverting Phases 1-4 makes
/// this test fail. Concretely, on pre-Phase-1 `main`, verified directly
/// against commit `803255e` (the Phase 0 repro commit, immediately before
/// Phase 1's fix): this exact fixture and assertion, run against that
/// commit's `configured_actions`, panics with a non-empty `fp2` - the
/// `lint` arm still fingerprints permanently-unfixable violation paths (a
/// literal `"(vault-wide)"` phantom entry plus `k8s-notes.md`,
/// `no-title-note.md`, `notes/thing.md`) and the `sweep` arm re-migrates the
/// daily digest note every cycle (`notes/ai/daily/<date>.md`, predating
/// Phase 2's tagless-digest fix). Two defects drive this:
///
/// 1. The `lint` arm fingerprinted `report.violations` paths (every
///    `tags.non-canonical`/`frontmatter.date-format`/etc. violation,
///    including permanently-unfixable ones), so `lint` alone kept both
///    fingerprints non-empty forever -
///    `configured_actions_lint_fingerprint_excludes_unfixable_violations`
///    (Phase 1) pins exactly this defect on a single-rule fixture.
/// 2. The `link` arm fingerprinted `lint_linking`'s pre-apply suggestion
///    paths rather than `apply_linking`'s real applied paths - before
///    Phase 4's matcher reconciliation, `find_mention` (detection) and
///    `insert_first_wikilink` (mutation) could disagree, so a reported
///    suggestion was not guaranteed appliable.
///
/// Both defects are independently pinned by their own Phase 1/4 regression
/// tests (`configured_actions_lint_fingerprint_excludes_unfixable_violations`,
/// `linking::tests::two_consecutive_link_passes_converge_to_zero_writes`,
/// `linking::tests::every_lint_linking_suggestion_is_appliable`); this
/// test's job is to prove the FULL action set converges together, not to
/// re-litigate each fix in isolation.
#[test]
fn full_action_set_periodic_sweep_fingerprint_converges_after_all_phases() {
    // The "sweep" arm calls `validate_canonical_assets()` against the REAL
    // env - see the lock comment on
    // `intel_sweep_two_writer_fight_no_longer_reproduces`.
    let _lock = crate::testutil::lock_env();
    // Provision a private XDG_CONFIG_HOME: `intel::run` and `sweep::*`
    // call `validate_canonical_assets`, which would otherwise resolve the
    // developer's real ~/.config/sb/ and fail in any clean checkout.
    let _cfg = crate::testutil::hermetic_config_home();

    let vault_dir = tempfile::tempdir().expect("vault tmpdir");
    let vault_root = vault_dir.path();
    let assets_dir = tempfile::tempdir().expect("assets tmpdir");

    // Sweep's canonical vocabulary covers every tag this fixture legitimately
    // uses (rust, programming, kubernetes) - unlike `sweep_config_without_digest`
    // elsewhere in this file (an EMPTY canonical set, chosen there specifically
    // to make the digest tagless), a real, populated vocabulary here means
    // `sweep::migrate` only strips the ONE deliberately-foreign,
    // non-aliased tag below, not every tag in the vault - so sweep's
    // contribution to cycle 1 is a real, targeted, observable write.
    std::fs::write(
        assets_dir.path().join("canonical-tags.yml"),
        "max-per-note: 7\ntags:\n  tech:\n    - rust\n    - programming\n    - kubernetes\n",
    )
    .expect("write canonical-tags.yml");
    std::fs::write(assets_dir.path().join("tag-mapping.yml"), "{}\n").expect("write tag-mapping.yml");
    std::fs::write(assets_dir.path().join("tag-proposals.yml"), "proposals: []\n").expect("write tag-proposals.yml");

    let config = Config {
        sweep: crate::config::SweepConfig {
            canonical_path: assets_dir.path().join("canonical-tags.yml"),
            mapping_path: assets_dir.path().join("tag-mapping.yml"),
            proposals_path: assets_dir.path().join("tag-proposals.yml"),
            ..crate::config::SweepConfig::default()
        },
        actions: crate::config::ActionsConfig {
            tags: crate::config::TagsConfig {
                aliases: [("k8s".to_string(), "kubernetes".to_string())].into_iter().collect(),
                ..crate::config::TagsConfig::default()
            },
            linking: crate::config::LinkingConfig {
                scan_for: vec!["concepts".to_string()],
                entities: crate::config::LinkingEntities {
                    concepts: vec!["langchain".to_string()],
                    ..crate::config::LinkingEntities::default()
                },
                min_word_length: 3,
                ..crate::config::LinkingConfig::default()
            },
            auto_tag: crate::config::AutoTagConfig {
                enabled: true,
                canonical_tags: vec!["kubernetes".to_string()],
                ..crate::config::AutoTagConfig::default()
            },
            ..crate::config::ActionsConfig::default()
        },
        // Point fabric at a binary that does not exist so classify's Tier-2 LLM
        // path (`classify_by_llm` -> `fabric::is_available`) is deterministically
        // OFF on every machine. Without this, a dev host that happens to have a
        // populated oracle index AND a real `fabric` binary would LLM-classify
        // the no-signal inbox note below instead of exercising `mark_needs_review`
        // - the exact path this test must pin. The fixture's auto-tag never uses
        // fabric (its `fabric_pattern` is None), so this only disables Tier-2.
        fabric: crate::config::FabricConfig {
            binary: "cortex-test-no-fabric-binary".to_string(),
            ..crate::config::FabricConfig::default()
        },
        ..Config::default()
    };

    let mut daemon_config = DaemonConfig::default();
    daemon_config.actions.clear();
    for name in [
        "classify",
        "lint",
        "link",
        "duplicates",
        "intel",
        "auto-tag",
        "sweep",
        "broken-links",
        "state",
        "quality",
    ] {
        daemon_config
            .actions
            .insert(name.to_string(), crate::config::DaemonAction { enable: true });
    }

    // -- classify: an inbox note that gets promoted to notes/ (real move). --
    let inbox_dir = vault_root.join("inbox");
    std::fs::create_dir_all(&inbox_dir).expect("mkdir inbox");
    std::fs::write(
        inbox_dir.join("thing.md"),
        "---\ntags:\n  - rust\n---\nSome inbox content about rust tooling.\n",
    )
    .expect("write inbox note");

    // -- classify (mark_needs_review path, Phase 8 audit finding #1): a
    // NO-SIGNAL inbox note - no tags, no source, Tier-2 disabled via the bogus
    // fabric binary above. classify returns None, so cycle 1 stamps
    // `cortex-needs-review: true`. It is NEVER marked `cortex-classified`, so
    // `filter_inbox_notes` re-selects it every cycle; the pre-Phase-8 code
    // rewrote it (byte-identically, new mtime) on EVERY cycle - the perpetual
    // self-write the zero-writes assertion below now catches. `origin: authored`
    // keeps quality/link/auto-tag/duplicates off it, so classify is the ONLY
    // action that ever touches it. --
    std::fs::write(
        inbox_dir.join("mystery.md"),
        "---\ntitle: Mystery Fragment\ndate: 2020-01-01\ntype: note\norigin: authored\n---\nA brief personal reflection with no classifiable signal whatsoever.\n",
    )
    .expect("write no-signal inbox note");

    // -- classify (catch-up path, Phase 8 audit finding #2): a domainless note
    // already in notes/ with a Tier-1-classifiable tag (`rust` -> tech). Cycle 1
    // enriches it in place (writes `domain: tech` + classified markers); once it
    // has a domain it leaves the unclassified target set, so cycle 2 leaves it
    // alone. `origin: authored` keeps every other action off it. Its catch-up
    // write was previously invisible to the fingerprint (finding #2). --
    std::fs::write(
        vault_root.join("orphan.md"),
        "---\ntitle: Orphan Note\ndate: 2020-01-01\ntype: note\norigin: authored\ntags:\n  - rust\n---\nAn orphaned note that lost its domain during a reingest.\n",
    )
    .expect("write domainless catch-up note");

    // -- link: a glossary-concept hub note (self-link-excluded by its own
    // stem) plus a note whose prose mentions the concept and gets a wikilink
    // inserted at first mention. --
    std::fs::write(
        vault_root.join("langchain.md"),
        "---\ntitle: LangChain\ndate: 2020-01-01\ntype: note\ndomain: tech\norigin: authored\ntags:\n  - programming\n---\nThe LangChain hub note.\n",
    )
    .expect("write langchain hub note");
    std::fs::write(
        vault_root.join("mentions-langchain.md"),
        "---\ntitle: Mentions LangChain\ndate: 2020-01-01\ntype: note\ndomain: tech\norigin: authored\ntags:\n  - programming\n---\nWe use LangChain daily in production for retrieval workflows.\n",
    )
    .expect("write mentions-langchain note");

    // -- duplicates: an exact-body-hash pair (non-authored, so eligible). --
    // Body carries an outbound wikilink from the START (not inserted mid-cycle
    // by the "link" action) so `quality`'s "no-outbound-links" issue is
    // never VOLATILE across actions within one cycle - `quality`'s own arm
    // runs once per cycle and stamps whatever `lint_quality` reports AT THAT
    // MOMENT; if a later action in the same cycle (or an unrelated
    // glossary-concept match against the real, machine-local
    // `~/.config/sb/glossary.yml` `link_with_notes` also consults) added a
    // wikilink to this body AFTER quality's write, cycle 2 would recompute a
    // different issue list and rewrite - a real flake this fixture hit
    // during authoring. Pre-baking the link keeps every quality-eligible
    // note's issue set constant regardless of daemon action ordering.
    let dup_body = "This exact body text appears twice on purpose for duplicate detection testing, see [[langchain]] for reference.\n";
    std::fs::write(
        vault_root.join("dup-a.md"),
        format!("---\ntitle: Duplicate A\ndate: 2020-01-01\ntype: note\ndomain: tech\norigin: assisted\ntags:\n  - programming\n---\n{dup_body}"),
    )
    .expect("write dup-a note");
    std::fs::write(
        vault_root.join("dup-b.md"),
        format!("---\ntitle: Duplicate B\ndate: 2020-01-01\ntype: note\ndomain: tech\norigin: assisted\ntags:\n  - programming\n---\n{dup_body}"),
    )
    .expect("write dup-b note");

    // -- auto-tag: few tags, assisted origin, body mentions the one
    // configured canonical tag ("kubernetes") verbatim. --
    std::fs::write(
        vault_root.join("k8s-notes.md"),
        "---\ntitle: K8s Notes\ndate: 2020-01-01\ntype: note\ndomain: tech\norigin: assisted\ntags:\n  - programming\n---\nNotes about kubernetes clusters and pods for the on-call rotation, see [[langchain]] too.\n",
    )
    .expect("write k8s-notes");

    // -- lint tags.alias + sweep: tagged with the alias "k8s" (not the
    // canonical form) AND not present in sweep's own canonical vocabulary
    // above under that exact spelling - whichever of lint's alias-rewrite or
    // sweep's canonical-migration runs first in this cycle resolves it (lint
    // rewrites to "kubernetes" and sweep then keeps it, since "kubernetes" IS
    // canonical; or sweep strips the unmapped "k8s" first and lint then has
    // nothing left to alias-fix) - both orders converge by cycle 2. --
    std::fs::write(
        vault_root.join("alias-note.md"),
        "---\ntitle: Alias Note\ndate: 2020-01-01\ntype: note\ndomain: tech\norigin: assisted\ntags:\n  - k8s\n---\nNotes about container orchestration and clusters at scale, see [[langchain]] too.\n",
    )
    .expect("write alias-note");

    // -- naming: a badly-named file lint renames to lowercase-hyphenated. --
    std::fs::write(
        vault_root.join("My Badly Named Note.md"),
        "---\ntitle: My Badly Named Note\ndate: 2020-01-01\ntype: note\ndomain: tech\norigin: authored\ntags:\n  - programming\n---\nThis filename violates the naming convention on purpose.\n",
    )
    .expect("write badly-named note");

    // -- frontmatter: missing title (auto_title fills it in); broken-links:
    // a dangling wikilink (read-only, never fingerprinted, included only
    // because the design doc names broken-links among the default set). --
    std::fs::write(
        vault_root.join("no-title-note.md"),
        "---\ndate: 2020-01-01\ntype: note\ndomain: tech\norigin: authored\ntags:\n  - programming\n---\nSee [[nonexistent-target]] for background reading on this topic.\n",
    )
    .expect("write no-title-note");

    // First periodic sweep: baseline. Every writable violation above gets
    // fixed exactly once; whichever cycle-1 ordering the daemon's
    // HashMap-iteration picks, every write this cycle makes is idempotent on
    // its own inputs, so it cannot recur.
    let fp1 = configured_actions(vault_root, &config, &daemon_config, &[]);

    // Snapshot the on-disk state of every note file AFTER cycle 1 has fully
    // settled. This is the Phase 8 strengthening (audit finding #2: the old
    // `fp2.is_empty()` assertion never observed the filesystem, so a
    // write-without-fingerprint - exactly what `mark_needs_review` and catch-up
    // did - passed silently). We capture bytes AND mtime: the no-signal inbox
    // note's perpetual rewrite is BYTE-IDENTICAL, so only the mtime moves - and
    // an mtime bump is precisely what fires the daemon's own vault watcher.
    let before = snapshot_note_files(vault_root);

    // Second periodic sweep over the SAME, now-fixed-up vault: every action
    // that wrote in cycle 1 finds its own fix already in place and writes
    // nothing more. This is the structural invariant the whole design doc
    // exists to enforce.
    let fp2 = configured_actions(vault_root, &config, &daemon_config, &[]);

    let after = snapshot_note_files(vault_root);

    // ZERO note files may be added, removed, or touched in cycle 2 - not merely
    // an empty fingerprint. This assertion BITES on the pre-Phase-8 classify
    // code: `mark_needs_review` rewrote `inbox/mystery.md` every cycle
    // (byte-identical, new mtime), tripping this exact check.
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "cycle 2 must not add or remove any note file (before={:?} after={:?})",
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    for (path, before_state) in &before {
        let after_state = after.get(path).expect("file present in both snapshots");
        assert_eq!(
            before_state.0,
            after_state.0,
            "cycle 2 rewrote the BYTES of {} - a non-idempotent write",
            path.display()
        );
        assert_eq!(
            before_state.1,
            after_state.1,
            "cycle 2 TOUCHED {} (mtime moved even if bytes are identical) - this is the \
             byte-identical self-write the daemon watcher fires on; a write happened without a \
             fingerprint entry",
            path.display()
        );
    }

    assert!(
        fp2.is_empty(),
        "expected the full default action set to converge to an EMPTY fingerprint on the \
         second periodic sweep (fp1={fp1:?} fp2={fp2:?})"
    );
}

/// Snapshot every `*.md` file under `root` as `path -> (bytes, mtime)`. Used by
/// the full-action-set convergence test to assert cycle 2 touches ZERO note
/// files - not just that the fingerprint is empty. Scoped to `*.md` on purpose:
/// the `state` action rewrites its own `.cortex/manifest.yml` cache (a
/// timestamped, non-note artifact) every cycle by design, which is not a vault
/// note and not part of the "writes zero note files" invariant.
fn snapshot_note_files(root: &Path) -> std::collections::BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    let mut map = std::collections::BTreeMap::new();
    for entry in walkdir::WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let bytes = std::fs::read(path).expect("read note file for snapshot");
        let mtime = entry.metadata().expect("note metadata").modified().expect("note mtime");
        map.insert(path.to_path_buf(), (bytes, mtime));
    }
    map
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
    {
        // `intel::run` calls `validate_canonical_assets()` against the REAL
        // env - see the lock comment on
        // `intel_sweep_two_writer_fight_no_longer_reproduces`. Scoped to drop
        // BEFORE the `.await` below - a `std::sync::Mutex` guard must never
        // span an `.await` point.
        let _lock = crate::testutil::lock_env();
        // Provision a private XDG_CONFIG_HOME: `intel::run` and `sweep::*`
        // call `validate_canonical_assets`, which would otherwise resolve the
        // developer's real ~/.config/sb/ and fail in any clean checkout.
        let _cfg = crate::testutil::hermetic_config_home();
        crate::intel::run(vault_root, &config, &intel_opts).expect("scheduled intel run");
    }
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
    // The "sweep" arm always calls `sweep::scan_proposals` (regardless of its
    // `enable` flag), which calls `validate_canonical_assets()` against the
    // REAL env - see the lock comment on
    // `intel_sweep_two_writer_fight_no_longer_reproduces`.
    let _lock = crate::testutil::lock_env();
    // Provision a private XDG_CONFIG_HOME: `intel::run` and `sweep::*`
    // call `validate_canonical_assets`, which would otherwise resolve the
    // developer's real ~/.config/sb/ and fail in any clean checkout.
    let _cfg = crate::testutil::hermetic_config_home();
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

/// X3: cortex writes the oracle DB under `~/.local/share/sb/`, so the unit's
/// `ReadWritePaths` must name that data dir alongside the vault or
/// `ProtectHome=read-only` blocks every embed write. Borg's unit already does.
#[serial_test::serial(xdg_data_home)]
#[test]
fn test_render_systemd_unit_readwritepaths_covers_vault_and_data_dir() {
    let xdg_tmp = tempfile::tempdir().expect("xdg tmpdir");
    let prior = std::env::var_os("XDG_DATA_HOME");
    // SAFETY: serialized by `serial_test::serial(xdg_data_home)`; no
    // concurrent reader of the env exists while this runs.
    unsafe { std::env::set_var("XDG_DATA_HOME", xdg_tmp.path()) };

    let result = std::panic::catch_unwind(|| {
        let config = Config::default();
        let home = std::path::Path::new("/home/user");
        let binary = std::path::Path::new("/home/user/.cargo/bin/sb");
        let vault_root = std::path::Path::new("/home/user/vault");

        let unit = render_systemd_unit(home, binary, vault_root, &config);

        let line = unit
            .lines()
            .find(|l| l.starts_with("ReadWritePaths="))
            .unwrap_or_else(|| panic!("no ReadWritePaths line:\n{unit}"));
        let data_dir = xdg_tmp.path().join("sb");
        assert!(line.contains("/home/user/vault"), "vault missing from {line}");
        assert!(
            line.contains(&data_dir.display().to_string()),
            "data dir {} missing from {line}",
            data_dir.display()
        );
        // The oracle DB cortex writes must fall inside the granted path.
        assert!(vault::paths::oracle_db_path().starts_with(&data_dir));
    });

    // SAFETY: same serialization as above.
    unsafe {
        match prior {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

/// PATH hygiene (Phase 5, 2026-07-20 harvest-completion): fabric is
/// mise-managed, so its shim dir must be on PATH and FIRST (mise-managed
/// tools win over any stale duplicate); the retired `~/go/bin` hand-built
/// fabric entry must be gone.
#[test]
fn test_render_systemd_unit_path_includes_mise_shims_and_excludes_go_bin() {
    let config = Config::default();

    let home = std::path::Path::new("/home/user");
    let binary = std::path::Path::new("/home/user/.cargo/bin/sb");
    let vault_root = std::path::Path::new("/home/user/vault");

    let unit = render_systemd_unit(home, binary, vault_root, &config);

    assert!(
        unit.contains("/home/user/.local/share/mise/shims"),
        "PATH must include the mise shims dir:\n{unit}"
    );
    assert!(
        !unit.contains("/home/user/go/bin"),
        "PATH must not carry the retired ~/go/bin entry:\n{unit}"
    );
    let path_line = unit
        .lines()
        .find(|l| l.contains("Environment=\"PATH="))
        .expect("expected a PATH line");
    let mise_pos = path_line.find("mise/shims").expect("mise shims present");
    let local_bin_pos = path_line.find(".local/bin").expect(".local/bin present");
    assert!(
        mise_pos < local_bin_pos,
        "mise shims must come before .local/bin so mise-managed tools win:\n{path_line}"
    );
}
