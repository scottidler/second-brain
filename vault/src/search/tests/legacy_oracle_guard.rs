//! The fail-closed guard that keeps `SearchIndex::open` from minting an empty
//! oracle DB at the post-R1 path while the pre-R1 one still holds the data.
//!
//! Linux-only: the redirect works by setting `XDG_DATA_HOME`, which
//! `vault::paths::xdg_data_dir` honors. macOS resolves the data dir through
//! system APIs that ignore env vars.
#![cfg(target_os = "linux")]

use crate::paths::{legacy_oracle_dir, oracle_db_path};
use crate::search::{SearchError, SearchIndex};

/// Point `XDG_DATA_HOME` at a fresh tempdir, run `body`, and restore the
/// prior value even if `body` panics.
fn with_xdg_data_home(body: impl FnOnce(&std::path::Path) + std::panic::UnwindSafe) {
    let tmp = tempfile::tempdir().expect("xdg tmpdir");
    let prior = std::env::var_os("XDG_DATA_HOME");
    // SAFETY: serialized by `serial_test::serial(xdg_data_home)`; no
    // concurrent reader of the env exists while this runs.
    unsafe { std::env::set_var("XDG_DATA_HOME", tmp.path()) };

    let result = std::panic::catch_unwind(|| body(tmp.path()));

    // SAFETY: same serialization as above.
    unsafe {
        match prior {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

/// Create a file that reads as a SQLite DB well enough for the guard's
/// `.exists()` check. The guard never opens it.
fn touch_db(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().expect("db parent")).expect("mkdir db parent");
    std::fs::write(path, b"").expect("touch db");
}

#[serial_test::serial(xdg_data_home)]
#[test]
fn legacy_oracle_guard_refuses_and_creates_nothing() {
    with_xdg_data_home(|root| {
        touch_db(&legacy_oracle_dir().join("oracle.db"));
        let db_path = oracle_db_path();
        let new_dir = db_path.parent().expect("db parent").to_path_buf();
        assert!(db_path.starts_with(root), "XDG_DATA_HOME redirect did not take");
        assert!(!new_dir.exists(), "precondition: sb/oracle/ must not exist yet");

        let Err(err) = SearchIndex::open(&db_path) else {
            panic!("open must refuse while the legacy DB exists");
        };
        let typed = err
            .downcast_ref::<SearchError>()
            .expect("guard must return a typed SearchError");
        let SearchError::LegacyOracleDb { legacy, new } = typed;
        assert_eq!(legacy, &legacy_oracle_dir());
        assert_eq!(new, &db_path);

        // The load-bearing assertion: the guard runs BEFORE `create_dir_all`,
        // so a refused open leaves the destination absent. If it ran after,
        // runbook R1's `mv -T` would nest the legacy dir inside this one.
        assert!(
            !new_dir.exists(),
            "guard created {} - it must sit before create_dir_all",
            new_dir.display()
        );
    });
}

#[serial_test::serial(xdg_data_home)]
#[test]
fn legacy_oracle_guard_creates_when_neither_exists() {
    with_xdg_data_home(|_root| {
        let db_path = oracle_db_path();
        assert!(!legacy_oracle_dir().exists(), "precondition: no legacy dir");

        SearchIndex::open(&db_path).expect("open must create a fresh index");
        assert!(db_path.exists(), "open did not create {}", db_path.display());
    });
}

#[serial_test::serial(xdg_data_home)]
#[test]
fn legacy_oracle_guard_ignores_legacy_once_the_new_db_exists() {
    with_xdg_data_home(|_root| {
        let db_path = oracle_db_path();
        drop(SearchIndex::open(&db_path).expect("first open creates the new DB"));
        assert!(db_path.exists());

        // Post-move state: R1 has run but the legacy dir is still around (a
        // stale copy, or the operator has not cleaned up). The new DB wins.
        touch_db(&legacy_oracle_dir().join("oracle.db"));
        drop(SearchIndex::open(&db_path).expect("open must succeed once the new DB exists"));
    });
}

/// A non-oracle path is not the guard's business even with a legacy DB present.
#[serial_test::serial(xdg_data_home)]
#[test]
fn legacy_oracle_guard_leaves_other_paths_alone() {
    with_xdg_data_home(|root| {
        touch_db(&legacy_oracle_dir().join("oracle.db"));
        let other = root.join("somewhere-else").join("index.db");
        SearchIndex::open(&other).expect("a non-oracle path is unaffected");
        assert!(other.exists());
    });
}
