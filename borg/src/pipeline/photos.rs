//! Photo description shared by image ingest and X posts: one section builder
//! (vision + OCR merge), and the per-photo download + describe an X post runs.
//!
//! The X path calls the `ocr` functions directly and never re-enters
//! `process_content`: the URL trace already holds a GENERAL permit, so a
//! nested `process_image` would wait on the pool it is holding.

use crate::config::Config;
use crate::ocr::{self, VisionResult};
use crate::x::{self, DescribedImage};
use eyre::{Context, Result};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// The OCR port: `ocr::ocr_extract` in a build, a canned reader in tests.
pub(crate) type OcrFn = fn(&Path, u64) -> Result<String>;

/// The Description / Extracted Text section for one image, headings at
/// `heading` (`##` for an image note, `####` under an X post's `### Image N`).
/// Vision text is preferred, OCR is the fallback; `Description` appears only
/// when vision returned one; the text is labeled `Extracted Text` when vision
/// ran and `OCR Text` when it did not. Empty when there is nothing to say.
pub(crate) fn image_section(vision: Option<&VisionResult>, ocr_text: &str, heading: &str) -> String {
    let extracted_text = vision
        .map(|v| v.extracted_text.as_str())
        .filter(|t| !t.is_empty())
        .unwrap_or(ocr_text);
    let mut parts = Vec::new();
    if let Some(v) = vision
        && !v.description.is_empty()
    {
        parts.push(format!("{heading} Description\n\n{}", v.description));
    }
    if !extracted_text.is_empty() {
        let label = if vision.is_some() { "Extracted Text" } else { "OCR Text" };
        parts.push(format!("{heading} {label}\n\n{extracted_text}"));
    }
    parts.join("\n\n")
}

/// Every photo on an X post and its quote, described in order. Returns the
/// sections that were built and whether any photo degraded (download failed,
/// vision enabled but failed or empty, or nothing to say). A degraded photo
/// with no section is omitted, never published as an empty heading.
pub(crate) async fn describe_x_photos(
    post: &x::Post,
    config: &Config,
    ocr: OcrFn,
    trace_id: &str,
) -> (Vec<DescribedImage>, bool) {
    let photos = x::photos(post);
    log::debug!("describe_x_photos[{trace_id}]: photos={}", photos.len());
    let described = futures::future::join_all(
        photos
            .iter()
            .map(|(_, photo)| describe_photo(&photo.url, config, ocr, trace_id)),
    )
    .await;
    let mut images = Vec::new();
    let mut degraded = false;
    for ((owner, _), (section, photo_degraded)) in photos.iter().zip(described) {
        degraded |= photo_degraded;
        if let Some(section) = section {
            images.push(DescribedImage { owner: *owner, section });
        }
    }
    log::debug!(
        "describe_x_photos[{trace_id}]: sections={} degraded={degraded}",
        images.len()
    );
    (images, degraded)
}

/// One photo: `(section, degraded)`. OCR runs on a blocking thread joined
/// with the vision call, as image ingest does; only vision takes a permit.
async fn describe_photo(url: &str, config: &Config, ocr: OcrFn, trace_id: &str) -> (Option<String>, bool) {
    let (bytes, mime) = match download_photo(url, config.pipeline.fxtwitter_timeout_secs).await {
        Ok(photo) => photo,
        Err(e) => {
            log::warn!("[{trace_id}] X photo omitted: {e:#}");
            return (None, true);
        }
    };
    let ocr_timeout = config.pipeline.ocr_timeout_secs;
    let ocr_bytes = bytes.clone();
    let ocr_handle = tokio::task::spawn_blocking(move || ocr_photo(&ocr_bytes, ocr, ocr_timeout));
    let vision_future = async {
        if !config.vision.enabled {
            return (None, false);
        }
        match ocr::vision_extract(&bytes, &mime, &config.vision, &config.llm).await {
            Ok(v) if !v.description.trim().is_empty() => (Some(v), false),
            Ok(_) => {
                log::warn!("[{trace_id}] vision returned no description for X photo {url}");
                (None, true)
            }
            Err(e) => {
                log::warn!("[{trace_id}] vision failed for X photo {url}: {e:#}");
                (None, true)
            }
        }
    };
    let (ocr_result, (vision, vision_failed)) = tokio::join!(ocr_handle, vision_future);
    let ocr_text = match ocr_result {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => {
            log::warn!("[{trace_id}] OCR failed for X photo {url}: {e:#}");
            String::new()
        }
        Err(e) => {
            log::warn!("[{trace_id}] OCR task failed for X photo {url}: {e}");
            String::new()
        }
    };
    let section = image_section(vision.as_ref(), &ocr_text, "####");
    if section.is_empty() {
        log::warn!("[{trace_id}] X photo omitted: neither vision nor OCR produced text for {url}");
        return (None, true);
    }
    (Some(section), vision_failed)
}

/// Fetch a photo's bytes and MIME type. Non-2xx, or a `Content-Type` that is
/// not `image/*`, is an `Err`. The MIME comes from the header because the URL
/// ends `.jpg?name=orig`, which `ocr::mime_from_extension` misreads.
async fn download_photo(url: &str, timeout_secs: u64) -> Result<(Vec<u8>, String)> {
    let client = vault::http::client(Duration::from_secs(timeout_secs)).context("build photo client")?;
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("download X photo {url}"))?;
    let status = response.status();
    if !status.is_success() {
        eyre::bail!("X photo {url} returned status {status}");
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if !mime.starts_with("image/") {
        eyre::bail!("X photo {url} has content-type {mime:?}, not an image");
    }
    let bytes = response.bytes().await.with_context(|| format!("read X photo {url}"))?;
    log::debug!("download_photo: url={url} mime={mime} bytes={}", bytes.len());
    Ok((bytes.to_vec(), mime))
}

/// OCR reads a file, so the bytes go to a temp file that lives for the call.
fn ocr_photo(bytes: &[u8], ocr: OcrFn, timeout_secs: u64) -> Result<String> {
    let mut file = tempfile::NamedTempFile::new().context("create OCR temp file")?;
    file.write_all(bytes).context("write OCR temp file")?;
    ocr(file.path(), timeout_secs)
}

#[cfg(test)]
mod tests;
