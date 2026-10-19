use super::*;
use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;

const VIC: &str = include_str!("vic.json");
const ARTICLE: &str = include_str!("article.json");
const LONG: &str = include_str!("long.json");
const NOT_FOUND: &str = include_str!("not-found.json");
const MALFORMED_ID_HTML: &str = include_str!("malformed-id.html");

/// Page chrome and counters the logged-out x.com scrape carried into notes.
const CHROME: &[&str] = &[
    "Log in",
    "Trending",
    "Relevant people",
    "Views",
    "53910",
    "571",
    "Sign up",
];

/// A stub fxtwitter serving `body` for every `/status/<id>`; returns its base URL.
/// Like the live API, it answers 401 to a request with no `User-Agent`.
async fn serve(status: StatusCode, content_type: &'static str, body: &'static str, delay: Duration) -> String {
    let app = Router::new().route(
        "/status/{id}",
        get(move |headers: axum::http::HeaderMap| async move {
            tokio::time::sleep(delay).await;
            if headers.get("user-agent").is_none_or(|ua| ua.is_empty()) {
                return (StatusCode::UNAUTHORIZED, [("content-type", "application/json")], "{}").into_response();
            }
            (status, [("content-type", content_type)], body).into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    format!("http://127.0.0.1:{port}")
}

async fn fetch_fixture(body: &'static str) -> Result<Post> {
    let base = serve(StatusCode::OK, "application/json", body, Duration::ZERO).await;
    fetch_post(&base, 2112024357551931564, 10).await
}

fn photo(owner: Owner, section: &str) -> DescribedImage {
    DescribedImage {
        owner,
        section: section.to_string(),
    }
}

#[tokio::test]
async fn x_post_renders_post_and_quote() {
    let post = fetch_fixture(VIC).await.expect("fetch");
    let (title, md) = render(&post, &[]);

    assert_eq!(title, "@VicVijayakumar on X");
    assert!(md.starts_with("# Post by Vic 📠 (@VicVijayakumar)\n\n"), "{md}");
    assert!(md.contains("every single org is building this loop right now."), "{md}");
    assert!(
        md.contains("## Quoted post by boris (@boristane)\n\nthe only sane code review loop"),
        "{md}"
    );
    assert!(md.contains("@VicVijayakumar") && md.contains("@boristane"), "{md}");
    for chrome in ["Log in", "Trending", "Relevant people", "Views"] {
        assert!(!md.contains(chrome), "{chrome:?} in the render:\n{md}");
    }
}

#[tokio::test]
async fn x_post_renders_no_chrome_or_counts() {
    for fixture in [VIC, LONG, ARTICLE] {
        let post = fetch_fixture(fixture).await.expect("fetch");
        let (_, md) = render(&post, &[]);
        for chrome in CHROME {
            assert!(!md.contains(chrome), "{chrome:?} in the render:\n{md}");
        }
        assert_eq!(
            vault::wikilink::parse(&md).count(),
            0,
            "no wikilink in the render:\n{md}"
        );
    }
}

#[tokio::test]
async fn x_post_renders_long_post_in_full() {
    let post = fetch_fixture(LONG).await.expect("fetch");
    let (title, md) = render(&post, &[]);

    assert_eq!(title, "@doodlestein on X");
    assert!(md.starts_with("# Post by Jeffrey Emanuel (@doodlestein)\n\n"), "{md}");
    assert!(md.contains("compile your projects to WebAssembly"), "{md}");
    assert!(md.len() > 7000, "long post truncated to {} bytes", md.len());
}

#[tokio::test]
async fn x_post_renders_article_blocks() {
    let post = fetch_fixture(ARTICLE).await.expect("fetch");
    let (title, md) = render(&post, &[]);

    assert_eq!(title, "@UberEng on X");
    assert!(
        md.starts_with("# Article by Uber Engineering (@UberEng): Designing MCP Gateway"),
        "{md}"
    );
    assert!(!md.contains("x.com/i/article"), "bare article link leaked:\n{md}");
    assert!(md.contains("\n## Introduction\n"), "header-one becomes ##:\n{md}");
    assert!(md.contains("\n## The Gateway\n"), "header-two becomes ##:\n{md}");
    assert!(
        md.contains("- Creating or updating MCP server representations\n- Generating or fetching tool definitions"),
        "list items stay adjacent:\n{md}"
    );
    assert!(
        md.contains("MCP Gateway follows a microservice-based architecture"),
        "{md}"
    );
    let order = |needle: &str| md.find(needle).unwrap_or_else(|| panic!("{needle:?} missing:\n{md}"));
    assert!(order("Introduction") < order("The Gateway") && order("The Gateway") < order("Control Plane"));
}

#[test]
fn x_post_article_drops_atomic_and_empty_blocks_and_numbers_lists() {
    let block = |kind: &str, text: &str| Block {
        kind: kind.to_string(),
        text: text.to_string(),
    };
    let md = render_blocks(&[
        block("unstyled", "before"),
        block("atomic", " "),
        block("unstyled", "   "),
        block("ordered-list-item", "first"),
        block("ordered-list-item", "second"),
        block("unstyled", "after"),
        block("ordered-list-item", "restart"),
    ]);
    assert_eq!(md, "before\n\n1. first\n2. second\n\nafter\n\n1. restart\n\n");
}

#[tokio::test]
async fn x_post_images_land_under_their_owning_post() {
    let post = fetch_fixture(VIC).await.expect("fetch");
    let images = [
        photo(Owner::Quote, "#### Description\nA flowchart."),
        photo(Owner::Post, "#### OCR Text\nmine"),
        photo(Owner::Quote, "#### OCR Text\nsecond"),
    ];
    let (_, md) = render(&post, &images);

    let quote_at = md.find("## Quoted post by boris").expect("quote heading");
    let own = md
        .find("### Image 1 (in @VicVijayakumar's post)\n\n#### OCR Text\nmine")
        .expect("own image");
    let first = md
        .find("### Image 1 (in @boristane's post)\n\n#### Description\nA flowchart.")
        .expect("quote image 1");
    let second = md
        .find("### Image 2 (in @boristane's post)\n\n#### OCR Text\nsecond")
        .expect("quote image 2");
    assert!(own < quote_at && quote_at < first && first < second, "{md}");
}

#[tokio::test]
async fn x_post_404_is_err() {
    let base = serve(StatusCode::NOT_FOUND, "application/json", NOT_FOUND, Duration::ZERO).await;
    let err = fetch_post(&base, 1, 10).await.expect_err("404 must fail");
    assert!(format!("{err:#}").contains("404"), "{err:#}");
}

#[tokio::test]
async fn x_post_html_body_is_err() {
    let base = serve(
        StatusCode::OK,
        "text/html; charset=utf-8",
        MALFORMED_ID_HTML,
        Duration::ZERO,
    )
    .await;
    let err = fetch_post(&base, 1, 10).await.expect_err("HTML must fail");
    assert!(format!("{err:#}").contains("not JSON"), "{err:#}");
}

#[tokio::test]
async fn x_post_tweet_null_is_err() {
    let base = serve(
        StatusCode::OK,
        "application/json",
        r#"{"code":200,"message":"OK","tweet":null}"#,
        Duration::ZERO,
    )
    .await;
    let err = fetch_post(&base, 1, 10).await.expect_err("null tweet must fail");
    assert!(format!("{err:#}").contains("no tweet"), "{err:#}");
}

#[tokio::test]
async fn x_post_malformed_json_is_err() {
    let base = serve(
        StatusCode::OK,
        "application/json",
        "{\"tweet\": {\"id\": ",
        Duration::ZERO,
    )
    .await;
    let err = fetch_post(&base, 1, 10).await.expect_err("malformed JSON must fail");
    assert!(format!("{err:#}").contains("parse fxtwitter response"), "{err:#}");
}

#[tokio::test]
async fn x_post_timeout_is_err() {
    let base = serve(StatusCode::OK, "application/json", VIC, Duration::from_secs(5)).await;
    let started = std::time::Instant::now();
    let err = fetch_post(&base, 1, 1).await.expect_err("a slow API must time out");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "did not honor the 1s timeout"
    );
    assert!(format!("{err:#}").contains("reach fxtwitter API"), "{err:#}");
}

#[test]
fn x_post_status_id_ignores_query_and_host_variant() {
    let id = Some(2112024357551931564);
    for url in [
        "https://x.com/VicVijayakumar/status/2112024357551931564",
        "https://x.com/VicVijayakumar/status/2112024357551931564?s=46",
        "https://twitter.com/VicVijayakumar/status/2112024357551931564",
        "https://www.twitter.com/VicVijayakumar/status/2112024357551931564",
        "https://mobile.twitter.com/VicVijayakumar/status/2112024357551931564?s=20&t=abc",
        "https://x.com/VicVijayakumar/status/2112024357551931564/photo/1",
    ] {
        assert_eq!(status_id(url), id, "{url}");
    }
}

#[test]
fn x_post_status_id_rejects_non_status_urls() {
    for url in [
        "https://x.com/VicVijayakumar",
        "https://x.com/i/article/2106059544376066048",
        "https://x.com/VicVijayakumar/status/notanumber",
        "https://x.com/VicVijayakumar/likes/2112024357551931564",
        "https://notx.com/VicVijayakumar/status/2112024357551931564",
        "https://example.com/x.com/a/status/1",
        "not a url",
    ] {
        assert_eq!(status_id(url), None, "{url}");
    }
}

#[tokio::test]
async fn x_post_title_passes_quality_gate() {
    for fixture in [VIC, LONG, ARTICLE] {
        let post = fetch_fixture(fixture).await.expect("fetch");
        let (title, md) = render(&post, &[]);
        assert_eq!(crate::quality::detect_blocked_content(&md, &title), None, "{title}");
    }
}

#[tokio::test]
async fn x_post_photos_are_post_then_quote() {
    let post = fetch_fixture(VIC).await.expect("fetch");
    let photos = photos(&post);
    assert_eq!(photos.len(), 1, "vic's only photo is on the quoted post");
    assert_eq!(photos[0].0, Owner::Quote);
    assert!(photos[0].1.url.starts_with("https://pbs.twimg.com/media/"));
}

#[tokio::test]
async fn x_post_has_text_reads_post_quote_and_article() {
    for fixture in [VIC, LONG, ARTICLE] {
        let post = fetch_fixture(fixture).await.expect("fetch");
        assert!(has_text(&post));
    }
    let mut post = fetch_fixture(VIC).await.expect("fetch");
    post.text = "  ".to_string();
    assert!(has_text(&post), "the quote's text still counts");
    if let Some(quote) = post.quote.as_mut() {
        quote.text = String::new();
    }
    assert!(!has_text(&post), "no post, quote or article text");
}

#[tokio::test]
async fn x_post_bare_article_link_with_no_blocks_has_no_text() {
    let mut post = fetch_fixture(ARTICLE).await.expect("fetch");
    post.text = "https://x.com/i/article/123".to_string();
    if let Some(article) = post.article.as_mut() {
        article.content = None;
    }
    assert!(!has_text(&post));
}

#[tokio::test]
async fn x_post_renders_quoted_article_blocks() {
    let mut post = fetch_fixture(VIC).await.expect("fetch");
    let article = fetch_fixture(ARTICLE).await.expect("fetch");
    post.text = String::new();
    post.quote = Some(Box::new(article));

    assert!(has_text(&post), "a quoted Article's blocks count as text");
    let (_, md) = render(&post, &[]);
    assert!(
        md.contains("## Quoted article by Uber Engineering (@UberEng): Designing MCP Gateway"),
        "{md}"
    );
    assert!(
        md.contains("MCP Gateway follows a microservice-based architecture"),
        "{md}"
    );
    assert!(!md.contains("x.com/i/article"), "bare article link leaked:\n{md}");
}

#[tokio::test]
async fn x_post_fetch_sends_descriptive_user_agent() {
    let app = Router::new().route(
        "/status/{id}",
        get(|headers: axum::http::HeaderMap| async move {
            let ua = headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let status = if ua == USER_AGENT { StatusCode::OK } else { StatusCode::UNAUTHORIZED };
            (status, [("content-type", "application/json")], VIC).into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });

    let post = fetch_post(&format!("http://127.0.0.1:{port}"), 2112024357551931564, 10).await;
    assert!(post.is_ok(), "fxtwitter 401s without a User-Agent: {post:?}");
}
