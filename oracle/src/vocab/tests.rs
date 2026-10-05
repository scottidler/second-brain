use super::*;
use crate::testutil::{HomeGuard, write_borg_yml, write_vocab};

#[test]
fn an_absent_borg_yml_resolves_to_the_default_path() {
    let _home = HomeGuard::hold();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = vocabulary_path(&dir.path().join("borg.yml")).expect("absent is not an error");
    assert_eq!(path, vault::paths::canonical_tags());
}

#[test]
fn a_borg_yml_without_the_key_resolves_to_the_default_path() {
    let _home = HomeGuard::hold();
    let dir = tempfile::tempdir().expect("tempdir");
    let yml = write_borg_yml(dir.path(), "hotkey:\n  port: 9999\n");
    assert_eq!(vocabulary_path(&yml).expect("resolve"), vault::paths::canonical_tags());
}

#[test]
fn a_borg_yml_that_cannot_be_stat_ed_is_an_error_not_the_default() {
    // A path beneath a regular file fails metadata with NotADirectory, which
    // `Path::exists()` would flatten into "absent" and silently use the default.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("plain-file");
    std::fs::write(&file, "x").expect("write");
    let err = vocabulary_path(&file.join("borg.yml")).expect_err("unreadable must not be absent");
    assert!(format!("{err:#}").contains("borg.yml"), "{err:#}");
}

#[test]
fn a_borg_yml_that_cannot_be_read_is_an_error_naming_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let as_dir = dir.path().join("borg.yml");
    std::fs::create_dir(&as_dir).expect("mkdir");
    let err = vocabulary_path(&as_dir).expect_err("a directory is not a readable config");
    assert!(format!("{err:#}").contains("borg.yml"), "{err:#}");
}

#[test]
fn an_unparseable_borg_yml_is_an_error_naming_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let yml = write_borg_yml(dir.path(), "tags: [not: {a mapping\n");
    let err = vocabulary_path(&yml).expect_err("unparseable must fail");
    assert!(format!("{err:#}").contains("borg.yml"), "{err:#}");
}

#[test]
fn a_tilde_canonical_path_is_expanded_under_the_fixture_home() {
    let home = tempfile::tempdir().expect("tempdir");
    let _guard = HomeGuard::set(home.path());
    write_vocab(&home.path().join("tags.yml"), &["zz-fixture-only"]);
    let yml = write_borg_yml(home.path(), "tags:\n  canonical-path: ~/tags.yml\n");

    let path = vocabulary_path(&yml).expect("resolve");
    assert_eq!(path, home.path().join("tags.yml"));
    assert_eq!(resolve(&yml).expect("load"), vec!["zz-fixture-only".to_string()]);
}

#[test]
fn load_canonical_tags_sorts_and_takes_the_path_as_a_parameter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tags.yml");
    write_vocab(&path, &["rust", "privacy"]);
    assert_eq!(
        load_canonical_tags(&path).expect("load"),
        vec!["privacy".to_string(), "rust".to_string()]
    );
}

#[test]
fn load_canonical_tags_names_the_path_when_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nope.yml");
    let err = load_canonical_tags(&path).expect_err("missing must fail");
    assert!(format!("{err:#}").starts_with(&path.display().to_string()), "{err:#}");
}

#[test]
fn load_canonical_tags_names_the_path_when_unparseable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tags.yml");
    std::fs::write(&path, "tags: [unterminated\n").expect("write");
    let err = load_canonical_tags(&path).expect_err("unparseable must fail");
    assert!(format!("{err:#}").contains(&path.display().to_string()), "{err:#}");
}
