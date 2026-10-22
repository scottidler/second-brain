//! X Article figures through the classified path
//! (`docs/design/2026-10-22-capture-all-diagrams.md`): the UberEng
//! fixture served by a stub that plays fxtwitter, the image host, and a
//! vision API that answers classify and describe calls separately and labels
//! each figure by the bytes it was sent.

use super::*;
use base64::Engine;

const ARTICLE: &str = include_str!("../../../x/article.json");
/// Figure 3's media name: the stub labels it `title-card`.
const TITLE_CARD: &str = "HTo9NuKbEAA6ECf";

#[derive(Clone)]
struct Vision {
    /// Media names classified `title-card`; every other figure is `flowchart`.
    title_cards: Vec<&'static str>,
    /// Media names whose classify call answers 500.
    classify_errors: Vec<&'static str>,
}

#[derive(Clone, Default)]
struct Calls(Arc<Mutex<Vec<String>>>);

impl Calls {
    fn of(&self, kind: &str) -> usize {
        self.0
            .lock()
            .expect("calls")
            .iter()
            .filter(|c| c.as_str() == kind)
            .count()
    }
}

/// Serve fxtwitter (the UberEng fixture, figure URLs pointed back here),
/// `/media/<name>` (a JPEG whose bytes are its own name), and `/v1/messages`.
async fn serve_article(vision: Vision) -> (String, Calls) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
    let status = ARTICLE.replace("https://pbs.twimg.com/media/", &format!("{base}/media/"));
    let calls = Calls::default();
    let recorded = calls.clone();
    let app = Router::new().fallback(move |uri: Uri, body: String| {
        let (status, vision, calls) = (status.clone(), vision.clone(), recorded.clone());
        async move {
            let path = uri.path().to_string();
            let reply = |code: u16, content_type: &'static str, body: String| {
                let code = StatusCode::from_u16(code).expect("status");
                (code, [("content-type", content_type)], body).into_response()
            };
            if path.starts_with("/status/") {
                return reply(200, "application/json", status);
            }
            if let Some(name) = path.strip_prefix("/media/") {
                return reply(200, "image/jpeg", name.to_string());
            }
            let request: serde_json::Value = serde_json::from_str(&body).expect("vision body");
            let content = &request["messages"][0]["content"];
            let image = base64::engine::general_purpose::STANDARD
                .decode(content[0]["source"]["data"].as_str().expect("image data"))
                .expect("base64");
            let image = String::from_utf8(image).expect("utf8 name");
            let prompt = content[1]["text"].as_str().expect("prompt");
            if !prompt.contains("CATEGORY:") {
                calls.0.lock().expect("calls").push("describe".into());
                return reply(200, "application/json", VISION_OK.to_string());
            }
            calls.0.lock().expect("calls").push("classify".into());
            if vision.classify_errors.iter().any(|n| image.contains(n)) {
                return reply(500, "application/json", VISION_500.to_string());
            }
            let category = if vision.title_cards.iter().any(|n| image.contains(n)) {
                "title-card"
            } else {
                "flowchart"
            };
            let text = format!("CATEGORY: {category}\nCONFIDENCE: 0.9");
            let body = serde_json::json!({"content": [{"type": "text", "text": text}]});
            reply(200, "application/json", body.to_string())
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    (base, calls)
}

fn vision(title_cards: Vec<&'static str>, classify_errors: Vec<&'static str>) -> Vision {
    Vision {
        title_cards,
        classify_errors,
    }
}

fn holds(figure: &figures::Figure, name: &str) -> bool {
    String::from_utf8_lossy(&figure.bytes).contains(name)
}

/// Criterion 3: with stubbed vision labeling Figure 3 `title-card`, that
/// figure is not kept; the other five are classified, described, placed at
/// their atomic blocks, and stored under `## Images` in block order.
#[tokio::test]
async fn x_article_title_card_figure_is_not_kept() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, calls) = serve_article(vision(vec![TITLE_CARD], vec![])).await;
    let config = config(dir.path());

    let post = fetch(&base, &config).await.expect("fetch");

    assert_eq!(calls.of("classify"), 6, "every figure is classified");
    assert_eq!(calls.of("describe"), 5, "only kept figures are described");
    assert!(!post.degraded, "a not-a-diagram verdict does not degrade");
    let kept = &post.figures.figures;
    assert_eq!(kept.len(), 5);
    assert!(
        !kept.iter().any(|f| holds(f, TITLE_CARD)),
        "the title-card figure is kept"
    );
    let provenance: Vec<&str> = kept.iter().map(|f| f.provenance.as_str()).collect();
    assert_eq!(
        provenance,
        vec![
            "Figure 1: MCP Gateway (APIs as tools).",
            "Figure 2: Auto Crawler.",
            "Figure 4: MCP tool UI.",
            "Figure 5: MCP Gateway Data Plane.",
            "In @UberEng's article",
        ]
    );
    assert!(
        !post.markdown.contains("Figure 3: MCP Registry UI."),
        "{}",
        post.markdown
    );
    let at = |needle: &str| {
        post.markdown
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing:\n{}", post.markdown))
    };
    assert!(
        at("This shared foundation enables MCP") < at("*Figure 2: Auto Crawler.*")
            && at("*Figure 2: Auto Crawler.*") < at("Discovery for IDL-Backed Services"),
        "the figure sits at its atomic block:\n{}",
        post.markdown
    );

    let published = publish_post(&post, &config, dir.path());
    let (_, images) = published.note.split_once("## Images\n").expect("## Images");
    assert_eq!(images.lines().filter(|l| l.starts_with("![[")).count(), 5);
    assert!(images.contains("*Figure 2: Auto Crawler.*"), "{}", published.note);
    assert!(!images.contains("Figure 3"), "{}", published.note);
    assert_eq!(published.images_frontmatter.len(), 5);
    for path in &published.images_frontmatter {
        assert!(published.vault.join(path).is_file(), "{path} missing");
    }
}

/// A classify error drops that figure and degrades the fetch; the rest stand.
#[tokio::test]
async fn x_article_classify_error_drops_the_figure_and_degrades() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, calls) = serve_article(vision(vec![], vec![TITLE_CARD])).await;
    let config = config(dir.path());

    let post = fetch(&base, &config).await.expect("fetch");

    assert!(post.degraded, "a classify error degrades");
    assert!(post.figures.degraded);
    assert_eq!(post.figures.figures.len(), 5);
    assert!(!post.figures.figures.iter().any(|f| holds(f, TITLE_CARD)));
    assert_eq!(calls.of("describe"), 5, "the failed figure is never described");
}

/// The cap runs before describe: 6 classified, `max-per-note` described and
/// kept, in source order; the cap is truncation, not a degradation.
#[tokio::test]
async fn x_article_cap_runs_before_describe() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, calls) = serve_article(vision(vec![], vec![])).await;
    let mut config = config(dir.path());
    config.images.max_per_note = 2;

    let post = fetch(&base, &config).await.expect("fetch");

    assert_eq!(calls.of("classify"), 6);
    assert_eq!(calls.of("describe"), 2);
    assert!(!post.degraded);
    let provenance: Vec<&str> = post.figures.figures.iter().map(|f| f.provenance.as_str()).collect();
    assert_eq!(
        provenance,
        vec!["Figure 1: MCP Gateway (APIs as tools).", "Figure 2: Auto Crawler."]
    );
}

/// `content-filter.enabled: false` keeps every figure unclassified.
#[tokio::test]
async fn x_article_filter_disabled_keeps_every_figure_unclassified() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, calls) = serve_article(vision(vec![TITLE_CARD], vec![])).await;
    let mut config = config(dir.path());
    config.images.content_filter.enabled = false;

    let post = fetch(&base, &config).await.expect("fetch");

    assert_eq!(calls.of("classify"), 0);
    assert_eq!(post.figures.figures.len(), 6);
}

/// `images.enabled: false` restores today's behavior: Article figures are not
/// fetched, classified, described, or placed.
#[tokio::test]
async fn x_article_images_disabled_touches_no_figure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, calls) = serve_article(vision(vec![], vec![])).await;
    let mut config = config(dir.path());
    config.images.enabled = false;

    let post = fetch(&base, &config).await.expect("fetch");

    assert_eq!(calls.of("classify") + calls.of("describe"), 0);
    assert!(post.figures.figures.is_empty());
    assert!(!post.markdown.contains("### Image"), "{}", post.markdown);
}
