use super::*;

fn service_config_with(yaml: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("cortex.yml");
    std::fs::write(&path, yaml).expect("write config");
    (dir, path)
}

#[test]
fn install_guard_refuses_100s_against_default_120s_timeouts_naming_both_keys() {
    let (_dir, path) = service_config_with("daemon:\n  stop-timeout-secs: 100\n");
    let err = ensure_stop_budget(&path).expect_err("100 < 120 + 30 must refuse");
    let text = format!("{err}");
    assert!(text.contains("stop-timeout-secs"), "{text}");
    assert!(text.contains("fabric.timeout-secs"), "{text}");
}

#[test]
fn install_guard_reads_the_file_it_is_given() {
    let (_dir, tight) = service_config_with("daemon:\n  stop-timeout-secs: 100\n");
    let (_dir2, roomy) = service_config_with("daemon:\n  stop-timeout-secs: 400\n");
    assert!(ensure_stop_budget(&tight).is_err());
    assert!(ensure_stop_budget(&roomy).is_ok());
}

#[test]
fn install_guard_passes_the_default_and_a_missing_config() {
    let (_dir, path) = service_config_with("log-level: info\n");
    assert!(ensure_stop_budget(&path).is_ok());
    assert!(ensure_stop_budget(&path.with_file_name("absent.yml")).is_ok());
}

#[test]
fn install_guard_errors_on_an_unparseable_config() {
    let (_dir, path) = service_config_with("daemon: [not, a, map]\n");
    assert!(ensure_stop_budget(&path).is_err());
}

/// The audit repro: the service's cortex.yml says 180, the install ran with
/// `--config short.yml` saying 100. The guard checks cortex.yml, so the unit
/// must carry cortex.yml's 180, not the run's 100.
#[test]
fn installed_timeout_comes_from_the_config_the_guard_validated() {
    let (_dir, service) = service_config_with("daemon:\n  stop-timeout-secs: 180\n");
    let (_short_dir, short_path) = service_config_with("daemon:\n  stop-timeout-secs: 100\n");
    let short = Config::load_service_config(&short_path).expect("short config");
    assert_eq!(short.daemon.stop_timeout_secs, 100);
    let dir = tempfile::tempdir().expect("tmpdir");

    ensure_stop_budget(&service).expect("the service config holds the budget");
    let unit =
        crate::daemon::service_unit(dir.path(), &short, dir.path(), dir.path(), dir.path(), &service).expect("render");

    assert!(unit.contains("\nTimeoutStopSec=180\n"), "{unit}");
    assert!(!unit.contains("TimeoutStopSec=100"), "{unit}");
    assert!(
        unit.contains(&format!("--config {}", service.display())),
        "the unit loads the file whose timeout it carries:\n{unit}"
    );
}
