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
        place: Place::Photo,
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
        entity_ranges: Vec::new(),
    };
    let md = render_blocks(
        &[
            block("unstyled", "before"),
            block("atomic", " "),
            block("unstyled", "   "),
            block("ordered-list-item", "first"),
            block("ordered-list-item", "second"),
            block("unstyled", "after"),
            block("ordered-list-item", "restart"),
        ],
        2,
        &[],
    );
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
    assert!(
        md.contains("\n### Introduction\n"),
        "quoted Article headings nest under the quote:\n{md}"
    );
    assert!(!md.contains("\n## Introduction\n"), "{md}");
}

#[tokio::test]
async fn x_post_quote_with_empty_text_and_article_blocks_has_text() {
    let mut post = fetch_fixture(VIC).await.expect("fetch");
    let mut article = fetch_fixture(ARTICLE).await.expect("fetch");
    article.text = String::new();
    post.text = String::new();
    post.quote = Some(Box::new(article));
    assert!(has_text(&post), "a quote with no text but Article blocks is content");
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

// ---------------------------------------------------------------------------
// X Article figures (docs/design/2026-10-22-capture-all-diagrams.md).
// `article.json` is trimmed from the live UberEng Article (status
// 2106071967619322330) with `entityRanges`, `entityMap` and `media_entities`.
// ---------------------------------------------------------------------------

fn article_post() -> Post {
    let envelope: Envelope = serde_json::from_str(ARTICLE).expect("fixture parses");
    envelope.tweet.expect("tweet")
}

/// Criterion 1: six figures, in block order, each with its caption (Figure
/// 1-5; the sixth, the Omni MCP diagram, has none in the source).
#[test]
fn x_article_ubereng_yields_six_figures_in_block_order_with_captions() {
    let figures = article_figures(&article_post());

    assert_eq!(figures.len(), 6, "{figures:#?}");
    let blocks: Vec<usize> = figures.iter().map(|f| f.block).collect();
    assert_eq!(blocks, vec![5, 20, 41, 42, 46, 84]);
    let captions: Vec<Option<&str>> = figures.iter().map(|f| f.caption.as_deref()).collect();
    assert_eq!(
        captions,
        vec![
            Some("Figure 1: MCP Gateway (APIs as tools)."),
            Some("Figure 2: Auto Crawler."),
            Some("Figure 3: MCP Registry UI."),
            Some("Figure 4: MCP tool UI."),
            Some("Figure 5: MCP Gateway Data Plane."),
            None,
        ]
    );
    assert_eq!(figures[1].url, "https://pbs.twimg.com/media/HTo9FH8bQAAiYk8.jpg");
    assert!(
        figures
            .iter()
            .all(|f| f.url.starts_with("https://pbs.twimg.com/media/")),
        "{figures:#?}"
    );
}

/// Criterion 2: an atomic block with no matching entity (and one with no
/// entity range, and one naming a LINK) is skipped; the others resolve and
/// the render still succeeds with the skipped block's neighbors intact.
#[test]
fn x_article_unresolvable_atomic_block_is_skipped_and_render_succeeds() {
    let mut post = article_post();
    let content = post.article.as_mut().and_then(|a| a.content.as_mut()).expect("content");
    content.blocks[5].entity_ranges[0].key = Some("999".to_string());
    content.blocks[20].entity_ranges.clear();
    content.blocks[41].entity_ranges[0].key = Some("8".to_string()); // a LINK entity

    let figures = article_figures(&post);
    let blocks: Vec<usize> = figures.iter().map(|f| f.block).collect();
    assert_eq!(blocks, vec![42, 46, 84], "{figures:#?}");

    let images: Vec<DescribedImage> = (0..figures.len())
        .map(|n| DescribedImage {
            owner: Owner::Post,
            place: Place::Figure(n),
            section: format!("#### Description\n\nfigure {n}"),
        })
        .collect();
    let (_, md) = render(&post, &images);
    assert!(md.contains("We built the MCP Gateway to address this need"), "{md}");
    assert!(md.contains("In this blog, we review the design"), "{md}");
    assert!(
        md.contains("*Figure 4: MCP tool UI.*\n\n#### Description\n\nfigure 0"),
        "{md}"
    );
    assert!(!md.contains("Figure 1: MCP Gateway"), "skipped figure leaked:\n{md}");
}

/// A described figure renders at its atomic block, between the paragraphs
/// around it, with its caption; an undescribed one leaves nothing behind.
#[test]
fn x_article_figure_renders_at_its_atomic_block() {
    let post = article_post();
    let images = [
        DescribedImage {
            owner: Owner::Post,
            place: Place::Figure(1),
            section: "#### Description\n\nThe crawler.".to_string(),
        },
        DescribedImage {
            owner: Owner::Quote,
            place: Place::Figure(0),
            section: "#### Description\n\nnot this post's".to_string(),
        },
    ];
    let (_, md) = render(&post, &images);

    let figure = md
        .find("### Image 1\n\n*Figure 2: Auto Crawler.*\n\n#### Description\n\nThe crawler.")
        .unwrap_or_else(|| panic!("figure at its block:\n{md}"));
    let before = md.find("This shared foundation enables MCP").expect("block 19");
    let after = md.find("Discovery for IDL-Backed Services").expect("block 21");
    assert!(before < figure && figure < after, "{md}");
    assert!(
        !md.contains("Figure 1:"),
        "an undescribed figure renders nothing:\n{md}"
    );
    assert!(
        !md.contains("not this post's"),
        "a quote's figure stays off the post:\n{md}"
    );
}

/// A key that is neither a string nor a number reads as no key: the block
/// it names is skipped by `article_figures`, never a parse error (this test
/// pinned a parse error before the implementation-audit fold-in).
#[test]
fn x_article_entity_key_non_scalar_reads_as_no_key() {
    let range = serde_json::from_str::<EntityRange>(r#"{"key": [1]}"#).expect("array key parses");
    assert_eq!(range.key, None);
    let range = serde_json::from_str::<EntityRange>(r#"{"key": 7}"#).expect("number key parses");
    assert_eq!(range.key.as_deref(), Some("7"));
}

/// A malformed entity in the raw fxtwitter JSON (an empty entity range, an
/// entity whose value is not an object, a media item and a media entity with
/// no id) parses, skips only the blocks it breaks, and the rest resolve.
#[test]
fn x_article_malformed_entities_in_raw_json_skip_their_block_not_the_post() {
    let mut raw: serde_json::Value = serde_json::from_str(ARTICLE).expect("fixture json");
    let article = &mut raw["tweet"]["article"];
    let blocks = article["content"]["blocks"].as_array_mut().expect("blocks");
    blocks[5]["entityRanges"] = serde_json::json!([{}]);
    // Block 20's entity: key 1 in the fixture's numbering, located by search.
    let key20 = blocks[20]["entityRanges"][0]["key"].clone();
    let map = article["content"]["entityMap"].as_array_mut().expect("entityMap");
    let at = map
        .iter()
        .position(|e| e["key"].to_string().trim_matches('"') == key20.to_string().trim_matches('"'))
        .expect("block 20's entity");
    map[at]["value"] = serde_json::json!("not an object");
    map.push(serde_json::json!({"key": {"nested": true}, "value": {"type": "MEDIA", "data": {"mediaItems": [{}]}}}));
    let media = article["media_entities"].as_array_mut().expect("media_entities");
    media.push(serde_json::json!({"media_info": {"original_img_url": "https://pbs.twimg.com/media/orphan.jpg"}}));

    let envelope: Envelope = serde_json::from_value(raw).expect("a malformed entity never fails the post");
    let post = envelope.tweet.expect("tweet");
    let figures = article_figures(&post);
    let blocks: Vec<usize> = figures.iter().map(|f| f.block).collect();
    assert_eq!(blocks, vec![41, 42, 46, 84], "{figures:#?}");
    assert!(figures.iter().all(|f| !f.url.contains("orphan")), "{figures:#?}");
}
