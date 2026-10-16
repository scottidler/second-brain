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
        api::scope(bases, process_youtube(stub::URL, &config, "yt-gemini-off", None)),
    )
    .await;

    let result = result.expect("process_youtube");
    assert_eq!(spawns.count(), 0, "yt-dlp spawns under engine: gemini, slides off");
    assert!(!result.fell_back);
    assert_eq!(result.title, "21+ Rust Pro Tips (TOP SECRET)");
    assert_eq!(hits.get("/interactions"), 1);
}

/// Slides on: exactly one yt-dlp call, the mp4 download. Transcript binding
/// reads the fetch's segments and makes no caption call.
#[tokio::test]
async fn gemini_engine_slides_on_spawns_only_the_mp4_download() {
    permits::HEAVY_PERMITS.init(4);
    let dir = tempfile::tempdir().unwrap();
    let mut config = stub::config(dir.path(), YoutubeEngine::Gemini, None);
    config.youtube.slides.enabled = true;
    let (bases, _hits) = bases(stub::routes(stub::DATAAPI_LIVE, gemini_ok())).await;

    let (result, spawns) = spawn::scope(
        stub::yt_dlp(dir.path()),
        api::scope(bases, process_youtube(stub::URL, &config, "yt-gemini-on", None)),
    )
    .await;

    let result = result.expect("a failed slides download degrades to text-only, it does not fail");
    assert_eq!(spawns.count(), 1, "calls: {:?}", spawns.calls());
    assert_eq!(
        spawns.count_with("--merge-output-format"),
        1,
        "the one call is the mp4 download"
    );
    assert_eq!(spawns.count_with("--write-auto-sub"), 0, "binding made no caption call");
    assert!(result.slide_payload.is_none());
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
        api::scope(bases, process_youtube(stub::URL, &config, "yt-fallback", None)),
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
