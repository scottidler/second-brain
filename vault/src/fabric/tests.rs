use super::*;
use serial_test::serial;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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
#[serial(process)]
fn build_fabric_command_sets_anthropic_key_from_named_env_var() {
    // Hold ENV_LOCK for the whole env-mutation window so no parallel test
    // observes our var override.
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let var = "VAULT_FABRIC_TEST_KEY";
    let original = std::env::var_os(var);
    // SAFETY: env mutation is intentional for testing child-env wiring.
    unsafe { std::env::set_var(var, "sekret-value-123") };

    let cmd = build_fabric_command("fabric", "summarize", "", var, 0);
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
#[serial(process)]
fn build_fabric_command_leaves_anthropic_key_unset_when_env_name_empty() {
    // No api_key_env => the child must carry no explicit ANTHROPIC_API_KEY
    // override (fabric falls back to its own .env). get_envs() reports only
    // explicitly-set child overrides, so an empty result is the assertion.
    let cmd = build_fabric_command("fabric", "summarize", "", "", 0);
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

/// `--maxTokens` must be ABSENT when the ceiling is 0, so an unpatched fabric
/// on PATH (which has no such flag) is not handed an argument it will reject.
#[test]
#[serial(process)]
fn build_fabric_command_omits_max_tokens_flag_when_zero() {
    let cmd = build_fabric_command("fabric", "summarize", "", "", 0);
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
    assert!(
        !args.iter().any(|a| a.starts_with("--maxTokens")),
        "expected no --maxTokens arg, got {args:?}"
    );
}

#[test]
#[serial(process)]
fn build_fabric_command_passes_max_tokens_flag_when_set() {
    let cmd = build_fabric_command("fabric", "summarize", "", "", 16384);
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
    assert!(
        args.iter().any(|a| a == "--maxTokens=16384"),
        "expected --maxTokens=16384 in {args:?}"
    );
}

/// The flag rides alongside `-m`, not instead of it: a regression here would
/// silently drop the model and fall back to fabric's DEFAULT_MODEL.
#[test]
#[serial(process)]
fn build_fabric_command_keeps_model_alongside_max_tokens() {
    let cmd = build_fabric_command("fabric", "summarize", "claude-sonnet-5", "", 8192);
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
    assert!(
        args.iter().any(|a| a == "claude-sonnet-5"),
        "model missing from {args:?}"
    );
    assert!(
        args.iter().any(|a| a == "--maxTokens=8192"),
        "max-tokens missing from {args:?}"
    );
}

/// Writes an executable `#!/bin/sh` script standing in for the fabric binary.
fn fake_fabric(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fabric");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake fabric");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod fake fabric");
    path
}

#[test]
#[serial(process)]
fn run_pattern_returns_the_whole_stdout_of_a_large_reply() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fabric = fake_fabric(dir.path(), "cat");
    let input = "x".repeat(512 * 1024);
    let out = run_pattern("summarize", &input, &fabric.to_string_lossy(), "", "", 0, 30).expect("run_pattern");
    assert_eq!(out.len(), input.len(), "every byte past the pipe buffer must come back");
}

#[test]
#[serial(process)]
fn run_pattern_past_its_timeout_is_a_typed_fabric_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fabric = fake_fabric(dir.path(), "sleep 30");
    let start = std::time::Instant::now();
    let err = run_pattern("summarize", "in", &fabric.to_string_lossy(), "", "", 0, 1).expect_err("must time out");
    assert!(FabricError::is_timeout(&err), "got {err:#}");
    assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
}

#[test]
#[serial(process)]
fn run_pattern_nonzero_exit_is_failed_with_the_stderr() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fabric = fake_fabric(dir.path(), "cat >/dev/null; echo 'model not found' >&2; exit 1");
    let err = run_pattern("summarize", "in", &fabric.to_string_lossy(), "", "", 0, 30).expect_err("must fail");
    match err.downcast_ref::<FabricError>() {
        Some(FabricError::Failed { pattern, stderr }) => {
            assert_eq!(pattern, "summarize");
            assert_eq!(stderr, "model not found");
        }
        other => panic!("expected FabricError::Failed, got {other:?}"),
    }
}

/// `fabric --version` once ran with the parent's stdin and hung for 37 minutes
/// when that stdin was a pipe that never closed. Reproduced here: fd 0 is such
/// a pipe, and the fake reads its stdin to end-of-file before answering.
#[test]
#[serial(process)]
fn is_available_does_not_hang_on_a_parent_stdin_that_never_closes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fabric = fake_fabric(dir.path(), "cat >/dev/null");
    let _stdin = crate::process::tests::ParentStdinIsAnOpenPipe::install();
    let start = std::time::Instant::now();
    assert!(is_available(&fabric.to_string_lossy()));
    assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
}

#[test]
#[serial(process)]
fn is_available_is_false_for_a_missing_or_failing_binary() {
    assert!(!is_available("/nonexistent/vault-fabric-test-binary"));
    let dir = tempfile::tempdir().expect("tempdir");
    let fabric = fake_fabric(dir.path(), "exit 1");
    assert!(!is_available(&fabric.to_string_lossy()));
}
