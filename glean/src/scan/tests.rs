#![allow(clippy::unwrap_used)]
use super::*;

#[test]
fn discover_finds_only_jsonl_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let projects = tmp.path();
    let sub = projects.join("-home-saidler-some-repo");
    std::fs::create_dir_all(&sub).expect("mkdir sub");
    std::fs::write(sub.join("session-a.jsonl"), b"{}\n").expect("write");
    std::fs::write(sub.join("notes.txt"), b"ignore me").expect("write");
    let sub2 = projects.join("-home-saidler-other");
    std::fs::create_dir_all(&sub2).expect("mkdir sub2");
    std::fs::write(sub2.join("session-b.jsonl"), b"{}\n").expect("write");

    let mut found = discover(projects)
        .expect("discover")
        .into_iter()
        .map(|s| s.jsonl_path.file_name().unwrap().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    found.sort();
    assert_eq!(found, vec!["session-a.jsonl", "session-b.jsonl"]);
}

#[test]
fn discover_returns_error_on_missing_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("not-there");
    let err = discover(&missing).expect_err("should fail");
    assert!(format!("{err}").contains("not found"));
}
