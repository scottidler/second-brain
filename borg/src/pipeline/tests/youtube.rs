//! The youtube engine seam through the pipeline
//! (docs/design/2026-10-16-youtube-gemini-engine.md): spawn counts, fallback,
//! and the receipt.

use super::*;
use crate::config::YoutubeEngine;
use crate::youtube::engine::bases::{self as api, ApiBases};
use crate::youtube::engine::stub;
use crate::youtube::spawn;

async fn bases(routes: stub::Routes) -> (ApiBases, stub::Hits) {
    let (base, hits) = stub::serve(routes).await;
    (
        ApiBases {
            data_api: base.clone(),
            gemini: base,
        },
        hits,
    )
}

fn gemini_ok() -> Vec<(u16, String)> {
    vec![(200, stub::GEMINI_OK.to_string())]
}

fn gemini_5xx_twice() -> Vec<(u16, String)> {
    vec![(503, stub::GEMINI_5XX.to_string()), (503, stub::GEMINI_5XX.to_string())]
}

/// `engine: gemini`, slides off, through `process_youtube` (fetch, distill,
/// result): no yt-dlp process at all.
#[tokio::test]
async fn gemini_engine_slides_off_spawns_zero_yt_dlp() {
    permits::HEAVY_PERMITS.init(4);
    let dir = tempfile::tempdir().unwrap();
    let config = stub::config(dir.path(), YoutubeEngine::Gemini, Some(YoutubeEngine::YtDlp));
    let (bases, hits) = bases(stub::routes(stub::DATAAPI_LIVE, gemini_ok())).await;

    let (result, spawns) = spawn::scope(
        stub::yt_dlp(dir.path()),
        api::scope(
            bases,
            process_youtube(stub::URL, &config, &PublishPolicy::Vault, "yt-gemini-off", None),
        ),
    )
    .await;

    let result = result.expect("process_youtube");
    assert_eq!(spawns.count(), 0, "yt-dlp spawns under engine: gemini, slides off");
    assert!(!result.fell_back);
    assert_eq!(result.title, "21+ Rust Pro Tips (TOP SECRET)");
    assert_eq!(hits.get("/interactions"), 1);
}

/// Slides on: exactly one yt-dlp call, the mp4 download, and transcript
/// binding runs on the gemini fetch's segments. The canned download writes a
/// real mp4, so frame extraction (real ffmpeg), clustering and binding all
/// run; a fake fabric answers the slide-aware pattern so the manifest comes
/// back in the result. Before implementation audit round 1 the stub failed
/// the download, binding never ran, and the "no caption call" assert held
/// trivially.
#[tokio::test]
async fn gemini_engine_slides_on_spawns_only_the_mp4_download() {
    permits::HEAVY_PERMITS.init(4);
    let dir = tempfile::tempdir().unwrap();
    let mut config = stub::config(dir.path(), YoutubeEngine::Gemini, None);
    config.youtube.slides.enabled = true;
    // Keep every cluster and never propose text-only, so the manifest (and
    // its bound transcript) is what the slide path returns.
    config.youtube.slides.transition_min_seconds = 0.0;
    config.youtube.slides.slide_thresholds.min_unique_slides = 1;
    config.youtube.slides.slide_thresholds.text_only_max_ratio = 2.0;
    config.fabric.binary = stub::slides_fabric(dir.path()).to_string_lossy().to_string();
    let (bases, _hits) = bases(stub::routes(stub::DATAAPI_LIVE, gemini_ok())).await;

    let (result, spawns) = spawn::scope(
        stub::yt_dlp(dir.path()),
        api::scope(
            bases,
            process_youtube(stub::URL, &config, &PublishPolicy::Vault, "yt-gemini-on", None),
        ),
    )
    .await;

    let result = result.expect("process_youtube");
    assert_eq!(spawns.count(), 1, "calls: {:?}", spawns.calls());
    assert_eq!(
        spawns.count_with("--merge-output-format"),
        1,
        "the one call is the mp4 download"
    );
    let payload = result
        .slide_payload
        .expect("the slide path ran to a manifest: download, frames, binding");
    let _ = std::fs::remove_dir_all(&payload.slides_source_root);
    let manifest = payload.manifest;
    assert!(!manifest.slides.is_empty(), "frames clustered into slides");
    let bound: Vec<&String> = manifest.slides.iter().flat_map(|s| s.transcript.iter()).collect();
    assert!(
        bound
            .iter()
            .any(|line| line.as_str() == "[00:00] Welcome back to the channel."),
        "the first gemini segment is bound to a slide: {bound:?}"
    );
    let gemini_texts = [
        "Welcome back to the channel.",
        "Today we look at Rust error handling, starting with the question mark operator.",
        "Let's open the editor.",
        "Here is the first example.",
        "Thanks for watching.",
    ];
    for line in &bound {
        assert!(
            gemini_texts.iter().any(|t| line.ends_with(t)),
            "every bound line comes from VideoFetch.segments, none from a caption fetch: {line}"
        );
    }
}

/// Gemini 5xx twice (the first try and its one retry) + `fallback: yt-dlp`:
/// the yt-dlp arm runs and the result is marked fell-back.
#[tokio::test]
async fn gemini_5xx_twice_falls_back_to_yt_dlp() {
    permits::HEAVY_PERMITS.init(4);
    let dir = tempfile::tempdir().unwrap();
    let config = stub::config(dir.path(), YoutubeEngine::Gemini, Some(YoutubeEngine::YtDlp));
    let (bases, hits) = bases(stub::routes(stub::DATAAPI_LIVE, gemini_5xx_twice())).await;

    let (result, spawns) = spawn::scope(
        stub::yt_dlp(dir.path()),
        api::scope(
            bases,
            process_youtube(stub::URL, &config, &PublishPolicy::Vault, "yt-fallback", None),
        ),
    )
    .await;

    let result = result.expect("the fallback recovers the ingest");
    assert_eq!(hits.get("/interactions"), 2, "one try and one retry");
    assert_eq!(spawns.count_with("--dump-json"), 1, "the yt-dlp arm ran");
    assert!(result.fell_back, "a fallback fetch is marked");
    let not_degraded = transcript_distilled();
    assert!(!receipt_degraded(&not_degraded, false, false));
    assert!(
        receipt_degraded(&not_degraded, false, result.fell_back),
        "fell_back alone makes the receipt degraded"
    );
}

#[tokio::test]
async fn gemini_5xx_twice_without_fallback_fails() {
    let dir = tempfile::tempdir().unwrap();
    let config = stub::config(dir.path(), YoutubeEngine::Gemini, None);
    let (bases, _hits) = bases(stub::routes(stub::DATAAPI_LIVE, gemini_5xx_twice())).await;

    let (result, spawns) = spawn::scope(
        stub::yt_dlp(dir.path()),
        api::scope(bases, fetch_video(stub::URL, &config, "yt-no-fallback")),
    )
    .await;

    let err = result.expect_err("no fallback, so the gemini error fails the ingest");
    assert!(format!("{err:#}").contains("service error"), "{err:#}");
    assert_eq!(spawns.count(), 0);
}

/// Both engines failing names both errors in the receipt reason.
#[tokio::test]
async fn a_failed_fallback_names_both_engines() {
    let dir = tempfile::tempdir().unwrap();
    let config = stub::config(dir.path(), YoutubeEngine::Gemini, Some(YoutubeEngine::YtDlp));
    let (bases, _hits) = bases(stub::routes(stub::DATAAPI_LIVE, gemini_5xx_twice())).await;
    let failing: spawn::Stub = std::sync::Arc::new(|_: &[String]| {
        Ok(spawn::exit(
            1,
            "",
            "ERROR: [youtube] x: Requested format is not available",
        ))
    });

    let (result, _spawns) = spawn::scope(
        failing,
        api::scope(bases, fetch_video(stub::URL, &config, "yt-both-fail")),
    )
    .await;

    let reason = format!("{:#}", result.expect_err("both engines failed"));
    assert!(reason.contains("youtube engine gemini failed"), "{reason}");
    assert!(reason.contains("fallback yt-dlp failed"), "{reason}");
    assert!(reason.contains("yt-dlp metadata failed"), "{reason}");
}

/// `engine: yt-dlp` with no fallback: the error is the arm's own report,
/// so the receipt reason reads as it did before the seam.
#[tokio::test]
async fn yt_dlp_engine_error_reason_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let config = stub::config(dir.path(), YoutubeEngine::YtDlp, None);
    let failing: spawn::Stub = std::sync::Arc::new(|_: &[String]| {
        Ok(spawn::exit(
            1,
            "",
            "ERROR: [youtube] x: Requested format is not available",
        ))
    });
    let (result, _spawns) = spawn::scope(failing, fetch_video(stub::URL, &config, "yt-ytdlp-only")).await;
    let reason = format!("{:#}", result.expect_err("metadata failed"));
    assert!(reason.starts_with("yt-dlp metadata failed: yt-dlp failed:"), "{reason}");
}

/// One `process_content` run of [`stub::URL`] under `engine: gemini` with
/// `fallback`, against the Google stub serving `videos`, with receipts,
/// ledger, archive and vault in a temp XDG_DATA_HOME.
async fn run_pipeline(
    trace: &str,
    videos: &str,
    fallback: Option<YoutubeEngine>,
) -> (IngestResult, receipts::Receipt, spawn::Spawns, stub::Hits) {
    let _xdg = crate::harvest::TEST_XDG_LOCK.lock().await;
    permits::GENERAL_PERMITS.init(4);
    permits::HEAVY_PERMITS.init(4);
    let data_home = tempfile::TempDir::new().unwrap();
    let prior_xdg = std::env::var("XDG_DATA_HOME").ok();
    unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };
    let _xdg_sandbox = crate::receipts::sandbox::enter();

    let mut config = stub::config(data_home.path(), YoutubeEngine::Gemini, fallback);
    let vault_root = data_home.path().join("vault");
    std::fs::create_dir_all(vault_root.join(".obsidian")).unwrap();
    config.vault.root_path = Some(vault_root.to_string_lossy().to_string());
    let (bases, hits) = bases(stub::routes(videos, gemini_ok())).await;
    {
        let conn = receipts::open_default().unwrap();
        receipts::record_received(
            &conn,
            trace,
            vault::schema::Method::Http,
            vault::receipts::ReceiptKind::Url,
            stub::URL,
        )
        .unwrap();
    }

    let (result, spawns) = spawn::scope(
        stub::yt_dlp(data_home.path()),
        api::scope(
            bases,
            process_content(
                ContentKind::Url {
                    url: stub::URL.to_string(),
                    note: None,
                },
                Vec::new(),
                IngestMethod::Http,
                true,
                &config,
                &PublishPolicy::Vault,
                Some(trace.to_string()),
                None,
            ),
        ),
    )
    .await;

    let row = {
        let conn = receipts::open_default().unwrap();
        receipts::get(&conn, trace).unwrap().expect("receipt row")
    };
    if let Some(path) = &result.note_path {
        assert!(
            Path::new(path).starts_with(&vault_root),
            "the note lands in the temp vault: {path}"
        );
        assert!(Path::new(path).exists(), "published note exists: {path}");
    }
    unsafe {
        match prior_xdg {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
    (result, row, spawns, hits)
}

/// `engine: gemini`, slides off, the whole pipeline: a note publishes and
/// no yt-dlp process ran.
#[tokio::test]
async fn gemini_engine_publishes_a_note_with_zero_yt_dlp_spawns() {
    let (result, row, spawns, hits) = run_pipeline("20261016-120000-ytgem", stub::DATAAPI_LIVE, None).await;
    assert!(matches!(result.status, IngestStatus::Completed), "{:?}", result.status);
    assert!(result.note_path.is_some());
    assert_eq!(row.status, "succeeded");
    assert_eq!(spawns.count(), 0, "yt-dlp calls: {:?}", spawns.calls());
    assert_eq!(hits.get("/interactions"), 1);
}

/// Data API `items: []` + `fallback: yt-dlp`, through `process_content`:
/// the receipt is `fetch-failed` with a "not found" reason, and the yt-dlp
/// arm never runs.
#[tokio::test]
async fn data_api_not_found_fails_the_receipt_without_fallback() {
    let (result, row, spawns, hits) =
        run_pipeline("20261016-120000-yt404", stub::DATAAPI_EMPTY, Some(YoutubeEngine::YtDlp)).await;
    assert!(
        matches!(result.status, IngestStatus::Failed { .. }),
        "{:?}",
        result.status
    );
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure_stage.as_deref(), Some("fetch-failed"));
    let reason = row.failure_reason.expect("failure reason recorded");
    assert!(reason.contains("not found"), "{reason}");
    assert_eq!(spawns.count(), 0, "the yt-dlp arm never runs: {:?}", spawns.calls());
    assert_eq!(hits.get("/interactions"), 0);
}

/// What the stub vision API answers every slide classification with.
fn classified_as(category: &str) -> (&'static str, Vec<(u16, String)>) {
    let text = format!("CATEGORY: {category}\nCONFIDENCE: 0.9");
    let body = serde_json::json!({"content": [{"type": "text", "text": text}]});
    ("/v1/messages", vec![(200, body.to_string())])
}

/// What one slides-on, content-filter-on `process_content` run left behind.
struct FramesRun {
    result: IngestResult,
    /// The scratch root under `Scratch`; the same unused path under `Vault`.
    root: PathBuf,
    vault_root: PathBuf,
    work_dir: PathBuf,
    classify_calls: usize,
    _data_home: tempfile::TempDir,
}

/// What a frames run obstructs, to make its preserve fail.
#[derive(Clone, Copy, PartialEq)]
enum Obstruct {
    Nothing,
    /// A file at `root/frames`, so the preserve copy cannot create its dest.
    FramesDest,
    /// A directory at the work dir's `runs.yml`, so `write_runs` fails.
    RunsFile,
}

/// One `process_content` run of [`stub::URL`] with slides and the content
/// filter on, every run classified as `category`, and `fabric` as the fabric
/// binary (`None`: one that does not exist). `scratch` picks the policy.
async fn run_frames(trace: &str, scratch: bool, category: &str, fabric: Option<fn(&Path) -> PathBuf>) -> FramesRun {
    run_frames_obstructed(trace, scratch, category, fabric, Obstruct::Nothing).await
}

/// [`run_frames`] with one part of the preserve obstructed.
async fn run_frames_obstructed(
    trace: &str,
    scratch: bool,
    category: &str,
    fabric: Option<fn(&Path) -> PathBuf>,
    obstruct: Obstruct,
) -> FramesRun {
    let _xdg = crate::harvest::TEST_XDG_LOCK.lock().await;
    permits::GENERAL_PERMITS.init(4);
    permits::HEAVY_PERMITS.init(4);
    let data_home = tempfile::TempDir::new().unwrap();
    let prior_xdg = std::env::var("XDG_DATA_HOME").ok();
    unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };
    let _xdg_sandbox = crate::receipts::sandbox::enter();

    let mut config = stub::config(data_home.path(), YoutubeEngine::Gemini, None);
    let vault_root = data_home.path().join("vault");
    std::fs::create_dir_all(vault_root.join(".obsidian")).unwrap();
    config.vault.root_path = Some(vault_root.to_string_lossy().to_string());
    config.youtube.slides.enabled = true;
    config.youtube.slides.content_filter.enabled = true;
    let key = data_home.path().join("anthropic.key");
    std::fs::write(&key, "stub-key").unwrap();
    config.llm.api_key = key.to_string_lossy().to_string();
    if let Some(fabric) = fabric {
        config.fabric.binary = fabric(data_home.path()).to_string_lossy().to_string();
    }
    let root = data_home.path().join("distill").join(trace);
    if obstruct == Obstruct::FramesDest {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("frames"), b"in the way").unwrap();
    }
    let yt_dlp = stub::yt_dlp(data_home.path());
    let yt_dlp: spawn::Stub = if obstruct == Obstruct::RunsFile {
        // The mp4 download is the first write into the work dir after it is
        // made, so the obstruction lands before `write_runs` runs.
        std::sync::Arc::new(move |argv: &[String]| {
            let out = yt_dlp(argv)?;
            if let Some(o) = argv.iter().position(|a| a == "-o").and_then(|i| argv.get(i + 1)) {
                std::fs::create_dir_all(Path::new(o).with_file_name(crate::pipeline::frames::RUNS_FILE))?;
            }
            Ok(out)
        })
    } else {
        yt_dlp
    };
    let policy = if scratch {
        PublishPolicy::Scratch { root: root.clone() }
    } else {
        PublishPolicy::Vault
    };
    let mut routes = stub::routes(stub::DATAAPI_LIVE, gemini_ok());
    routes.push(classified_as(category));
    let (base, hits) = stub::serve(routes).await;
    let bases = ApiBases {
        data_api: base.clone(),
        gemini: base.clone(),
    };
    {
        let conn = receipts::open_default().unwrap();
        receipts::record_received(
            &conn,
            trace,
            vault::schema::Method::Cli,
            vault::receipts::ReceiptKind::Url,
            stub::URL,
        )
        .unwrap();
    }

    let (result, _spawns) = spawn::scope(
        yt_dlp,
        api::scope(
            bases,
            crate::ocr::bases::scope(
                base,
                // Boxed: the slides-on pipeline future overflows the test
                // thread's stack when held inline under three scopes.
                Box::pin(process_content(
                    ContentKind::Url {
                        url: stub::URL.to_string(),
                        note: None,
                    },
                    Vec::new(),
                    IngestMethod::Cli,
                    true,
                    &config,
                    &policy,
                    Some(trace.to_string()),
                    None,
                )),
            ),
        ),
    )
    .await;

    unsafe {
        match prior_xdg {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
    FramesRun {
        result,
        root,
        vault_root,
        work_dir: std::env::temp_dir().join("borg-youtube-frames").join(trace),
        classify_calls: hits.get("/v1/messages"),
        _data_home: data_home,
    }
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// `root/frames/` holds every extracted frame `frames.yml` lists, and no
/// source video anywhere under `root`.
fn assert_frames_preserved(run: &FramesRun) {
    let preserved = run.root.join("frames");
    let sidecar: crate::youtube::FramesSidecar =
        serde_yaml::from_str(&std::fs::read_to_string(preserved.join("frames.yml")).expect("frames.yml preserved"))
            .expect("frames.yml parses");
    assert!(sidecar.frames_extracted > 0, "the stub mp4 yielded frames");
    for frame in &sidecar.frames {
        assert!(
            frame.path.is_relative(),
            "frames.yml points into the preserved dir, not /tmp: {}",
            frame.path.display()
        );
        assert!(
            preserved.join(&frame.path).is_file(),
            "{} resolves",
            frame.path.display()
        );
    }
    let jpgs = files_under(&preserved.join("frames"))
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jpg"))
        .count();
    assert_eq!(
        jpgs, sidecar.frames_extracted as usize,
        "every extracted frame, no more"
    );
    let videos: Vec<PathBuf> = files_under(&run.root)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "mp4"))
        .collect();
    assert!(videos.is_empty(), "no source video preserved: {videos:?}");
    assert!(
        !run.work_dir.exists(),
        "the /tmp work dir is gone: {}",
        run.work_dir.display()
    );
}

/// `root/frames/runs.yml` has one entry per classified run, each with `ocr`.
fn assert_runs_recorded(run: &FramesRun) {
    let runs: Vec<serde_yaml::Mapping> =
        serde_yaml::from_str(&std::fs::read_to_string(run.root.join("frames/runs.yml")).expect("runs.yml preserved"))
            .expect("runs.yml is a list");
    assert!(run.classify_calls > 0, "the content filter classified runs");
    assert_eq!(runs.len(), run.classify_calls, "one entry per classified run");
    for entry in &runs {
        assert!(entry.contains_key("ocr"), "every run carries ocr: {entry:?}");
        let frame = entry["best-frame"].as_str().expect("best-frame");
        assert!(run.root.join("frames").join(frame).is_file(), "{frame} resolves");
    }
}

/// The classifier drops every run, so the slide path returns text-only from
/// inside the handler: under `Scratch` the guard still preserves the whole
/// frames tree plus `runs.yml`; under `Vault` nothing is preserved anywhere.
#[tokio::test]
async fn distill_policy_keeps_frames_on_text_only() {
    let scratch = run_frames("20261025-120000-ytfrtx", true, "talking-head", None).await;
    assert!(
        matches!(scratch.result.status, IngestStatus::Completed),
        "{:?}",
        scratch.result.status
    );
    assert_frames_preserved(&scratch);
    assert_runs_recorded(&scratch);
    // The tally line and the manifest take one `trace_id`; it must be the
    // run's trace, not the video id, so `tally[<trace>]` matches runs.yml.
    let manifest: crate::slides::SlideManifest =
        serde_yaml::from_str(&std::fs::read_to_string(scratch.root.join("frames/slides.yml")).expect("slides.yml"))
            .expect("slides.yml parses");
    assert_eq!(manifest.trace_id, "20261025-120000-ytfrtx");
    let slides = files_under(&scratch.root.join("frames/slides"));
    assert!(slides.is_empty(), "no run kept, so none materialized: {slides:?}");

    let vault = run_frames("20261025-120000-ytfrtv", false, "talking-head", None).await;
    assert!(
        matches!(vault.result.status, IngestStatus::Completed),
        "{:?}",
        vault.result.status
    );
    assert!(!vault.root.join("frames").exists(), "Vault preserves no frames");
    assert!(!vault.work_dir.exists(), "Vault still removes the work dir");
    let runs_files: Vec<PathBuf> = files_under(&vault.vault_root)
        .into_iter()
        .filter(|p| p.ends_with("runs.yml"))
        .collect();
    assert!(runs_files.is_empty(), "Vault writes no runs.yml: {runs_files:?}");
}

/// Every run is kept, so the slide path goes on to the slides pattern, and
/// the fabric binary does not exist: the slide path errors after frame
/// extraction and the note falls back to text-only. The frames are preserved.
#[tokio::test]
async fn distill_policy_keeps_frames_on_slide_error() {
    let run = run_frames("20261025-120000-ytfrer", true, "architecture-diagram", None).await;
    assert!(
        matches!(run.result.status, IngestStatus::Completed),
        "{:?}",
        run.result.status
    );
    assert_frames_preserved(&run);
    assert_runs_recorded(&run);
    assert!(
        !files_under(&run.root.join("frames/slides")).is_empty(),
        "the kept runs were materialized before the error"
    );
    assert!(
        files_under(&run.root.join("system/attachments")).is_empty(),
        "the slide path failed, so the note embeds no slide"
    );
}

/// Every run is kept and the slides pattern answers: the note embeds a slide
/// and `process_url_inner` retires the work dir after `publish_slides`, which
/// preserves it the same way.
#[tokio::test]
async fn distill_policy_keeps_frames_on_slide_note() {
    let run = run_frames(
        "20261025-120000-ytfrsn",
        true,
        "architecture-diagram",
        Some(stub::slides_fabric),
    )
    .await;
    assert!(
        matches!(run.result.status, IngestStatus::Completed),
        "{:?}",
        run.result.status
    );
    assert_frames_preserved(&run);
    assert_runs_recorded(&run);
    let embedded: Vec<PathBuf> = files_under(&run.root.join("system/attachments"))
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jpg"))
        .collect();
    assert!(
        !embedded.is_empty(),
        "publish_slides copied the embedded slide under root"
    );
}

/// A failed preserve copy on the slide-note path (retired in
/// `process_url_inner`) fails the run `publish-failed`; the note is still the
/// slide note, and the /tmp work dir is still removed.
#[tokio::test]
async fn distill_policy_fails_on_a_failed_frames_preserve() {
    let run = run_frames_obstructed(
        "20261025-120000-ytfrpf",
        true,
        "architecture-diagram",
        Some(stub::slides_fabric),
        Obstruct::FramesDest,
    )
    .await;
    let IngestStatus::Failed { reason } = &run.result.status else {
        panic!("a failed preserve must fail the run: {:?}", run.result.status);
    };
    assert!(reason.starts_with("frames preserve failed:"), "{reason}");
    assert_eq!(run.result.failure_stage, Some(FailureStage::PublishFailed));
    let note = run.result.note_path.as_deref().expect("the note was written");
    assert!(Path::new(note).is_file(), "{note}");
    assert!(
        !files_under(&run.root.join("system/attachments")).is_empty(),
        "the slide path did not fail: the note still embeds its slide"
    );
    assert!(!run.work_dir.exists(), "the /tmp work dir is still removed");
}

/// A failed `runs.yml` write on the text-only path (the guard drops in the
/// handler) fails the run the same way.
#[tokio::test]
async fn distill_policy_fails_on_a_failed_runs_write() {
    let run = run_frames_obstructed("20261025-120000-ytfrrf", true, "talking-head", None, Obstruct::RunsFile).await;
    let IngestStatus::Failed { reason } = &run.result.status else {
        panic!("a failed runs.yml write must fail the run: {:?}", run.result.status);
    };
    assert!(reason.contains("runs.yml write failed"), "{reason}");
    assert_eq!(run.result.failure_stage, Some(FailureStage::PublishFailed));
    assert!(!run.work_dir.exists(), "the /tmp work dir is still removed");
}
