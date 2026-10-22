//! Figures: the images borg stores from a note's source and embeds in it
//! (`docs/design/2026-10-22-capture-all-diagrams.md`).
//!
//! This module owns both sides of the pipeline. The source side: the one
//! section builder ([`image_section`], shared with image ingest) and the
//! per-photo download + describe an X post runs, which fills a [`FigureSet`].
//! The vault side: [`plan`] assigns every kept figure its path and bounds its
//! bytes (pure, no I/O) and builds the `## Images` body; [`store`] is the only
//! writer.
//!
//! The X path calls the `ocr` functions directly and never re-enters
//! `process_content`: the URL trace already holds a GENERAL permit, so a
//! nested `process_image` would wait on the pool it is holding.

use crate::config::{Config, ImagesConfig};
use crate::ocr::{self, VisionResult};
use crate::slides::publish::month_subdir;
use crate::x::{self, DescribedImage, Owner};
use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The OCR port: `ocr::ocr_extract` in a build, a canned reader in tests.
pub(crate) type OcrFn = fn(&Path, u64) -> Result<String>;

/// JPEG quality for a downscale re-encode.
const DOWNSCALE_JPEG_QUALITY: u8 = 85;

/// The formats borg stores: what the vision API accepts, plus svg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FigureMime {
    Png,
    Jpeg,
    Gif,
    Webp,
    Svg,
}

impl FigureMime {
    /// The stored format for a response `Content-Type`; `None` for anything
    /// borg does not store (avif, bmp, tiff, ...).
    pub(crate) fn from_content_type(mime: &str) -> Option<Self> {
        match mime {
            "image/png" => Some(Self::Png),
            "image/jpeg" | "image/jpg" => Some(Self::Jpeg),
            "image/gif" => Some(Self::Gif),
            "image/webp" => Some(Self::Webp),
            "image/svg+xml" => Some(Self::Svg),
            _ => None,
        }
    }

    /// The stored file's extension, implied by the MIME and never by the URL.
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Svg => "svg",
        }
    }
}

/// A kept image: its bytes, format, built Description / Extracted Text
/// section (already capped at `max-section-bytes`), and where it came from
/// (`In @handle's post`), which the `## Images` block prints under the embed.
#[derive(Debug, Clone)]
pub(crate) struct Figure {
    pub bytes: Vec<u8>,
    pub mime: FigureMime,
    pub section: String,
    pub provenance: String,
}

/// What the figure stage produced for a note. `degraded` is true when a
/// candidate that passed the prefilter failed download, classify, or describe.
#[derive(Debug, Clone, Default)]
pub(crate) struct FigureSet {
    pub figures: Vec<Figure>,
    pub degraded: bool,
}

/// A figure with its vault path assigned and its bytes bounded, ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedFigure {
    /// Vault-relative, as it goes into the note's `images:` list.
    pub vault_path: String,
    /// The bare filename, as it goes into the `![[...]]` embed.
    pub basename: String,
    pub bytes: Vec<u8>,
}

/// The planned figures plus whether planning dropped any (oversize that could
/// not be brought under `max-bytes`, or undecodable). A drop degrades the receipt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    pub planned: Vec<PlannedFigure>,
    pub degraded: bool,
    /// The `## Images` body section for the planned figures, in source order;
    /// empty when nothing was planned.
    pub images_section: String,
}

/// Assign every figure `system/attachments/images/YYYY-MM/<trace-id>-<n>-<sha8>.<ext>`.
///
/// The trace id is unique per ingest, so two notes never share a file and one
/// note's cleanup can never remove another's image; a title is not unique, so
/// it cannot carry ownership. `n` is the figure's 1-based position in the set
/// and survives a drop, so a dropped figure leaves a gap, not a renumbering.
/// A figure over `max-bytes` is downscaled here, not in [`store`], because the
/// decision and the extension it can change belong in the path the note embeds.
pub(crate) fn plan(set: &FigureSet, trace_id: &str, now: &DateTime<Utc>, cfg: &ImagesConfig) -> Plan {
    log::debug!(
        "figures::plan: trace={trace_id} figures={} max_bytes={}",
        set.figures.len(),
        cfg.max_bytes
    );
    let subdir = month_subdir(now);
    let mut out = Plan::default();
    let mut blocks: Vec<String> = Vec::new();
    for (index, figure) in set.figures.iter().enumerate() {
        let (bytes, mime) = match bound(figure, cfg) {
            Ok(bounded) => bounded,
            Err(why) => {
                log::warn!("[{trace_id}] figure {} dropped: {why:#}", index + 1);
                out.degraded = true;
                continue;
            }
        };
        let basename = format!("{trace_id}-{}-{}.{}", index + 1, sha8(&bytes), mime.extension());
        blocks.push(image_block(&basename, &figure.provenance, &figure.section));
        out.planned.push(PlannedFigure {
            vault_path: format!("system/attachments/{subdir}/{basename}"),
            basename,
            bytes,
        });
    }
    out.images_section = images_section(&blocks);
    log::info!(
        "[{trace_id}] figures planned: kept={} dropped={}",
        out.planned.len(),
        set.figures.len() - out.planned.len()
    );
    out
}

/// One `## Images` sub-block: the embed, its provenance line, then the section.
fn image_block(basename: &str, provenance: &str, section: &str) -> String {
    let mut parts = vec![format!("![[{basename}]]")];
    if !provenance.trim().is_empty() {
        parts.push(format!("*{}.*", provenance.trim().trim_end_matches('.')));
    }
    if !section.trim().is_empty() {
        parts.push(section.trim().to_string());
    }
    parts.join("\n\n")
}

fn images_section(blocks: &[String]) -> String {
    if blocks.is_empty() {
        return String::new();
    }
    format!("## Images\n\n{}\n", blocks.join("\n\n"))
}

/// Append the `## Images` section to a rendered body, after the distilled
/// content and before the Source footer `render_note` adds. A plan with no
/// figures returns the body untouched.
pub(crate) fn with_images_section(body: &str, plan: &Plan) -> String {
    if plan.images_section.is_empty() {
        return body.to_string();
    }
    let trimmed = body.trim_end();
    if trimmed.is_empty() {
        return plan.images_section.clone();
    }
    format!("{trimmed}\n\n{}", plan.images_section)
}

/// The figure's stored bytes and MIME: untouched when within `max-bytes` (and
/// always for svg), otherwise downscaled to `max-side-px` and re-encoded.
fn bound(figure: &Figure, cfg: &ImagesConfig) -> Result<(Vec<u8>, FigureMime)> {
    if figure.mime == FigureMime::Svg || figure.bytes.len() as u64 <= cfg.max_bytes {
        return Ok((figure.bytes.clone(), figure.mime));
    }
    let decoded = image::load_from_memory(&figure.bytes).context("decode oversize figure")?;
    let resized = if decoded.width().max(decoded.height()) > cfg.max_side_px {
        decoded.resize(cfg.max_side_px, cfg.max_side_px, image::imageops::FilterType::Lanczos3)
    } else {
        decoded
    };
    let mut out = Cursor::new(Vec::new());
    let mime = if figure.mime == FigureMime::Jpeg {
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, DOWNSCALE_JPEG_QUALITY);
        resized
            .to_rgb8()
            .write_with_encoder(encoder)
            .context("re-encode figure as jpeg")?;
        FigureMime::Jpeg
    } else {
        resized
            .write_to(&mut out, image::ImageFormat::Png)
            .context("re-encode figure as png")?;
        FigureMime::Png
    };
    let bytes = out.into_inner();
    if bytes.len() as u64 > cfg.max_bytes {
        eyre::bail!(
            "still {} bytes after downscale to {}px, over max-bytes {}",
            bytes.len(),
            cfg.max_side_px,
            cfg.max_bytes
        );
    }
    Ok((bytes, mime))
}

fn sha8(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Write each planned figure under `vault_root`, never overwriting
/// (`vault::note::write_atomic_new`). Returns only the files THIS call
/// created: a path that already held a file is reused and left alone, so the
/// caller can remove exactly what it made if a later step fails. On a write
/// error the files already created here are removed before the error returns.
pub(crate) fn store(vault_root: &Path, planned: &[PlannedFigure]) -> Result<Vec<PathBuf>> {
    log::debug!(
        "figures::store: root={} figures={}",
        vault_root.display(),
        planned.len()
    );
    let mut created: Vec<PathBuf> = Vec::new();
    for figure in planned {
        let dest = vault_root.join(&figure.vault_path);
        let written = dest
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .map_err(eyre::Report::from)
            .and_then(|_| vault::note::write_atomic_new(&dest, &figure.bytes));
        match written {
            Ok(true) => created.push(dest),
            Ok(false) => log::info!("figures::store: {} already exists, reusing", dest.display()),
            Err(e) => {
                if let Err(cleanup) = crate::rkvr::remove(&created) {
                    log::warn!(
                        "figures::store: rollback of {} file(s) failed: {cleanup:#}",
                        created.len()
                    );
                }
                return Err(e).with_context(|| format!("store figure {}", dest.display()));
            }
        }
    }
    Ok(created)
}

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

/// What describing an X post's photos produced.
pub(crate) struct XPhotos {
    /// One section per described photo, for the distiller's source markdown.
    pub images: Vec<DescribedImage>,
    /// The photos to store and embed; empty when `images.enabled` is false.
    pub figures: FigureSet,
    /// Any photo degraded: download failed or over `max-download-bytes`,
    /// vision enabled but failed or empty, or nothing to say.
    pub degraded: bool,
}

/// Every photo on an X post and its quote, described in order. A degraded
/// photo with no section is omitted, never published as an empty heading.
/// With `images.enabled: false` the photos are still described (and still
/// degrade the fetch) but nothing is stored.
pub(crate) async fn describe_x_photos(post: &x::Post, config: &Config, ocr: OcrFn, trace_id: &str) -> XPhotos {
    let photos = x::photos(post);
    log::debug!("describe_x_photos[{trace_id}]: photos={}", photos.len());
    let described = futures::future::join_all(
        photos
            .iter()
            .map(|(_, photo)| describe_photo(&photo.url, config, ocr, trace_id)),
    )
    .await;
    let mut out = XPhotos {
        images: Vec::new(),
        figures: FigureSet::default(),
        degraded: false,
    };
    for ((owner, _), (photo, photo_degraded)) in photos.iter().zip(described) {
        out.degraded |= photo_degraded;
        let Some(photo) = photo else { continue };
        out.images.push(DescribedImage {
            owner: *owner,
            section: photo.section.clone(),
        });
        if !config.images.enabled {
            continue;
        }
        match photo.mime {
            Some(mime) => out.figures.figures.push(Figure {
                bytes: photo.bytes,
                mime,
                section: photo.section,
                provenance: format!("In @{}'s post", handle(post, *owner)),
            }),
            None => log::info!("[{trace_id}] X photo described but not stored: unsupported format"),
        }
    }
    out.figures.degraded = config.images.enabled && out.degraded;
    log::debug!(
        "describe_x_photos[{trace_id}]: sections={} figures={} degraded={}",
        out.images.len(),
        out.figures.figures.len(),
        out.degraded
    );
    out
}

fn handle(post: &x::Post, owner: Owner) -> &str {
    match (owner, &post.quote) {
        (Owner::Quote, Some(quote)) => &quote.author.screen_name,
        _ => &post.author.screen_name,
    }
}

/// A downloaded, described photo: its bytes, the format borg stores it as
/// (`None` for a format it does not store), and its capped section.
struct DescribedPhoto {
    bytes: Vec<u8>,
    mime: Option<FigureMime>,
    section: String,
}

/// One photo: `(photo, degraded)`. OCR runs on a blocking thread joined
/// with the vision call, as image ingest does; only vision takes a permit.
async fn describe_photo(url: &str, config: &Config, ocr: OcrFn, trace_id: &str) -> (Option<DescribedPhoto>, bool) {
    let (bytes, mime) = match download_photo(
        url,
        config.pipeline.fxtwitter_timeout_secs,
        config.images.max_download_bytes,
    )
    .await
    {
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
    let section = cap_section(&section, config.images.max_section_bytes);
    let photo = DescribedPhoto {
        bytes,
        mime: FigureMime::from_content_type(&mime),
        section,
    };
    (Some(photo), vision_failed)
}

/// `section` within `max_bytes`: over it, cut at the last line boundary that
/// fits (room kept for the marker) and end with `…`.
pub(crate) fn cap_section(section: &str, max_bytes: usize) -> String {
    const MARK: &str = "\u{2026}";
    if section.len() <= max_bytes {
        return section.to_string();
    }
    let budget = max_bytes.saturating_sub(MARK.len());
    let mut end = budget.min(section.len());
    while !section.is_char_boundary(end) {
        end -= 1;
    }
    let head = &section[..end];
    let head = head.rfind('\n').map_or(head, |i| &head[..i]);
    format!("{}{MARK}", head.trim_end())
}

/// Fetch a photo's bytes and MIME type. Non-2xx, a `Content-Type` that is
/// not `image/*`, or a body over `max_bytes` (declared or read) is an `Err`.
/// The MIME comes from the header because the URL ends `.jpg?name=orig`,
/// which `ocr::mime_from_extension` misreads.
async fn download_photo(url: &str, timeout_secs: u64, max_bytes: u64) -> Result<(Vec<u8>, String)> {
    let client = vault::http::client(Duration::from_secs(timeout_secs)).context("build photo client")?;
    let mut response = client
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
    if response.content_length().is_some_and(|len| len > max_bytes) {
        eyre::bail!(
            "X photo {url} declares {:?} bytes, over max-download-bytes {max_bytes}",
            response.content_length()
        );
    }
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.with_context(|| format!("read X photo {url}"))? {
        if bytes.len() as u64 + chunk.len() as u64 > max_bytes {
            eyre::bail!("X photo {url} is over max-download-bytes {max_bytes}");
        }
        bytes.extend_from_slice(&chunk);
    }
    log::debug!("download_photo: url={url} mime={mime} bytes={}", bytes.len());
    Ok((bytes, mime))
}

/// OCR reads a file, so the bytes go to a temp file that lives for the call.
fn ocr_photo(bytes: &[u8], ocr: OcrFn, timeout_secs: u64) -> Result<String> {
    let mut file = tempfile::NamedTempFile::new().context("create OCR temp file")?;
    file.write_all(bytes).context("write OCR temp file")?;
    ocr(file.path(), timeout_secs)
}

#[cfg(test)]
mod tests;
