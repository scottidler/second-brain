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
fn parse_repo_url_rejects_gist_subdomain() {
    // gist.github.com URLs look like github.com/owner/hash but route to a
    // different service. The REST /repos endpoint 404s for them, which would
    // force the pipeline into the fallback path and persist gist HTML under
    // a structured-looking but inaccurate owner-hash filename. Reject here
    // so gists fall through to the regular article path instead.
    assert!(parse_repo_url("https://gist.github.com/scottidler/abc123def456").is_none());
}

#[test]
fn parse_repo_url_rejects_other_github_subdomains() {
    assert!(parse_repo_url("https://api.github.com/repos/scottidler/second-brain").is_none());
    assert!(parse_repo_url("https://raw.githubusercontent.com/scottidler/second-brain/main/README.md").is_none());
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

#[test]
fn decode_readme_from_bytes_handles_base64_body() {
    let body = br#"{"content": "SGVsbG8s\nIHdvcmxk\nIQo=\n", "encoding": "base64"}"#;
    let decoded = decode_readme_from_bytes(body).expect("decode");
    assert_eq!(decoded, "Hello, world!\n");
}

#[test]
fn decode_readme_from_bytes_handles_plain_body() {
    let body = br#"{"content": "plain markdown body", "encoding": ""}"#;
    let decoded = decode_readme_from_bytes(body).expect("decode");
    assert_eq!(decoded, "plain markdown body");
}

#[test]
fn decode_readme_from_bytes_handles_missing_content() {
    let body = br#"{}"#;
    let decoded = decode_readme_from_bytes(body).expect("decode");
    assert_eq!(decoded, "");
}

#[test]
fn extract_repo_slugs_bare_host_no_scheme() {
    assert_eq!(
        extract_repo_slugs("code at github.com/coleam00/archon today"),
        vec!["coleam00/archon".to_string()]
    );
}

#[test]
fn extract_repo_slugs_https_and_www_prefixes() {
    assert_eq!(
        extract_repo_slugs("see https://github.com/scottidler/second-brain"),
        vec!["scottidler/second-brain".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("see http://www.github.com/scottidler/second-brain"),
        vec!["scottidler/second-brain".to_string()]
    );
}

#[test]
fn extract_repo_slugs_truncates_deep_paths() {
    assert_eq!(
        extract_repo_slugs("https://github.com/owner/repo/tree/main/src"),
        vec!["owner/repo".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("github.com/owner/repo/issues/42"),
        vec!["owner/repo".to_string()]
    );
}

#[test]
fn extract_repo_slugs_strips_dot_git_suffix() {
    assert_eq!(
        extract_repo_slugs("clone github.com/owner/repo.git"),
        vec!["owner/repo".to_string()]
    );
}

#[test]
fn extract_repo_slugs_strips_query_and_fragment() {
    assert_eq!(
        extract_repo_slugs("github.com/owner/repo?tab=readme"),
        vec!["owner/repo".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("github.com/owner/repo#install"),
        vec!["owner/repo".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("github.com/owner/repo/tree/main?x=1#frag"),
        vec!["owner/repo".to_string()]
    );
}

#[test]
fn extract_repo_slugs_strips_trailing_prose_punctuation() {
    assert_eq!(
        extract_repo_slugs("Repo is github.com/owner/repo."),
        vec!["owner/repo".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("Repo is github.com/owner/repo,"),
        vec!["owner/repo".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("(github.com/owner/repo)"),
        vec!["owner/repo".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("link: <github.com/owner/repo>"),
        vec!["owner/repo".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("\"github.com/owner/repo\""),
        vec!["owner/repo".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("github.com/owner/repo/"),
        vec!["owner/repo".to_string()]
    );
}

/// Regression: the old `TRAILING_NOISE` char list missed `: ; ! ?`, so
/// sentence punctuation leaked into published slugs (`"see github.com/foo/bar!"`
/// -> `foo/bar!`). Charset validation against `[A-Za-z0-9._-]` trims them.
#[test]
fn extract_repo_slugs_strips_sentence_terminator_punctuation() {
    assert_eq!(
        extract_repo_slugs("see github.com/foo/bar!"),
        vec!["foo/bar".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("really? github.com/foo/bar?"),
        vec!["foo/bar".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("here: github.com/foo/bar:"),
        vec!["foo/bar".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("first github.com/foo/bar; then"),
        vec!["foo/bar".to_string()]
    );
}

/// A segment carrying an *interior* char outside GitHub's name charset cannot
/// name a real owner/repo and is rejected (not just trailing-trimmed).
#[test]
fn extract_repo_slugs_rejects_interior_invalid_chars() {
    assert!(extract_repo_slugs("github.com/fo!o/bar").is_empty());
    assert!(extract_repo_slugs("github.com/owner/ba:z").is_empty());
}

#[test]
fn extract_repo_slugs_rejects_every_reserved_owner() {
    for owner in RESERVED_OWNERS {
        let text = format!("see github.com/{owner}/something");
        assert!(
            extract_repo_slugs(&text).is_empty(),
            "reserved owner {owner} should yield no slug, got {:?}",
            extract_repo_slugs(&text)
        );
    }
}

#[test]
fn extract_repo_slugs_excludes_gist_and_raw_hosts() {
    assert!(extract_repo_slugs("https://gist.github.com/owner/abc123").is_empty());
    assert!(extract_repo_slugs("https://raw.githubusercontent.com/owner/repo/main/x.rs").is_empty());
    // Other subdomains (docs, api) are equally rejected.
    assert!(extract_repo_slugs("https://docs.github.com/owner/repo").is_empty());
}

#[test]
fn extract_repo_slugs_rejects_prefixed_hostnames() {
    // A non-github host that merely ends in `github.com` must not match.
    assert!(extract_repo_slugs("notgithub.com/owner/repo").is_empty());
    assert!(extract_repo_slugs("evil-github.com/owner/repo").is_empty());
    assert!(extract_repo_slugs("github.com.evil.com/owner/repo").is_empty());
}

#[test]
fn extract_repo_slugs_case_insensitive_dedup_preserves_first_casing() {
    let text = "github.com/Owner/Repo and again GitHub.com/owner/repo";
    assert_eq!(extract_repo_slugs(text), vec!["Owner/Repo".to_string()]);
}

#[test]
fn extract_repo_slugs_case_insensitive_host_matches() {
    assert_eq!(
        extract_repo_slugs("GITHUB.COM/owner/repo"),
        vec!["owner/repo".to_string()]
    );
    assert_eq!(
        extract_repo_slugs("https://GitHub.com/owner/repo"),
        vec!["owner/repo".to_string()]
    );
}

#[test]
fn extract_repo_slugs_multiple_repos_first_seen_order() {
    let text = "first github.com/a/one then github.com/b/two and github.com/c/three";
    assert_eq!(
        extract_repo_slugs(text),
        vec!["a/one".to_string(), "b/two".to_string(), "c/three".to_string(),]
    );
}

#[test]
fn extract_repo_slugs_no_repo_yields_empty() {
    assert!(extract_repo_slugs("just some prose with no links at all").is_empty());
    assert!(extract_repo_slugs("a bare https://example.com/owner/repo link").is_empty());
    // Owner-only github URL has no repo segment.
    assert!(extract_repo_slugs("github.com/owner").is_empty());
    assert!(extract_repo_slugs("github.com/owner/").is_empty());
}

#[test]
fn extract_repo_slugs_matches_at_start_of_text() {
    assert_eq!(
        extract_repo_slugs("github.com/owner/repo is the link"),
        vec!["owner/repo".to_string()]
    );
}

#[test]
fn extract_repo_slugs_matches_at_start_of_line() {
    let text = "Description:\ngithub.com/owner/repo\nmore text";
    assert_eq!(extract_repo_slugs(text), vec!["owner/repo".to_string()]);
}
