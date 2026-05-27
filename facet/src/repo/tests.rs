use super::*;

#[test]
fn parses_ssh_url() {
    assert_eq!(
        parse_slug("git@github.com:scottidler/loopr.git").as_deref(),
        Some("scottidler/loopr")
    );
}

#[test]
fn parses_https_url() {
    assert_eq!(
        parse_slug("https://github.com/scottidler/loopr.git").as_deref(),
        Some("scottidler/loopr")
    );
}

#[test]
fn parses_ssh_protocol_url() {
    assert_eq!(
        parse_slug("ssh://git@github.com/tatari-tv/philo.git").as_deref(),
        Some("tatari-tv/philo")
    );
}

#[test]
fn parses_http_url_no_git_suffix() {
    assert_eq!(parse_slug("http://example.com/me/repo").as_deref(), Some("me/repo"));
}

#[test]
fn rejects_bare_path() {
    assert!(parse_slug("plain/old/path").is_none());
}

#[test]
fn rejects_empty() {
    assert!(parse_slug("").is_none());
}

#[test]
fn rejects_too_many_segments() {
    // org/repo/subdir is not a valid owner/repo
    assert!(parse_slug("git@github.com:org/repo/sub.git").is_none());
}

#[test]
fn resolver_caches_lookups() {
    // Best-effort: just exercise that the cache path doesn't blow up.
    let r = Resolver::new();
    let nonexistent = std::path::PathBuf::from("/definitely/does/not/exist/xyz123");
    assert!(r.resolve(&nonexistent).is_none());
    assert!(r.resolve(&nonexistent).is_none());
}
