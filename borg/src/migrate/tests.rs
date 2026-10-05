use super::*;

#[test]
fn test_split_frontmatter_valid() {
    let content = "---\ntitle: Test\ntype: link\n---\n\n# Body\n";
    let (fm, body) = split_frontmatter(content).expect("should split");
    assert!(fm.contains("title: Test"));
    assert!(body.contains("# Body"));
}

#[test]
fn test_split_frontmatter_no_frontmatter() {
    let content = "# Just a heading\n\nSome text.\n";
    assert!(split_frontmatter(content).is_none());
}

#[test]
fn test_split_frontmatter_unclosed() {
    let content = "---\ntitle: Test\nno closing delimiter\n";
    assert!(split_frontmatter(content).is_none());
}

#[test]
fn test_extract_title_from_body() {
    let body = "\n\n# My Title\n\nSome content.";
    assert_eq!(extract_title_from_body(body), Some("My Title".to_string()));
}

#[test]
fn test_extract_title_from_body_none() {
    let body = "\n\nSome content without heading.";
    assert_eq!(extract_title_from_body(body), None);
}

#[test]
fn test_render_frontmatter_ordering() {
    let mut fm = HashMap::new();
    fm.insert("type".to_string(), serde_yaml::Value::String("article".to_string()));
    fm.insert("title".to_string(), serde_yaml::Value::String("Test".to_string()));
    fm.insert(
        "source".to_string(),
        serde_yaml::Value::String("https://example.com".to_string()),
    );
    let result = render_frontmatter(&fm, "\n# Body\n");
    let lines: Vec<&str> = result.lines().collect();
    // title should come before type
    let title_pos = lines.iter().position(|l| l.contains("title")).expect("title");
    let type_pos = lines.iter().position(|l| l.contains("type")).expect("type");
    assert!(title_pos < type_pos);
}

#[test]
fn test_render_frontmatter_tags() {
    let mut fm = HashMap::new();
    fm.insert(
        "tags".to_string(),
        serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::String("ai".to_string()),
            serde_yaml::Value::String("rust".to_string()),
        ]),
    );
    let result = render_frontmatter(&fm, "\n");
    assert!(result.contains("tags:\n  - ai\n  - rust"));
}

#[test]
fn test_reclassify_type_youtube() {
    assert_eq!(reclassify_type("https://www.youtube.com/watch?v=abc123"), "youtube");
    assert_eq!(reclassify_type("https://youtu.be/abc123"), "youtube");
    assert_eq!(reclassify_type("https://www.youtube.com/shorts/abc123"), "youtube");
}

#[test]
fn test_reclassify_type_github() {
    assert_eq!(reclassify_type("https://github.com/open-webui/open-terminal"), "github");
    assert_eq!(reclassify_type("https://github.com/Infatoshi/OpenSquirrel/"), "github");
}

#[test]
fn test_reclassify_type_github_deep_path_is_article() {
    assert_eq!(
        reclassify_type("https://github.com/owner/repo/blob/main/README.md"),
        "article"
    );
    assert_eq!(reclassify_type("https://github.com/owner/repo/issues/42"), "article");
}

#[test]
fn test_reclassify_type_social() {
    assert_eq!(
        reclassify_type("https://x.com/Zai_org/status/2033221428640674015"),
        "social"
    );
}

#[test]
fn test_reclassify_type_reddit() {
    assert_eq!(
        reclassify_type("https://www.reddit.com/r/footballstrategy/comments/lhb3ku/help/"),
        "reddit"
    );
}

#[test]
fn test_reclassify_type_article() {
    assert_eq!(reclassify_type("https://blog.example.com/post"), "article");
    assert_eq!(
        reclassify_type("https://www.xda-developers.com/some-article/"),
        "article"
    );
}

mod reingest_failed_daemon {
    use super::*;
    use crate::stub::{Behavior, serve};
    use std::time::{Duration, Instant};

    const T: Duration = Duration::from_millis(400);

    /// A vault with one note whose body carries the failed-fetch signature.
    fn vault_with_failed_note() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("notes")).expect("notes dir");
        std::fs::write(
            dir.path().join("notes/failed.md"),
            "---\ntitle: Failed\nsource: https://example.com/failed\n---\n\nThis page contains only an error message.\n",
        )
        .expect("write note");
        dir
    }

    fn config_for(vault: &std::path::Path, port: u16) -> Config {
        let mut config = Config::default();
        config.vault.root_path = Some(vault.display().to_string());
        config.hotkey.host = "127.0.0.1".to_string();
        config.hotkey.port = port;
        config.hotkey.request_timeout = T;
        config
    }

    async fn run_against(behavior: Behavior, token_file: Option<&std::path::Path>) -> (Vec<String>, Duration) {
        let vault = vault_with_failed_note();
        let (port, _) = serve(behavior).await;
        let mut config = config_for(vault.path(), port);
        config.server.auth_token = token_file.map(|p| p.display().to_string());
        let mut events = Vec::new();
        let started = Instant::now();
        reingest_failed(&config, false, |e| events.push(format!("{e:?}")))
            .await
            .expect("per-item errors are events, not fatal");
        (events, started.elapsed())
    }

    #[tokio::test]
    async fn a_silent_daemon_is_an_http_error_event_within_twice_the_timeout() {
        let (events, took) = run_against(Behavior::Silent, None).await;
        assert!(events.iter().any(|e| e.starts_with("HttpError")), "{events:?}");
        assert!(took < T * 2 + Duration::from_millis(300), "took {took:?}");
    }

    #[tokio::test]
    async fn a_stalled_body_is_an_error_event_within_twice_the_timeout() {
        let (events, took) = run_against(Behavior::HeadersThenStall, None).await;
        assert!(
            events
                .iter()
                .any(|e| e.starts_with("ParseError") || e.starts_with("HttpError")),
            "{events:?}"
        );
        assert!(took < T * 2 + Duration::from_millis(300), "took {took:?}");
    }

    #[tokio::test]
    async fn a_401_names_the_401_not_a_parse_error() {
        let (events, _) = run_against(Behavior::Unauthorized, None).await;
        let http = events
            .iter()
            .find(|e| e.starts_with("HttpError"))
            .expect("HttpError event");
        assert!(http.contains("(401)"), "{http}");
        assert!(!events.iter().any(|e| e.starts_with("ParseError")), "{events:?}");
    }

    #[tokio::test]
    async fn a_token_requiring_daemon_accepts_the_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        let token_file = dir.path().join("token");
        std::fs::write(&token_file, "s3cret\n").expect("write token");
        let behavior = Behavior::RequireToken {
            token: "s3cret".to_string(),
            body: r#"{"status":"Completed","note_path":null,"title":"T","tags":[]}"#.to_string(),
        };
        let (events, _) = run_against(behavior, Some(&token_file)).await;
        assert!(events.iter().any(|e| e.starts_with("Ok")), "{events:?}");
        assert!(!events.iter().any(|e| e.starts_with("HttpError")), "{events:?}");
    }

    #[tokio::test]
    async fn without_a_token_a_token_requiring_daemon_rejects() {
        let behavior = Behavior::RequireToken {
            token: "s3cret".to_string(),
            body: r#"{"status":"Completed","note_path":null,"title":"T","tags":[]}"#.to_string(),
        };
        let (events, _) = run_against(behavior, None).await;
        assert!(events.iter().any(|e| e.starts_with("HttpError")), "{events:?}");
    }
}
