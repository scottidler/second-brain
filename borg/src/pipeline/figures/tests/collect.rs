//! `collect` against a stub that plays the image host and the vision API:
//! the prefilter, SVG rasterize-and-store-original, and classify routing,
//! observed from the outside (what was downloaded, what vision was sent).

use super::super::*;
use axum::Router;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use base64::Engine;
use std::sync::{Arc, Mutex};

const VISION_OK: &str = r#"{"content":[{"type":"text","text":"TEXT: Agent generates code\nDESCRIPTION: A flowchart.\nTITLE: t\nTAGS: ai"}]}"#;

const DIAGRAM_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="200"><rect x="10" y="10" width="100" height="50" fill="none" stroke="black"/><text x="20" y="40" font-size="20" fill="black">Gateway</text></svg>"#;

#[derive(Clone, Default)]
struct Seen {
    paths: Arc<Mutex<Vec<String>>>,
    vision: Arc<Mutex<Vec<String>>>,
    /// `(bytes, longer side px)` of the image each vision call carried.
    sent: Arc<Mutex<Vec<(usize, u32)>>>,
}

impl Seen {
    fn fetched(&self, prefix: &str) -> usize {
        self.paths
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with(prefix))
            .count()
    }

    /// `kind:media_type` for every vision call, kind `classify` or `describe`.
    fn vision(&self) -> Vec<String> {
        self.vision.lock().unwrap().clone()
    }

    fn sent(&self) -> Vec<(usize, u32)> {
        self.sent.lock().unwrap().clone()
    }

    fn calls(&self, kind: &str) -> usize {
        self.vision().iter().filter(|c| c.starts_with(kind)).count()
    }
}

fn png(width: u32, height: u32, shade: u8) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(width, height, image::Rgb([shade, 7, 9])))
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

/// Incompressible noise, 1400x1400: a PNG well over [`VISION_MAX_BYTES`] whose
/// longer side is still under the default `max-side-px`, so only the byte
/// bound can bring it under.
fn huge_png() -> Vec<u8> {
    let mut state: u32 = 0x9e37_79b9;
    let buf = image::RgbImage::from_fn(1400, 1400, |_, _| {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let [a, b, c, _] = state.to_be_bytes();
        image::Rgb([a, b, c])
    });
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(buf)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

/// `/img/<name>`: `big*` a 400px PNG (shade from the name's last byte), `small*`
/// a 90px PNG, `huge.png` a 1400px noise PNG over [`VISION_MAX_BYTES`], `a.svg` the diagram, `x.avif` avif-typed bytes; `/vision`
/// records the call and answers flowchart / a description.
async fn serve() -> (String, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
    let seen = Seen::default();
    let recorded = seen.clone();
    let app = Router::new().fallback(move |uri: Uri, body: String| {
        let seen = recorded.clone();
        async move {
            let path = uri.path().to_string();
            seen.paths.lock().unwrap().push(path.clone());
            let reply = |content_type: &'static str, body: Vec<u8>| {
                (StatusCode::OK, [("content-type", content_type)], body).into_response()
            };
            if path == "/vision" || path == "/v1/messages" {
                let request: serde_json::Value = serde_json::from_str(&body).expect("vision body");
                let content = &request["messages"][0]["content"];
                let media = content[0]["source"]["media_type"].as_str().unwrap_or("?").to_string();
                let kind = if content[1]["text"].as_str().unwrap_or("").contains("CATEGORY:") {
                    "classify"
                } else {
                    "describe"
                };
                seen.vision.lock().unwrap().push(format!("{kind}:{media}"));
                let image = base64::engine::general_purpose::STANDARD
                    .decode(content[0]["source"]["data"].as_str().unwrap_or(""))
                    .expect("image is base64");
                let side = image::load_from_memory(&image).map_or(0, |i| i.width().max(i.height()));
                seen.sent.lock().unwrap().push((image.len(), side));
                if kind == "classify" {
                    let text = "CATEGORY: flowchart\nCONFIDENCE: 0.9";
                    let body = serde_json::json!({"content": [{"type": "text", "text": text}]});
                    return reply("application/json", body.to_string().into_bytes());
                }
                return reply("application/json", VISION_OK.as_bytes().to_vec());
            }
            let name = path.rsplit('/').next().unwrap_or_default();
            match name {
                n if n.starts_with("big") => reply("image/png", png(400, 400, n.bytes().last().unwrap())),
                n if n.starts_with("small") => reply("image/png", png(90, 90, n.bytes().last().unwrap())),
                "huge.png" => reply("image/png", huge_png()),
                "broken.svg" => reply("image/svg+xml", b"<svg".to_vec()),
                "a.svg" => reply("image/svg+xml", DIAGRAM_SVG.as_bytes().to_vec()),
                "x.avif" => reply("image/avif", b"avif bytes".to_vec()),
                _ => (StatusCode::NOT_FOUND, "no").into_response(),
            }
        }
    });
    // A bounded vision image is still megabytes of base64: lift axum's 2 MB cap.
    let app = app.layer(axum::extract::DefaultBodyLimit::disable());
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    (base, seen)
}

fn ocr_stub(_: &Path, _: u64) -> Result<String> {
    Ok("ocr words".to_string())
}

fn config(dir: &Path) -> Config {
    let key = dir.join("anthropic.key");
    std::fs::write(&key, "stub-key").unwrap();
    let mut config = Config::default();
    config.llm.api_key = key.to_string_lossy().to_string();
    config.vision.enabled = true;
    config
}

fn candidate(base: &str, name: &str, source: Source) -> Candidate {
    Candidate {
        url: format!("{base}/img/{name}"),
        provenance: format!("prov {name}"),
        source,
    }
}

async fn run(base: &str, config: &Config, candidates: &[Candidate]) -> Collected {
    crate::ocr::bases::scope(base.to_string(), collect(candidates, config, ocr_stub, "t")).await
}

#[tokio::test]
async fn svg_is_rasterized_for_vision_and_the_original_svg_is_what_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (base, seen) = serve().await;

    let out = run(
        &base,
        &config(dir.path()),
        &[candidate(&base, "a.svg", Source::XArticle)],
    )
    .await;

    let figure = out.figures[0].as_ref().expect("svg kept");
    assert_eq!(figure.mime, FigureMime::Svg);
    assert_eq!(
        figure.bytes,
        DIAGRAM_SVG.as_bytes(),
        "the stored bytes are the original svg"
    );
    assert_eq!(seen.vision(), vec!["classify:image/png", "describe:image/png"]);
    assert!(!out.degraded);
}

#[tokio::test]
async fn svg_that_will_not_render_is_a_prefilter_drop_not_a_degradation() {
    let dir = tempfile::tempdir().unwrap();
    let (base, seen) = serve().await;

    let out = run(
        &base,
        &config(dir.path()),
        &[candidate(&base, "broken.svg", Source::XArticle)],
    )
    .await;

    assert!(out.figures[0].is_none());
    assert!(!out.degraded);
    assert_eq!(
        seen.vision(),
        Vec::<String>::new(),
        "no vision call for an unrenderable svg"
    );
}

#[tokio::test]
async fn x_photo_skips_classify_but_an_article_figure_is_classified() {
    let dir = tempfile::tempdir().unwrap();
    let (base, seen) = serve().await;
    let cands = [
        candidate(&base, "big1", Source::XPhoto),
        candidate(&base, "big2", Source::XArticle),
    ];

    let out = run(&base, &config(dir.path()), &cands).await;

    assert!(out.figures.iter().all(Option::is_some));
    assert_eq!(seen.calls("classify"), 1, "only the article figure is classified");
    assert_eq!(seen.calls("describe"), 2);
}

#[tokio::test]
async fn x_photo_in_an_unstored_format_is_dropped_before_vision() {
    let dir = tempfile::tempdir().unwrap();
    let (base, seen) = serve().await;
    let cands = [
        candidate(&base, "x.avif", Source::XPhoto),
        candidate(&base, "big1", Source::XPhoto),
    ];

    let out = run(&base, &config(dir.path()), &cands).await;

    assert!(out.figures[0].is_none());
    assert!(out.figures[1].is_some());
    assert_eq!(seen.calls("describe"), 1, "the avif is never described");
    assert!(!out.degraded, "a prefilter drop is expected, not degraded");
}

#[tokio::test]
async fn web_decoration_is_dropped_by_size_and_url_but_x_photos_of_the_same_size_stay() {
    let dir = tempfile::tempdir().unwrap();
    let (base, seen) = serve().await;
    let cands = [
        candidate(&base, "small1", Source::WebArticle),
        candidate(&base, "logo/big3", Source::WebArticle),
        candidate(&base, "big4", Source::WebArticle),
        candidate(&base, "small2", Source::XPhoto),
    ];

    let out = run(&base, &config(dir.path()), &cands).await;

    let kept: Vec<bool> = out.figures.iter().map(Option::is_some).collect();
    assert_eq!(kept, vec![false, false, true, true]);
    assert_eq!(seen.fetched("/img/logo"), 0, "a decoration URL is never downloaded");
    assert!(!out.degraded);
}

#[tokio::test]
async fn data_uri_candidate_is_dropped_without_a_download_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = serve().await;
    let cands = [Candidate {
        url: "data:image/gif;base64,R0lGODlh".to_string(),
        provenance: String::new(),
        source: Source::WebArticle,
    }];

    let out = run(&base, &config(dir.path()), &cands).await;

    assert!(out.figures[0].is_none());
    assert!(!out.degraded, "a download error would have degraded");
}

#[tokio::test]
async fn identical_bytes_within_a_note_keep_the_first_only() {
    let dir = tempfile::tempdir().unwrap();
    let (base, seen) = serve().await;
    let cands = [
        candidate(&base, "big5", Source::XPhoto),
        candidate(&base, "dup/big5", Source::XPhoto),
        candidate(&base, "big6", Source::XPhoto),
    ];

    let out = run(&base, &config(dir.path()), &cands).await;

    let kept: Vec<bool> = out.figures.iter().map(Option::is_some).collect();
    assert_eq!(kept, vec![true, false, true]);
    assert_eq!(seen.calls("describe"), 2);
}

#[tokio::test]
async fn an_image_over_the_vision_limit_is_bounded_before_classify_and_describe_but_stored_whole() {
    let dir = tempfile::tempdir().unwrap();
    let (base, seen) = serve().await;
    let original = huge_png();
    assert!(original.len() > VISION_MAX_BYTES, "fixture is over the bound");
    let config = config(dir.path());

    let out = run(&base, &config, &[candidate(&base, "huge.png", Source::WebArticle)]).await;

    assert_eq!(seen.calls("classify"), 1);
    assert_eq!(seen.calls("describe"), 1);
    for (bytes, side) in seen.sent() {
        assert!(bytes <= VISION_MAX_BYTES, "vision was sent {bytes} bytes");
        assert!(side <= config.images.max_side_px, "vision was sent {side}px");
    }
    let figure = out.figures[0].as_ref().expect("bounded figure kept");
    assert_eq!(figure.bytes, original, "the stored bytes are the original download");
    assert!(!out.degraded);
}

#[tokio::test]
async fn an_image_over_max_side_px_reaches_vision_at_max_side_px() {
    let dir = tempfile::tempdir().unwrap();
    let (base, seen) = serve().await;
    let mut config = config(dir.path());
    config.images.max_side_px = 100;

    let out = run(&base, &config, &[candidate(&base, "big7", Source::XArticle)]).await;

    assert!(out.figures[0].is_some());
    let sent = seen.sent();
    assert_eq!(sent.len(), 2, "classify and describe");
    assert!(sent.iter().all(|&(_, side)| side == 100), "{sent:?}");
}
