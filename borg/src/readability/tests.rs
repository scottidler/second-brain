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

    let md = extract_markdown(html, "https://example.com/post", "https://example.com/post")
        .expect("extraction should succeed");
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
    let result = extract_markdown(
        "<html><body></body></html>",
        "https://example.com/x",
        "https://example.com/x",
    );
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

/// One canned reply: path, status, an optional extra header, body.
type Route = (&'static str, u16, Option<(&'static str, String)>, String);

/// Serve `routes` on a loopback port; an unlisted path is a 404. Records every
/// requested path.
async fn serve_routes(routes: Vec<Route>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use axum::http::{StatusCode, Uri};
    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = hits.clone();
    let app = axum::Router::new().fallback(move |uri: Uri| {
        let (routes, hits) = (routes.clone(), recorded.clone());
        async move {
            hits.lock().expect("hits").push(uri.path().to_string());
            match routes.iter().find(|(path, ..)| *path == uri.path()) {
                Some((_, status, header, body)) => {
                    let mut response = (
                        StatusCode::from_u16(*status).expect("status"),
                        [("content-type", "text/html; charset=utf-8")],
                        body.clone(),
                    )
                        .into_response();
                    if let Some((name, value)) = header {
                        response
                            .headers_mut()
                            .insert(*name, value.parse().expect("header value"));
                    }
                    response
                }
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    (base, hits)
}

/// The 2026-10-22 chroma canary (cl-190dffbd): `research.trychroma.com/context-rot`
/// 301s to `www.trychroma.com/research/context-rot`, and its `/img/...` charts
/// were made absolute against the REQUESTED host, which 404s. A page reached
/// through a cross-origin redirect reports the final URL, and its root-relative
/// and path-relative images resolve against that final page.
#[tokio::test]
async fn redirected_page_reports_the_final_url_and_resolves_images_against_it() {
    let prose = "<p>This is the first substantial paragraph of the real article body. It carries \
        several complete sentences of genuine prose so the readability heuristics lock onto \
        this block as the main content rather than navigation or boilerplate.</p>";
    let page = format!(
        "<!doctype html><html><head><title>Context Rot</title></head><body><article>\
         <h1>Context Rot</h1>{prose}<p><img src=\"/img/root.png\" alt=\"root\"></p>{prose}\
         <p><img src=\"rel.png\" alt=\"rel\"></p>{prose}</article></body></html>"
    );
    let (final_host, _) = serve_routes(vec![("/research/context-rot", 200, None, page)]).await;
    let final_url = format!("{final_host}/research/context-rot");
    let (requested_host, requested_hits) = serve_routes(vec![(
        "/context-rot",
        301,
        Some(("location", final_url.clone())),
        String::new(),
    )])
    .await;
    let requested = format!("{requested_host}/context-rot");

    let Fetched::Article { markdown, page_url } = fetch_article_readable(&requested, 5).await.expect("fetch") else {
        panic!("expected an article");
    };

    assert_eq!(page_url, final_url, "the post-redirect URL, not the requested one");
    assert!(
        markdown.contains(&format!("({final_host}/img/root.png")),
        "root-relative image resolves against the final host:\n{markdown}"
    );
    assert!(
        markdown.contains(&format!("({final_host}/research/rel.png")),
        "path-relative image resolves against the final page:\n{markdown}"
    );
    assert!(
        !markdown.contains(&requested_host),
        "nothing resolves against the requested host:\n{markdown}"
    );
    assert_eq!(*requested_hits.lock().expect("hits"), vec!["/context-rot".to_string()]);
}

/// An unredirected fetch reports the requested URL as the page URL.
#[tokio::test]
async fn unredirected_page_reports_the_requested_url() {
    let page = "<!doctype html><html><head><title>T</title></head><body><article><h1>T</h1>\
        <p>This is the first substantial paragraph of the real article body. It carries several \
        complete sentences of genuine prose so the readability heuristics lock onto it.</p>\
        </article></body></html>"
        .to_string();
    let (host, _) = serve_routes(vec![("/post", 200, None, page)]).await;
    let url = format!("{host}/post");
    let Fetched::Article { page_url, .. } = fetch_article_readable(&url, 5).await.expect("fetch") else {
        panic!("expected an article");
    };
    assert_eq!(page_url, url);
}
