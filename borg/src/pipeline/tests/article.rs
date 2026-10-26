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
    let article = process_article_readable(&url, config, TRACE).await.expect("readable");
    let stage = crate::ocr::bases::scope(
        base.to_string(),
        distill_article(
            &url,
            &article.page_url,
            &article.markdown,
            true,
            None,
            config,
            ocr_ok,
            TRACE,
        ),
    )
    .await;
    (article.markdown, stage)
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
            distill_article(&url, &url, &markdown, false, None, &config, ocr_ok, TRACE),
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

/// A host that answers `/old/post` with a 301 to `location` and 404s every
/// other path, recording each request; the requested host of a redirect.
async fn redirect_host(location: String) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
    let hits = Arc::new(Mutex::new(Vec::new()));
    let recorded = hits.clone();
    let app = Router::new().fallback(move |uri: Uri| {
        let (location, hits) = (location.clone(), recorded.clone());
        async move {
            hits.lock().expect("hits").push(uri.path().to_string());
            if uri.path() == "/old/post" {
                (StatusCode::MOVED_PERMANENTLY, [("location", location)]).into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    (base, hits)
}

/// The 2026-10-22 chroma canary (cl-190dffbd): the requested article 301s to
/// another host, and every chart was resolved against the requested host and
/// 404'd. A root-relative and a path-relative image on the final page are
/// both downloaded from the final host and kept; the requested host sees only
/// the article request.
#[tokio::test]
async fn redirected_article_images_resolve_against_the_final_page() {
    let dir = tempfile::tempdir().expect("tempdir");
    let page = format!(
        "<!doctype html><html><head><title>How the gateway works</title></head><body><article>\
         <h1>How the gateway works</h1><p>{PROSE} Section 1.</p>\
         <p><img src=\"/img/1.png\" alt=\"Figure 1\"></p><p>{PROSE} Section 2.</p>\
         <p><img src=\"img/2.png\" alt=\"Figure 2\"></p><p>{PROSE} The end.</p></article></body></html>"
    );
    let (final_host, calls) = serve(stub(page, &[])).await;
    let (requested_host, requested_hits) = redirect_host(format!("{final_host}/article")).await;
    let config = config(dir.path());
    let url = format!("{requested_host}/old/post");

    let article = process_article_readable(&url, &config, TRACE).await.expect("readable");
    assert_eq!(article.page_url, format!("{final_host}/article"));
    let stage = crate::ocr::bases::scope(
        final_host.clone(),
        distill_article(
            &url,
            &article.page_url,
            &article.markdown,
            true,
            None,
            &config,
            ocr_ok,
            TRACE,
        ),
    )
    .await;

    let kept: Vec<u8> = stage.figures.figures.iter().map(|f| image_id(&f.bytes)).collect();
    assert_eq!(kept, vec![1, 2], "both images download from the final host");
    assert!(!stage.figures.degraded, "no image failed to download");
    assert_eq!(calls.of("classify"), 2);
    assert_eq!(*requested_hits.lock().expect("hits"), vec!["/old/post".to_string()]);
}

/// `distill_article` resolves relative images against `page_url`, not `url`:
/// markdown still carrying a relative image (as a fetcher that does not
/// absolutize leaves it) downloads it from the page's host even when `url`
/// names a host that 404s it.
#[tokio::test]
async fn distill_article_resolves_relative_images_against_page_url_not_url() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (page_host, calls) = serve(stub(String::new(), &[])).await;
    let (requested_host, requested_hits) = redirect_host(format!("{page_host}/article")).await;
    let config = config(dir.path());
    let markdown = format!("# How the gateway works\n\n{PROSE}\n\n![Figure 1](/img/1.png)\n\n{PROSE}\n");

    let stage = crate::ocr::bases::scope(
        page_host.clone(),
        distill_article(
            &format!("{requested_host}/old/post"),
            &format!("{page_host}/article"),
            &markdown,
            false,
            None,
            &config,
            ocr_ok,
            TRACE,
        ),
    )
    .await;

    let kept: Vec<u8> = stage.figures.figures.iter().map(|f| image_id(&f.bytes)).collect();
    assert_eq!(kept, vec![1]);
    assert_eq!(calls.of("classify"), 1);
    assert!(
        requested_hits.lock().expect("hits").is_empty(),
        "the requested host is never asked for the image"
    );
}

// ---------------------------------------------------------------------------
// `PublishPolicy::Scratch` (docs/design/2026-10-25-borg-distill.md):
// the stub article through the real `process_content`, with the vault, the
// shared staging root, the ledger, the archive and the receipts DB all under
// one temp XDG_DATA_HOME.
// ---------------------------------------------------------------------------

/// Every file and directory under `root`, with each file's bytes (`None` for a
/// directory). Empty when `root` does not exist.
fn tree(root: &Path) -> std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = std::collections::BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.map(|e| e.expect("entry").path()) {
        if entry.is_dir() {
            out.insert(entry.clone(), None);
            out.extend(tree(&entry));
        } else {
            out.insert(entry.clone(), Some(std::fs::read(&entry).expect("read")));
        }
    }
    out
}

/// One stub article and a config whose every write target sits under `home`.
struct Scene {
    base: String,
    url: String,
    config: Config,
    vault: PathBuf,
    shared_stages: PathBuf,
    archive: PathBuf,
}

impl Scene {
    async fn new(home: &Path) -> Self {
        let (base, _) = serve(stub(page(1..=2), &[])).await;
        let mut config = config(home);
        config.archive.root = home.join("archive");
        let vault = home.join("vault");
        std::fs::create_dir_all(vault.join(".obsidian")).expect("vault marker");
        std::fs::write(vault.join("existing.md"), "---\ntitle: existing\n---\n").expect("seed note");
        let shared_stages = config.staging.root.clone();
        std::fs::create_dir_all(shared_stages.join("cl-older000")).expect("seed stages");
        std::fs::write(shared_stages.join("cl-older000/distilled.yml"), "summary: s\n").expect("seed staged");
        std::fs::create_dir_all(config.archive.root.join("cl-older000")).expect("seed archive");
        Self {
            url: format!("{base}/article"),
            base,
            archive: config.archive.root.clone(),
            config,
            vault,
            shared_stages,
        }
    }

    /// A ledger row for an unrelated URL, so the ledger has bytes to compare.
    fn seed_ledger(&self) -> PathBuf {
        let ledger = ledger::ledger_path().expect("ledger path");
        ledger::append_entry(
            &ledger,
            &LedgerEntry {
                date: "2026-10-01".to_string(),
                time: "09:00".to_string(),
                method: IngestMethod::Cli,
                filename: Some("existing.md".to_string()),
                source: "https://unrelated.example/post".to_string(),
                trace_id: Some("cl-older000".to_string()),
            },
        )
        .expect("seed ledger");
        ledger
    }

    fn snapshot(&self, ledger: &Path) -> Vec<std::collections::BTreeMap<PathBuf, Option<Vec<u8>>>> {
        let ledger_bytes = std::fs::read(ledger).ok();
        vec![
            tree(&self.vault),
            tree(&self.shared_stages),
            tree(&self.archive),
            std::iter::once((ledger.to_path_buf(), ledger_bytes)).collect(),
        ]
    }

    async fn run(&self, content: ContentKind, policy: &PublishPolicy, trace: &str) -> IngestResult {
        {
            let conn = receipts::open_default().expect("receipts");
            receipts::record_received(
                &conn,
                trace,
                vault::schema::Method::Cli,
                vault::receipts::ReceiptKind::Url,
                &self.url,
            )
            .expect("received");
        }
        crate::jina::bases::scope(
            format!("{}/jina", self.base),
            crate::ocr::bases::scope(
                self.base.clone(),
                process_content(
                    content,
                    Vec::new(),
                    IngestMethod::Cli,
                    false,
                    &self.config,
                    policy,
                    Some(trace.to_string()),
                    None,
                ),
            ),
        )
        .await
    }

    async fn run_url(&self, policy: &PublishPolicy, trace: &str) -> IngestResult {
        let content = ContentKind::Url {
            url: self.url.clone(),
            note: None,
        };
        self.run(content, policy, trace).await
    }

    fn canonical(&self) -> String {
        hygiene::normalize_url(&self.url, &self.config.canonicalization.rules).expect("canonical")
    }
}

fn init_permits() {
    permits::GENERAL_PERMITS.init(4);
    permits::HEAVY_PERMITS.init(4);
}

fn top_level_notes(root: &Path) -> Vec<PathBuf> {
    let mut notes: Vec<PathBuf> = std::fs::read_dir(root)
        .map(|entries| entries.map(|e| e.expect("entry").path()).collect())
        .unwrap_or_default();
    notes.retain(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"));
    notes
}

fn receipt(trace: &str) -> receipts::Receipt {
    let conn = receipts::open_default().expect("receipts");
    receipts::get(&conn, trace).expect("get").expect("receipt row")
}

/// Under `Scratch` the vault, the shared staging root, the archive and the
/// ledger are byte-identical before and after, and the run's output is the
/// note, its figures and its staging, all under `root`.
#[tokio::test]
async fn distill_policy_writes_nothing_to_vault() {
    init_permits();
    let home = tempfile::tempdir().expect("home");
    crate::harvest::with_xdg_data_home(home.path(), || async {
        let scene = Scene::new(home.path()).await;
        let ledger = scene.seed_ledger();
        let before = scene.snapshot(&ledger);
        let trace = "cl-dst00001";
        let root = home.path().join("distill").join(trace);

        let result = scene
            .run_url(&PublishPolicy::Scratch { root: root.clone() }, trace)
            .await;

        assert!(matches!(result.status, IngestStatus::Completed), "{:?}", result.status);
        assert_eq!(
            scene.snapshot(&ledger),
            before,
            "vault, shared staging, archive, ledger"
        );
        let notes = top_level_notes(&root);
        assert_eq!(notes.len(), 1, "one top-level note: {:?}", tree(&root).keys());
        assert_eq!(result.note_path.as_deref(), Some(notes[0].to_string_lossy().as_ref()));
        assert_eq!(result.obsidian_url, None, "a scratch note is in no vault");
        let staged = root.join("stages").join(trace);
        assert!(staged.join("envelope.yml").is_file(), "{:?}", tree(&root).keys());
        assert!(staged.join("distilled.yml").is_file(), "{:?}", tree(&root).keys());
        let figures: Vec<_> = tree(&root.join("system/attachments/images"))
            .into_iter()
            .filter(|(_, bytes)| bytes.is_some())
            .collect();
        assert_eq!(figures.len(), 2, "both figures land under root");
        assert_eq!(receipt(trace).status, "succeeded");
    })
    .await;
}

/// The note's frontmatter values derived from the wall clock, masked so two
/// runs a moment apart compare equal: `date:`, `ingested:`, `trace-expires:`
/// and the `YYYY-MM` of every figure path.
fn mask_clock(note: &str) -> String {
    const ATTACH: &str = "system/attachments/images/";
    let lines: Vec<String> = note
        .lines()
        .map(|line| {
            for key in ["date:", "ingested:", "trace-expires:"] {
                if line.starts_with(key) {
                    return format!("{key} <clock>");
                }
            }
            line.to_string()
        })
        .collect();
    let mut masked = lines.join("\n");
    let mut from = 0;
    while let Some(at) = masked[from..].find(ATTACH) {
        let month = from + at + ATTACH.len();
        masked.replace_range(month..month + "YYYY-MM".len(), "YYYY-MM");
        from = month;
    }
    masked
}

/// With the trace pinned, the note a scratch run renders is the note an
/// ingest publishes from the same stub, byte for byte apart from the clock.
/// The ingest runs first, so its ledger row is present for the scratch run,
/// which must still not turn into a reingest.
#[tokio::test]
async fn distill_policy_matches_ingest_note() {
    init_permits();
    let home = tempfile::tempdir().expect("home");
    crate::harvest::with_xdg_data_home(home.path(), || async {
        let scene = Scene::new(home.path()).await;
        let trace = "cl-dst00002";
        let root = home.path().join("distill").join(trace);

        let ingest = scene.run_url(&PublishPolicy::Vault, trace).await;
        assert!(matches!(ingest.status, IngestStatus::Completed), "{:?}", ingest.status);
        let ingest_path = PathBuf::from(ingest.note_path.expect("ingest note"));
        assert!(
            ingest_path.starts_with(scene.vault.join("inbox")),
            "{}",
            ingest_path.display()
        );
        let ingest_note = std::fs::read_to_string(&ingest_path).expect("ingest note");

        let distill = scene
            .run_url(&PublishPolicy::Scratch { root: root.clone() }, trace)
            .await;
        assert!(
            matches!(distill.status, IngestStatus::Completed),
            "{:?}",
            distill.status
        );
        let distill_path = PathBuf::from(distill.note_path.expect("distill note"));
        assert_eq!(distill_path.parent(), Some(root.as_path()));
        let distill_note = std::fs::read_to_string(&distill_path).expect("distill note");

        assert_eq!(distill_path.file_name(), ingest_path.file_name(), "same filename");
        assert!(ingest_note.contains("\ningested: "), "{ingest_note}");
        assert!(
            ingest_note.contains("\n  - system/attachments/images/"),
            "{ingest_note}"
        );
        assert_eq!(mask_clock(&distill_note), mask_clock(&ingest_note));
        assert_eq!(
            std::fs::read_to_string(&ingest_path).expect("ingest note"),
            ingest_note,
            "the scratch run did not reingest over the vault note"
        );
    })
    .await;
}

/// A non-URL input under `Scratch` is refused before Stage 0: `Failed`
/// `intake-rejected`, and nothing is written anywhere, the scratch root and
/// shared staging included.
#[tokio::test]
async fn distill_policy_refuses_non_url() {
    init_permits();
    let home = tempfile::tempdir().expect("home");
    crate::harvest::with_xdg_data_home(home.path(), || async {
        let scene = Scene::new(home.path()).await;
        let ledger = scene.seed_ledger();
        let before = scene.snapshot(&ledger);
        let trace = "cl-dst00003";
        let root = home.path().join("distill").join(trace);

        let result = scene
            .run(
                ContentKind::Text("remember to read the gateway article".to_string()),
                &PublishPolicy::Scratch { root: root.clone() },
                trace,
            )
            .await;

        let IngestStatus::Failed { reason } = &result.status else {
            panic!("expected Failed, got {:?}", result.status);
        };
        assert!(reason.contains("URL only"), "{reason}");
        assert_eq!(result.failure_stage, Some(FailureStage::IntakeRejected));
        assert!(
            !root.exists(),
            "nothing under the scratch root: {:?}",
            tree(&root).keys()
        );
        assert_eq!(
            scene.snapshot(&ledger),
            before,
            "vault, shared staging, archive, ledger"
        );
        let row = receipt(trace);
        assert_eq!(row.status, "failed");
        assert_eq!(row.failure_stage.as_deref(), Some("intake-rejected"));
    })
    .await;
}

/// A distill of a URL already being distilled fails `intake-rejected`, never
/// `Duplicate` (which would read as `succeeded`), and an ingest of the same
/// URL in that window is not blocked by it.
#[tokio::test]
async fn distill_inflight_hit_fails() {
    init_permits();
    let home = tempfile::tempdir().expect("home");
    crate::harvest::with_xdg_data_home(home.path(), || async {
        let scene = Scene::new(home.path()).await;
        let root = home.path().join("distill").join("cl-dst00004");
        let _distilling =
            InflightGuard::try_acquire(&format!("distill:{}", scene.canonical())).expect("the distill key is free");

        let distill = scene
            .run_url(&PublishPolicy::Scratch { root: root.clone() }, "cl-dst00004")
            .await;
        let IngestStatus::Failed { reason } = &distill.status else {
            panic!("expected Failed, got {:?}", distill.status);
        };
        assert_eq!(reason, DISTILL_INFLIGHT_REASON);
        assert_eq!(distill.failure_stage, Some(FailureStage::IntakeRejected));
        assert_eq!(receipt("cl-dst00004").status, "failed");
        assert!(top_level_notes(&root).is_empty());

        let ingest = scene.run_url(&PublishPolicy::Vault, "cl-dst00005").await;
        assert!(
            matches!(ingest.status, IngestStatus::Completed),
            "an in-flight distill must not block an ingest: {:?}",
            ingest.status
        );
    })
    .await;
}

/// Under `Scratch` a staging write failure fails the run, so `succeeded`
/// always means `distilled.yml` exists. A `root/stages` that is a file is
/// refused at Stage 0; a `distilled.yml` that cannot be written (a directory
/// sits at its path) passes Stage 0 and fails `publish-failed`, with no note.
#[tokio::test]
async fn distill_persist_failure_fails() {
    init_permits();
    let home = tempfile::tempdir().expect("home");
    crate::harvest::with_xdg_data_home(home.path(), || async {
        let scene = Scene::new(home.path()).await;

        // A file where `root/stages` should be: unwritable even when CI runs as root.
        let unwritable = home.path().join("distill").join("cl-dst00006");
        std::fs::create_dir_all(&unwritable).expect("root");
        std::fs::write(unwritable.join("stages"), b"not a dir").expect("stages file");
        let result = scene
            .run_url(
                &PublishPolicy::Scratch {
                    root: unwritable.clone(),
                },
                "cl-dst00006",
            )
            .await;
        assert!(
            matches!(result.status, IngestStatus::Failed { .. }),
            "{:?}",
            result.status
        );
        assert!(top_level_notes(&unwritable).is_empty());

        let trace = "cl-dst00007";
        let root = home.path().join("distill").join(trace);
        std::fs::create_dir_all(root.join("stages").join(trace).join("distilled.yml")).expect("obstruction");
        let result = scene
            .run_url(&PublishPolicy::Scratch { root: root.clone() }, trace)
            .await;
        let IngestStatus::Failed { reason } = &result.status else {
            panic!("expected Failed, got {:?}", result.status);
        };
        assert!(reason.contains("staging persistence failed"), "{reason}");
        assert_eq!(result.failure_stage, Some(FailureStage::PublishFailed));
        assert!(
            root.join("stages").join(trace).join("envelope.yml").is_file(),
            "Stage 0 ran"
        );
        assert!(top_level_notes(&root).is_empty(), "no note without its staging");
        assert_eq!(receipt(trace).failure_stage.as_deref(), Some("publish-failed"));
    })
    .await;
}
