//! Figures: the images borg stores from a note's source and embeds in it
//! (`docs/design/2026-10-22-capture-all-diagrams.md`).
//!
//! This module owns the vault side of the pipeline: [`plan`] assigns every kept
//! figure its path and bounds its bytes (pure, no I/O); [`store`] is the only
//! writer. Download, filter, classify, and describe arrive in later phases and
//! fill a [`FigureSet`].

use crate::config::ImagesConfig;
use crate::slides::publish::month_subdir;
use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// JPEG quality for a downscale re-encode.
const DOWNSCALE_JPEG_QUALITY: u8 = 85;

/// The formats borg stores: what the vision API accepts, plus svg.
#[expect(
    dead_code,
    reason = "Gif and Webp are constructed by the download path in the next phase"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FigureMime {
    Png,
    Jpeg,
    Gif,
    Webp,
    Svg,
}

impl FigureMime {
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

/// A kept image: its bytes, format, and built Description / Extracted Text section.
#[derive(Debug, Clone)]
pub(crate) struct Figure {
    pub bytes: Vec<u8>,
    pub mime: FigureMime,
    #[expect(dead_code, reason = "rendered into `## Images` by the next phase")]
    pub section: String,
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
        out.planned.push(PlannedFigure {
            vault_path: format!("system/attachments/{subdir}/{basename}"),
            basename,
            bytes,
        });
    }
    log::info!(
        "[{trace_id}] figures planned: kept={} dropped={}",
        out.planned.len(),
        set.figures.len() - out.planned.len()
    );
    out
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

#[cfg(test)]
mod tests;
