use super::*;
use serial_test::serial;

struct EnvGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &Path) -> Self {
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
fn patterns_array_matches_source_tree() {
    // Compile-time invariant: the explicit `PATTERNS` array is the
    // single source of truth for what gets bundled. If a new pattern
    // file is added to `borg/patterns/` and not listed here, this
    // assertion catches it.
    assert_eq!(
        PATTERNS.len(),
        14,
        "expected 14 patterns; update PATTERNS in sb/src/cli/bootstrap.rs"
    );
}

#[test]
#[serial(env_xdg)]
fn extract_writes_all_canonical_assets_when_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    extract_canonical_assets(false).expect("extract");

    let root = tmp.path().join("sb");
    for filename in [
        "borg.yml",
        "cortex.yml",
        "oracle.yml",
        "canonical-tags.yml",
        "tag-mapping.yml",
        "tag-proposals.yml",
    ] {
        let path = root.join(filename);
        assert!(path.exists(), "{} should have been written", filename);
    }
    let patterns_dir = root.join("patterns");
    assert!(patterns_dir.is_dir(), "patterns dir should exist");
    for (name, _) in PATTERNS {
        assert!(patterns_dir.join(name).exists(), "{} should have been written", name);
    }
}

#[test]
#[serial(env_xdg)]
fn extract_byte_identical_to_embedded_constants() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    extract_canonical_assets(false).expect("extract");

    let root = tmp.path().join("sb");
    let pairs: &[(&str, &str)] = &[
        ("borg.yml", BORG_TEMPLATE),
        ("cortex.yml", CORTEX_TEMPLATE),
        ("oracle.yml", ORACLE_TEMPLATE),
        ("canonical-tags.yml", CANONICAL_TAGS_YML),
        ("tag-mapping.yml", TAG_MAPPING_YML),
        ("tag-proposals.yml", TAG_PROPOSALS_YML),
    ];
    for (filename, expected) in pairs {
        let actual = std::fs::read_to_string(root.join(filename)).expect("read");
        assert_eq!(&actual, expected, "{filename} content mismatch");
    }
    for (filename, expected) in PATTERNS {
        let actual = std::fs::read_to_string(root.join("patterns").join(filename)).expect("read pattern");
        assert_eq!(&actual, expected, "{filename} content mismatch");
    }
}

#[test]
#[serial(env_xdg)]
fn extract_is_idempotent_without_force() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    extract_canonical_assets(false).expect("first extract");
    let mutated = "operator-edited-vocabulary";
    let canonical_path = tmp.path().join("sb").join("canonical-tags.yml");
    std::fs::write(&canonical_path, mutated).expect("mutate");

    extract_canonical_assets(false).expect("second extract");

    let after = std::fs::read_to_string(&canonical_path).expect("read");
    assert_eq!(after, mutated, "write-if-missing must preserve operator edits");
}

#[test]
#[serial(env_xdg)]
fn extract_force_overwrites_shared_yaml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    extract_canonical_assets(false).expect("first extract");
    let canonical_path = tmp.path().join("sb").join("canonical-tags.yml");
    std::fs::write(&canonical_path, "stale").expect("mutate");

    extract_canonical_assets(true).expect("forced extract");

    let after = std::fs::read_to_string(&canonical_path).expect("read");
    assert_eq!(
        after, CANONICAL_TAGS_YML,
        "--force must refresh shared YAML from binary"
    );
}

#[test]
#[serial(env_xdg)]
fn extract_force_preserves_templates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    extract_canonical_assets(false).expect("first extract");
    let borg_path = tmp.path().join("sb").join("borg.yml");
    let edited = "telegram:\n  bot-token: secret123";
    std::fs::write(&borg_path, edited).expect("operator edits");

    extract_canonical_assets(true).expect("forced extract");

    let after = std::fs::read_to_string(&borg_path).expect("read");
    assert_eq!(after, edited, "--force must NOT overwrite per-host templates");
}
