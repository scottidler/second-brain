use super::*;
use crate::config::Config;
use vault::systemd::EnvBootstrap;

fn cfg() -> Config {
    Config::default()
}

/// A default config (no `log-level` set) must render `--log-level info`, and
/// must never emit the old hardcoded `--log-level debug`.
#[test]
fn render_systemd_unit_defaults_log_level_to_info() {
    let home = Path::new("/home/tester");
    let vault = Path::new("/home/tester/repos/scottidler/obsidian");
    let data = Path::new("/home/tester/.local/share/sb");
    let unit = render_systemd_unit("/home/tester/.cargo/bin/sb", home, vault, data, None, &cfg());

    assert!(
        unit.contains("--log-level info"),
        "default config must render --log-level info:\n{unit}"
    );
    assert!(
        !unit.contains("log-level debug"),
        "default config must never render log-level debug:\n{unit}"
    );
}

/// `borg.yml`'s `log-level` field must reach the rendered unit.
#[test]
fn render_systemd_unit_carries_configured_log_level() {
    let mut config = cfg();
    config.log_level = Some("debug".to_string());

    let home = Path::new("/home/tester");
    let vault = Path::new("/home/tester/repos/scottidler/obsidian");
    let data = Path::new("/home/tester/.local/share/sb");
    let unit = render_systemd_unit("/home/tester/.cargo/bin/sb", home, vault, data, None, &config);

    assert!(
        unit.contains("--log-level debug"),
        "log_level: Some(\"debug\") must render --log-level debug:\n{unit}"
    );
}

/// With no `daemon.env_bootstrap` configured, `borg.service` must omit BOTH
/// the `ExecStartPre` decrypt AND the `EnvironmentFile` directive - a host
/// with nothing to bootstrap still gets a valid, complete unit.
#[test]
fn render_systemd_unit_omits_env_bootstrap_when_unconfigured() {
    let home = Path::new("/home/tester");
    let vault = Path::new("/home/tester/repos/scottidler/obsidian");
    let data = Path::new("/home/tester/.local/share/sb");
    let unit = render_systemd_unit("/home/tester/.cargo/bin/sb", home, vault, data, None, &cfg());
    assert!(
        !unit.contains("ExecStartPre"),
        "no env-bootstrap configured, so ExecStartPre must be absent:\n{unit}"
    );
    assert!(
        !unit.contains("EnvironmentFile"),
        "no env-bootstrap configured, so EnvironmentFile must be absent:\n{unit}"
    );
}

/// A configured `daemon.env_bootstrap` must reach the generated unit as the
/// `ExecStartPre` decrypt + `EnvironmentFile` pair.
#[test]
fn render_systemd_unit_carries_env_bootstrap_when_configured() {
    let mut config = cfg();
    config.daemon.env_bootstrap = Some(EnvBootstrap {
        command: "manifest age decrypt ~/repos/scottidler/keep/.secrets -f env".to_string(),
        env_file: PathBuf::from("/run/user/1000/borg.env"),
    });

    let home = Path::new("/home/tester");
    let vault = Path::new("/home/tester/repos/scottidler/obsidian");
    let data = Path::new("/home/tester/.local/share/sb");
    let unit = render_systemd_unit("/home/tester/.cargo/bin/sb", home, vault, data, None, &config);

    assert!(
        unit.contains(
            "ExecStartPre=/bin/sh -c 'manifest age decrypt ~/repos/scottidler/keep/.secrets -f env > /run/user/1000/borg.env'"
        ),
        "missing secret ExecStartPre:\n{unit}"
    );
    assert!(
        unit.contains("EnvironmentFile=-/run/user/1000/borg.env"),
        "missing EnvironmentFile:\n{unit}"
    );
}

/// PATH hygiene (Phase 5, 2026-07-20 harvest-completion): fabric is
/// mise-managed, so its shim dir must be on PATH and FIRST; the retired
/// `~/go/bin` hand-built-fabric entry must be gone.
#[test]
fn render_systemd_unit_path_includes_mise_shims_and_excludes_go_bin() {
    let home = Path::new("/home/tester");
    let vault = Path::new("/home/tester/repos/scottidler/obsidian");
    let data = Path::new("/home/tester/.local/share/sb");
    let unit = render_systemd_unit("/home/tester/.cargo/bin/sb", home, vault, data, None, &cfg());

    assert!(
        unit.contains("/home/tester/.local/share/mise/shims"),
        "PATH must include the mise shims dir:\n{unit}"
    );
    assert!(
        !unit.contains("/home/tester/go/bin"),
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

// Byte-exact goldens (2026-10-05 quality-review-fixes): the renderer's output
// at fixed inputs, so a refactor of the unit renderers cannot change a byte
// unnoticed. `render_systemd_unit` is pure, so the inputs are just the args.

fn golden_unit(config: &Config) -> String {
    render_systemd_unit(
        "/home/tester/.cargo/bin/sb",
        Path::new("/home/tester"),
        Path::new("/home/tester/repos/scottidler/obsidian"),
        Path::new("/home/tester/.local/share/sb"),
        Some(Path::new("<XDG_CONFIG_HOME>/sb/borg.yml")),
        config,
    )
}

#[test]
fn golden_borg_service_minimal() {
    assert_eq!(golden_unit(&cfg()), include_str!("golden/minimal.service"));
}

#[test]
fn golden_borg_service_full() {
    let mut config = cfg();
    config.log_level = Some("debug".to_string());
    config.daemon.env_bootstrap = Some(EnvBootstrap {
        command: "manifest age decrypt ~/repos/scottidler/keep/.secrets -f env".to_string(),
        env_file: PathBuf::from("/run/user/1000/borg.env"),
    });
    assert_eq!(golden_unit(&config), include_str!("golden/full.service"));
}

/// borg pins `--config` like cortex and harvest, before the `daemon`
/// subcommand (it is a flag on `sb borg`).
#[test]
fn render_systemd_unit_pins_config_before_daemon() {
    let unit = golden_unit(&cfg());
    assert!(
        unit.contains(
            "ExecStart=/home/tester/.cargo/bin/sb borg --config <XDG_CONFIG_HOME>/sb/borg.yml --log-level info daemon --start\n"
        ),
        "ExecStart must carry --config <path>:\n{unit}"
    );
}

/// No config file, no `--config` flag.
#[test]
fn render_systemd_unit_omits_config_flag_when_no_config_path() {
    let unit = render_systemd_unit(
        "/home/tester/.cargo/bin/sb",
        Path::new("/home/tester"),
        Path::new("/home/tester/repos/scottidler/obsidian"),
        Path::new("/home/tester/.local/share/sb"),
        None,
        &cfg(),
    );
    assert!(!unit.contains("--config"), "no config path, no flag:\n{unit}");
}

/// The unit is written under `XDG_CONFIG_HOME`, not a hardcoded `~/.config`.
#[test]
fn write_systemd_unit_lands_under_xdg_config_home() {
    let _guard = crate::harvest::TEST_XDG_LOCK.blocking_lock();
    let xdg = tempfile::tempdir().expect("tempdir");
    let vault_dir = tempfile::tempdir().expect("vault tempdir");
    let original = std::env::var_os("XDG_CONFIG_HOME");
    // SAFETY: serialized by TEST_XDG_LOCK; restored below.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", xdg.path()) };

    let mut config = cfg();
    config.vault.root_path = Some(vault_dir.path().display().to_string());
    let written = write_systemd_unit("/home/tester/.cargo/bin/sb", &config);

    unsafe {
        match original {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
    let written = written.expect("write unit");
    assert_eq!(written, xdg.path().join("systemd/user/borg.service"));
    let text = std::fs::read_to_string(&written).expect("read unit");
    assert!(text.contains("ReadWritePaths="), "unit body written:\n{text}");
}

/// An unresolvable vault is an error, never a guessed
/// `~/repos/scottidler/obsidian` baked into ReadWritePaths.
#[test]
fn desired_systemd_unit_errors_when_vault_does_not_resolve() {
    let cwd_without_marker = std::env::current_dir().expect("cwd");
    assert!(
        !cwd_without_marker.join(".obsidian").exists(),
        "precondition: test cwd is not a vault"
    );
    let err = desired_systemd_unit("/home/tester/.cargo/bin/sb", &cfg()).expect_err("no vault configured");
    assert!(
        format!("{err:#}").contains("vault root does not resolve"),
        "error names the cause: {err:#}"
    );
}
