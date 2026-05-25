use super::*;

#[test]
fn validate_cap_accepts_min() {
    assert!(validate_cap("k", MIN_CAP).is_ok());
}

#[test]
fn validate_cap_accepts_max() {
    assert!(validate_cap("k", MAX_CAP).is_ok());
}

#[test]
fn validate_cap_rejects_zero() {
    let err = validate_cap("max-concurrent-traces", 0).expect_err("zero must fail");
    let msg = format!("{err}");
    assert!(msg.contains("max-concurrent-traces"));
    assert!(msg.contains("0"));
}

#[test]
fn validate_cap_rejects_above_max() {
    let err = validate_cap("max-concurrent-heavy-traces", MAX_CAP + 1).expect_err("oversize must fail");
    let msg = format!("{err}");
    assert!(msg.contains("max-concurrent-heavy-traces"));
}

mod canonical_assets {
    use super::super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: intentional env mutation for path-resolution tests.
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: restore env to avoid leaking state.
            unsafe {
                match self.original.take() {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn write_minimal_assets(sb_root: &std::path::Path) {
        std::fs::create_dir_all(sb_root).expect("create");
        std::fs::write(sb_root.join("canonical-tags.yml"), "max-per-note: 7\ntags: {}\n")
            .expect("write canonical-tags");
        std::fs::write(sb_root.join("tag-mapping.yml"), "{}\n").expect("write tag-mapping");
        std::fs::create_dir_all(sb_root.join("patterns")).expect("create patterns");
    }

    #[test]
    fn errors_when_canonical_tags_missing() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

        let err = validate_canonical_assets().expect_err("should error");
        let msg = format!("{err:#}");
        assert!(msg.contains("sb bootstrap"), "should mention sb bootstrap: {msg}");
        assert!(msg.contains("canonical-tags"), "should mention canonical-tags: {msg}");
    }

    #[test]
    fn errors_when_patterns_dir_missing() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

        let sb_root = tmp.path().join("sb");
        std::fs::create_dir_all(&sb_root).expect("create");
        std::fs::write(sb_root.join("canonical-tags.yml"), "max-per-note: 7\ntags: {}\n")
            .expect("write canonical-tags");
        std::fs::write(sb_root.join("tag-mapping.yml"), "{}\n").expect("write tag-mapping");
        // Note: NOT creating patterns dir.

        let err = validate_canonical_assets().expect_err("should error");
        let msg = format!("{err:#}");
        assert!(msg.contains("sb bootstrap"), "should mention sb bootstrap: {msg}");
        assert!(msg.contains("patterns"), "should mention patterns: {msg}");
    }

    #[test]
    fn errors_when_canonical_tags_malformed() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

        let sb_root = tmp.path().join("sb");
        std::fs::create_dir_all(&sb_root).expect("create");
        std::fs::write(sb_root.join("canonical-tags.yml"), "not: [valid: yaml: {{").expect("garbage");
        std::fs::write(sb_root.join("tag-mapping.yml"), "{}\n").expect("tag-mapping");
        std::fs::create_dir_all(sb_root.join("patterns")).expect("patterns");

        let err = validate_canonical_assets().expect_err("parse error must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("--force"), "parse error should suggest --force: {msg}");
    }

    #[test]
    fn ok_when_all_assets_present_and_valid() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

        let sb_root = tmp.path().join("sb");
        write_minimal_assets(&sb_root);

        validate_canonical_assets().expect("should succeed");
    }
}
