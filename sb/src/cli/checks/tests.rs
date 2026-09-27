use super::*;

fn make_signal(host: &str) -> SignalConfig {
    SignalConfig {
        allowed_senders: vec![],
        notification_recipient: None,
        host: host.to_string(),
        notetoself_rate_threshold_per_hour: 100,
    }
}

fn make_telegram(token: &str, host: Option<&str>) -> TelegramConfig {
    TelegramConfig {
        bot_token: token.to_string(),
        allowed_chat_ids: vec![],
        notification_chat_id: None,
        host: host.map(str::to_string),
    }
}

#[test]
fn signal_findings_host_mismatch_short_circuits() {
    let sg = make_signal("definitely-not-this-host-abc-xyz");
    let findings = signal_findings_for(&sg);
    assert_eq!(findings.len(), 1, "host mismatch must short-circuit");
    assert_eq!(findings[0].severity, Severity::Info);
    assert!(findings[0].message.contains("does not run Signal ingest"));
}

#[test]
fn signal_findings_empty_host_is_error() {
    let sg = make_signal("");
    let findings = signal_findings_for(&sg);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Error);
}

#[test]
fn signal_state_dir_missing_is_error() {
    let findings = state_dir_findings(Path::new("/nonexistent-signal-state-dir-xyz"));
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Error);
    assert!(findings[0].message.contains("does not exist"));
}

#[test]
fn telegram_findings_host_mismatch_short_circuits() {
    let tg = make_telegram("DUMMY_TOKEN_ENV_VAR", Some("definitely-not-this-host-abc-xyz"));
    let findings = telegram_findings_for(&tg);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Info);
    assert!(findings[0].message.contains("does not run Telegram ingest"));
}

#[test]
fn firefox_finding_warns_on_snap_with_migration_fix() {
    use borg::extension::install::FirefoxInstall;
    let f = firefox_finding(&FirefoxInstall::Snap);
    assert_eq!(f.severity, Severity::Warn);
    assert!(f.message.contains("snap"));
    assert!(
        f.suggested_fix.as_deref().unwrap_or_default().contains("firefox-opt"),
        "snap fix must name the firefox-opt migration"
    );
}

#[test]
fn firefox_finding_ok_on_opt_tarball_and_info_on_unknown() {
    use borg::extension::install::FirefoxInstall;
    assert_eq!(
        firefox_finding(&FirefoxInstall::Tarball(std::path::PathBuf::from("/opt/firefox"))).severity,
        Severity::Ok
    );
    assert_eq!(firefox_finding(&FirefoxInstall::Unknown).severity, Severity::Info);
}

#[test]
fn telegram_findings_empty_token_is_error() {
    let tg = make_telegram("", None);
    let findings = telegram_findings_for(&tg);
    assert!(
        findings
            .iter()
            .any(|f| f.severity == Severity::Error && f.message.contains("bot-token")),
        "empty bot-token must emit Error finding"
    );
}

mod drift {
    use super::super::*;
    use serial_test::serial;

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: env mutation for path-resolution tests; restored on Drop.
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: restoring env.
            unsafe {
                match self.original.take() {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    #[serial(env_xdg)]
    fn shared_config_missing_emits_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

        let findings = shared_config_findings();
        let errors: Vec<_> = findings.iter().filter(|f| f.severity == Severity::Error).collect();
        assert_eq!(
            errors.len(),
            2,
            "both SHIPPED shared YAMLs should be missing; tag-proposals.yml is generated state and no longer checked"
        );
        for f in &errors {
            assert!(f.suggested_fix.as_deref() == Some("sb bootstrap"));
        }
    }

    #[test]
    #[serial(env_xdg)]
    fn shared_config_match_emits_ok() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());
        crate::cli::bootstrap::extract_canonical_assets(false).expect("extract");

        let findings = shared_config_findings();
        assert!(
            findings.iter().all(|f| f.severity == Severity::Ok),
            "all match -> all Ok"
        );
    }

    #[test]
    #[serial(env_xdg)]
    fn shared_config_edit_emits_info() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());
        crate::cli::bootstrap::extract_canonical_assets(false).expect("extract");
        std::fs::write(tmp.path().join("sb").join("canonical-tags.yml"), "edited").expect("mutate");

        let findings = shared_config_findings();
        let info: Vec<_> = findings.iter().filter(|f| f.severity == Severity::Info).collect();
        assert!(info.iter().any(|f| f.message.contains("canonical-tags.yml")));
    }

    #[test]
    #[serial(env_xdg)]
    fn patterns_missing_emits_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

        let findings = pattern_findings();
        assert!(
            findings
                .iter()
                .any(|f| f.severity == Severity::Error && f.message.contains("missing")),
            "missing patterns must emit Error: {:?}",
            findings
        );
    }

    #[test]
    #[serial(env_xdg)]
    fn patterns_match_emits_ok() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());
        crate::cli::bootstrap::extract_canonical_assets(false).expect("extract");

        let findings = pattern_findings();
        assert!(
            findings.iter().all(|f| f.severity == Severity::Ok),
            "all findings must be Ok when patterns match: {findings:?}"
        );
        assert!(
            findings.iter().any(|f| f.message.contains("patterns match binary")),
            "expected the patterns-match Ok: {findings:?}"
        );
    }

    #[test]
    #[serial(env_xdg)]
    fn fabric_cli_findings_emits_error_when_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("PATH", tmp.path());

        let findings = super::super::fabric_cli_findings();
        assert!(findings.iter().any(|f| f.severity == Severity::Error
            && f.message.contains("fabric")
            && f.suggested_fix.as_deref() == Some(super::super::FABRIC_INSTALL_HINT)));
    }

    #[test]
    #[serial(env_xdg)]
    fn fabric_cli_findings_emits_ok_when_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let stub = tmp.path().join("fabric");
        std::fs::write(&stub, "#!/bin/sh\necho 'fabric 1.2.3'\n").expect("write stub");
        // chmod +x via a Permissions write
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let _guard = EnvGuard::set("PATH", tmp.path());

        let findings = super::super::fabric_cli_findings();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Ok);
        assert!(findings[0].message.contains("fabric 1.2.3"));
    }

    #[test]
    #[serial(env_xdg)]
    fn signal_rs_cli_findings_emits_error_when_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("PATH", tmp.path());

        let findings = super::super::signal_rs_cli_findings();
        assert!(findings.iter().any(|f| f.severity == Severity::Error
            && f.suggested_fix.as_deref() == Some(super::super::SIGNAL_RS_INSTALL_HINT)));
    }

    #[test]
    #[serial(env_xdg)]
    fn patterns_drift_emits_warn() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());
        crate::cli::bootstrap::extract_canonical_assets(false).expect("extract");
        let pattern_path = tmp.path().join("sb").join("patterns").join("distill-article.md");
        std::fs::write(&pattern_path, "drifted pattern body").expect("mutate pattern");

        let findings = pattern_findings();
        let warns: Vec<_> = findings.iter().filter(|f| f.severity == Severity::Warn).collect();
        assert_eq!(warns.len(), 1, "one drifted pattern -> one Warn: {:?}", findings);
        assert!(warns[0].message.contains("drifted"));
        assert_eq!(warns[0].suggested_fix.as_deref(), Some("sb bootstrap --force"));
    }
}

#[test]
fn classifier_settings_name_the_subsystem_and_effective_values() {
    let cfg = distillers::tags::TagsClassifierConfig::default();
    let finding = classifier_settings_finding("cortex", &cfg);
    assert_eq!(finding.severity, Severity::Info);
    for want in [
        "cortex classifier settings",
        "classifier Deterministic",
        "threshold 0.9",
        "fallback Deterministic",
        "https://classifier.dev/v1/classify",
        "timeout 30s",
        "token-env none (keyless)",
    ] {
        assert!(
            finding.message.contains(want),
            "missing {want:?} in {}",
            finding.message
        );
    }

    let named = distillers::tags::TagsClassifierConfig {
        classifier: distillers::tags::ClassifierKind::ClassifierDev,
        token_env: "CLASSIFIER_DEV_TOKEN".to_string(),
        ..cfg
    };
    let finding = classifier_settings_finding("borg", &named);
    assert!(finding.message.starts_with("borg classifier settings"));
    assert!(finding.message.contains("classifier ClassifierDev"));
    assert!(finding.message.contains("token-env CLASSIFIER_DEV_TOKEN"));
}

#[test]
fn classifier_settings_carry_the_spend_budget_keys() {
    let finding = classifier_settings_finding("cortex", &distillers::tags::TagsClassifierConfig::default());
    for want in ["daily-usd-budget $0.50", "ingest-reserve-usd $0.10"] {
        assert!(
            finding.message.contains(want),
            "missing {want:?} in {}",
            finding.message
        );
    }
}

/// Implementation audit r1: an existing host silently runs the code default
/// `repeats` (3, so three calls per shard) and `instructions`; doctor is where
/// the operator sees both.
#[test]
fn classifier_settings_carry_repeats_and_instructions() {
    let default = classifier_settings_finding("cortex", &distillers::tags::TagsClassifierConfig::default());
    for want in ["repeats 3", "instructions none"] {
        assert!(
            default.message.contains(want),
            "missing {want:?} in {}",
            default.message
        );
    }

    let set = distillers::tags::TagsClassifierConfig {
        repeats: 1,
        instructions: "Apply strictly.".to_string(),
        ..distillers::tags::TagsClassifierConfig::default()
    };
    let finding = classifier_settings_finding("borg", &set);
    for want in ["repeats 1", "instructions \"Apply strictly.\""] {
        assert!(
            finding.message.contains(want),
            "missing {want:?} in {}",
            finding.message
        );
    }
}

#[test]
fn tag_description_finding_counts_the_undescribed_tags() {
    let file: vault::canonical::CanonicalTagsFile =
        serde_yaml::from_str("tags:\n  tech:\n    - rust: \"the Rust language\"\n    - cli\n  ai:\n    - ai\n")
            .expect("mixed vocabulary");
    let finding = tag_description_finding(&file.canonical_set());
    assert_eq!(finding.severity, Severity::Info);
    assert_eq!(finding.message, "2 of 3 tags have no description");

    let shipped: vault::canonical::CanonicalTagsFile =
        serde_yaml::from_str(crate::cli::bootstrap::CANONICAL_TAGS_YML).expect("shipped vocabulary");
    let set = shipped.canonical_set();
    let undescribed = set.all.len() - set.descriptions.len();
    assert_eq!(
        tag_description_finding(&set).message,
        format!("{undescribed} of {} tags have no description", set.all.len())
    );
}

struct FixedDay(chrono::NaiveDate);

impl distillers::tags::spend::Clock for FixedDay {
    fn today_utc(&self) -> chrono::NaiveDate {
        self.0
    }
}

fn spend_over(ledger: &std::path::Path) -> distillers::tags::spend::DaySpend {
    distillers::tags::spend::DaySpend::new(
        distillers::tags::spend::SpendLedger::new(ledger),
        std::sync::Arc::new(FixedDay("2026-09-26".parse().expect("date"))),
        &distillers::tags::TagsClassifierConfig::default(),
    )
}

#[test]
fn classifier_spend_reports_todays_ledger_total_and_allowance() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ledger = tmp.path().join("classifier-spend.jsonl");
    std::fs::write(
        &ledger,
        concat!(
            r#"{"day":"2026-09-25","usd":0.3,"tokens":1,"path":"retag"}"#,
            "\n",
            r#"{"day":"2026-09-26","usd":0.0125,"tokens":1,"path":"tag-apply","tag":"ci-cd"}"#,
            "\n",
        ),
    )
    .expect("write");
    let finding = classifier_spend_finding(&spend_over(&ledger));
    assert_eq!(finding.severity, Severity::Info);
    for want in [
        "bulk spend today: $0.0125",
        "$0.3875 left of the $0.50 day",
        "2026-09-26 UTC",
    ] {
        assert!(
            finding.message.contains(want),
            "missing {want:?} in {}",
            finding.message
        );
    }

    std::fs::write(&ledger, "garbage\n").expect("write");
    let finding = classifier_spend_finding(&spend_over(&ledger));
    assert_eq!(finding.severity, Severity::Warn);
}
