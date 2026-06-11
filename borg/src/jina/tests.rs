#[test]
fn test_jina_url_format() {
    let url = "https://blog.example.com/post";
    let jina_url = format!("https://r.jina.ai/{url}");
    assert_eq!(jina_url, "https://r.jina.ai/https://blog.example.com/post");
}
