use super::*;
use crate::testutil::TestVault;

#[test]
fn test_scan_finds_md_files() {
    let v = TestVault::new();
    let manifest = VaultManifest::scan(v.root(), &[]).expect("scan");
    // All .md files in the vault (including .obsidian and protected - manifest doesn't filter)
    assert!(manifest.files.len() >= 14);
}

#[test]
fn test_scan_ignores_directories() {
    let v = TestVault::new();
    let all = VaultManifest::scan(v.root(), &[]).expect("all");
    let filtered = VaultManifest::scan(v.root(), &[".obsidian".to_string()]).expect("filtered");
    assert!(filtered.files.len() < all.files.len());
}

#[test]
fn test_diff_detects_added() {
    let v = TestVault::new();
    let before = VaultManifest::scan(v.root(), &[]).expect("before");
    v.add_note("new-note.md", "---\ntitle: New\n---\nFresh.\n");
    let after = VaultManifest::scan(v.root(), &[]).expect("after");

    let diff = before.diff(&after);
    assert!(diff.added.iter().any(|p| p.to_string_lossy().contains("new-note")));
}

#[test]
fn test_diff_detects_removed() {
    let v = TestVault::new();
    let before = VaultManifest::scan(v.root(), &[]).expect("before");
    std::fs::remove_file(v.root().join("bare-note.md")).expect("remove");
    let after = VaultManifest::scan(v.root(), &[]).expect("after");

    let diff = before.diff(&after);
    assert!(diff.removed.iter().any(|p| p.to_string_lossy().contains("bare-note")));
}

#[test]
fn test_diff_detects_modified() {
    let v = TestVault::new();
    let before = VaultManifest::scan(v.root(), &[]).expect("before");
    // Touch the file to change mtime/size
    std::fs::write(
        v.root().join("bare-note.md"),
        "Updated content that is different and longer than before.\n",
    )
    .expect("write");
    let after = VaultManifest::scan(v.root(), &[]).expect("after");

    let diff = before.diff(&after);
    assert!(diff.modified.iter().any(|p| p.to_string_lossy().contains("bare-note")));
}

#[test]
fn test_manifest_roundtrip() {
    let v = TestVault::new();
    let manifest = VaultManifest::scan(v.root(), &[]).expect("scan");
    let path = v.root().join(".cortex/manifest.yml");
    manifest.save(&path).expect("save");

    let loaded = VaultManifest::load(&path).expect("load");
    assert_eq!(loaded.files.len(), manifest.files.len());
}
