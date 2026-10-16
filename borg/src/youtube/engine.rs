//! The YouTube fetch engine seam: one call gives an ingest its metadata and
//! transcript, from whichever engine `youtube.engine` selects.
//!
//! [`fetch`] matches on the kind, the way `distillers::tags::build_kind` does.
//! The `yt-dlp` arm is the pipeline's original calls, moved here: metadata
//! (the only call that records into `youtube::breaker`), the fabric `-y` ->
//! subtitles -> Groq whisper transcript chain, and one raw-VTT fetch for the
//! timestamped segments. The `gemini` arm is the Data API plus Gemini and
//! never consults the breaker: breaker state is yt-dlp's.
//!
//! Fallback policy lives in the caller (`pipeline::process_youtube`).
//! Design: `docs/design/2026-10-16-youtube-gemini-engine.md`, Architecture.

use std::time::Instant;

use eyre::{Context, Result};

use super::dataapi::DataApiClient;
use super::error::FetchError;
use super::gemini::GeminiClient;
use super::{VideoMetadata, clean_vtt, extract_video_id, parse_vtt_segments};
use crate::config::{Config, YoutubeEngine};
use crate::fabric;
use crate::transcription::TranscriptionClient;
use crate::types::AudioFormat;

/// Everything one ingest needs from YouTube, fetched once.
#[derive(Debug, Clone)]
pub struct VideoFetch {
    pub metadata: VideoMetadata,
    /// Plain transcript, for the consumers that take untimestamped text.
    pub transcript: String,
    /// Timestamped `(start seconds, text)` segments, the shape
    /// `parse_vtt_segments` yields. Empty when the engine had none.
    pub segments: Vec<(f64, String)>,
    /// The engine that produced this fetch.
    pub engine: YoutubeEngine,
    /// True when the fallback engine produced it; the receipt is then degraded.
    pub fell_back: bool,
}

/// Fetch `url` through the `kind` engine. `fell_back` is always false here;
/// the caller that ran a fallback sets it.
pub async fn fetch(kind: YoutubeEngine, url: &str, config: &Config) -> Result<VideoFetch, FetchError> {
    log::debug!("engine::fetch: kind={} url={url}", kind.as_str());
    let result = match kind {
        YoutubeEngine::YtDlp => fetch_yt_dlp(url, config).await.map_err(FetchError::YtDlp),
        YoutubeEngine::Gemini => fetch_gemini(url, config).await,
    };
    match &result {
        Ok(v) => log::debug!(
            "engine::fetch: kind={} ok title={:?} transcript_bytes={} segments={}",
            kind.as_str(),
            v.metadata.title,
            v.transcript.len(),
            v.segments.len()
        ),
        Err(e) => log::debug!(
            "engine::fetch: kind={} err permanent={} {e}",
            kind.as_str(),
            e.is_permanent()
        ),
    }
    result
}

/// The `yt-dlp` arm: today's metadata and transcript calls, moved unchanged.
/// Metadata (yt-dlp) and transcript (fabric) run concurrently; fabric is
/// optional. The raw VTT is fetched once, after metadata: its segments carry
/// the timestamps, and its cleaned text is the transcript when fabric has none.
async fn fetch_yt_dlp(url: &str, config: &Config) -> Result<VideoFetch> {
    let use_fabric = fabric::is_available(&config.fabric);

    let url_owned = url.to_string();
    let yt_dlp_timeout = config.pipeline.yt_dlp_timeout_secs;
    let metadata_future = super::fetch_metadata(&url_owned, yt_dlp_timeout);

    let transcript_future = async {
        if use_fabric {
            let config_fabric = config.fabric.clone();
            let config_pipeline = config.pipeline.clone();
            let url = url_owned.clone();
            let result =
                tokio::task::spawn_blocking(move || fabric::fetch_transcript(&url, &config_fabric, &config_pipeline))
                    .await
                    .unwrap_or_else(|e| {
                        log::warn!("Fabric transcript task panicked: {e}");
                        Ok(String::new())
                    });
            result.unwrap_or_else(|e| {
                log::warn!("Fabric transcript failed: {e:#}");
                String::new()
            })
        } else {
            String::new()
        }
    };

    let (metadata_result, fabric_transcript) = tokio::join!(metadata_future, transcript_future);
    let metadata = metadata_result.context("yt-dlp metadata failed")?;

    let raw_vtt = super::fetch_subtitles_raw(url, &config.pipeline).await;
    let segments = match &raw_vtt {
        Ok(Some(vtt)) => parse_vtt_segments(vtt),
        Ok(None) => Vec::new(),
        Err(e) => {
            log::warn!("yt-dlp raw subtitle fetch failed: {e:#}; no timestamped segments");
            Vec::new()
        }
    };

    // Transcript fallback chain: fabric -> yt-dlp subtitles -> audio extraction + Groq
    let transcript = if !fabric_transcript.is_empty() {
        fabric_transcript
    } else {
        if use_fabric {
            log::warn!("Fabric returned empty transcript, falling back to yt-dlp subtitles");
        }
        match raw_vtt? {
            Some(vtt) => clean_vtt(&vtt),
            None => {
                log::warn!("No subtitles available, falling back to audio extraction + Groq");
                whisper_transcript(url, config).await?
            }
        }
    };

    Ok(VideoFetch {
        metadata,
        transcript,
        segments,
        engine: YoutubeEngine::YtDlp,
        fell_back: false,
    })
}

/// The last link of the yt-dlp transcript chain: extract audio, send it to Groq.
async fn whisper_transcript(url: &str, config: &Config) -> Result<String> {
    let temp_dir = std::env::temp_dir().join("obsidian-borg");
    std::fs::create_dir_all(&temp_dir)?;
    let audio_path = super::extract_audio(
        url,
        &temp_dir.to_string_lossy(),
        config.youtube.yt_dlp_postprocessor_threads(),
        config.pipeline.yt_dlp_timeout_secs,
    )
    .await?;
    let audio_bytes = std::fs::read(&audio_path)?;
    let _ = std::fs::remove_file(&audio_path);

    let groq_key = crate::config::resolve_secret(&config.groq.api_key).ok();
    let client = TranscriptionClient::new(
        &config.transcriber.url,
        groq_key,
        &config.groq.model,
        config.transcriber.timeout_secs,
    )?;
    let response = client.transcribe(audio_bytes, AudioFormat::Mp3, None).await?;
    Ok(response.text)
}

/// The `gemini` arm: Data API metadata, then Gemini over clip windows. No
/// yt-dlp process and no breaker. The budget clock starts before the Data
/// API request, because windows need its `duration_secs`.
async fn fetch_gemini(url: &str, config: &Config) -> Result<VideoFetch, FetchError> {
    let video_id =
        extract_video_id(url).ok_or_else(|| FetchError::Response(format!("no youtube video id in url {url}")))?;
    let started = Instant::now();
    let budget = config.youtube.gemini.budget;
    // The budget covers the whole fetch, Data API included: each Data API call
    // is bounded by `youtube.data-api.timeout` (the client) and, here, by what
    // is left of the budget, whichever is smaller.
    let metadata = match tokio::time::timeout(budget, data_api_client(config)?.fetch_metadata(&video_id)).await {
        Ok(result) => result?,
        Err(_) => {
            log::warn!(
                "engine::fetch_gemini: video {video_id} data api did not answer within the gemini budget of {}",
                humantime::format_duration(budget)
            );
            return Err(FetchError::Timeout);
        }
    };
    let transcript = gemini_client(config)?
        .fetch_transcript(&video_id, metadata.duration_secs, started)
        .await?;
    Ok(VideoFetch {
        metadata,
        transcript: transcript.plain_text(),
        segments: transcript.segments,
        engine: YoutubeEngine::Gemini,
        fell_back: false,
    })
}

/// Each Data API request is bounded by `youtube.data-api.timeout`;
/// [`fetch_gemini`] also bounds the pair by the gemini budget.
#[cfg(not(test))]
fn data_api_client(config: &Config) -> Result<DataApiClient, FetchError> {
    DataApiClient::from_config(&config.youtube.data_api)
}

#[cfg(not(test))]
fn gemini_client(config: &Config) -> Result<GeminiClient, FetchError> {
    GeminiClient::from_config(&config.youtube.gemini)
}

/// Tests reach only the stubs named by [`bases::scope`]; outside one the
/// gemini arm fails instead of calling Google.
#[cfg(test)]
fn data_api_client(config: &Config) -> Result<DataApiClient, FetchError> {
    let bases = bases::current()?;
    let key = vault::config::resolve_secret(&config.youtube.data_api.api_key).unwrap_or_default();
    DataApiClient::with_base_url(key, &bases.data_api, config.youtube.data_api.timeout)
}

#[cfg(test)]
fn gemini_client(config: &Config) -> Result<GeminiClient, FetchError> {
    let bases = bases::current()?;
    let key = vault::config::resolve_secret(&config.youtube.gemini.api_key).unwrap_or_default();
    GeminiClient::with_base_url(key, &config.youtube.gemini, &bases.gemini)
}

#[cfg(test)]
pub(crate) mod bases;

#[cfg(test)]
pub(crate) mod stub;

#[cfg(test)]
mod tests;
