//! Web-article figures (`docs/design/2026-10-22-capture-all-diagrams.md`):
//! a stub serves the article page, its images, Jina Reader, and a
//! vision API that answers classify and describe calls separately and labels
//! each image by the id painted into its pixels. The fetch runs through the
//! real readable (or Jina) handler, then `article::distill_article` (figures,
//! splice, distill with no fabric binary, so the fallback distill carries the
//! spliced markdown as its transcript), then plan -> render ->
//! `publish_url_note`, the steps `process_url_inner` runs.

use super::x::{Published, publish_figures};
use super::*;
use crate::pipeline::article::{ArticleStage, distill_article};
use axum::Router;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use base64::Engine;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

const VISION_500: &str = r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#;

/// One paragraph of article prose, a single long line.
const PROSE: &str = "The gateway sits between every agent and the services it calls, translating \
    tool requests into authenticated RPCs and recording each call for audit and replay.";

/// What the stub's vision API says about image `id` when asked to classify it.
#[derive(Clone)]
enum Label {
    Category(&'static str),
    Error,
}

#[derive(Clone)]
struct Stub {
    /// The article HTML served at `/article`, `{base}` replaced.
    page: String,
    /// The markdown Jina Reader returns, `{base}` replaced.
    jina: String,
    /// Classify verdict per image id; an unlisted id is `flowchart`.
    labels: HashMap<u8, Label>,
    /// The `TEXT:` the describe call returns.
    describe_text: String,
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

/// A 300px PNG whose red channel is `id`: distinct bytes per image (no
/// dedupe collision), over `min-side-px`, and readable back by the stub.
fn png(id: u8) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(300, 300, image::Rgb([id, 50, 50])))
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

fn image_id(bytes: &[u8]) -> u8 {
    image::load_from_memory(bytes)
        .expect("decode")
        .to_rgb8()
        .get_pixel(0, 0)[0]
}

async fn serve(stub: Stub) -> (String, Calls) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
    let page = stub.page.replace("{base}", &base);
    let jina = stub.jina.replace("{base}", &base);
    let calls = Calls::default();
    let recorded = calls.clone();
    let app = Router::new().fallback(move |uri: Uri, body: String| {
        let (page, jina, stub, calls) = (page.clone(), jina.clone(), stub.clone(), recorded.clone());
        async move {
            let path = uri.path().to_string();
            let reply = |code: u16, content_type: &'static str, body: Vec<u8>| {
                let code = StatusCode::from_u16(code).expect("status");
                (code, [("content-type", content_type)], body).into_response()
            };
            if path == "/article" {
                return reply(200, "text/html; charset=utf-8", page.into_bytes());
            }
            if path.starts_with("/jina/") {
                return reply(200, "text/markdown", jina.into_bytes());
            }
            if let Some(name) = path.strip_prefix("/img/") {
                let id: u8 = name.trim_end_matches(".png").parse().expect("image id");
                return reply(200, "image/png", png(id));
            }
            let request: serde_json::Value = serde_json::from_str(&body).expect("vision body");
            let content = &request["messages"][0]["content"];
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(content[0]["source"]["data"].as_str().expect("image data"))
                .expect("base64");
            let prompt = content[1]["text"].as_str().expect("prompt");
            if !prompt.contains("CATEGORY:") {
                calls.0.lock().expect("calls").push("describe".into());
                let text = format!(
                    "TEXT: {}\nDESCRIPTION: Diagram {}.\nTITLE: t\nTAGS: ai",
                    stub.describe_text,
                    image_id(&bytes)
                );
                let body = serde_json::json!({"content": [{"type": "text", "text": text}]});
                return reply(200, "application/json", body.to_string().into_bytes());
            }
            calls.0.lock().expect("calls").push("classify".into());
            let category = match stub.labels.get(&image_id(&bytes)) {
                Some(Label::Error) => return reply(500, "application/json", VISION_500.as_bytes().to_vec()),
                Some(Label::Category(c)) => *c,
                None => "flowchart",
            };
            let text = format!("CATEGORY: {category}\nCONFIDENCE: 0.9");
            let body = serde_json::json!({"content": [{"type": "text", "text": text}]});
            reply(200, "application/json", body.to_string().into_bytes())
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    (base, calls)
}

/// An article page: a paragraph of prose before each image, images `1..=n`
/// with alt `Figure <id>`, and a closing paragraph.
fn page(ids: impl IntoIterator<Item = u8>) -> String {
    let mut body = String::from("<h1>How the gateway works</h1>\n");
    for id in ids {
        body.push_str(&format!(
            "<p>{PROSE} Section {id}.</p>\n<p><img src=\"{{base}}/img/{id}.png\" alt=\"Figure {id}\"></p>\n"
        ));
    }
    body.push_str(&format!("<p>{PROSE} The end.</p>\n"));
    format!(
        "<!doctype html><html><head><title>How the gateway works</title></head>\
         <body><article>{body}</article></body></html>"
    )
}

fn stub(page: String, labels: &[(u8, Label)]) -> Stub {
    Stub {
        page,
        jina: String::new(),
        labels: labels.iter().cloned().collect(),
        describe_text: "Gateway\nAgent\nService".to_string(),
    }
}

fn ocr_ok(_: &Path, _: u64) -> Result<String> {
    Ok("tesseract words".to_string())
}

fn config(dir: &Path) -> Config {
    let key = dir.join("anthropic.key");
    std::fs::write(&key, "stub-key").expect("key");
    let mut config = Config::default();
    config.links = crate::config::default_links();
    config.llm.api_key = key.to_string_lossy().to_string();
    config.vision.enabled = true;
    config.staging.enabled = true;
    config.staging.root = dir.join("stages");
    config.fabric.binary = dir.join("no-such-fabric").to_string_lossy().to_string();
    config.vault.root_path = Some(dir.join("vault").to_string_lossy().to_string());
    config
}

const TRACE: &str = "cl-art00001";

/// Fetch `{base}/article` through the readable handler, then the article stage.
async fn readable(base: &str, config: &Config) -> (String, ArticleStage) {
    let url = format!("{base}/article");
    let (_, markdown, _) = process_article_readable(&url, config, TRACE).await.expect("readable");
    let stage = crate::ocr::bases::scope(
        base.to_string(),
        distill_article(&url, &markdown, true, None, config, ocr_ok, TRACE),
    )
    .await;
    (markdown, stage)
}

fn transcript(stage: &ArticleStage) -> &str {
    stage.distilled.transcript.as_deref().expect("transcript kept")
}

fn stored_ids(published: &Published) -> Vec<u8> {
    published
        .images_frontmatter
        .iter()
        .map(|p| image_id(&std::fs::read(published.vault.join(p)).expect("stored file")))
        .collect()
}

/// Criterion 1: vision labels three images `flowchart`, `app-ui`, `chart`;
/// the note keeps exactly the flowchart and the chart, each described at its
/// image in the distiller input, embedded under `## Images`, listed in
/// `images:`, and present in the vault. The app-ui image is left as written.
#[tokio::test]
async fn article_keeps_the_flowchart_and_the_chart_not_the_app_ui() {
    let dir = tempfile::tempdir().expect("tempdir");
    let labels = [
        (1, Label::Category("flowchart")),
        (2, Label::Category("app-ui")),
        (3, Label::Category("chart")),
    ];
    let (base, calls) = serve(stub(page(1..=3), &labels)).await;
    let config = config(dir.path());

    let (markdown, stage) = readable(&base, &config).await;

    assert_eq!(
        markdown.matches("![").count(),
        3,
        "readable keeps the images:\n{markdown}"
    );
    assert_eq!(calls.of("classify"), 3);
    assert_eq!(calls.of("describe"), 2, "only kept images are described");
    assert!(!stage.figures.degraded, "a not-a-diagram verdict does not degrade");
    let kept: Vec<u8> = stage.figures.figures.iter().map(|f| image_id(&f.bytes)).collect();
    assert_eq!(kept, vec![1, 3]);

    let spliced = transcript(&stage);
    let at = |needle: &str| {
        spliced
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing:\n{spliced}"))
    };
    assert!(
        at("Section 1.") < at("### Image 1\n\n*Figure 1*\n\n#### Description\n\nDiagram 1.")
            && at("Diagram 1.") < at("Section 2."),
        "the flowchart is described where it was:\n{spliced}"
    );
    assert!(
        at("Section 2.") < at("/img/2.png)") && at("/img/2.png)") < at("Section 3."),
        "the app-ui image stays as written:\n{spliced}"
    );
    assert!(
        at("Section 3.") < at("### Image 2\n\n*Figure 3*") && at("Diagram 3.") < at("The end."),
        "{spliced}"
    );
    assert!(
        !spliced.contains("/img/1.png") && !spliced.contains("/img/3.png"),
        "{spliced}"
    );

    let published = publish_figures(&stage.figures, "How the gateway works", &config, dir.path());
    let (_, images) = published.note.split_once("## Images\n").expect("## Images");
    assert_eq!(
        images.lines().filter(|l| l.starts_with("![[")).count(),
        2,
        "{}",
        published.note
    );
    assert!(
        images.contains("*Figure 1.*") && images.contains("*Figure 3.*"),
        "{}",
        published.note
    );
    assert!(!images.contains("Figure 2"), "{}", published.note);
    assert_eq!(stored_ids(&published), vec![1, 3]);
}

/// Criterion 2: 30 candidates all `flowchart` under `max-per-note: 20` cost
/// exactly 30 classify and 20 describe calls; the first 20 in source order
/// are kept, and the cap is truncation, not a degradation.
#[tokio::test]
async fn thirty_flowcharts_cost_thirty_classify_and_twenty_describe_calls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, calls) = serve(stub(page(1..=30), &[])).await;
    let mut config = config(dir.path());
    config.images.max_per_note = 20;

    let (markdown, stage) = readable(&base, &config).await;

    assert_eq!(markdown.matches("![").count(), 30);
    assert_eq!(calls.of("classify"), 30);
    assert_eq!(calls.of("describe"), 20);
    assert!(!stage.figures.degraded);
    let kept: Vec<u8> = stage.figures.figures.iter().map(|f| image_id(&f.bytes)).collect();
    assert_eq!(kept, (1..=20).collect::<Vec<u8>>());
    assert!(transcript(&stage).contains("### Image 20\n\n*Figure 20*"));
    assert!(
        transcript(&stage).contains("/img/21.png)"),
        "a capped image stays as written"
    );
}

/// Criterion 3, first half: a classify error drops that image (never
/// described, never stored) and degrades the image stage, which
/// `process_url_inner` ORs into `fetch_degraded` and so the receipt.
#[tokio::test]
async fn a_classify_error_drops_that_image_and_sets_fetch_degraded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, calls) = serve(stub(page(1..=3), &[(2, Label::Error)])).await;
    let config = config(dir.path());

    let (_, stage) = readable(&base, &config).await;

    assert!(stage.figures.degraded, "a classify error degrades the image stage");
    assert_eq!(calls.of("classify"), 3);
    assert_eq!(calls.of("describe"), 2, "the failed image is never described");
    let kept: Vec<u8> = stage.figures.figures.iter().map(|f| image_id(&f.bytes)).collect();
    assert_eq!(kept, vec![1, 3]);

    let plan = figures::plan(&stage.figures, TRACE, &chrono::Utc::now(), &config.images);
    let fetch_degraded = stage.figures.degraded || plan.degraded;
    let clean = vault::distilled::Distilled::default();
    assert!(
        receipt_degraded(&clean, false, fetch_degraded),
        "the receipt is degraded"
    );
    let published = publish_figures(&stage.figures, "How the gateway works", &config, dir.path());
    assert_eq!(stored_ids(&published), vec![1, 3]);
}

/// Criterion 3, second half: a readable article whose transcript passes the
/// quality gate still passes it after 20 figure sections of short OCR lines
/// are spliced in, because the gate judges the pre-splice source; the
/// transcript kept, in the returned `Distilled` and in staging, is the
/// spliced one.
#[tokio::test]
async fn a_readable_article_still_passes_the_quality_gate_with_twenty_figures_spliced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut stub = stub(page(1..=20), &[]);
    // A dense chart's OCR dump: many short lines, capped at max-section-bytes.
    stub.describe_text = vec!["p50 42ms"; 150].join("\n");
    let (base, _) = serve(stub).await;
    let config = config(dir.path());

    let (markdown, stage) = readable(&base, &config).await;

    assert!(
        crate::stages::distill::transcript_quality_ok(&markdown),
        "precondition: the article as fetched passes the gate"
    );
    assert_eq!(stage.figures.figures.len(), 20);
    let spliced = transcript(&stage);
    assert!(spliced.contains("### Image 20"), "{spliced}");
    assert!(
        !crate::stages::distill::transcript_quality_ok(spliced),
        "precondition: judged on its own, the spliced text would be cleared"
    );
    let staged = std::fs::read_to_string(dir.path().join("stages").join(TRACE).join("distilled.yml"))
        .expect("staged distilled.yml");
    assert!(staged.contains("Image 20"), "the staged transcript is the spliced one");
}

/// The Jina path feeds the same stage: its markdown's images are kept and
/// stored, while the non-clean source still has its transcript cleared.
#[tokio::test]
async fn jina_markdown_images_become_figures_and_the_transcript_is_still_cleared() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut stub = stub(String::new(), &[(2, Label::Category("b-roll"))]);
    stub.jina = format!(
        "Title: How the gateway works\n\n{PROSE}\n\n![Image 1: arch]({{base}}/img/1.png)\n\n{PROSE}\n\n\
         ![Image 2: team photo]({{base}}/img/2.png)\n"
    );
    let (base, calls) = serve(stub).await;
    let config = config(dir.path());
    let url = format!("{base}/article");

    let stage = crate::jina::bases::scope(format!("{base}/jina"), async {
        let (_, markdown, _) = process_article_jina(&url, &config, TRACE).await.expect("jina");
        crate::ocr::bases::scope(
            base.clone(),
            distill_article(&url, &markdown, false, None, &config, ocr_ok, TRACE),
        )
        .await
    })
    .await;

    assert_eq!(calls.of("classify"), 2);
    let kept: Vec<u8> = stage.figures.figures.iter().map(|f| image_id(&f.bytes)).collect();
    assert_eq!(kept, vec![1]);
    assert_eq!(stage.figures.figures[0].provenance, "Image 1: arch");
    assert!(
        stage.distilled.transcript.is_none(),
        "non-clean source: transcript cleared"
    );
}

/// `images.enabled: false` restores today's behavior: the article markdown
/// reaches the distiller untouched and no image is fetched or classified.
#[tokio::test]
async fn images_disabled_leaves_the_article_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, calls) = serve(stub(page(1..=2), &[])).await;
    let mut config = config(dir.path());
    config.images.enabled = false;

    let (markdown, stage) = readable(&base, &config).await;

    assert_eq!(calls.of("classify") + calls.of("describe"), 0);
    assert!(stage.figures.figures.is_empty() && !stage.figures.degraded);
    assert_eq!(transcript(&stage), markdown);
}
