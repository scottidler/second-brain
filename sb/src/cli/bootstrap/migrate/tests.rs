#![allow(clippy::unwrap_used)]

use super::*;
use serial_test::serial;
use tempfile::TempDir;

/// Restores the previous value (or absence) of an env var on drop. Used to
/// override `XDG_CONFIG_HOME` for `prune_legacy`, which resolves the legacy
/// config root through `vault::paths::xdg_config_dir()`. Env vars are
/// process-global, so every test using this guard is tagged
/// `#[serial(env_xdg)]`, the same key `sb/src/cli/bootstrap/tests.rs` uses,
/// so the two test modules never race each other over the same var.
struct EnvGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &std::path::Path) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: env mutation is intentional for testing path resolution.
        unsafe { std::env::set_var(key, value) };
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: restoring env to avoid leaking state.
        unsafe {
            match self.original.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

#[test]
fn rewrite_replaces_legacy_second_brain_paths() {
    let input = b"canonical-path: ~/.config/second-brain/canonical-tags.yml\nmapping-path: ~/.config/second-brain/tag-mapping.yml\n";
    let src = std::path::PathBuf::from("/tmp/borg.yml");
    let out = rewrite_legacy_paths_in_yaml(&src, input);
    let out_str = std::str::from_utf8(&out).unwrap();
    assert!(out_str.contains("~/.config/sb/canonical-tags.yml"));
    assert!(out_str.contains("~/.config/sb/tag-mapping.yml"));
    assert!(!out_str.contains("~/.config/second-brain/"));
}

#[test]
fn rewrite_leaves_custom_paths_untouched() {
    let input = b"canonical-path: /custom/path/tags.yml\n";
    let src = std::path::PathBuf::from("/tmp/borg.yml");
    let out = rewrite_legacy_paths_in_yaml(&src, input);
    let out_str = std::str::from_utf8(&out).unwrap();
    assert!(out_str.contains("/custom/path/tags.yml"));
}

#[test]
fn copy_file_migrates_when_dst_missing() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("borg-legacy.yml");
    let dst = tmp.path().join("borg-new.yml");
    std::fs::write(&src, "log-level: info\n").unwrap();

    let mut report = Report::default();
    copy_file_with_rewrite(&src, &dst, &mut report).unwrap();

    assert!(dst.exists());
    assert!(!report.had_conflicts);
    assert!(report.lines.iter().any(|l| l.contains("migrated")));
}

#[test]
fn copy_file_noops_when_dst_matches() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("borg-legacy.yml");
    let dst = tmp.path().join("borg-new.yml");
    let content = b"log-level: info\n";
    std::fs::write(&src, content).unwrap();
    std::fs::write(&dst, content).unwrap();

    let mut report = Report::default();
    copy_file_with_rewrite(&src, &dst, &mut report).unwrap();

    assert!(!report.had_conflicts);
    assert!(report.lines.iter().any(|l| l.contains("noop")));
}

#[test]
fn copy_file_conflicts_when_dst_differs() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("borg-legacy.yml");
    let dst = tmp.path().join("borg-new.yml");
    std::fs::write(&src, b"log-level: info\n").unwrap();
    std::fs::write(&dst, b"log-level: warn\n").unwrap();

    let mut report = Report::default();
    copy_file_with_rewrite(&src, &dst, &mut report).unwrap();

    assert!(report.had_conflicts);
    assert!(report.lines.iter().any(|l| l.contains("conflict")));
}

#[test]
#[serial(env_xdg)]
fn prune_legacy_dry_run_lists_then_apply_removes_known_dir() {
    let tmp = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    let borg_dir = tmp.path().join("borg");
    std::fs::create_dir_all(borg_dir.join("patterns")).unwrap();
    std::fs::write(borg_dir.join("borg.yml"), "vault: {}\n").unwrap();
    std::fs::write(borg_dir.join("patterns").join("condense.md"), "# pattern\n").unwrap();
    std::fs::write(borg_dir.join(MARKER), "sb bootstrap migrated this directory\n").unwrap();

    let dry_run = prune_legacy(false).unwrap();
    assert!(!dry_run.had_conflicts);
    assert!(
        dry_run
            .lines
            .iter()
            .any(|l| l.contains("would remove") && l.contains("borg")),
        "unexpected lines: {:?}",
        dry_run.lines
    );
    assert!(borg_dir.exists(), "dry run must never delete");

    let applied = prune_legacy(true).unwrap();
    assert!(!applied.had_conflicts);
    assert!(
        applied
            .lines
            .iter()
            .any(|l| l.contains("removed") && l.contains("borg")),
        "unexpected lines: {:?}",
        applied.lines
    );
    assert!(!borg_dir.exists(), "apply must delete the legacy dir");
}

#[test]
#[serial(env_xdg)]
fn prune_legacy_refuses_dir_with_stranger_file() {
    let tmp = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    let cortex_dir = tmp.path().join("cortex");
    std::fs::create_dir_all(&cortex_dir).unwrap();
    std::fs::write(cortex_dir.join("cortex.yml"), "vault: {}\n").unwrap();
    std::fs::write(cortex_dir.join("private-notes.txt"), "do not delete me\n").unwrap();
    std::fs::write(cortex_dir.join(MARKER), "sb bootstrap migrated this directory\n").unwrap();

    let report = prune_legacy(true).unwrap();

    assert!(report.had_conflicts);
    assert!(
        report
            .lines
            .iter()
            .any(|l| l.contains("refused") && l.contains("private-notes.txt")),
        "unexpected lines: {:?}",
        report.lines
    );
    assert!(cortex_dir.exists(), "refusal must not delete the directory");
    assert!(cortex_dir.join("private-notes.txt").exists());
}

#[test]
#[serial(env_xdg)]
fn prune_legacy_refuses_dir_without_marker() {
    let tmp = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    let oracle_dir = tmp.path().join("oracle");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::write(oracle_dir.join("oracle.yml"), "vault: {}\n").unwrap();

    let report = prune_legacy(true).unwrap();

    assert!(report.had_conflicts);
    assert!(
        report
            .lines
            .iter()
            .any(|l| l.contains("refused") && l.contains("no .migrated-to-sb marker")),
        "unexpected lines: {:?}",
        report.lines
    );
    assert!(oracle_dir.exists(), "no marker must leave the directory untouched");
    assert!(oracle_dir.join("oracle.yml").exists());
}

#[test]
#[serial(env_xdg)]
fn prune_legacy_is_silent_when_dir_absent() {
    let tmp = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());
    // No legacy dirs created at all under this fresh XDG_CONFIG_HOME.

    let report = prune_legacy(false).unwrap();

    assert!(!report.had_conflicts);
    assert!(report.lines.is_empty(), "unexpected lines: {:?}", report.lines);
}
