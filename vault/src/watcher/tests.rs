use super::*;
use std::fs;
use tempfile::TempDir;
use tokio::time;

#[test]
fn test_watcher_config_default() {
    let config = WatcherConfig::default();
    assert_eq!(config.debounce_secs, 5);
    assert!(config.ignore_dirs.contains(&".git".to_string()));
    assert!(config.ignore_dirs.contains(&".obsidian".to_string()));
    assert!(config.ignore_dirs.contains(&"quarantine".to_string()));
    assert!(config.ignore_dirs.contains(&"templates".to_string()));
}

#[test]
fn test_should_process_event_create() {
    let event = notify::Event {
        kind: EventKind::Create(notify::event::CreateKind::File),
        paths: vec![PathBuf::from("/vault/notes/test.md")],
        attrs: Default::default(),
    };
    assert!(should_process_event(&event, &[]));
}

#[test]
fn test_should_process_event_access_ignored() {
    let event = notify::Event {
        kind: EventKind::Access(notify::event::AccessKind::Read),
        paths: vec![PathBuf::from("/vault/notes/test.md")],
        attrs: Default::default(),
    };
    assert!(!should_process_event(&event, &[]));
}

#[test]
fn test_should_process_event_ignore_dirs() {
    let event = notify::Event {
        kind: EventKind::Modify(notify::event::ModifyKind::Data(notify::event::DataChange::Content)),
        paths: vec![PathBuf::from("/vault/.git/config")],
        attrs: Default::default(),
    };
    let ignore = vec![".git".to_string()];
    assert!(!should_process_event(&event, &ignore));
}

#[test]
fn test_should_process_event_obsidian_ignored() {
    let event = notify::Event {
        kind: EventKind::Modify(notify::event::ModifyKind::Data(notify::event::DataChange::Content)),
        paths: vec![PathBuf::from("/vault/.obsidian/workspace.json")],
        attrs: Default::default(),
    };
    let ignore = vec![".obsidian".to_string()];
    assert!(!should_process_event(&event, &ignore));
}

#[tokio::test]
async fn test_vault_watcher_debounce() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let vault_root = tmp.path();

    // Create initial .md file so the directory isn't empty
    let notes_dir = vault_root.join("notes");
    fs::create_dir_all(&notes_dir).expect("failed to create notes dir");

    let config = WatcherConfig {
        debounce_secs: 1, // short debounce for testing
        ignore_dirs: vec![".git".into()],
    };

    let (watcher, mut rx) = VaultWatcher::start(vault_root, config, None).expect("failed to start watcher");

    // Give the watcher a moment to initialize
    time::sleep(Duration::from_millis(100)).await;

    // Write two files in rapid succession
    fs::write(notes_dir.join("test-one.md"), "# Test One").expect("write failed");
    time::sleep(Duration::from_millis(50)).await;
    fs::write(notes_dir.join("test-two.md"), "# Test Two").expect("write failed");

    // Wait for debounce to fire (1s debounce + buffer)
    let result = time::timeout(Duration::from_secs(3), rx.recv()).await;
    assert!(result.is_ok(), "should receive a VaultChange within timeout");

    let change = result.expect("timeout").expect("channel closed");
    // Both files should be in a single batch
    assert!(
        change.changed_paths.len() >= 2,
        "expected at least 2 paths, got {}",
        change.changed_paths.len()
    );

    // Verify .md files are present
    let path_strs: Vec<String> = change
        .changed_paths
        .iter()
        .filter_map(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .collect();
    assert!(path_strs.contains(&"test-one.md".to_string()));
    assert!(path_strs.contains(&"test-two.md".to_string()));

    // Keep watcher alive for the duration of the test
    drop(watcher);
}

#[tokio::test]
async fn test_vault_watcher_emits_two_batches() {
    // Regression: the debounce task used to die after the FIRST batch
    // because `reset(now + Duration::MAX)` overflowed and panicked. A
    // healthy watcher must emit a SECOND batch for a later write.
    let tmp = TempDir::new().expect("failed to create temp dir");
    let vault_root = tmp.path();

    let config = WatcherConfig {
        debounce_secs: 1,
        ignore_dirs: vec![],
    };

    let (watcher, mut rx) = VaultWatcher::start(vault_root, config, None).expect("failed to start watcher");

    time::sleep(Duration::from_millis(100)).await;

    // First batch.
    fs::write(vault_root.join("first.md"), "# First").expect("write failed");
    let first = time::timeout(Duration::from_secs(3), rx.recv()).await;
    assert!(first.is_ok(), "should receive the first VaultChange");
    let first = first.expect("timeout").expect("channel closed");
    let first_names: Vec<String> = first
        .changed_paths
        .iter()
        .filter_map(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .collect();
    assert!(first_names.contains(&"first.md".to_string()));

    // Second batch - this is the one that used to never arrive.
    time::sleep(Duration::from_millis(100)).await;
    fs::write(vault_root.join("second.md"), "# Second").expect("write failed");
    let second = time::timeout(Duration::from_secs(3), rx.recv()).await;
    assert!(
        second.is_ok(),
        "should receive a SECOND VaultChange (debounce survived)"
    );
    let second = second.expect("timeout").expect("channel closed");
    let second_names: Vec<String> = second
        .changed_paths
        .iter()
        .filter_map(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .collect();
    assert!(second_names.contains(&"second.md".to_string()));

    drop(watcher);
}

#[tokio::test]
async fn test_vault_watcher_ignores_non_md() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let vault_root = tmp.path();

    let config = WatcherConfig {
        debounce_secs: 1,
        ignore_dirs: vec![],
    };

    let (watcher, mut rx) = VaultWatcher::start(vault_root, config, None).expect("failed to start watcher");

    time::sleep(Duration::from_millis(100)).await;

    // Write a non-.md file
    fs::write(vault_root.join("data.json"), "{}").expect("write failed");

    // Also write an .md file so we get a debounce event
    time::sleep(Duration::from_millis(50)).await;
    fs::write(vault_root.join("note.md"), "# Note").expect("write failed");

    let result = time::timeout(Duration::from_secs(3), rx.recv()).await;
    assert!(result.is_ok());

    let change = result.expect("timeout").expect("channel closed");
    let path_strs: Vec<String> = change
        .changed_paths
        .iter()
        .filter_map(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .collect();

    // .md file should be present, .json should not
    assert!(path_strs.contains(&"note.md".to_string()));
    assert!(!path_strs.contains(&"data.json".to_string()));

    drop(watcher);
}
