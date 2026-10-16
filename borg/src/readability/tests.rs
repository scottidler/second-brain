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

#[test]
fn max_nesting_depth_counts_open_elements_only() {
    assert_eq!(max_nesting_depth("<p>a</p>"), 1);
    assert_eq!(max_nesting_depth("<div><p>a<br>b<img src=x/></p></div>"), 2);
    assert_eq!(
        max_nesting_depth("<!doctype html><!-- c --><ul><li>a</li><li>b</li></ul>"),
        2
    );
    assert_eq!(max_nesting_depth("plain text, no tags"), 0);
}

/// A PDF parsed as HTML nested ~2,400 deep and `htmd`'s recursion overflowed
/// the stack, aborting the daemon. Deep input must fail this one conversion.
/// Without the guard this test aborts the whole test binary.
#[test]
fn render_markdown_refuses_pathologically_deep_html() {
    let depth = 3_000;
    let html = format!("{}x{}", "<span>".repeat(depth), "</span>".repeat(depth));
    let err = render_markdown(&html, "https://example.com/deep").expect_err("deep html must be refused");
    assert!(
        matches!(
            err,
            ReadableError::TooDeep {
                depth: 3_000,
                limit: MAX_NESTING_DEPTH,
                ..
            }
        ),
        "got {err:?}"
    );
    assert_eq!(
        render_markdown("<p>ok</p>", "https://example.com/").expect("shallow"),
        "ok"
    );
}

#[test]
fn only_html_content_types_go_to_readability() {
    assert!(is_html_content_type(Some("text/html; charset=utf-8")));
    assert!(is_html_content_type(Some("application/xhtml+xml")));
    assert!(is_html_content_type(None));
    assert!(!is_html_content_type(Some("application/pdf")));
    assert!(!is_html_content_type(Some("image/png")));
}

#[test]
fn pdf_is_recognised_by_content_type_or_magic_bytes() {
    let pdf = b"%PDF-1.7\n%binary";
    assert_eq!(body_kind(Some("application/pdf"), b""), BodyKind::Pdf);
    assert_eq!(body_kind(Some("Application/PDF; qs=0.1"), pdf), BodyKind::Pdf);
    assert_eq!(body_kind(Some("text/html"), pdf), BodyKind::Pdf, "mislabeled PDF");
    assert_eq!(body_kind(None, pdf), BodyKind::Pdf, "unlabeled PDF");
    assert_eq!(body_kind(Some("text/html"), b"<html></html>"), BodyKind::Html);
    assert_eq!(body_kind(None, b"<p>x</p>"), BodyKind::Html);
    assert_eq!(body_kind(Some("image/png"), b"\x89PNG"), BodyKind::Other);
}

/// Serve one canned HTTP response on a loopback port and return its base URL.
async fn serve_once(content_type: &'static str, body: &'static [u8]) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        sock.write_all(head.as_bytes()).await.expect("head");
        sock.write_all(body).await.expect("body");
    });
    format!("http://{addr}")
}

/// The 2026-10-15 crash: a PDF behind a query-string URL went down the HTML
/// path. It must come back as PDF bytes, whatever the URL path looks like.
#[tokio::test]
async fn query_string_pdf_url_is_fetched_as_pdf() {
    let base = serve_once("application/pdf", b"%PDF-1.7\nbody").await;
    let url = format!("{base}/download?token=abc&id=42");
    match fetch_article_readable(&url, 5).await.expect("fetch") {
        Fetched::Pdf(bytes) => assert!(bytes.starts_with(PDF_MAGIC)),
        other => panic!("expected a PDF, got {other:?}"),
    }
}

#[tokio::test]
async fn pdf_served_as_html_is_fetched_as_pdf() {
    let base = serve_once("text/html; charset=utf-8", b"%PDF-1.4\nbody").await;
    match fetch_article_readable(&format!("{base}/paper"), 5)
        .await
        .expect("fetch")
    {
        Fetched::Pdf(_) => {}
        other => panic!("expected a PDF, got {other:?}"),
    }
}
