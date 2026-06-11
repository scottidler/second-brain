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
