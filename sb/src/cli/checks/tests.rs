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
        assert_eq!(errors.len(), 3, "all three shared YAMLs should be missing");
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
        assert_eq!(findings.len(), 1, "single Ok finding when all match");
        assert_eq!(findings[0].severity, Severity::Ok);
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
