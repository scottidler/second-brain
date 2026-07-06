use super::*;

/// The in-process extractor pulls the article body out of a chrome-wrapped page
/// (nav / header / footer stripped) and renders markdown. Exercises the sync
/// `extract_markdown` seam directly, no network.
#[test]
fn extract_markdown_keeps_article_and_drops_chrome() {
    let html = r#"<!DOCTYPE html><html><head><title>Test Article</title></head>
<body>
<nav><a href="/">Home</a> <a href="/subscribe">Subscribe</a> <a href="/login">Log in</a></nav>
<header id="site-chrome">Newsletter signup. Cookie banner. Accept all cookies.</header>
<article>
<h1>The Real Article Title</h1>
<p>This is the first substantial paragraph of the real article body. It carries
several complete sentences of genuine prose so the readability heuristics lock
onto this block as the main content rather than navigation or boilerplate.</p>
<p>Here is a second real paragraph continuing the article with more meaningful
sentences, giving the extractor ample signal to treat this element as the article
body and discard the surrounding page furniture.</p>
</article>
<footer id="site-footer">Footer chrome and copyright notice.</footer>
</body></html>"#;

    let md = extract_markdown(html, "https://example.com/post").expect("extraction should succeed");
    assert!(
        md.contains("first substantial paragraph"),
        "the article body must survive extraction:\n{md}"
    );
    assert!(
        md.len() < html.len(),
        "extracted markdown must be smaller than the raw page"
    );
    // Every chrome region the fixture contains must be dropped, not just the
    // cookie banner: nav (Subscribe / Log in), header (newsletter / cookie), and
    // footer. A regression that strips the banner but leaks nav/footer must fail.
    let low = md.to_lowercase();
    for chrome in [
        "accept all cookies",
        "newsletter signup",
        "subscribe",
        "log in",
        "footer chrome",
    ] {
        assert!(!low.contains(chrome), "chrome {chrome:?} must be stripped:\n{md}");
    }
}

/// Non-HTML / contentless input yields an error (the caller falls through).
#[test]
fn extract_markdown_errors_on_contentless_input() {
    let result = extract_markdown("<html><body></body></html>", "https://example.com/x");
    // Either an Extract error or empty content is acceptable "fall through"
    // signal; what matters is it does not return a usable article.
    if let Ok(md) = result {
        assert!(
            md.trim().is_empty(),
            "contentless input must not yield article text: {md:?}"
        );
    }
}
