use super::*;
use tempfile::tempdir;

#[test]
fn extract_frontmatter_field_works() {
    let c = "---\ntitle: T\norigin: assisted\ndate: 2026-04-16\n---\nbody\n";
    assert_eq!(extract_frontmatter_field(c, "origin"), Some("assisted".to_string()));
    assert_eq!(extract_frontmatter_field(c, "date"), Some("2026-04-16".to_string()));
    assert_eq!(extract_frontmatter_field(c, "ingested"), None);
}

#[test]
fn extract_frontmatter_field_returns_none_for_unfenced() {
    assert!(extract_frontmatter_field("no frontmatter", "date").is_none());
}

#[test]
fn collect_md_files_skips_listed_folders() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("a.md"), "x").expect("write");
    std::fs::create_dir_all(root.join(".obsidian")).expect("mkdir");
    std::fs::write(root.join(".obsidian/cache.md"), "x").expect("write");
    std::fs::create_dir_all(root.join("inbox")).expect("mkdir");
    std::fs::write(root.join("inbox/b.md"), "x").expect("write");
    let files = collect_md_files(root, &[".obsidian".to_string()]).expect("collect");
    let names: Vec<String> = files
        .iter()
        .map(|p| p.file_name().expect("name").to_string_lossy().to_string())
        .collect();
    assert!(names.contains(&"a.md".to_string()));
    assert!(names.contains(&"b.md".to_string()));
    assert!(!names.contains(&"cache.md".to_string()));
}
