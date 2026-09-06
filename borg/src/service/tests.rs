use super::*;
use crate::config::{Config, EnvBootstrapConfig};

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
    let unit = render_systemd_unit("/home/tester/.cargo/bin/sb", home, vault, data, &cfg());

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
    let unit = render_systemd_unit("/home/tester/.cargo/bin/sb", home, vault, data, &config);

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
    let unit = render_systemd_unit("/home/tester/.cargo/bin/sb", home, vault, data, &cfg());
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
    config.daemon.env_bootstrap = Some(EnvBootstrapConfig {
        command: "manifest age decrypt ~/repos/scottidler/keep/.secrets -f env".to_string(),
        env_file: PathBuf::from("/run/user/1000/borg.env"),
    });

    let home = Path::new("/home/tester");
    let vault = Path::new("/home/tester/repos/scottidler/obsidian");
    let data = Path::new("/home/tester/.local/share/sb");
    let unit = render_systemd_unit("/home/tester/.cargo/bin/sb", home, vault, data, &config);

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
    let unit = render_systemd_unit("/home/tester/.cargo/bin/sb", home, vault, data, &cfg());

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
