//! Figures: the images borg stores from a note's source and embeds in it
//! (`docs/design/2026-10-22-capture-all-diagrams.md`).
//!
//! This module owns both sides of the pipeline. The source side: the one
//! section builder ([`image_section`], shared with image ingest) and the one
//! download -> prefilter -> classify -> cap -> describe path ([`collect`]) that
//! an X post ([`describe_x_media`]) and a web article
//! ([`describe_article_media`]) both run, each filling a [`FigureSet`].
//! The vault side: [`plan`] assigns every kept figure its path and bounds its
//! bytes (pure, no I/O) and builds the `## Images` body; [`store`] is the only
//! writer.
//!
//! Both paths call the `ocr` functions directly and never re-enter
//! `process_content`: the URL trace already holds a GENERAL permit, so a
//! nested `process_image` would wait on the pool it is holding.

use crate::config::{Config, ImagesConfig};
use crate::ocr::{self, VisionResult};
use crate::slides::classify::{self, KeepOutcome};
use crate::slides::publish::month_subdir;
use crate::x::{self, DescribedImage, Owner, Place};
use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use prefilter::{Dedupe, Source, Verdict};
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

    /// The `Content-Type` this format is sent to the vision API as.
    pub(crate) fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Svg => "image/svg+xml",
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
    let (bytes, mime) = downscale(&figure.bytes, figure.mime, cfg.max_side_px)?;
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

/// Decode `bytes`, shrink the longer side to `max_side_px` when it is over,
/// and re-encode: jpeg -> jpeg q85, everything else -> png (the `image` webp
/// encoder is lossless-only; a gif keeps its first frame).
fn downscale(bytes: &[u8], mime: FigureMime, max_side_px: u32) -> Result<(Vec<u8>, FigureMime)> {
    let decoded = image::load_from_memory(bytes).context("decode figure for downscale")?;
    let resized = fit(decoded, max_side_px);
    if mime == FigureMime::Jpeg {
        Ok((encode_jpeg(&resized)?, FigureMime::Jpeg))
    } else {
        Ok((encode_png(&resized)?, FigureMime::Png))
    }
}

fn fit(decoded: image::DynamicImage, max_side_px: u32) -> image::DynamicImage {
    if decoded.width().max(decoded.height()) > max_side_px {
        decoded.resize(max_side_px, max_side_px, image::imageops::FilterType::Lanczos3)
    } else {
        decoded
    }
}

fn encode_jpeg(img: &image::DynamicImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, DOWNSCALE_JPEG_QUALITY);
    img.to_rgb8()
        .write_with_encoder(encoder)
        .context("re-encode figure as jpeg")?;
    Ok(out.into_inner())
}

fn encode_png(img: &image::DynamicImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .context("re-encode figure as png")?;
    Ok(out.into_inner())
}

/// The vision API refuses an image over 5 MiB. The bound is set so the
/// base64 payload the request carries (4 bytes per 3) is also under 5 MiB,
/// which holds whichever of the two the API measures.
pub(crate) const VISION_MAX_BYTES: usize = 5 * 1024 * 1024 / 4 * 3;

/// The one image classify and describe both see, bounded before either call:
/// `None` when `bytes` already has its longer side within `max_side_px` and
/// is at most `max_bytes` (sent as is); otherwise the image downscaled to
/// `max_side_px` and re-encoded as [`downscale`] does. A png still over
/// `max_bytes` goes as a jpeg (vision reads the pixels, not the format), and
/// a jpeg still over it is shrunk again in proportion to the overshoot, at
/// most [`VISION_SHRINK_ATTEMPTS`] times. Undecodable over the bound, or
/// still over it after that, is an `Err`: the figure drops and degrades. The
/// stored bytes are not this; they follow [`bound`].
pub(crate) fn vision_image(
    bytes: &[u8],
    mime: FigureMime,
    max_side_px: u32,
    max_bytes: usize,
) -> Result<Option<(Vec<u8>, FigureMime)>> {
    let within_side = sniff_dims(bytes).is_none_or(|(w, h)| w.max(h) <= max_side_px);
    if within_side && bytes.len() <= max_bytes {
        return Ok(None);
    }
    let decoded = image::load_from_memory(bytes).context("decode figure for vision")?;
    let mut resized = fit(decoded, max_side_px);
    let (mut out, mut out_mime) = if mime == FigureMime::Jpeg {
        (encode_jpeg(&resized)?, FigureMime::Jpeg)
    } else {
        (encode_png(&resized)?, FigureMime::Png)
    };
    if out.len() > max_bytes && out_mime == FigureMime::Png {
        out = encode_jpeg(&resized)?;
        out_mime = FigureMime::Jpeg;
    }
    for _ in 0..VISION_SHRINK_ATTEMPTS {
        if out.len() <= max_bytes {
            break;
        }
        // Encoded size tracks pixel count, so the side scales by the root.
        let ratio = (max_bytes as f64 / out.len() as f64).sqrt() * 0.9;
        let side = ((resized.width().max(resized.height()) as f64) * ratio)
            .floor()
            .max(1.0) as u32;
        resized = fit(resized, side);
        out = encode_jpeg(&resized)?;
    }
    if out.len() > max_bytes {
        eyre::bail!(
            "still {} bytes for vision after downscale to {}px, over {max_bytes}",
            out.len(),
            resized.width().max(resized.height())
        );
    }
    log::debug!(
        "vision_image: {} -> {} bytes, {}x{} as {}",
        bytes.len(),
        out.len(),
        resized.width(),
        resized.height(),
        out_mime.content_type()
    );
    Ok(Some((out, out_mime)))
}

/// How many times [`vision_image`] shrinks a jpeg that is still over the bound.
const VISION_SHRINK_ATTEMPTS: usize = 3;

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

/// What describing an X post's photos and Article figures produced.
pub(crate) struct XMedia {
    /// One section per described photo and kept Article figure, for the
    /// distiller's source markdown (`x::render` places each).
    pub images: Vec<DescribedImage>,
    /// The images to store and embed, in source order (per post: its
    /// Article figures, then its photos; the post before its quote). Empty
    /// when `images.enabled` is false.
    pub figures: FigureSet,
    /// Any photo or Article figure degraded: download failed or over
    /// `max-download-bytes`, classify failed, vision enabled but failed or
    /// empty, or nothing to say.
    pub degraded: bool,
}

/// Every photo on an X post and its quote plus every figure of the post's and
/// the quote's X Article, through the one [`collect`] path: photos skip the
/// classifier (author-chosen), Article figures are classified. A degraded
/// photo with no section is omitted, never published as an empty heading. With
/// `images.enabled: false` the photos are still described (and still degrade
/// the fetch), nothing is stored, and Article figures are not fetched at all.
pub(crate) async fn describe_x_media(post: &x::Post, config: &Config, ocr: OcrFn, trace_id: &str) -> XMedia {
    let photos = x::photos(post);
    let mut owners: Vec<(Owner, &x::Post)> = vec![(Owner::Post, post)];
    if let Some(quote) = &post.quote {
        owners.push((Owner::Quote, quote));
    }
    // Per owner, in source order: the Article's figures, then the photos; the
    // post before its quote. This is also the order `collect` caps in and the
    // order the FigureSet is stored in.
    let mut placed: Vec<(Owner, Place)> = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for (owner, p) in &owners {
        if config.images.enabled {
            for (n, figure) in x::article_figures(p).into_iter().enumerate() {
                placed.push((*owner, Place::Figure(n)));
                candidates.push(Candidate {
                    url: figure.url,
                    provenance: figure
                        .caption
                        .unwrap_or_else(|| format!("In @{}'s article", handle(post, *owner))),
                    source: Source::XArticle,
                });
            }
        }
        for (photo_owner, photo) in &photos {
            if photo_owner == owner {
                placed.push((*owner, Place::Photo));
                candidates.push(Candidate {
                    url: photo.url.clone(),
                    provenance: format!("In @{}'s post", handle(post, *owner)),
                    source: Source::XPhoto,
                });
            }
        }
    }
    log::debug!(
        "describe_x_media[{trace_id}]: photos={} candidates={}",
        photos.len(),
        candidates.len()
    );
    let collected = collect(&candidates, config, ocr, trace_id).await;

    let mut out = XMedia {
        images: Vec::new(),
        figures: FigureSet::default(),
        degraded: collected.degraded,
    };
    for ((owner, place), figure) in placed.into_iter().zip(collected.figures) {
        let Some(figure) = figure else { continue };
        out.images.push(DescribedImage {
            owner,
            place,
            section: figure.section.clone(),
        });
        if config.images.enabled {
            out.figures.figures.push(figure);
        }
    }
    out.figures.degraded = config.images.enabled && out.degraded;
    log::debug!(
        "describe_x_media[{trace_id}]: sections={} figures={} degraded={}",
        out.images.len(),
        out.figures.figures.len(),
        out.degraded
    );
    out
}

/// What the figure stage produced for a web article: the source markdown with
/// each kept figure's block spliced in at its image, for the distiller, and
/// the figures to store and embed.
pub(crate) struct ArticleMedia {
    pub markdown: String,
    pub figures: FigureSet,
}

/// Every `![alt](url)` in a fetched article (readable, fabric, or jina
/// markdown) through the one [`collect`] path as a web-article candidate:
/// content heuristics, classify, cap, describe. Each kept figure's block
/// replaces its image in the markdown, so the distiller reads the diagram
/// where the author put it; the provenance printed under its embed is the alt
/// text. With `images.enabled: false` the markdown is returned untouched and
/// nothing is fetched.
pub(crate) async fn describe_article_media(
    markdown: &str,
    page_url: &str,
    config: &Config,
    ocr: OcrFn,
    trace_id: &str,
) -> ArticleMedia {
    if !config.images.enabled {
        log::debug!("describe_article_media[{trace_id}]: images disabled");
        return ArticleMedia {
            markdown: markdown.to_string(),
            figures: FigureSet::default(),
        };
    }
    let images = candidates::parse_markdown_images(markdown, page_url);
    let candidates: Vec<Candidate> = images
        .iter()
        .map(|image| Candidate {
            url: image.url.clone(),
            provenance: image.alt.trim().to_string(),
            source: Source::WebArticle,
        })
        .collect();
    log::debug!(
        "describe_article_media[{trace_id}]: page={page_url} candidates={}",
        candidates.len()
    );
    let collected = collect(&candidates, config, ocr, trace_id).await;

    let mut figures = FigureSet {
        figures: Vec::new(),
        degraded: collected.degraded,
    };
    let mut placed: Vec<(&candidates::MarkdownImage, String)> = Vec::new();
    for (image, figure) in images.iter().zip(collected.figures) {
        let Some(figure) = figure else { continue };
        placed.push((
            image,
            splice::figure_block(placed.len() + 1, &image.alt, &figure.section),
        ));
        figures.figures.push(figure);
    }
    let markdown = splice::splice(markdown, &placed);
    log::debug!(
        "describe_article_media[{trace_id}]: kept={} degraded={}",
        figures.figures.len(),
        figures.degraded
    );
    ArticleMedia { markdown, figures }
}

/// An image to run through [`collect`]: where it is, where it came from
/// (which decides the checks and whether the classifier runs), and the line
/// the `## Images` block prints under its embed.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub url: String,
    pub provenance: String,
    pub source: Source,
}

/// What [`collect`] kept: one slot per candidate, positionally aligned
/// (`None` = dropped), and whether any drop degrades the receipt.
#[derive(Debug, Default)]
pub(crate) struct Collected {
    pub figures: Vec<Option<Figure>>,
    pub degraded: bool,
}

/// Why a candidate did not become a figure, counted for the per-trace log line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Drops {
    /// The prefilter said no: a format borg does not store, a decoration URL
    /// or size, an exact duplicate, an SVG that would not render. Expected;
    /// not degraded.
    pub prefilter: usize,
    /// The classifier's category is not in `keep`, or under `min-confidence`.
    /// Expected; not degraded.
    pub not_diagram: usize,
    /// Past `max-candidates` or `max-per-note`. Intended truncation; not degraded.
    pub cap: usize,
    /// Download, classify, or describe failed. Degrades the receipt.
    pub error: usize,
}

/// A downloaded image that passed the prefilter. `bytes` is the original that
/// is stored; `vision` is the bounded image classify and describe are sent
/// ([`vision_image`]: an SVG's raster, or a downscale of an image over the
/// vision bound), `None` when the original is sent as is.
struct Fetched {
    index: usize,
    bytes: Vec<u8>,
    mime: FigureMime,
    vision: Option<(Vec<u8>, FigureMime)>,
}

impl Fetched {
    fn vision_bytes(&self) -> &[u8] {
        self.vision.as_ref().map_or(&self.bytes, |(bytes, _)| bytes)
    }

    fn vision_mime(&self) -> &'static str {
        self.vision.as_ref().map_or(self.mime, |(_, mime)| *mime).content_type()
    }
}

/// The one figure path for every source: download the first `max-candidates`,
/// run the [`prefilter`] (format checks for all, content heuristics for web
/// articles only, sha256 dedupe), rasterize SVGs, classify the sources that are
/// not author-chosen with `FIGURE_TAXONOMY_PROMPT` (keep if the category is in
/// `images.content-filter.keep` at `min-confidence` or above; X post photos
/// skip this), cap the survivors at `max-per-note` in source order, and only
/// then describe, so a note costs at most `max-candidates` classify plus
/// `max-per-note` describe vision calls. A download, classify, or describe
/// failure drops that image and degrades; a prefilter drop, a "not a diagram"
/// verdict, and the cap do not. Writes the one `figures: kept=N dropped={...}`
/// line per call. With `content-filter.enabled: false` nothing is classified.
pub(crate) async fn collect(candidates: &[Candidate], config: &Config, ocr: OcrFn, trace_id: &str) -> Collected {
    let cfg = &config.images;
    let filter = &cfg.content_filter;
    let mut drops = Drops::default();
    let mut out = Collected {
        figures: vec![None; candidates.len()],
        degraded: false,
    };
    if candidates.is_empty() {
        return out;
    }
    let considered = candidates.len().min(cfg.max_candidates);
    drops.cap += candidates.len() - considered;
    log::debug!(
        "collect[{trace_id}]: candidates={} considered={considered} classify={}",
        candidates.len(),
        filter.enabled
    );

    // Candidates the URL alone rules out are never downloaded.
    let mut to_download: Vec<usize> = Vec::new();
    for (index, candidate) in candidates[..considered].iter().enumerate() {
        match prefilter::prefilter_url(candidate.source, &candidate.url) {
            Verdict::Keep => to_download.push(index),
            Verdict::Drop(reason) => {
                log::info!("[{trace_id}] figure {} dropped: {}", candidate.url, reason.as_str());
                drops.prefilter += 1;
            }
        }
    }
    let downloads = futures::future::join_all(to_download.iter().map(|&i| {
        download_image(
            &candidates[i].url,
            config.pipeline.fxtwitter_timeout_secs,
            cfg.max_download_bytes,
        )
    }))
    .await;

    let mut dedupe = Dedupe::default();
    let mut fetched: Vec<Fetched> = Vec::new();
    for (&index, download) in to_download.iter().zip(downloads) {
        let candidate = &candidates[index];
        let (bytes, content_type) = match download {
            Ok(downloaded) => downloaded,
            Err(e) => {
                log::warn!("[{trace_id}] figure dropped: {e:#}");
                drops.error += 1;
                continue;
            }
        };
        let mime = FigureMime::from_content_type(&content_type);
        let (raster, dims) = match mime {
            Some(FigureMime::Svg) => match rasterize_svg(&bytes, cfg.max_side_px).await {
                Ok(raster) => (Some(raster.png), Some(raster.intrinsic)),
                Err(e) => {
                    log::info!(
                        "[{trace_id}] figure {} dropped: svg did not render: {e:#}",
                        candidate.url
                    );
                    drops.prefilter += 1;
                    continue;
                }
            },
            _ => (None, sniff_dims(&bytes)),
        };
        if let Verdict::Drop(reason) =
            prefilter::prefilter(cfg, candidate.source, &candidate.url, &content_type, dims, &bytes)
        {
            log::info!(
                "[{trace_id}] figure {} dropped: {} {content_type}",
                candidate.url,
                reason.as_str()
            );
            drops.prefilter += 1;
            continue;
        }
        if !dedupe.first_seen(&bytes) {
            log::info!("[{trace_id}] figure {} dropped: duplicate", candidate.url);
            drops.prefilter += 1;
            continue;
        }
        // The prefilter has already refused a MIME borg does not store.
        let Some(mime) = mime else { continue };
        let vision = match bound_for_vision(&bytes, mime, raster, cfg.max_side_px).await {
            Ok(vision) => vision,
            Err(e) => {
                log::warn!("[{trace_id}] figure {} dropped: {e:#}", candidate.url);
                drops.error += 1;
                continue;
            }
        };
        fetched.push(Fetched {
            index,
            bytes,
            mime,
            vision,
        });
    }

    let verdicts: Vec<Option<KeepOutcome>> = futures::future::join_all(fetched.iter().map(|f| async move {
        if !filter.enabled || !candidates[f.index].source.is_classified() {
            return None;
        }
        let result = classify::classify_figure(f.vision_bytes(), f.vision_mime(), filter, &config.llm).await;
        let outcome = classify::keep_outcome_for(&result, &filter.keep, filter.min_confidence);
        match &result {
            Ok(class) => log::debug!(
                "[{trace_id}] figure {} classified {:?} {:.2}: {outcome:?}",
                candidates[f.index].url,
                class.category,
                class.confidence
            ),
            Err(e) => log::warn!("[{trace_id}] figure {} classify failed: {e}", candidates[f.index].url),
        }
        Some(outcome)
    }))
    .await;

    let mut survivors: Vec<Fetched> = Vec::new();
    for (image, verdict) in fetched.into_iter().zip(verdicts) {
        match verdict {
            None | Some(KeepOutcome::Keep) => {}
            Some(KeepOutcome::DroppedApiError | KeepOutcome::DroppedParseError) => {
                drops.error += 1;
                continue;
            }
            Some(KeepOutcome::DroppedNotInKeep | KeepOutcome::DroppedLowConfidence) => {
                drops.not_diagram += 1;
                continue;
            }
        }
        if survivors.len() == cfg.max_per_note {
            drops.cap += 1;
            continue;
        }
        survivors.push(image);
    }

    let described = futures::future::join_all(survivors.into_iter().map(|image| async move {
        let described = describe_bytes(
            image.vision_bytes().to_vec(),
            image.vision_mime().to_string(),
            &candidates[image.index].url,
            config,
            ocr,
            trace_id,
        )
        .await;
        (image, described)
    }))
    .await;
    for (image, (section, image_degraded)) in described {
        out.degraded |= image_degraded;
        let Some(section) = section else {
            drops.error += 1;
            continue;
        };
        out.figures[image.index] = Some(Figure {
            bytes: image.bytes,
            mime: image.mime,
            section,
            provenance: candidates[image.index].provenance.clone(),
        });
    }
    out.degraded |= drops.error > 0;
    let kept = out.figures.iter().filter(|f| f.is_some()).count();
    log::info!(
        "[{trace_id}] figures: kept={kept} dropped={{prefilter:{}, not-diagram:{}, cap:{}, error:{}}}",
        drops.prefilter,
        drops.not_diagram,
        drops.cap,
        drops.error
    );
    out
}

/// A raster image's `(width, height)` from its header alone; `None` when the
/// bytes are not an image the `image` crate can read.
fn sniff_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// [`vision_image`] for a fetched figure (an SVG's raster stands in for the
/// SVG, and goes as is when within the bound). Decode and resize are
/// CPU-bound; keep them off the async workers.
async fn bound_for_vision(
    bytes: &[u8],
    mime: FigureMime,
    raster: Option<Vec<u8>>,
    max_side_px: u32,
) -> Result<Option<(Vec<u8>, FigureMime)>> {
    let (source, source_mime) = match raster {
        Some(png) => (png, FigureMime::Png),
        None => (bytes.to_vec(), mime),
    };
    let is_raster = mime == FigureMime::Svg;
    tokio::task::spawn_blocking(move || {
        let bounded = vision_image(&source, source_mime, max_side_px, VISION_MAX_BYTES)?;
        Ok(match bounded {
            Some(bounded) => Some(bounded),
            None if is_raster => Some((source, FigureMime::Png)),
            None => None,
        })
    })
    .await
    .context("vision bound task")?
}

/// SVG rasterization is CPU-bound; keep it off the async workers.
async fn rasterize_svg(bytes: &[u8], max_side_px: u32) -> Result<svg::Raster> {
    let owned = bytes.to_vec();
    tokio::task::spawn_blocking(move || svg::rasterize(&owned, max_side_px))
        .await
        .context("svg rasterize task")?
}

fn handle(post: &x::Post, owner: Owner) -> &str {
    match (owner, &post.quote) {
        (Owner::Quote, Some(quote)) => &quote.author.screen_name,
        _ => &post.author.screen_name,
    }
}

/// Describe downloaded image bytes: `(section, degraded)`. OCR runs on a
/// blocking thread joined with the vision call, as image ingest does; only
/// vision takes a permit. Vision failing leaves an OCR-only section and
/// degrades; neither producing text omits the image and degrades.
async fn describe_bytes(
    bytes: Vec<u8>,
    mime: String,
    url: &str,
    config: &Config,
    ocr: OcrFn,
    trace_id: &str,
) -> (Option<String>, bool) {
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
                log::warn!("[{trace_id}] vision returned no description for image {url}");
                (None, true)
            }
            Err(e) => {
                log::warn!("[{trace_id}] vision failed for image {url}: {e:#}");
                (None, true)
            }
        }
    };
    let (ocr_result, (vision, vision_failed)) = tokio::join!(ocr_handle, vision_future);
    let ocr_text = match ocr_result {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => {
            log::warn!("[{trace_id}] OCR failed for image {url}: {e:#}");
            String::new()
        }
        Err(e) => {
            log::warn!("[{trace_id}] OCR task failed for image {url}: {e}");
            String::new()
        }
    };
    let section = image_section(vision.as_ref(), &ocr_text, "####");
    if section.is_empty() {
        log::warn!("[{trace_id}] image omitted: neither vision nor OCR produced text for {url}");
        return (None, true);
    }
    (
        Some(cap_section(&section, config.images.max_section_bytes)),
        vision_failed,
    )
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

/// Fetch an image's bytes and MIME type, as a browser would ask for it (the
/// design doc's fetch measurement used this UA; some image CDNs refuse a bare
/// client). Non-2xx, a `Content-Type` that is not `image/*`, or a body over
/// `max_bytes` (declared or read) is an `Err`. The MIME comes from the header
/// because an X URL ends `.jpg?name=orig`, which `ocr::mime_from_extension`
/// misreads, and a web URL often carries no extension at all.
async fn download_image(url: &str, timeout_secs: u64, max_bytes: u64) -> Result<(Vec<u8>, String)> {
    let client = vault::http::builder(vault::http::Timeouts::total(Duration::from_secs(timeout_secs)))
        .user_agent(crate::readability::BROWSER_UA)
        .build()
        .context("build image client")?;
    let mut response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("download image {url}"))?;
    let status = response.status();
    if !status.is_success() {
        eyre::bail!("image {url} returned status {status}");
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if !mime.starts_with("image/") {
        eyre::bail!("image {url} has content-type {mime:?}, not an image");
    }
    if response.content_length().is_some_and(|len| len > max_bytes) {
        eyre::bail!(
            "image {url} declares {:?} bytes, over max-download-bytes {max_bytes}",
            response.content_length()
        );
    }
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.with_context(|| format!("read image {url}"))? {
        if bytes.len() as u64 + chunk.len() as u64 > max_bytes {
            eyre::bail!("image {url} is over max-download-bytes {max_bytes}");
        }
        bytes.extend_from_slice(&chunk);
    }
    log::debug!("download_image: url={url} mime={mime} bytes={}", bytes.len());
    Ok((bytes, mime))
}

/// OCR reads a file, so the bytes go to a temp file that lives for the call.
fn ocr_photo(bytes: &[u8], ocr: OcrFn, timeout_secs: u64) -> Result<String> {
    let mut file = tempfile::NamedTempFile::new().context("create OCR temp file")?;
    file.write_all(bytes).context("write OCR temp file")?;
    ocr(file.path(), timeout_secs)
}

mod candidates;
mod prefilter;
mod splice;
mod svg;

#[cfg(test)]
mod tests;
