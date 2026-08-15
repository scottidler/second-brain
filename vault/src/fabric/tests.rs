use super::*;
use std::sync::Mutex;

/// Serializes every env-var-mutating test in this file so they can't race
/// each other's `XDG_CONFIG_HOME` reads/writes (cargo runs tests in parallel
/// by default). Per the `rust.md` env-test pattern.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn fabric_error_timeout_is_downcastable_through_eyre() {
    let report: eyre::Report = FabricError::Timeout {
        pattern: "distill-article".to_string(),
        timeout_secs: 60,
    }
    .into();
    assert!(FabricError::is_timeout(&report));
    // A Failed (non-timeout) fabric error must NOT read as a timeout.
    let failed: eyre::Report = FabricError::Failed {
        pattern: "distill-article".to_string(),
        stderr: "boom".to_string(),
    }
    .into();
    assert!(!FabricError::is_timeout(&failed));
    // An unrelated error (even one whose text says "timed out") must not
    // masquerade as a typed fabric timeout.
    let unrelated = eyre::eyre!("connection timed out");
    assert!(!FabricError::is_timeout(&unrelated));
}

#[test]
fn wait_with_timeout_drains_large_stdout_without_deadlock() {
    // Regression: emit far more than the ~64KB OS pipe buffer. The old
    // poll-without-reading loop deadlocked the child against an unread
    // pipe until the timeout killed it. The drain threads must collect
    // all of it and return successfully well inside the timeout.
    use std::process::{Command, Stdio};
    let big = 500_000usize;
    let child = Command::new("sh")
        .arg("-c")
        .arg(format!("head -c {big} /dev/zero | tr '\\0' 'a'"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(child) = child else {
        eprintln!("sh not available; skipping deadlock test");
        return;
    };
    let out = wait_with_timeout(child, Vec::new(), Duration::from_secs(30)).expect("wait");
    let (status, stdout, _stderr) = out.expect("must not time out on large output");
    assert!(status.success());
    assert_eq!(stdout.len(), big, "all stdout bytes must be drained");
}

#[test]
fn wait_with_timeout_kills_on_timeout() {
    use std::process::{Command, Stdio};
    let child = Command::new("sh")
        .arg("-c")
        .arg("sleep 30")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(child) = child else {
        eprintln!("sh not available; skipping timeout test");
        return;
    };
    let out = wait_with_timeout(child, Vec::new(), Duration::from_millis(300)).expect("wait");
    assert!(out.is_none(), "a child exceeding the timeout returns None (killed)");
}

#[test]
fn build_fabric_command_sets_anthropic_key_from_named_env_var() {
    // Hold ENV_LOCK for the whole env-mutation window so no parallel test
    // observes our var override.
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let var = "VAULT_FABRIC_TEST_KEY";
    let original = std::env::var_os(var);
    // SAFETY: env mutation is intentional for testing child-env wiring.
    unsafe { std::env::set_var(var, "sekret-value-123") };

    let cmd = build_fabric_command("fabric", "summarize", "", var);
    let entry = cmd
        .get_envs()
        .find(|(k, _)| *k == std::ffi::OsStr::new("ANTHROPIC_API_KEY"));
    let (_, value) = entry.expect("ANTHROPIC_API_KEY must be set on the child");
    assert_eq!(value, Some(std::ffi::OsStr::new("sekret-value-123")));

    // SAFETY: restore env to avoid leaking state to other tests.
    unsafe {
        match original {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
    }
}

#[test]
fn build_fabric_command_leaves_anthropic_key_unset_when_env_name_empty() {
    // No api_key_env => the child must carry no explicit ANTHROPIC_API_KEY
    // override (fabric falls back to its own .env). get_envs() reports only
    // explicitly-set child overrides, so an empty result is the assertion.
    let cmd = build_fabric_command("fabric", "summarize", "", "");
    let has_key = cmd
        .get_envs()
        .any(|(k, _)| k == std::ffi::OsStr::new("ANTHROPIC_API_KEY"));
    assert!(
        !has_key,
        "empty api_key_env must not set ANTHROPIC_API_KEY on the child"
    );
}

#[test]
fn test_extract_json_bare() {
    let input = r#"{"folder": "Tech", "confidence": 0.9}"#;
    let result = extract_json(input);
    assert!(result.starts_with('{'));
    assert!(result.ends_with('}'));
}

#[test]
fn test_extract_json_markdown_wrapped() {
    let input = "```json\n{\"folder\": \"Tech\", \"confidence\": 0.8}\n```";
    let result = extract_json(input);
    assert!(result.starts_with('{'));
}

#[test]
fn test_truncate_input() {
    assert_eq!(truncate_input("hello world", 5), "hello");
    assert_eq!(truncate_input("hello", 10), "hello");
    assert_eq!(truncate_input("hello", 0), "hello");
}

#[test]
fn test_resolve_pattern_passes_paths_through() {
    // Path-like inputs return unchanged regardless of filesystem state.
    assert_eq!(resolve_pattern("/abs/path.md"), "/abs/path.md");
    assert_eq!(resolve_pattern("./rel.md"), "./rel.md");
    assert_eq!(resolve_pattern("~/in-home.md"), "~/in-home.md");
}

#[test]
fn test_resolve_pattern_unknown_name_passes_through() {
    // No file at vault::paths::patterns_dir() for this name, so the bare
    // name is returned and fabric's own loader can try.
    let result = resolve_pattern("this-pattern-does-not-exist-xyz-12345");
    assert_eq!(result, "this-pattern-does-not-exist-xyz-12345");
}

#[test]
fn test_resolve_pattern_canonical_path_for_present_file() {
    // Write a fake pattern file at the canonical patterns_dir() location
    // (under a tempdir-pointed XDG_CONFIG_HOME) and assert the resolver
    // joins it correctly.
    // Hold ENV_LOCK for the whole env-mutation window so no parallel test
    // observes our XDG_CONFIG_HOME override.
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let tmp = tempfile::tempdir().expect("tempdir");
    let original = std::env::var_os("XDG_CONFIG_HOME");
    // SAFETY: env mutation is intentional for testing path resolution.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.path()) };

    let patterns_dir = crate::paths::patterns_dir();
    std::fs::create_dir_all(&patterns_dir).expect("create patterns dir");
    let pattern_path = patterns_dir.join("test-pattern.md");
    std::fs::write(&pattern_path, "test content").expect("write pattern");

    let resolved = resolve_pattern("test-pattern");
    assert_eq!(resolved, pattern_path.to_string_lossy());

    // SAFETY: restore env to avoid leaking state to other tests.
    unsafe {
        match original {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}
