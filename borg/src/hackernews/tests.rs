use super::*;
use axum::Router;
use axum::response::IntoResponse;
use axum::routing::get;

/// A story with a nested reply and a deleted comment that still has a live
/// reply, shaped like `hn.algolia.com/api/v1/items/47759035`.
const ITEM_JSON: &str = r#"{
  "id": 47759035, "type": "story", "author": "agentseal", "points": 112,
  "title": "Show HN: CodeBurn – Analyze Claude Code token usage by task",
  "url": "https://github.com/AgentSeal/codeburn", "text": null,
  "children": [
    {"id": 1, "type": "comment", "author": "giancarlostoro", "points": null, "title": null, "url": null,
     "text": "&gt; built with Ink<p>Just like Claude Code btw. I&#x27;m building <a href=\"https://example.com\">my own</a>.",
     "children": [
       {"id": 2, "type": "comment", "author": "agentseal", "points": null, "title": null, "url": null,
        "text": "yeah Ink, it was the fastest path", "children": []}
     ]},
    {"id": 3, "type": "comment", "author": null, "points": null, "title": null, "url": null, "text": null,
     "children": [
       {"id": 4, "type": "comment", "author": "kristianp", "points": null, "title": null, "url": null,
        "text": "That’s really helpful – token usage matters.", "children": []}
     ]}
  ]
}"#;

/// Chrome the page fetch carried into the 12 live HN notes.
const PAGE_CHROME: &[&str] = &[
    "javascript:void(0)",
    "[–]",
    "vote?id=",
    "hide?id=",
    "fave?id=",
    "from?site=",
    "user?id=",
    "s.gif",
    "y18.svg",
    "newsguidelines",
    "[parent]",
    "[next]",
    "[prev]",
    "[root]",
];

#[test]
fn item_id_reads_only_hn_discussion_urls() {
    assert_eq!(item_id("https://news.ycombinator.com/item?id=47759035"), Some(47759035));
    assert_eq!(
        item_id("https://news.ycombinator.com/item?p=2&id=47759035"),
        Some(47759035)
    );
    assert_eq!(item_id("https://news.ycombinator.com/news"), None);
    assert_eq!(item_id("https://news.ycombinator.com/user?id=agentseal"), None);
    assert_eq!(item_id("https://example.com/item?id=1"), None);
}

#[test]
fn render_thread_keeps_the_conversation() {
    let item: Item = serde_json::from_str(ITEM_JSON).expect("parse");
    let (title, md) = render_thread(&item).expect("render");

    assert_eq!(title, "Show HN: CodeBurn – Analyze Claude Code token usage by task");
    assert!(md.starts_with(&format!(
        "Title: {title}\nURL Source: https://news.ycombinator.com/item?id=47759035\n"
    )));
    assert!(md.contains("Posted by agentseal (112 points)"), "{md}");
    assert!(md.contains("Link: https://github.com/AgentSeal/codeburn"), "{md}");
    assert!(md.contains("**giancarlostoro** (replying to agentseal):"), "{md}");
    assert!(md.contains("I'm building [my own](https://example.com)."), "{md}");
    assert!(
        md.contains("**agentseal** (replying to giancarlostoro):\n\nyeah Ink"),
        "{md}"
    );
    assert!(md.contains("**kristianp** (replying to [deleted]):"), "{md}");
    assert!(md.contains("That’s really helpful – token usage matters."), "{md}");
}

/// The regression guard: a thread fetched through the items API carries no
/// page chrome and no wikilink, so `## Transcript` cannot hold `[[–]]`.
#[tokio::test]
async fn fetch_thread_body_has_no_page_chrome() {
    let app = Router::new().route(
        "/api/v1/items/47759035",
        get(|| async { ([("content-type", "application/json")], ITEM_JSON).into_response() }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });

    let base = format!("http://127.0.0.1:{port}/api/v1/items/");
    let (_, md) = fetch_thread(&base, 47759035, 10).await.expect("fetch");

    for chrome in PAGE_CHROME {
        assert!(!md.contains(chrome), "page chrome {chrome:?} in the thread body:\n{md}");
    }
    assert_eq!(vault::wikilink::parse(&md).count(), 0, "no wikilink in the body:\n{md}");
    assert!(md.contains("yeah Ink, it was the fastest path"), "{md}");
}
