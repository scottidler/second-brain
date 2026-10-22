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
use crate::slides::classify::{self, KeepOutcome};
use crate::slides::publish::month_subdir;
use crate::x::{self, DescribedImage, Owner, Place};
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

/// Every photo on an X post and its quote, described in order, plus every
/// figure of the post's and the quote's X Article through the classified
/// path ([`collect_classified`]). A degraded photo with no section is
/// omitted, never published as an empty heading. With `images.enabled:
/// false` the photos are still described (and still degrade the fetch),
/// nothing is stored, and Article figures are not fetched at all.
pub(crate) async fn describe_x_media(post: &x::Post, config: &Config, ocr: OcrFn, trace_id: &str) -> XMedia {
    let photos = x::photos(post);
    let mut owners: Vec<(Owner, &x::Post)> = vec![(Owner::Post, post)];
    if let Some(quote) = &post.quote {
        owners.push((Owner::Quote, quote));
    }
    let article_figures: Vec<(Owner, usize, x::ArticleFigure)> = if config.images.enabled {
        owners
            .iter()
            .flat_map(|(owner, p)| {
                x::article_figures(p)
                    .into_iter()
                    .enumerate()
                    .map(move |(n, f)| (*owner, n, f))
            })
            .collect()
    } else {
        Vec::new()
    };
    log::debug!(
        "describe_x_media[{trace_id}]: photos={} article_figures={}",
        photos.len(),
        article_figures.len()
    );
    let candidates: Vec<Candidate> = article_figures
        .iter()
        .map(|(owner, _, f)| Candidate {
            url: f.url.clone(),
            provenance: f
                .caption
                .clone()
                .unwrap_or_else(|| format!("In @{}'s article", handle(post, *owner))),
        })
        .collect();
    let (described, collected) = tokio::join!(
        futures::future::join_all(
            photos
                .iter()
                .map(|(_, photo)| describe_photo(&photo.url, config, ocr, trace_id)),
        ),
        collect_classified(&candidates, config, ocr, trace_id),
    );

    let mut out = XMedia {
        images: Vec::new(),
        figures: FigureSet::default(),
        degraded: collected.degraded,
    };
    // Per owner, in source order: the Article's figures, then the photos.
    let mut photo_figures: Vec<(Owner, Figure)> = Vec::new();
    for ((owner, _), (photo, photo_degraded)) in photos.iter().zip(described) {
        out.degraded |= photo_degraded;
        let Some(photo) = photo else { continue };
        out.images.push(DescribedImage {
            owner: *owner,
            place: Place::Photo,
            section: photo.section.clone(),
        });
        if !config.images.enabled {
            continue;
        }
        match photo.mime {
            Some(mime) => photo_figures.push((
                *owner,
                Figure {
                    bytes: photo.bytes,
                    mime,
                    section: photo.section,
                    provenance: format!("In @{}'s post", handle(post, *owner)),
                },
            )),
            None => log::info!("[{trace_id}] X photo described but not stored: unsupported format"),
        }
    }
    let mut kept: Vec<(Owner, Figure)> = Vec::new();
    for ((owner, n, _), figure) in article_figures.iter().zip(collected.figures) {
        let Some(figure) = figure else { continue };
        out.images.push(DescribedImage {
            owner: *owner,
            place: Place::Figure(*n),
            section: figure.section.clone(),
        });
        kept.push((*owner, figure));
    }
    for (owner, _) in &owners {
        let article = kept.iter().filter(|(o, _)| o == owner);
        let photos = photo_figures.iter().filter(|(o, _)| o == owner);
        out.figures
            .figures
            .extend(article.chain(photos).map(|(_, f)| f.clone()));
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

/// An image to run through the classified path: where it is, and the line
/// the `## Images` block prints under its embed.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub url: String,
    pub provenance: String,
}

/// What [`collect_classified`] kept: one slot per candidate, positionally
/// aligned (`None` = dropped), and whether any drop degrades the receipt.
#[derive(Debug, Default)]
pub(crate) struct Collected {
    pub figures: Vec<Option<Figure>>,
    pub degraded: bool,
}

/// Why a candidate did not become a figure, counted for the per-trace log line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Drops {
    /// A format borg does not classify or store. Expected; not degraded.
    pub prefilter: usize,
    /// The classifier's category is not in `keep`, or under `min-confidence`.
    /// Expected; not degraded.
    pub not_diagram: usize,
    /// Past `max-candidates` or `max-per-note`. Intended truncation; not degraded.
    pub cap: usize,
    /// Download, classify, or describe failed. Degrades the receipt.
    pub error: usize,
}

/// The classified figure path, for any source whose images are not all
/// author-chosen (X Article figures; web articles reuse it): download the
/// first `max-candidates`, classify each with `FIGURE_TAXONOMY_PROMPT` (keep
/// if the category is in `images.content-filter.keep` at `min-confidence` or
/// above), cap the survivors at `max-per-note` in source order, and only then
/// describe, so a note costs at most `max-candidates` classify plus
/// `max-per-note` describe vision calls. A download, classify, or describe
/// failure drops that image and degrades; a "not a diagram" verdict, a format
/// borg does not store, and the cap do not. With `content-filter.enabled:
/// false` every downloaded image is kept (up to the cap) unclassified.
pub(crate) async fn collect_classified(
    candidates: &[Candidate],
    config: &Config,
    ocr: OcrFn,
    trace_id: &str,
) -> Collected {
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
        "collect_classified[{trace_id}]: candidates={} considered={considered} classify={}",
        candidates.len(),
        filter.enabled
    );

    let downloads = futures::future::join_all(
        candidates[..considered]
            .iter()
            .map(|c| download_photo(&c.url, config.pipeline.fxtwitter_timeout_secs, cfg.max_download_bytes)),
    )
    .await;
    let mut fetched: Vec<(usize, Vec<u8>, String)> = Vec::new();
    for (index, download) in downloads.into_iter().enumerate() {
        match download {
            Ok((bytes, mime)) => match FigureMime::from_content_type(&mime) {
                Some(FigureMime::Svg) | None => {
                    log::info!(
                        "[{trace_id}] figure {} dropped: unsupported-format {mime}",
                        candidates[index].url
                    );
                    drops.prefilter += 1;
                }
                Some(_) => fetched.push((index, bytes, mime)),
            },
            Err(e) => {
                log::warn!("[{trace_id}] figure dropped: {e:#}");
                drops.error += 1;
            }
        }
    }

    let verdicts: Vec<Option<KeepOutcome>> = if filter.enabled {
        let classified = futures::future::join_all(
            fetched
                .iter()
                .map(|(_, bytes, mime)| classify::classify_figure(bytes, mime, filter, &config.llm)),
        )
        .await;
        classified
            .iter()
            .zip(&fetched)
            .map(|(result, (index, _, _))| {
                let outcome = classify::keep_outcome_for(result, &filter.keep, filter.min_confidence);
                match result {
                    Ok(class) => log::debug!(
                        "[{trace_id}] figure {} classified {:?} {:.2}: {outcome:?}",
                        candidates[*index].url,
                        class.category,
                        class.confidence
                    ),
                    Err(e) => log::warn!("[{trace_id}] figure {} classify failed: {e}", candidates[*index].url),
                }
                Some(outcome)
            })
            .collect()
    } else {
        vec![None; fetched.len()]
    };

    let mut survivors: Vec<(usize, Vec<u8>, String)> = Vec::new();
    for ((index, bytes, mime), verdict) in fetched.into_iter().zip(verdicts) {
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
        survivors.push((index, bytes, mime));
    }

    let described = futures::future::join_all(survivors.into_iter().map(|(index, bytes, mime)| async move {
        let url = &candidates[index].url;
        (index, describe_bytes(bytes, mime, url, config, ocr, trace_id).await)
    }))
    .await;
    for (index, (image, image_degraded)) in described {
        out.degraded |= image_degraded;
        // Classified and kept, so the MIME is one borg stores.
        let Some((image, mime)) = image.and_then(|i| i.mime.map(|m| (i, m))) else {
            drops.error += 1;
            continue;
        };
        out.figures[index] = Some(Figure {
            bytes: image.bytes,
            mime,
            section: image.section,
            provenance: candidates[index].provenance.clone(),
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

fn handle(post: &x::Post, owner: Owner) -> &str {
    match (owner, &post.quote) {
        (Owner::Quote, Some(quote)) => &quote.author.screen_name,
        _ => &post.author.screen_name,
    }
}

/// A downloaded, described image: its bytes, the format borg stores it as
/// (`None` for a format it does not store), and its capped section.
struct DescribedPhoto {
    bytes: Vec<u8>,
    mime: Option<FigureMime>,
    section: String,
}

/// One photo: `(photo, degraded)`. Download, then [`describe_bytes`].
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
    describe_bytes(bytes, mime, url, config, ocr, trace_id).await
}

/// Describe downloaded image bytes: `(image, degraded)`. OCR runs on a
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
) -> (Option<DescribedPhoto>, bool) {
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

/// Fetch an image's bytes and MIME type. Non-2xx, a `Content-Type` that is
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
