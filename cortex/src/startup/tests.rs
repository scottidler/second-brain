use super::*;

// Suite-wide lock (not a private-to-this-file one): every test anywhere in
// this crate's test binary that mutates `XDG_CONFIG_HOME`, or resolves it
// indirectly via `validate_canonical_assets`, acquires the SAME
// `crate::testutil::ENV_LOCK` before touching the env var. See that static's
// doc comment for the race this closes (2026-07-05
// cortex-daemon-oscillation-loop design doc, Phase 7).
use crate::testutil::EnvGuard;

fn write_minimal_canonical_assets(root: &std::path::Path) {
    std::fs::create_dir_all(root).expect("create dir");
    std::fs::write(root.join("canonical-tags.yml"), "max-per-note: 7\ntags: {}\n").expect("write canonical-tags");
    std::fs::write(root.join("tag-mapping.yml"), "{}\n").expect("write tag-mapping");
}

#[test]
fn errors_when_canonical_tags_missing() {
    let _lock = crate::testutil::lock_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    let err = validate_canonical_assets().expect_err("should error");
    let msg = format!("{err:#}");
    assert!(msg.contains("sb bootstrap"), "error should mention sb bootstrap: {msg}");
    assert!(
        msg.contains("canonical-tags"),
        "error should mention the missing path: {msg}"
    );
}

#[test]
fn errors_when_tag_mapping_missing() {
    let _lock = crate::testutil::lock_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    let sb_root = tmp.path().join("sb");
    std::fs::create_dir_all(&sb_root).expect("create");
    std::fs::write(sb_root.join("canonical-tags.yml"), "max-per-note: 7\ntags: {}\n").expect("write canonical-tags");

    let err = validate_canonical_assets().expect_err("should error");
    let msg = format!("{err:#}");
    assert!(msg.contains("sb bootstrap"), "error should mention sb bootstrap: {msg}");
    assert!(msg.contains("tag-mapping"), "error should mention tag-mapping: {msg}");
}

#[test]
fn errors_when_canonical_tags_malformed() {
    let _lock = crate::testutil::lock_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    let sb_root = tmp.path().join("sb");
    std::fs::create_dir_all(&sb_root).expect("create");
    std::fs::write(sb_root.join("canonical-tags.yml"), "not: [valid: yaml: {{").expect("write garbage");
    std::fs::write(sb_root.join("tag-mapping.yml"), "{}\n").expect("write tag-mapping");

    let err = validate_canonical_assets().expect_err("should error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("sb bootstrap --force") || msg.contains("--force"),
        "parse error should suggest --force: {msg}"
    );
}

#[test]
fn ok_when_canonical_assets_present_and_valid() {
    let _lock = crate::testutil::lock_env();
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    let sb_root = tmp.path().join("sb");
    write_minimal_canonical_assets(&sb_root);

    validate_canonical_assets().expect("should succeed");
}
