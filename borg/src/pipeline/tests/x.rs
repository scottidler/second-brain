//! X posts through the pipeline: photo description (vision + OCR stubs), the
//! degraded flag, and the fail-closed dispatch. One stub server plays
//! fxtwitter, the photo host, the Anthropic Messages API and Jina Reader;
//! a stub fabric records every invocation. No test reaches the network.

use super::*;
use axum::Router;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use std::sync::{Arc, Mutex};

const VISION_OK: &str = r#"{"content":[{"type":"text","text":"TEXT: Agent generates code\nDESCRIPTION: A flowchart of an AI code review loop.\nTITLE: Review loop\nTAGS: ai"}]}"#;
/// A 200 with no text block: `vision_call` returns `Ok("")`.
const VISION_NO_TEXT: &str = r#"{"content":[]}"#;
const VISION_500: &str = r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#;
const OCR_TEXT: &str = "tesseract words";

type Reply = (u16, String);

#[derive(Clone, Default)]
struct Hits(Arc<Mutex<Vec<String>>>);

impl Hits {
    fn count(&self, prefix: &str) -> usize {
        self.0
            .lock()
            .expect("hits lock")
            .iter()
            .filter(|p| p.starts_with(prefix))
            .count()
    }
}

/// Serve fxtwitter (`/status/<id>`, body built from the base URL so photo URLs
/// point back here), photos (`/media/ok*` a JPEG, `/media/html*` an HTML page,
/// any other `/media/` a 404), vision (`/v1/messages`) and Jina (`/jina/...`).
async fn serve(status: impl FnOnce(&str) -> Reply, vision: Reply) -> (String, Hits) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
    let status = status(&base);
    let hits = Hits::default();
    let recorded = hits.clone();
    let app = Router::new().fallback(move |uri: Uri| {
        let (status, vision, hits) = (status.clone(), vision.clone(), recorded.clone());
        async move {
            let path = uri.path().to_string();
            hits.0.lock().expect("hits lock").push(path.clone());
            let reply = |(code, body): Reply, content_type: &'static str| {
                let code = StatusCode::from_u16(code).expect("status");
                (code, [("content-type", content_type)], body).into_response()
            };
            if path.starts_with("/status/") {
                reply(status, "application/json")
            } else if path.starts_with("/media/ok") {
                reply((200, "\u{ff}fake-jpeg".to_string()), "image/jpeg")
            } else if path.starts_with("/media/html") {
                reply((200, "<html>login</html>".to_string()), "text/html")
            } else if path.starts_with("/media/") {
                reply((404, "not found".to_string()), "text/plain")
            } else if path == "/v1/messages" {
                reply(vision, "application/json")
            } else {
                reply((200, "Title: jina\n\njina body".to_string()), "text/markdown")
            }
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    (base, hits)
}

/// An fxtwitter envelope: a post by @poster quoting @quoter, photos named by
/// path under `/media/`.
fn post_json(base: &str, text: &str, quote_text: &str, own: &[&str], quoted: &[&str]) -> Reply {
    let media = |names: &[&str]| -> serde_json::Value {
        if names.is_empty() {
            return serde_json::Value::Null;
        }
        let photos: Vec<_> = names
            .iter()
            .map(|n| serde_json::json!({"url": format!("{base}/media/{n}.jpg?name=orig")}))
            .collect();
        serde_json::json!({ "photos": photos })
    };
    let body = serde_json::json!({
        "code": 200,
        "tweet": {
            "id": "100",
            "text": text,
            "author": {"name": "Poster", "screen_name": "poster"},
            "media": media(own),
            "quote": {
                "id": "99",
                "text": quote_text,
                "author": {"name": "Quoter", "screen_name": "quoter"},
                "media": media(quoted),
            },
        },
    });
    (200, body.to_string())
}

fn ocr_ok(_: &Path, _: u64) -> Result<String> {
    Ok(OCR_TEXT.to_string())
}

fn config(dir: &Path) -> Config {
    let key = dir.join("anthropic.key");
    std::fs::write(&key, "stub-key").expect("key");
    let mut config = Config::default();
    config.links = crate::config::default_links();
    config.llm.api_key = key.to_string_lossy().to_string();
    config.vision.enabled = true;
    config.staging.enabled = false;
    config.staging.root = dir.join("stages");
    config.fabric.binary = dir.join("no-such-fabric").to_string_lossy().to_string();
    config.vault.root_path = Some(dir.join("vault").to_string_lossy().to_string());
    config
}

async fn fetch(base: &str, config: &Config) -> Result<FetchedPost> {
    crate::ocr::bases::scope(
        base.to_string(),
        process_x_post_from(base, ocr_ok, "https://x.com/poster/status/100", 100, config, "xp-test"),
    )
    .await
}

#[tokio::test]
async fn x_post_image_description_under_quoted_post() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, hits) = serve(
        |b| post_json(b, "my take", "the loop", &[], &["ok1"]),
        (200, VISION_OK.into()),
    )
    .await;

    let post = fetch(&base, &config(dir.path())).await.expect("fetch");

    let quote_at = post.markdown.find("## Quoted post by Quoter (@quoter)").expect("quote");
    let image_at = post
        .markdown
        .find(
            "### Image 1 (in @quoter's post)\n\n#### Description\n\nA flowchart of an AI code review loop.\n\n\
             #### Extracted Text\n\nAgent generates code",
        )
        .unwrap_or_else(|| panic!("image section:\n{}", post.markdown));
    assert!(quote_at < image_at, "{}", post.markdown);
    assert!(!post.markdown.contains("OCR Text"), "vision ran: {}", post.markdown);
    assert!(!post.degraded);
    assert_eq!(post.title, "@poster on X");
    assert_eq!(hits.count("/v1/messages"), 1);
}

#[tokio::test]
async fn x_post_vision_error_degrades() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, _) = serve(|b| post_json(b, "my take", "", &["ok1"], &[]), (500, VISION_500.into())).await;

    let post = fetch(&base, &config(dir.path())).await.expect("fetch");

    assert!(
        post.markdown
            .contains("### Image 1 (in @poster's post)\n\n#### OCR Text\n\ntesseract words"),
        "{}",
        post.markdown
    );
    assert!(!post.markdown.contains("#### Description"), "{}", post.markdown);
    assert!(post.degraded, "a failed vision call degrades the fetch");
}

#[tokio::test]
async fn x_post_vision_empty_text_degrades() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, _) = serve(
        |b| post_json(b, "my take", "", &["ok1"], &[]),
        (200, VISION_NO_TEXT.into()),
    )
    .await;

    let post = fetch(&base, &config(dir.path())).await.expect("fetch");

    assert!(
        post.markdown.contains("#### OCR Text\n\ntesseract words"),
        "{}",
        post.markdown
    );
    assert!(post.degraded, "vision Ok with no text block degrades the fetch");
}

#[tokio::test]
async fn x_post_vision_disabled_is_ocr_text_not_degraded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, hits) = serve(|b| post_json(b, "my take", "", &["ok1"], &[]), (200, VISION_OK.into())).await;
    let mut config = config(dir.path());
    config.vision.enabled = false;

    let post = fetch(&base, &config).await.expect("fetch");

    assert!(
        post.markdown.contains("#### OCR Text\n\ntesseract words"),
        "{}",
        post.markdown
    );
    assert!(!post.degraded, "vision off is the operator's choice, not a degradation");
    assert_eq!(hits.count("/v1/messages"), 0);
}

#[tokio::test]
async fn x_post_photo_download_error_omits_section() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, hits) = serve(
        |b| post_json(b, "my take", "the loop", &["missing1"], &["html1"]),
        (200, VISION_OK.into()),
    )
    .await;

    let post = fetch(&base, &config(dir.path())).await.expect("text still publishes");

    assert!(!post.markdown.contains("### Image"), "{}", post.markdown);
    assert!(post.markdown.contains("my take") && post.markdown.contains("the loop"));
    assert!(post.degraded, "an omitted photo degrades the fetch");
    assert_eq!(
        hits.count("/v1/messages"),
        0,
        "no vision call for an undownloadable photo"
    );
}

#[tokio::test]
async fn x_post_image_only_all_failed_is_err() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, _) = serve(
        |b| post_json(b, "  ", "", &["missing1", "missing2"], &["missing3"]),
        (200, VISION_OK.into()),
    )
    .await;

    let err = fetch(&base, &config(dir.path()))
        .await
        .err()
        .expect("an image-only post with no described photo must fail");
    assert!(format!("{err:#}").contains("no text"), "{err:#}");
}

/// A fake fabric that answers the `--version` availability probe, appends
/// any other argv to `hits` and prints a scrape titled like a block page. A
/// regressed fallback is counted here, stops before Jina, and is then refused
/// by the quality gate, so it never reaches publish: the ledger is the real
/// XDG one and a test must not write a row to it.
fn recording_fabric(dir: &Path, hits: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-fabric");
    let script = format!(
        "#!/bin/sh\n[ \"$1\" = --version ] && echo v1 && exit 0\necho \"$*\" >> '{}'\nprintf 'Title: Just a moment...\\n\\nLog in to X\\n'\n",
        hits.display()
    );
    std::fs::write(&path, script).expect("fabric");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path.to_string_lossy().to_string()
}

fn vault_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").path())
    {
        if entry.is_dir() {
            files.extend(vault_files(&entry));
        } else {
            files.push(entry);
        }
    }
    files.sort();
    files
}

/// Through the URL dispatcher: fxtwitter 500 fails the ingest `fetch-failed`,
/// neither fabric nor Jina is touched, and the note already in the vault for
/// the source is byte-identical afterward.
#[tokio::test]
async fn x_post_fetch_error_fails_closed() {
    // A fallback would reach fabric's HEAVY_PERMITS site; init so it gets
    // there and is counted rather than panicking first.
    permits::HEAVY_PERMITS.init(4);
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, hits) = serve(|_| (500, r#"{"code":500}"#.to_string()), (200, VISION_OK.into())).await;
    let fabric_hits = dir.path().join("fabric-hits");
    let mut config = config(dir.path());
    config.fabric.binary = recording_fabric(dir.path(), &fabric_hits);
    config.archive.root = dir.path().join("archive");
    let url = "https://x.com/poster/status/100";
    let note = dir.path().join("vault/notes/poster-on-x.md");
    std::fs::create_dir_all(note.parent().expect("parent")).expect("mkdir");
    let before = format!("---\ntitle: \"@poster on X\"\nsource: {url}\ntype: social\n---\n\n## Summary\n\nuseful\n");
    std::fs::write(&note, &before).expect("note");
    let files_before = vault_files(&dir.path().join("vault"));

    let result = crate::x::bases::scope(
        base.clone(),
        crate::jina::bases::scope(
            format!("{base}/jina"),
            crate::ocr::bases::scope(
                base.clone(),
                process_url(
                    url,
                    None,
                    vec![],
                    IngestMethod::Cli,
                    true,
                    &config,
                    &PublishPolicy::Vault,
                    "xp-closed",
                ),
            ),
        ),
    )
    .await;

    assert!(
        matches!(result.status, IngestStatus::Failed { .. }),
        "got {:?}",
        result.status
    );
    assert_eq!(result.failure_stage, Some(FailureStage::FetchFailed));
    assert_eq!(hits.count("/status/100"), 1, "fxtwitter was asked");
    assert_eq!(hits.count("/jina"), 0, "Jina must not be reached");
    assert!(!fabric_hits.exists(), "fabric must not be invoked");
    assert_eq!(std::fs::read_to_string(&note).expect("note"), before);
    assert_eq!(vault_files(&dir.path().join("vault")), files_before);
}

// ---------------------------------------------------------------------------
// Stored photos (docs/design/2026-10-22-capture-all-diagrams.md):
// fetch an X post through the stubs, then plan -> render -> publish through the
// real note writer (`publish_url_note`), the same helpers `process_url_inner`
// calls, so the note and its files are what a reingest would leave.
// ---------------------------------------------------------------------------

const VIC: &str = include_str!("../../x/vic.json");

/// The vic fixture with its photo pointed at the stub's JPEG.
fn vic_reply(base: &str) -> Reply {
    (
        200,
        VIC.replace("https://pbs.twimg.com/media/", &format!("{base}/media/ok")),
    )
}

pub(super) struct Published {
    pub(super) vault: PathBuf,
    pub(super) note: String,
    pub(super) images_frontmatter: Vec<String>,
}

fn publish_post(post: &FetchedPost, config: &Config, dir: &Path) -> Published {
    publish_figures(&post.figures, &post.title, config, dir)
}

/// plan -> render -> `publish_url_note` for a figure set, the steps
/// `process_url_inner` runs after the fetch, under trace `cl-8c531660`.
pub(super) fn publish_figures(set: &figures::FigureSet, title: &str, config: &Config, dir: &Path) -> Published {
    let vault = dir.join("vault");
    std::fs::create_dir_all(vault.join("inbox")).expect("inbox");
    let now = chrono::Utc::now();
    let plan = figures::plan(set, "cl-8c531660", &now, &config.images);
    let body = figures::with_images_section("## Summary\n\nvic argues.", &plan);
    let images: Vec<String> = plan.planned.iter().map(|f| f.vault_path.clone()).collect();
    let content = crate::markdown::NoteContent {
        title: title.to_string(),
        source_url: Some("https://x.com/poster/status/100".to_string()),
        trace_id: Some("cl-8c531660".to_string()),
        images: images.clone(),
        distilled_body: Some(body),
        ..Default::default()
    };
    let rendered = crate::markdown::render_note(&content, &config.frontmatter);
    let distilled = vault::distilled::Distilled {
        summary: "s".to_string(),
        ..Default::default()
    };
    let archive_root = dir.join("archive");
    let archive = crate::archive::SourceArchive {
        root: &archive_root,
        trace_id: "cl-8c531660",
        distilled: &distilled,
        source_md: None,
    };
    let desired = vault.join("inbox/vic.md");
    let inbox = vault.join("inbox");
    let old_owned = crate::slides::cleanup::Owned::default();
    let (note_path, _) = publish_url_note(&UrlPublish {
        archive: &archive,
        desired_path: &desired,
        note_bytes: rendered.as_bytes(),
        replaces: None,
        vault_dirs: std::slice::from_ref(&inbox),
        vault_root: &vault,
        max_note_bytes: config.pipeline.max_note_bytes,
        figures: &plan.planned,
        image_stage_degraded: set.degraded || plan.degraded,
        old_owned: &old_owned,
        new_slides: &[],
    })
    .expect("publish");
    Published {
        vault,
        note: std::fs::read_to_string(note_path).expect("note"),
        images_frontmatter: rendered
            .lines()
            .filter(|l| l.starts_with("  - system/attachments/images/"))
            .map(|l| l.trim_start_matches("  - ").to_string())
            .collect(),
    }
}

/// Criterion 1: the vic note embeds its one photo under `## Images`, lists it
/// in `images:`, and the file is in the vault.
#[tokio::test]
async fn vic_photo_is_stored_embedded_and_listed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, _) = serve(vic_reply, (200, VISION_OK.into())).await;
    let config = config(dir.path());

    let post = fetch(&base, &config).await.expect("fetch");
    assert!(!post.degraded);
    let published = publish_post(&post, &config, dir.path());

    let (head, images) = published.note.split_once("## Images\n").expect("## Images section");
    assert!(
        !head.contains("![["),
        "the embed lives only under ## Images:\n{}",
        published.note
    );
    let embeds: Vec<&str> = images.lines().filter(|l| l.contains("![[")).collect();
    assert_eq!(embeds.len(), 1, "{}", published.note);
    assert!(
        embeds[0].starts_with("![[cl-8c531660-1-") && embeds[0].ends_with(".jpg]]"),
        "{embeds:?}"
    );
    assert!(images.contains("*In @boristane's post.*"), "{}", published.note);
    assert!(
        images.contains("#### Description\n\nA flowchart of an AI code review loop."),
        "{}",
        published.note
    );
    assert!(
        images
            .find("---\n\n*Source:")
            .is_some_and(|at| at > images.find("![[").unwrap()),
        "`## Images` sits before the Source footer:\n{}",
        published.note
    );
    assert_eq!(published.images_frontmatter.len(), 1, "{}", published.note);
    let stored = published.vault.join(&published.images_frontmatter[0]);
    assert!(stored.is_file(), "{} missing", stored.display());
    assert_eq!(std::fs::read(&stored).expect("bytes"), "\u{ff}fake-jpeg".as_bytes());
    assert!(
        published.images_frontmatter[0].ends_with(embeds[0].trim_start_matches("![[").trim_end_matches("]]")),
        "embed and images: name the same file"
    );
}

/// Criterion 2: an undownloadable photo leaves no `## Images`, no `images:`
/// entry, no file, and the fetch is degraded (so a reingest over a note that
/// owns images is refused by the writer).
#[tokio::test]
async fn photo_download_error_yields_no_images_section_and_degrades() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, _) = serve(
        |b| post_json(b, "my take", "", &["missing1"], &[]),
        (200, VISION_OK.into()),
    )
    .await;
    let config = config(dir.path());

    let post = fetch(&base, &config).await.expect("text still publishes");
    assert!(post.degraded, "fetch_degraded");
    assert!(post.figures.degraded, "the image stage is degraded");
    assert!(post.figures.figures.is_empty());
    let published = publish_post(&post, &config, dir.path());

    assert!(!published.note.contains("## Images"), "{}", published.note);
    assert!(published.images_frontmatter.is_empty());
    assert!(!published.vault.join("system").exists(), "no asset written");
}

/// `max-download-bytes` is a ceiling on the read, not a note: an over-size
/// photo is dropped and degrades.
#[tokio::test]
async fn photo_over_max_download_bytes_is_dropped_and_degrades() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, hits) = serve(vic_reply, (200, VISION_OK.into())).await;
    let mut config = config(dir.path());
    config.images.max_download_bytes = 4; // "\u{ff}fake-jpeg" is 11 bytes

    let post = fetch(&base, &config).await.expect("text still publishes");

    assert!(post.degraded);
    assert!(post.figures.figures.is_empty());
    assert!(!post.markdown.contains("### Image"), "{}", post.markdown);
    assert_eq!(hits.count("/v1/messages"), 0, "no vision call for a refused download");
}

/// `images.enabled: false` restores today's behavior: described, not stored.
#[tokio::test]
async fn images_disabled_describes_but_stores_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, _) = serve(vic_reply, (200, VISION_OK.into())).await;
    let mut config = config(dir.path());
    config.images.enabled = false;

    let post = fetch(&base, &config).await.expect("fetch");

    assert!(post.markdown.contains("#### Description"), "still described");
    assert!(post.figures.figures.is_empty());
    assert!(!post.figures.degraded);
    let published = publish_post(&post, &config, dir.path());
    assert!(!published.note.contains("## Images"));
    assert!(published.images_frontmatter.is_empty());
}

/// A long description is capped at `max-section-bytes` before it reaches the
/// distiller source or the note.
#[tokio::test]
async fn photo_section_is_capped_at_max_section_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let long = format!(
        r#"{{"content":[{{"type":"text","text":"TEXT: {}\nDESCRIPTION: A chart.\nTITLE: t\nTAGS: ai"}}]}}"#,
        ["line of extracted text"; 40].join("\\n")
    );
    let (base, _) = serve(vic_reply, (200, long)).await;
    let mut config = config(dir.path());
    config.images.max_section_bytes = 120;

    let post = fetch(&base, &config).await.expect("fetch");

    let section = &post.figures.figures[0].section;
    assert!(section.len() <= 120 + '\u{2026}'.len_utf8(), "{} bytes", section.len());
    assert!(section.ends_with('\u{2026}'), "{section}");
    assert!(
        post.markdown.contains(section.as_str()),
        "the distiller sees the capped section"
    );
}

mod article;
