//! The web-article stage between fetch and publish: figures, then distill
//! (`docs/design/2026-10-22-capture-all-diagrams.md`). `process_url_inner`
//! calls [`distill_article`] for every URL the article distiller handles,
//! whichever fetcher (readable, fabric, jina) produced the markdown.

use super::figures::{self, FigureSet, OcrFn};
use crate::config::Config;
use crate::stages::distill::{ArticleInput, distill_for_publish_article};
use vault::distilled::Distilled;

/// What the article stage produced: the distilled article and the figures to
/// store and embed (`figures.degraded` is the image stage's degraded flag).
pub(crate) struct ArticleStage {
    pub distilled: Distilled,
    pub figures: FigureSet,
}

/// Describe the article's figures, splice each kept one into the markdown at
/// its image, and distill the spliced markdown. The transcript quality gate
/// judges `article_md` as fetched, before the splice; the transcript kept is
/// the spliced one. `clean_source` is true only for readable output.
pub(crate) async fn distill_article(
    url: &str,
    article_md: &str,
    clean_source: bool,
    capture_note: Option<&str>,
    config: &Config,
    ocr: OcrFn,
    trace_id: &str,
) -> ArticleStage {
    log::debug!(
        "distill_article[{trace_id}]: url={url} clean_source={clean_source} bytes={}",
        article_md.len()
    );
    let media = figures::describe_article_media(article_md, url, config, ocr, trace_id).await;
    let distilled = distill_for_publish_article(
        &config.fabric,
        &config.staging,
        trace_id,
        url,
        ArticleInput {
            markdown: &media.markdown,
            quality_source: article_md,
            clean_source,
        },
        capture_note,
    )
    .await;
    log::debug!(
        "distill_article[{trace_id}]: figures={} degraded={} transcript={}",
        media.figures.figures.len(),
        media.figures.degraded,
        distilled.transcript.is_some()
    );
    ArticleStage {
        distilled,
        figures: media.figures,
    }
}
