use super::*;
use std::path::PathBuf;
use vault::systemd::EnvBootstrap;

fn bootstrap() -> EnvBootstrap {
    EnvBootstrap {
        command: "manifest secrets env -f env".to_string(),
        env_file: PathBuf::from("/run/user/1000/sb-eval.env"),
    }
}

/// A config that names no judge keys (`Config::default()` names the default
/// Anthropic var, which would demand a bootstrap).
fn keyless() -> Config {
    let mut config = Config::default();
    config.llm.api_keys = vault::fabric::ApiKeys::default();
    config
}

fn units(config: &Config) -> Result<(String, String)> {
    render_units(
        &PathBuf::from("/home/tester"),
        &PathBuf::from("/home/tester/.cargo/bin/sb"),
        Some(&PathBuf::from("/home/tester/.config/sb/borg.yml")),
        config,
    )
}

#[test]
fn service_runs_eval_unscored_with_config_before_the_subcommand() {
    let (service, _) = units(&keyless()).unwrap();
    assert!(
        service.contains(
            "\nExecStart=/home/tester/.cargo/bin/sb borg --config /home/tester/.config/sb/borg.yml eval --unscored\n"
        ),
        "{service}"
    );
    assert!(service.contains("Type=oneshot"));
    assert!(service.contains("Environment=\"PATH="));
    assert!(
        !service.contains("[Install]"),
        "installed through the timer:\n{service}"
    );
}

#[test]
fn timer_bakes_only_oncalendar_from_eval_schedule() {
    let mut config = keyless();
    config.eval.schedule = "*-*-* 06:15:00".to_string();
    let (_, timer) = units(&config).unwrap();
    assert!(timer.contains("OnCalendar=*-*-* 06:15:00"), "{timer}");
    assert!(timer.contains("Persistent=true"));
    assert!(timer.contains("WantedBy=timers.target"));
    for baked in ["ExecStart", "--limit", "judge", "unscored"] {
        assert!(!timer.contains(baked), "timer must not bake `{baked}`:\n{timer}");
    }
}

#[test]
fn env_bootstrap_reaches_the_service_with_its_own_env_file() {
    let mut config = keyless();
    config.eval.env_bootstrap = Some(bootstrap());
    let (service, _) = units(&config).unwrap();
    assert!(
        service.contains("ExecStartPre=/bin/sh -c 'manifest secrets env -f env > /run/user/1000/sb-eval.env'"),
        "{service}"
    );
    assert!(
        service.contains("EnvironmentFile=-/run/user/1000/sb-eval.env"),
        "{service}"
    );
}

#[test]
fn no_keys_and_no_bootstrap_still_renders_a_valid_unit() {
    let (service, _) = units(&keyless()).unwrap();
    assert!(!service.contains("ExecStartPre"));
    assert!(!service.contains("EnvironmentFile"));
}

#[test]
fn named_api_keys_without_an_env_bootstrap_refuse_to_render() {
    let config: Config = serde_yaml::from_str("llm:\n  api-keys:\n    gemini: GEMINI_API_KEY\n").unwrap();
    let err = units(&config).unwrap_err().to_string();
    assert!(err.contains("GEMINI_API_KEY"), "{err}");
    assert!(err.contains("eval.env-bootstrap"), "{err}");

    let mut with = config;
    with.eval.env_bootstrap = Some(bootstrap());
    assert!(units(&with).is_ok(), "the same map renders once a bootstrap exists");
}

#[test]
fn eval_block_parses_and_rejects_unknown_keys() {
    let config: Config = serde_yaml::from_str(
        "eval:\n  schedule: daily\n  limit: 25\n  judge-models: [gemini-2.5-pro, claude-sonnet-5]\n  env-bootstrap:\n    command: c\n    env-file: /run/user/1000/sb-eval.env\n",
    )
    .unwrap();
    assert_eq!(config.eval.limit, Some(25));
    assert_eq!(config.eval.judge_models, vec!["gemini-2.5-pro", "claude-sonnet-5"]);
    assert!(serde_yaml::from_str::<Config>("eval:\n  lmit: 5\n").is_err());
}

#[test]
fn bootstrap_enables_the_timer_now_after_a_daemon_reload() {
    let [reload, enable] = enable_commands();
    assert_eq!(reload, ["--user", "daemon-reload"]);
    assert_eq!(enable, ["--user", "enable", "--now", "sb-eval.timer"]);
}
