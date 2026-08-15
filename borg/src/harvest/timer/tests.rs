use super::*;
use crate::config::{Config, EnvBootstrapConfig};
use std::path::PathBuf;

fn cfg(schedule: &str) -> Config {
    let mut c = Config::default();
    c.harvest.schedule = schedule.to_string();
    c
}

#[test]
fn service_uses_absolute_binary_and_explicit_path() {
    // The stripped-timer-PATH criterion: an absolute ExecStart binary PLUS an
    // explicit `Environment="PATH=..."` means the unit resolves even with an
    // empty inherited PATH (a systemd timer's environment).
    let home = PathBuf::from("/home/tester");
    let binary = PathBuf::from("/home/tester/.cargo/bin/sb");
    let (service, _timer) = render_units(&home, &binary, &cfg("daily"));
    // Asserts the binary, not the whole argv: `--config` sits between `borg` and
    // `harvest` whenever a config file exists, so pinning the full string here
    // made this test env-dependent.
    assert!(
        service.contains("ExecStart=/home/tester/.cargo/bin/sb borg"),
        "ExecStart must use the absolute binary path, not a bare `sb`:\n{service}"
    );
    assert!(
        service.contains(" harvest\n"),
        "ExecStart must invoke the harvest subcommand:\n{service}"
    );
    assert!(
        service.contains("Environment=\"PATH="),
        "service must set an explicit PATH so it runs with an empty inherited env"
    );
    assert!(service.contains("Type=oneshot"), "harvest is a batch job, not a daemon");
}

#[test]
fn timer_bakes_only_oncalendar_from_config() {
    let home = PathBuf::from("/home/tester");
    let binary = PathBuf::from("/home/tester/.cargo/bin/sb");
    let (_service, timer) = render_units(&home, &binary, &cfg("*-*-* 04:30:00"));
    assert!(
        timer.contains("OnCalendar=*-*-* 04:30:00"),
        "OnCalendar is rendered from harvest.schedule:\n{timer}"
    );
    // No behavioral tunable is ever baked into the timer unit - every knob
    // stays in borg.yml, read by the service's ExecStart at fire time.
    for baked in [
        "min-msgs",
        "min_msgs",
        "token",
        "mode",
        "ExecStart",
        "--since",
        "--limit",
        "clyde",
    ] {
        assert!(
            !timer.contains(baked),
            "the timer unit must not bake in `{baked}`:\n{timer}"
        );
    }
}

/// Phase 5 (2026-07-20 harvest-completion): with no `harvest.env_bootstrap`
/// configured, `sb-harvest.service` must omit BOTH the `ExecStartPre` decrypt
/// AND the `EnvironmentFile` directive - a host with nothing to bootstrap
/// still gets a valid, complete unit, never a fabricated one.
#[test]
fn service_omits_env_bootstrap_when_unconfigured() {
    let home = PathBuf::from("/home/tester");
    let binary = PathBuf::from("/home/tester/.cargo/bin/sb");
    let (service, _timer) = render_units(&home, &binary, &cfg("daily"));
    assert!(
        !service.contains("ExecStartPre"),
        "no env-bootstrap configured, so ExecStartPre must be absent:\n{service}"
    );
    assert!(
        !service.contains("EnvironmentFile"),
        "no env-bootstrap configured, so EnvironmentFile must be absent:\n{service}"
    );
}

/// The critical Phase 5 fix: a configured `harvest.env_bootstrap` MUST reach
/// the generated `sb-harvest.service` as the same `ExecStartPre` decrypt +
/// `EnvironmentFile` pair the borg/cortex daemon units already emit. Without
/// this, the nightly timer fires with no decrypted secrets -> no
/// ANTHROPIC_API_KEY -> fabric distillation fails -> every note lands
/// degraded (the exact dead-on-arrival bug class this doc exists to kill).
#[test]
fn service_carries_env_bootstrap_when_configured() {
    let mut config = cfg("daily");
    config.harvest.env_bootstrap = Some(EnvBootstrapConfig {
        command: "manifest age decrypt ~/repos/scottidler/keep/.secrets -f env".to_string(),
        env_file: PathBuf::from("/run/user/1000/sb-harvest.env"),
    });

    let home = PathBuf::from("/home/tester");
    let binary = PathBuf::from("/home/tester/.cargo/bin/sb");
    let (service, _timer) = render_units(&home, &binary, &config);

    assert!(
        service.contains(
            "ExecStartPre=/bin/sh -c 'manifest age decrypt ~/repos/scottidler/keep/.secrets -f env > /run/user/1000/sb-harvest.env'"
        ),
        "missing secret ExecStartPre:\n{service}"
    );
    assert!(
        service.contains("EnvironmentFile=-/run/user/1000/sb-harvest.env"),
        "missing EnvironmentFile:\n{service}"
    );
}

/// The timer's env-file must be DISTINCT from the daemon's (`borg.env`) so a
/// one-shot harvest run never clobbers the long-running daemon's captured
/// environment.
#[test]
fn service_env_bootstrap_uses_distinct_env_file_from_daemon() {
    let mut config = cfg("daily");
    config.harvest.env_bootstrap = Some(EnvBootstrapConfig {
        command: "manifest age decrypt ~/repos/scottidler/keep/.secrets -f env".to_string(),
        env_file: PathBuf::from("/run/user/1000/sb-harvest.env"),
    });

    let home = PathBuf::from("/home/tester");
    let binary = PathBuf::from("/home/tester/.cargo/bin/sb");
    let (service, _timer) = render_units(&home, &binary, &config);

    assert!(
        !service.contains("/run/user/1000/borg.env"),
        "timer's env-file must not collide with the daemon's borg.env:\n{service}"
    );
    assert!(
        service.contains("sb-harvest.env"),
        "expected the distinct harvest env-file:\n{service}"
    );
}

/// PATH hygiene (Phase 5): fabric is mise-managed, so its shim dir must be on
/// PATH and FIRST (mise-managed tools win over stale duplicates); the retired
/// `~/go/bin` hand-built-fabric entry must be gone.
#[test]
fn service_path_includes_mise_shims_and_excludes_go_bin() {
    let home = PathBuf::from("/home/tester");
    let binary = PathBuf::from("/home/tester/.cargo/bin/sb");
    let (service, _timer) = render_units(&home, &binary, &cfg("daily"));
    assert!(
        service.contains("/home/tester/.local/share/mise/shims"),
        "PATH must include the mise shims dir:\n{service}"
    );
    assert!(
        !service.contains("/home/tester/go/bin"),
        "PATH must not carry the retired ~/go/bin entry:\n{service}"
    );
    let path_line = service
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

#[test]
fn schedule_change_is_the_only_timer_difference() {
    // Two configs differing only by schedule produce timers differing only in
    // the OnCalendar line - proof the cadence is the sole timer-resident knob.
    let home = PathBuf::from("/home/tester");
    let binary = PathBuf::from("/home/tester/.cargo/bin/sb");
    let (_s1, t1) = render_units(&home, &binary, &cfg("daily"));
    let (_s2, t2) = render_units(&home, &binary, &cfg("weekly"));
    let diff1: Vec<&str> = t1.lines().filter(|l| !t2.contains(*l)).collect();
    let diff2: Vec<&str> = t2.lines().filter(|l| !t1.contains(*l)).collect();
    assert_eq!(diff1, vec!["OnCalendar=daily"]);
    assert_eq!(diff2, vec!["OnCalendar=weekly"]);
}

/// Serializes the env mutation the config-flag tests need.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn config_flag_goes_before_the_subcommand_not_after() {
    // Regression: the unit shipped `borg harvest --config <path>`, but --config
    // is a flag on `sb borg`, not on `harvest`. Every scheduled run died with
    // `error: unexpected argument '--config' found` (exit 2). It went unnoticed
    // for weeks because the timer had never fired on the daemon host.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("sb")).expect("mkdir sb");
    std::fs::write(tmp.path().join("sb").join("borg.yml"), "").expect("write borg.yml");

    let prev = std::env::var_os("XDG_CONFIG_HOME");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    let (service, _timer) = render_units(
        &PathBuf::from("/home/tester"),
        &PathBuf::from("/home/tester/.cargo/bin/sb"),
        &cfg("daily"),
    );
    match prev {
        Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }

    let exec = service
        .lines()
        .find(|l| l.starts_with("ExecStart="))
        .expect("unit has an ExecStart");
    assert!(
        exec.contains("--config"),
        "test is vacuous unless the config file was actually found:\n{exec}"
    );
    assert!(
        !exec.contains("harvest --config"),
        "--config is a `sb borg` flag, not a `harvest` flag:\n{exec}"
    );
    let config_at = exec.find("--config").expect("--config present");
    let harvest_at = exec.rfind(" harvest").expect("subcommand present");
    assert!(config_at < harvest_at, "--config must precede the subcommand:\n{exec}");
}
