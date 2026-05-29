use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn empty_input_is_noop() {
    let empty: &[PathBuf] = &[];
    remove(empty).expect("empty remove is ok");
}

#[test]
fn removes_a_file() {
    let dir = TempDir::new().expect("tempdir");
    let f = dir.path().join("note.md");
    std::fs::write(&f, b"hi").expect("write");
    remove(&[&f]).expect("remove file");
    assert!(!f.exists(), "file should be gone");
}

#[test]
fn removes_a_directory_recursively() {
    let dir = TempDir::new().expect("tempdir");
    let sub = dir.path().join("frames");
    std::fs::create_dir_all(sub.join("nested")).expect("mkdir");
    std::fs::write(sub.join("nested").join("a.jpg"), b"x").expect("write");
    remove(&[&sub]).expect("remove dir");
    assert!(!sub.exists(), "dir should be gone");
}

#[test]
fn missing_path_is_noop() {
    let dir = TempDir::new().expect("tempdir");
    let gone = dir.path().join("does-not-exist");
    remove(&[&gone]).expect("removing a missing path is ok");
}

#[test]
fn removes_multiple_paths_of_mixed_kind() {
    let dir = TempDir::new().expect("tempdir");
    let f = dir.path().join("a.md");
    let d = dir.path().join("b");
    std::fs::write(&f, b"x").expect("write");
    std::fs::create_dir_all(&d).expect("mkdir");
    let paths: Vec<PathBuf> = vec![f.clone(), d.clone()];
    remove(&paths).expect("remove mixed");
    assert!(!f.exists() && !d.exists(), "both should be gone");
}
