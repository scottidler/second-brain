//! SVG -> PNG for the vision API, which takes png/jpeg/gif/webp only. The
//! stored file stays the original `.svg`; this raster exists to be classified
//! and described.
//!
//! The bytes are untrusted web input. `usvg`'s default `<image href>`
//! resolver reads local files, which would pull a file off this machine into
//! the raster sent to a vision model, so [`locked_options`] resolves every
//! href (path and data URI alike) to nothing and sets no `resources_dir`.
//! Font loading is a separate, read-only concern and does not reopen that.

use eyre::{Context, Result};
use resvg::{tiny_skia, usvg};
use std::sync::{Arc, OnceLock};

/// Raster's longer side is raised to this when the SVG's intrinsic size is
/// smaller (capped by `max-side-px`): a 100px-wide diagram is unreadable to a
/// vision model. A viewBox-only icon is not made useful by this; the
/// classifier is what separates icons from diagrams.
const LEGIBLE_SIDE_PX: u32 = 1024;

/// Fonts to prefer for text whose named webfont is absent, in order. The first
/// installed one backs every generic family; otherwise the first face found.
const FALLBACK_FAMILIES: &[&str] = &["DejaVu Sans", "Noto Sans", "Liberation Sans", "Arial", "Helvetica"];

/// A rasterized SVG and the SVG's own intrinsic size (what the prefilter's
/// `min-side-px` reads; the raster itself is rescaled).
#[derive(Debug)]
pub(crate) struct Raster {
    pub png: Vec<u8>,
    pub intrinsic: (u32, u32),
}

/// System fonts, loaded once: scanning them is slow and the set does not
/// change under a running daemon. The generic families are pointed at an
/// installed face, since `fontdb`'s defaults ("Times New Roman", "Arial") are
/// absent on a Linux host, which left every label undrawn.
fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            match fallback_family(&db) {
                Some(family) => {
                    db.set_serif_family(family.clone());
                    db.set_sans_serif_family(family.clone());
                    db.set_monospace_family(family.clone());
                    db.set_cursive_family(family.clone());
                    db.set_fantasy_family(family);
                }
                None => log::warn!("svg: no system fonts found; SVG text will not be drawn"),
            }
            Arc::new(db)
        })
        .clone()
}

fn fallback_family(db: &usvg::fontdb::Database) -> Option<String> {
    let has = |name: &str| db.faces().any(|f| f.families.iter().any(|(n, _)| n == name));
    FALLBACK_FAMILIES
        .iter()
        .find(|name| has(name))
        .map(|name| (*name).to_string())
        .or_else(|| {
            db.faces()
                .next()
                .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
        })
}

/// The parse options for untrusted SVG: no resource directory, every
/// `<image href>` resolves to nothing, system fonts loaded.
pub(crate) fn locked_options() -> usvg::Options<'static> {
    let mut options = usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        fontdb: fonts(),
        ..usvg::Options::default()
    };
    if let Some(family) = fallback_family(&options.fontdb) {
        options.font_family = family;
    }
    options
}

/// Rasterize `svg` onto a white background with the locked-down options, the
/// longer side at most `max_side_px`.
pub(crate) fn rasterize(svg: &[u8], max_side_px: u32) -> Result<Raster> {
    rasterize_with(svg, max_side_px, &locked_options())
}

/// [`rasterize`] with caller-supplied options. Production calls only go
/// through [`rasterize`]; the lockdown test uses this to show what the
/// default resolver would have read.
pub(crate) fn rasterize_with(svg: &[u8], max_side_px: u32, options: &usvg::Options) -> Result<Raster> {
    let tree = usvg::Tree::from_data(svg, options).context("parse svg")?;
    let size = tree.size();
    let (intrinsic_w, intrinsic_h) = (size.width(), size.height());
    let longer = intrinsic_w.max(intrinsic_h);
    let target = longer.clamp(LEGIBLE_SIDE_PX.min(max_side_px) as f32, max_side_px as f32);
    let scale = target / longer;
    let width = ((intrinsic_w * scale).ceil() as u32).clamp(1, max_side_px);
    let height = ((intrinsic_h * scale).ceil() as u32).clamp(1, max_side_px);
    let mut pixmap =
        tiny_skia::Pixmap::new(width, height).ok_or_else(|| eyre::eyre!("svg raster {width}x{height} is empty"))?;
    pixmap.fill(tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(width as f32 / intrinsic_w, height as f32 / intrinsic_h),
        &mut pixmap.as_mut(),
    );
    let png = pixmap.encode_png().context("encode svg raster")?;
    log::debug!(
        "svg::rasterize: intrinsic={intrinsic_w}x{intrinsic_h} raster={width}x{height} bytes={}",
        png.len()
    );
    Ok(Raster {
        png,
        intrinsic: (intrinsic_w.round().max(1.0) as u32, intrinsic_h.round().max(1.0) as u32),
    })
}

#[cfg(test)]
mod tests;
