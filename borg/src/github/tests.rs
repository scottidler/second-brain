use super::*;

#[test]
fn parse_repo_url_accepts_https_root() {
    let parsed = parse_repo_url("https://github.com/scottidler/second-brain").expect("parse");
    assert_eq!(parsed, ("scottidler".to_string(), "second-brain".to_string()));
}

#[test]
fn parse_repo_url_strips_dot_git_suffix() {
    let parsed = parse_repo_url("https://github.com/scottidler/second-brain.git").expect("parse");
    assert_eq!(parsed, ("scottidler".to_string(), "second-brain".to_string()));
}

#[test]
fn parse_repo_url_accepts_trailing_slash() {
    let parsed = parse_repo_url("https://github.com/scottidler/second-brain/").expect("parse");
    assert_eq!(parsed, ("scottidler".to_string(), "second-brain".to_string()));
}

#[test]
fn parse_repo_url_rejects_deep_paths() {
    assert!(parse_repo_url("https://github.com/scottidler/second-brain/issues/1").is_none());
    assert!(parse_repo_url("https://github.com/scottidler/second-brain/blob/main/README.md").is_none());
    assert!(parse_repo_url("https://github.com/scottidler/second-brain/pull/42").is_none());
}

#[test]
fn parse_repo_url_rejects_non_github_hosts() {
    assert!(parse_repo_url("https://gitlab.com/foo/bar").is_none());
    assert!(parse_repo_url("https://example.com").is_none());
}

#[test]
fn parse_repo_url_rejects_owner_only() {
    assert!(parse_repo_url("https://github.com/scottidler").is_none());
    assert!(parse_repo_url("https://github.com/").is_none());
}

#[test]
fn parse_repo_url_accepts_raw_github_subdomain() {
    let parsed = parse_repo_url("https://www.github.com/scottidler/second-brain").expect("parse");
    assert_eq!(parsed, ("scottidler".to_string(), "second-brain".to_string()));
}

#[test]
fn render_transcript_leads_with_metadata_block() {
    let metadata = RepoMetadata {
        owner: "scottidler".to_string(),
        repo: "second-brain".to_string(),
        stars: Some(42),
        primary_language: Some("Rust".to_string()),
        last_commit: Some("2026-05-16T10:00:00Z".to_string()),
        topics: vec!["obsidian".to_string(), "knowledge".to_string()],
        default_branch: Some("main".to_string()),
        description: Some("Second brain workspace".to_string()),
    };
    let rendered = render_transcript("# Hello\n\nWorld", &metadata);
    assert!(rendered.starts_with("# Repository Metadata"));
    assert!(rendered.contains("- repo: scottidler/second-brain"));
    assert!(rendered.contains("- stars: 42"));
    assert!(rendered.contains("- primary-language: Rust"));
    assert!(rendered.contains("- last-commit: 2026-05-16T10:00:00Z"));
    assert!(rendered.contains("- topics: obsidian, knowledge"));
    assert!(rendered.contains("- description: Second brain workspace"));
    assert!(rendered.contains("# README\n\n# Hello\n\nWorld"));
}

#[test]
fn render_transcript_handles_minimal_metadata() {
    let metadata = RepoMetadata {
        owner: "o".to_string(),
        repo: "r".to_string(),
        ..Default::default()
    };
    let rendered = render_transcript("readme body", &metadata);
    assert!(rendered.contains("- repo: o/r"));
    assert!(!rendered.contains("- stars:"));
    assert!(!rendered.contains("- topics:"));
    assert!(rendered.ends_with("readme body\n"));
}

#[test]
fn decode_base64_readme_strips_whitespace() {
    // "Hello, world!\n" -> base64 SGVsbG8sIHdvcmxkIQo=
    let wrapped = "SGVsbG8s\nIHdvcmxk\nIQo=\n";
    let decoded = decode_base64_readme(wrapped).expect("decode");
    assert_eq!(decoded, "Hello, world!\n");
}
