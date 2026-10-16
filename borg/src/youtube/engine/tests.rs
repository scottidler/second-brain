#![allow(clippy::unwrap_used)]

use super::bases::{self as api, ApiBases};
use super::stub;
use super::*;
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

#[tokio::test]
async fn gemini_arm_fills_the_fetch_with_zero_yt_dlp_spawns() {
    let dir = tempfile::tempdir().unwrap();
    let config = stub::config(dir.path(), YoutubeEngine::Gemini, None);
    let (bases, hits) = bases(stub::routes(
        stub::DATAAPI_LIVE,
        vec![(200, stub::GEMINI_OK.to_string())],
    ))
    .await;

    let (result, spawns) = spawn::scope(
        stub::yt_dlp(dir.path()),
        api::scope(bases, fetch(YoutubeEngine::Gemini, stub::URL, &config)),
    )
    .await;

    let video = result.unwrap();
    assert_eq!(spawns.count(), 0, "the gemini arm starts no yt-dlp process");
    assert_eq!(video.engine, YoutubeEngine::Gemini);
    assert!(!video.fell_back);
    assert_eq!(video.metadata.title, "21+ Rust Pro Tips (TOP SECRET)");
    assert_eq!(video.metadata.duration_secs, 512.0);
    assert_eq!(
        video.segments.first().unwrap(),
        &(0.0, "Welcome back to the channel.".to_string())
    );
    assert_eq!(video.segments.last().unwrap().0, 510.0);
    let joined: Vec<&str> = video.segments.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(
        video.transcript,
        joined.join("\n"),
        "transcript is the segment texts joined"
    );
    assert_eq!(hits.get("/videos"), 1);
    assert_eq!(hits.get("/interactions"), 1);
}

#[tokio::test]
async fn gemini_arm_reports_data_api_not_found_as_permanent() {
    let dir = tempfile::tempdir().unwrap();
    let config = stub::config(dir.path(), YoutubeEngine::Gemini, None);
    let (bases, hits) = bases(stub::routes(
        stub::DATAAPI_EMPTY,
        vec![(200, stub::GEMINI_OK.to_string())],
    ))
    .await;

    let err = api::scope(bases, fetch(YoutubeEngine::Gemini, stub::URL, &config))
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::NotFound(_)), "{err:?}");
    assert!(err.is_permanent());
    assert_eq!(hits.get("/interactions"), 0, "a dead video never reaches gemini");
}

#[tokio::test]
async fn gemini_arm_fails_on_a_url_with_no_video_id() {
    let dir = tempfile::tempdir().unwrap();
    let config = stub::config(dir.path(), YoutubeEngine::Gemini, None);
    let (bases, hits) = bases(stub::routes(
        stub::DATAAPI_LIVE,
        vec![(200, stub::GEMINI_OK.to_string())],
    ))
    .await;
    let err = api::scope(
        bases,
        fetch(YoutubeEngine::Gemini, "https://www.youtube.com/@handle", &config),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, FetchError::Response(_)), "{err:?}");
    assert!(!err.is_permanent());
    assert_eq!(hits.get("/videos"), 0);
}

#[tokio::test]
async fn gemini_arm_outside_a_test_scope_never_calls_google() {
    let dir = tempfile::tempdir().unwrap();
    let config = stub::config(dir.path(), YoutubeEngine::Gemini, None);
    let err = fetch(YoutubeEngine::Gemini, stub::URL, &config).await.unwrap_err();
    assert!(err.to_string().contains("outside bases::scope"), "{err}");
}

/// The yt-dlp arm, against the canned yt-dlp: one metadata call, one raw-VTT
/// call, and the transcript is the cleaned VTT because fabric is absent.
#[tokio::test]
async fn yt_dlp_arm_fetches_metadata_and_subtitles_once_each() {
    let dir = tempfile::tempdir().unwrap();
    let config = stub::config(dir.path(), YoutubeEngine::YtDlp, None);

    let (result, spawns) = spawn::scope(
        stub::yt_dlp(dir.path()),
        fetch(YoutubeEngine::YtDlp, stub::URL, &config),
    )
    .await;

    let video = result.unwrap();
    assert_eq!(video.engine, YoutubeEngine::YtDlp);
    assert_eq!(spawns.count_with("--dump-json"), 1);
    assert_eq!(spawns.count_with("--write-auto-sub"), 1, "the raw VTT is fetched once");
    assert_eq!(spawns.count(), 2);
    assert_eq!(
        video.segments,
        vec![
            (1.0, "from the yt-dlp arm".to_string()),
            (4.0, "second caption line".to_string())
        ]
    );
    assert_eq!(video.transcript, "from the yt-dlp arm second caption line");
}

#[tokio::test]
async fn yt_dlp_arm_wraps_a_metadata_failure_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let config = stub::config(dir.path(), YoutubeEngine::YtDlp, None);
    let failing: spawn::Stub =
        std::sync::Arc::new(|_: &[String]| Ok(spawn::exit(1, "", "ERROR: [youtube] x: Private video")));

    let (result, spawns) = spawn::scope(failing, fetch(YoutubeEngine::YtDlp, stub::URL, &config)).await;

    let err = result.unwrap_err();
    assert!(matches!(err, FetchError::YtDlp(_)), "{err:?}");
    assert!(!err.is_permanent());
    assert!(err.to_string().contains("yt-dlp metadata failed"), "{err}");
    assert_eq!(spawns.count(), 1, "no subtitle fetch after a metadata failure");
}

/// A loopback server that accepts every connection and never answers.
async fn hanging_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// The gemini budget covers the whole fetch, measured from the first Data
/// API request: a Data API that never answers is `Timeout` at the budget,
/// even when `youtube.data-api.timeout` alone would wait longer.
#[tokio::test]
async fn gemini_budget_bounds_the_data_api_calls() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = stub::config(dir.path(), YoutubeEngine::Gemini, None);
    config.youtube.data_api.timeout = std::time::Duration::from_secs(30);
    config.youtube.gemini.timeout = std::time::Duration::from_millis(300);
    config.youtube.gemini.budget = std::time::Duration::from_millis(300);
    let hang = hanging_server().await;
    let bases = ApiBases {
        data_api: hang.clone(),
        gemini: hang,
    };

    let started = Instant::now();
    let err = api::scope(bases, fetch(YoutubeEngine::Gemini, stub::URL, &config))
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(matches!(err, FetchError::Timeout), "{err:?}");
    assert!(!err.is_permanent(), "a timeout leaves the fallback free to run");
    assert!(elapsed >= std::time::Duration::from_millis(300), "{elapsed:?}");
    assert!(elapsed < std::time::Duration::from_secs(3), "{elapsed:?}");
}

/// Each Data API request has its own bound, `youtube.data-api.timeout`, not
/// the 5m gemini per-call timeout: a hung Data API with budget to spare is
/// a `Network` error at the data-api timeout.
#[tokio::test]
async fn data_api_calls_use_the_data_api_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = stub::config(dir.path(), YoutubeEngine::Gemini, None);
    config.youtube.data_api.timeout = std::time::Duration::from_millis(200);
    config.youtube.gemini.timeout = std::time::Duration::from_secs(30);
    config.youtube.gemini.budget = std::time::Duration::from_secs(30);
    let hang = hanging_server().await;
    let bases = ApiBases {
        data_api: hang.clone(),
        gemini: hang,
    };

    let started = Instant::now();
    let err = api::scope(bases, fetch(YoutubeEngine::Gemini, stub::URL, &config))
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Network(_)), "{err:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}
