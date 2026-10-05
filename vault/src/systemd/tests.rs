use super::*;

fn strict_unit() -> ServiceUnit {
    ServiceUnit {
        description: "demo daemon".to_string(),
        after: vec!["network-online.target".to_string()],
        wants: vec!["network-online.target".to_string()],
        start_limit: Some(StartLimit {
            burst: 5,
            interval_sec: 60,
        }),
        unit_type: UnitType::Simple,
        env_bootstrap: None,
        home: PathBuf::from("/home/tester"),
        path_comment: None,
        extra_env: Vec::new(),
        exec_start: vec!["/bin/demo".to_string(), "--start".to_string()],
        restart: Some(Restart {
            policy: RestartPolicy::Always,
            sec: 5,
        }),
        hardening: Hardening::Strict {
            rw_paths: vec![PathBuf::from("/a"), PathBuf::from("/b")],
        },
        wanted_by: Some("default.target".to_string()),
    }
}

#[test]
fn render_service_strict_full_text() {
    let expected = "[Unit]\n\
                    Description=demo daemon\n\
                    After=network-online.target\n\
                    Wants=network-online.target\n\
                    StartLimitBurst=5\n\
                    StartLimitIntervalSec=60\n\
                    \n\
                    [Service]\n\
                    Type=simple\n\
                    Environment=\"PATH=/home/tester/.local/share/mise/shims:/home/tester/.local/bin:/home/tester/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\"\n\
                    ExecStart=/bin/demo --start\n\
                    Restart=always\n\
                    RestartSec=5\n\
                    WorkingDirectory=/home/tester\n\
                    \n\
                    # Hardening\n\
                    NoNewPrivileges=true\n\
                    ProtectSystem=strict\n\
                    ProtectHome=read-only\n\
                    ReadWritePaths=/a /b\n\
                    PrivateTmp=true\n\
                    \n\
                    [Install]\n\
                    WantedBy=default.target\n";
    assert_eq!(render_service(&strict_unit()), expected);
}

#[test]
fn render_service_env_bootstrap_and_extra_env_in_order() {
    let mut unit = strict_unit();
    unit.env_bootstrap = Some(EnvBootstrap {
        command: "decrypt -f env".to_string(),
        env_file: PathBuf::from("/run/user/1000/demo.env"),
    });
    unit.path_comment = Some("first line\nsecond line".to_string());
    unit.extra_env = vec![EnvVar {
        comment: Some("cap threads".to_string()),
        name: "RAYON_NUM_THREADS".to_string(),
        value: "8".to_string(),
    }];
    let text = render_service(&unit);
    let service: Vec<&str> = text
        .lines()
        .skip_while(|l| *l != "Type=simple")
        .take_while(|l| !l.starts_with("ExecStart="))
        .collect();
    assert_eq!(
        service,
        vec![
            "Type=simple",
            "ExecStartPre=/bin/sh -c 'decrypt -f env > /run/user/1000/demo.env'",
            "EnvironmentFile=-/run/user/1000/demo.env",
            "# first line",
            "# second line",
            "Environment=\"PATH=/home/tester/.local/share/mise/shims:/home/tester/.local/bin:/home/tester/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\"",
            "# cap threads",
            "Environment=\"RAYON_NUM_THREADS=8\"",
        ]
    );
}

#[test]
fn render_service_minimal_hardening_no_restart_no_install() {
    let mut unit = strict_unit();
    unit.wants.clear();
    unit.start_limit = None;
    unit.unit_type = UnitType::Oneshot;
    unit.restart = None;
    unit.hardening = Hardening::Minimal {
        why: "needs to write home".to_string(),
    };
    unit.wanted_by = None;
    let text = render_service(&unit);
    assert!(text.contains("Type=oneshot\n"), "{text}");
    assert!(!text.contains("Wants="), "{text}");
    assert!(!text.contains("StartLimit"), "{text}");
    assert!(!text.contains("Restart"), "{text}");
    assert!(!text.contains("ProtectSystem"), "{text}");
    assert!(!text.contains("ReadWritePaths"), "{text}");
    assert!(!text.contains("[Install]"), "{text}");
    assert!(
        text.ends_with("\n# Hardening (needs to write home).\nNoNewPrivileges=true\nPrivateTmp=true\n"),
        "{text}"
    );
}

#[test]
fn render_service_restart_on_failure() {
    let mut unit = strict_unit();
    unit.restart = Some(Restart {
        policy: RestartPolicy::OnFailure,
        sec: 7,
    });
    let text = render_service(&unit);
    assert!(text.contains("\nRestart=on-failure\nRestartSec=7\n"), "{text}");
    assert!(!text.contains("Restart=always"), "{text}");
}

#[test]
fn render_timer_full_text() {
    let timer = TimerUnit {
        description: "demo timer".to_string(),
        on_calendar: "daily".to_string(),
        persistent: true,
        wanted_by: "timers.target".to_string(),
    };
    assert_eq!(
        render_timer(&timer),
        "[Unit]\nDescription=demo timer\n\n[Timer]\nOnCalendar=daily\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n"
    );
}

#[test]
fn render_timer_not_persistent_omits_line() {
    let timer = TimerUnit {
        description: "demo timer".to_string(),
        on_calendar: "daily".to_string(),
        persistent: false,
        wanted_by: "timers.target".to_string(),
    };
    assert!(!render_timer(&timer).contains("Persistent"));
}

#[test]
fn unit_path_puts_mise_shims_first() {
    let path = unit_path(Path::new("/h"));
    assert!(path.starts_with("/h/.local/share/mise/shims:/h/.local/bin:"), "{path}");
    assert!(!path.contains("/h/go/bin"), "{path}");
}

/// The shapes borg.yml (`daemon.env-bootstrap`, `harvest.env-bootstrap`) and
/// cortex.yml (`daemon.env-bootstrap`) carry: kebab-case keys, `env-file`
/// tilde-expanded at load.
#[test]
fn env_bootstrap_deserializes_config_form() {
    let yaml = "command: manifest age decrypt ~/keep/.secrets -f env\nenv-file: /run/user/1000/borg.env\n";
    let parsed: EnvBootstrap = serde_yaml::from_str(yaml).expect("deserialize");
    assert_eq!(
        parsed,
        EnvBootstrap {
            command: "manifest age decrypt ~/keep/.secrets -f env".to_string(),
            env_file: PathBuf::from("/run/user/1000/borg.env"),
        }
    );

    let tilde: EnvBootstrap = serde_yaml::from_str("command: x\nenv-file: ~/demo.env\n").expect("deserialize tilde");
    assert!(
        !tilde.env_file.starts_with("~"),
        "env-file must be tilde-expanded: {tilde:?}"
    );
    assert!(tilde.env_file.ends_with("demo.env"));
}

#[test]
fn env_bootstrap_rejects_snake_case_and_missing_fields() {
    assert!(serde_yaml::from_str::<EnvBootstrap>("command: x\nenv_file: /a\n").is_err());
    assert!(serde_yaml::from_str::<EnvBootstrap>("command: x\n").is_err());
}

#[test]
fn unit_dir_under_joins_systemd_user() {
    assert_eq!(
        unit_dir_under(Some(PathBuf::from("/xdg"))).unwrap(),
        PathBuf::from("/xdg/systemd/user")
    );
}

#[test]
fn unit_dir_under_errors_when_no_xdg_config() {
    let err = unit_dir_under(None).expect_err("None must be an error, not a panic");
    assert!(err.to_string().contains("HOME or XDG_CONFIG_HOME"), "{err}");
}

#[test]
fn compare_installed_distinguishes_current_drifted_and_absent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.service");
    assert_eq!(compare_installed(&path, "abc").unwrap(), UnitState::NotInstalled);
    std::fs::write(&path, "abc").unwrap();
    assert_eq!(compare_installed(&path, "abc").unwrap(), UnitState::Current);
    assert_eq!(compare_installed(&path, "abd").unwrap(), UnitState::Drifted);
    assert_eq!(compare_installed(&path, "abc\n").unwrap(), UnitState::Drifted);
}

#[test]
fn compare_installed_errors_on_unreadable_path() {
    let dir = tempfile::tempdir().unwrap();
    // A directory is not a readable file: an error, never "current".
    assert!(compare_installed(dir.path(), "abc").is_err());
}
