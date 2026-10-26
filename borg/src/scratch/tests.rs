#![allow(clippy::unwrap_used)]

use super::*;
use tempfile::TempDir;

#[test]
fn run_root_accepts_one_plain_component_only() {
    let base = Path::new("/data/distill");
    assert_eq!(run_root(base, "cl-1a2b"), Some(base.join("cl-1a2b")));
    for bad in ["", ".", "..", "a/b", "/abs", "../x", "x/.."] {
        assert_eq!(run_root(base, bad), None, "{bad:?}");
    }
}

#[test]
fn requested_path_refuses_parent_and_absolute_and_drops_dot() {
    assert_eq!(requested_path("frames/runs.yml"), Ok(PathBuf::from("frames/runs.yml")));
    assert_eq!(requested_path("./note.md"), Ok(PathBuf::from("note.md")));
    assert_eq!(requested_path("a/./b"), Ok(PathBuf::from("a/b")));
    assert_eq!(requested_path("../x"), Err(PathRefusal::ParentDir));
    assert_eq!(requested_path("a/../../x"), Err(PathRefusal::ParentDir));
    assert_eq!(requested_path("a/.."), Err(PathRefusal::ParentDir));
    assert_eq!(requested_path("/etc/passwd"), Err(PathRefusal::Absolute));
}

#[test]
fn regular_files_lists_nested_files_sorted_and_skips_symlinks() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("cl-1");
    std::fs::create_dir_all(root.join("frames/frames")).unwrap();
    std::fs::create_dir_all(root.join("stages/cl-1")).unwrap();
    std::fs::write(root.join("note.md"), b"# n\n").unwrap();
    std::fs::write(root.join("frames/runs.yml"), b"- ocr: x\n").unwrap();
    std::fs::write(root.join("stages/cl-1/distilled.yml"), b"summary: s\n").unwrap();

    let outside = tmp.path().join("secret.txt");
    std::fs::write(&outside, b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("leak.txt")).unwrap();
    std::os::unix::fs::symlink(tmp.path(), root.join("frames/up")).unwrap();

    let files = regular_files(&root).unwrap();
    let paths: Vec<&str> = files.iter().map(|(_, f)| f.path.as_str()).collect();
    assert_eq!(paths, vec!["frames/runs.yml", "note.md", "stages/cl-1/distilled.yml"]);
    assert_eq!(files[1].1.size, 4);
    assert_eq!(files[1].0, PathBuf::from("note.md"));
}

#[test]
fn regular_files_of_a_missing_root_is_empty_and_a_symlinked_root_lists_nothing() {
    let tmp = TempDir::new().unwrap();
    assert!(regular_files(&tmp.path().join("gone")).unwrap().is_empty());
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("x.md"), b"x").unwrap();
    let link = tmp.path().join("cl-link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(regular_files(&link).unwrap().is_empty());
}

#[test]
fn manifest_serializes_kebab_case() {
    let manifest = DistillManifest {
        trace_id: "cl-1".into(),
        status: "succeeded".into(),
        failure_stage: None,
        failure_reason: None,
        files: vec![ManifestFile {
            path: "note.md".into(),
            size: 3,
        }],
    };
    let json = serde_json::to_value(&manifest).unwrap();
    assert_eq!(json["trace-id"], "cl-1");
    assert!(json["failure-stage"].is_null());
    assert_eq!(json["files"][0]["path"], "note.md");
    let back: DistillManifest = serde_json::from_value(json).unwrap();
    assert_eq!(back, manifest);
}
